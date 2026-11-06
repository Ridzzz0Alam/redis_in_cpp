//! Server entry point. Ported from `apps/redis_server.cpp`: CLI options and
//! graceful shutdown via a self-pipe. (SIGPIPE needs no handling: the Rust
//! runtime ignores it, so a write to a closed socket returns `EPIPE`.)

use std::net::SocketAddr;
use std::os::raw::{c_int, c_void};
use std::process::ExitCode;
use std::sync::atomic::{AtomicI32, Ordering};

use redis_rust::server::{Server, ServerOptions};

const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;

/// C `sighandler_t`. It is declared as a plain integer, not as
/// `extern "C" fn`, because `signal()` returns `SIG_DFL` (0, a null function
/// pointer) or `SIG_ERR` (-1); materializing either as a Rust `fn` pointer,
/// which must be non-null and valid, is undefined behaviour.
type SigHandlerT = usize;
const SIG_ERR: SigHandlerT = usize::MAX;

unsafe extern "C" {
    fn signal(sig: c_int, handler: SigHandlerT) -> SigHandlerT;
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
}

// The signal handler needs to reach the running server's wake pipe. A single
// atomic fd + write() is async-signal-safe.
static WAKE_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(_sig: c_int) {
    let fd = WAKE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = 1u8;
        // SAFETY: writing one byte to a pipe fd is async-signal-safe.
        unsafe {
            let _ = write(fd, (&byte as *const u8).cast::<c_void>(), 1);
        }
    }
}

fn install_signal_handlers() -> Result<(), String> {
    let handler = on_signal as extern "C" fn(c_int) as SigHandlerT;
    for sig in [SIGINT, SIGTERM] {
        // SAFETY: `handler` is a valid `extern "C" fn(c_int)` that only
        // performs an async-signal-safe write(); the return value is only
        // compared, never called.
        let prev = unsafe { signal(sig, handler) };
        if prev == SIG_ERR {
            return Err(format!("signal({sig}) failed"));
        }
    }
    Ok(())
}

fn parse_options(args: &[String]) -> Option<ServerOptions> {
    let mut opts = ServerOptions::default();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--quiet" {
            opts.verbose = false;
            i += 1;
            continue;
        }
        let value = args.get(i + 1)?;
        match arg {
            "--bind" => opts.bind_addr = value.parse().ok()?,
            "--port" => opts.port = value.parse().ok()?,
            "--max-clients" => {
                opts.max_clients = value.parse().ok()?;
                if opts.max_clients == 0 {
                    return None;
                }
            }
            "--idle-timeout-ms" => opts.idle_timeout_ms = value.parse().ok()?,
            "--threads" => {
                opts.worker_threads = value.parse().ok()?;
                if opts.worker_threads == 0 {
                    return None;
                }
            }
            _ => return None,
        }
        i += 2;
    }
    Some(opts)
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(opts) = parse_options(&args) else {
        return Err(
            "usage: redis_server [--bind ADDR] [--port N] [--idle-timeout-ms N] [--threads N] \
             [--max-clients N] [--quiet]"
                .to_string(),
        );
    };

    let bind_addr = opts.bind_addr;
    let mut server = Server::new(opts).map_err(|e| e.to_string())?;
    WAKE_FD.store(server.wake_fd(), Ordering::Relaxed);
    install_signal_handlers()?;
    eprintln!("listening on {}", SocketAddr::new(bind_addr, server.port()));
    let result = server.run().map_err(|e| e.to_string());
    WAKE_FD.store(-1, Ordering::Relaxed);
    eprintln!("shutting down");
    result
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fatal: {e}");
            ExitCode::FAILURE
        }
    }
}
