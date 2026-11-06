//! The TCP server: non-blocking sockets + `poll()` event loop + timers,
//! ported from `server.h` / `server.cpp`.
//!
//! The server is a struct instead of `g_data`; the idle list is index based
//! rather than intrusive. The event-loop shape, buffering, pipelining and timer
//! ordering match the C++ implementation, including refreshing a connection's
//! idle timer *after* its IO is processed.

use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, RawFd};
use std::os::raw::{c_int, c_ulong};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use crate::buffer::{buf_append, buf_consume, Buffer};
use crate::clock::monotonic_ms;
use crate::database::Database;
use crate::list::DList;
use crate::protocol::{parse_req, response_begin, response_end, K_MAX_MSG};
use crate::thread_pool::ThreadPool;

#[repr(C)]
struct PollFd {
    fd: RawFd,
    events: i16,
    revents: i16,
}

const POLLIN: i16 = 0x001;
const POLLOUT: i16 = 0x004;
const POLLERR: i16 = 0x008;
const INT32_MAX: u64 = i32::MAX as u64;

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
}

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// 0 lets the OS pick an ephemeral port (see [`Server::port`]).
    pub port: u16,
    /// Close connections idle for this long.
    pub idle_timeout_ms: u64,
    /// Thread pool size for freeing large values.
    pub worker_threads: usize,
    /// Log connections to stderr.
    pub verbose: bool,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            port: 1234,
            idle_timeout_ms: 5 * 1000,
            worker_threads: 4,
            verbose: true,
        }
    }
}

struct Conn {
    stream: std::net::TcpStream,
    // application's intention, for the event loop
    want_read: bool,
    want_write: bool,
    want_close: bool,
    // buffered input and output
    incoming: Buffer,
    outgoing: Buffer,
    // idle timer
    last_active_ms: u64,
    idle_node: usize,
}

/// Stopping half of a [`Server`], usable from another thread.
pub struct StopHandle {
    wake_write: UnixStream,
}

impl StopHandle {
    pub fn stop(&self) {
        let mut stream = &self.wake_write;
        let _ = stream.write(&[1u8]);
    }
}

/// The running server.
pub struct Server {
    opts: ServerOptions,
    listener: TcpListener,
    wake_read: UnixStream,
    wake_write: UnixStream,
    bound_port: u16,
    db: Database,
    fd2conn: Vec<Option<Conn>>,
    idle: DList,
}

impl Server {
    /// Create, bind and start listening. Use port 0 for an ephemeral port.
    pub fn new(opts: ServerOptions) -> io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", opts.port))?;
        listener.set_nonblocking(true)?;
        let bound_port = listener.local_addr()?.port();

        let (wake_read, wake_write) = UnixStream::pair()?;
        wake_read.set_nonblocking(true)?;
        wake_write.set_nonblocking(true)?;

        let pool = Arc::new(ThreadPool::new(opts.worker_threads));
        let db = Database::with_pool(Some(Arc::clone(&pool)));

        Ok(Self {
            opts,
            listener,
            wake_read,
            wake_write,
            bound_port,
            db,
            fd2conn: Vec::new(),
            idle: DList::new(),
        })
    }

    /// The actual listening port (useful with port 0).
    pub fn port(&self) -> u16 {
        self.bound_port
    }

    /// Ask `run()` to return. Async-signal-safe: writes one byte to a pipe.
    pub fn stop(&self) {
        let mut stream = &self.wake_write;
        let _ = stream.write(&[1u8]);
    }

    /// Raw fd written by [`stop`](Self::stop); for signal handlers.
    pub fn wake_fd(&self) -> RawFd {
        self.wake_write.as_raw_fd()
    }

    /// A cloneable handle that can stop the server from another thread.
    pub fn stop_handle(&self) -> io::Result<StopHandle> {
        Ok(StopHandle {
            wake_write: self.wake_write.try_clone()?,
        })
    }

    fn handle_accept(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((stream, addr)) => {
                    if let Err(e) = stream.set_nonblocking(true) {
                        if self.opts.verbose {
                            eprintln!("set_nonblocking() error: {e}");
                        }
                        continue;
                    }
                    let fd = stream.as_raw_fd() as usize;
                    if self.opts.verbose {
                        eprintln!("new client from {addr}");
                    }
                    let idle_node = self.idle.push_back(fd);
                    let conn = Conn {
                        stream,
                        want_read: true,
                        want_write: false,
                        want_close: false,
                        incoming: Buffer::new(),
                        outgoing: Buffer::new(),
                        last_active_ms: monotonic_ms(),
                        idle_node,
                    };
                    if self.fd2conn.len() <= fd {
                        self.fd2conn.resize_with(fd + 1, || None);
                    }
                    debug_assert!(self.fd2conn[fd].is_none());
                    self.fd2conn[fd] = Some(conn);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    if self.opts.verbose {
                        eprintln!("accept() error: {e}");
                    }
                    break;
                }
            }
        }
    }

    fn conn_destroy(&mut self, fd: usize) {
        if let Some(conn) = self.fd2conn[fd].take() {
            self.idle.detach(conn.idle_node);
            drop(conn); // closes the socket
        }
    }

    // Process one request if there is enough data.
    fn try_one_request(&mut self, fd: usize) -> bool {
        let Some(conn) = self.fd2conn[fd].as_mut() else {
            return false;
        };
        if conn.incoming.len() < 4 {
            return false; // want read
        }
        let mut header = [0u8; 4];
        header.copy_from_slice(&conn.incoming[..4]);
        let len = u32::from_ne_bytes(header) as usize;
        if len > K_MAX_MSG {
            conn.want_close = true;
            return false;
        }
        if 4 + len > conn.incoming.len() {
            return false; // want read
        }

        let mut cmd = Vec::new();
        if !parse_req(&conn.incoming[4..4 + len], &mut cmd) {
            conn.want_close = true; // bad request
            return false;
        }

        let header_pos = response_begin(&mut conn.outgoing);
        self.db.execute(&mut cmd, &mut conn.outgoing);
        response_end(&mut conn.outgoing, header_pos);

        // remove only this request: more may follow (pipelining)
        buf_consume(&mut conn.incoming, 4 + len);
        true
    }

    fn handle_write(&mut self, fd: usize) {
        let Some(conn) = self.fd2conn[fd].as_mut() else {
            return;
        };
        if conn.outgoing.is_empty() {
            return;
        }
        match conn.stream.write(&conn.outgoing) {
            Ok(0) => conn.want_close = true,
            Ok(n) => {
                buf_consume(&mut conn.outgoing, n);
                if conn.outgoing.is_empty() {
                    conn.want_read = true;
                    conn.want_write = false;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
            Err(_) => conn.want_close = true,
        }
    }

    fn handle_read(&mut self, fd: usize) {
        let mut buf = [0u8; 64 * 1024];
        let result = match self.fd2conn[fd].as_mut() {
            Some(conn) => conn.stream.read(&mut buf),
            None => return,
        };
        match result {
            Ok(0) => {
                if let Some(conn) = self.fd2conn[fd].as_mut() {
                    conn.want_close = true; // EOF
                }
                return;
            }
            Ok(n) => {
                if let Some(conn) = self.fd2conn[fd].as_mut() {
                    buf_append(&mut conn.incoming, &buf[..n]);
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {
                return;
            }
            Err(_) => {
                if let Some(conn) = self.fd2conn[fd].as_mut() {
                    conn.want_close = true;
                }
                return;
            }
        }

        while self.try_one_request(fd) {}

        let has_output = self.fd2conn[fd]
            .as_ref()
            .is_some_and(|conn| !conn.outgoing.is_empty());
        if has_output {
            if let Some(conn) = self.fd2conn[fd].as_mut() {
                conn.want_read = false;
                conn.want_write = true;
            }
            // Optimistic: the socket is probably writable immediately.
            self.handle_write(fd);
        }
    }

    // poll() timeout until the nearest timer, or -1 for none.
    fn next_timer_ms(&mut self) -> i32 {
        let now_ms = monotonic_ms();
        let mut next_ms = u64::MAX;

        if let Some(fd) = self.idle.front() {
            if let Some(conn) = self.fd2conn.get(fd).and_then(Option::as_ref) {
                next_ms = conn.last_active_ms + self.opts.idle_timeout_ms;
            }
        }
        if let Some(expiry) = self.db.next_expiry_ms() {
            next_ms = next_ms.min(expiry);
        }
        if next_ms == u64::MAX {
            return -1;
        }
        if next_ms <= now_ms {
            return 0;
        }
        (next_ms - now_ms).min(INT32_MAX) as i32
    }

    fn process_timers(&mut self) {
        let now_ms = monotonic_ms();
        while let Some(fd) = self.idle.front() {
            let expired = self
                .fd2conn
                .get(fd)
                .and_then(Option::as_ref)
                .is_none_or(|conn| conn.last_active_ms + self.opts.idle_timeout_ms < now_ms);
            if !expired {
                break; // the list is ordered: nothing else has expired
            }
            if self.opts.verbose {
                eprintln!("removing idle connection: {fd}");
            }
            self.conn_destroy(fd);
        }
        let _ = self.db.process_expired(2000);
    }

    /// Run the event loop until [`stop`](Self::stop) is called.
    pub fn run(&mut self) -> io::Result<()> {
        let mut pollfds: Vec<PollFd> = Vec::new();

        loop {
            pollfds.clear();
            pollfds.push(PollFd {
                fd: self.listener.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            });
            pollfds.push(PollFd {
                fd: self.wake_read.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            });
            for (fd, slot) in self.fd2conn.iter().enumerate() {
                let Some(conn) = slot else { continue };
                let mut events = POLLERR;
                if conn.want_read {
                    events |= POLLIN;
                }
                if conn.want_write {
                    events |= POLLOUT;
                }
                pollfds.push(PollFd {
                    fd: fd as RawFd,
                    events,
                    revents: 0,
                });
            }

            let timeout = self.next_timer_ms();
            let rv = unsafe { poll(pollfds.as_mut_ptr(), pollfds.len() as c_ulong, timeout) };
            if rv < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if pollfds[1].revents != 0 {
                return Ok(()); // stop() was called
            }
            if pollfds[0].revents != 0 {
                self.handle_accept();
            }

            let ready: Vec<(usize, i16)> = pollfds[2..]
                .iter()
                .filter(|pfd| pfd.revents != 0)
                .map(|pfd| (pfd.fd as usize, pfd.revents))
                .collect();

            for (fd, events) in ready {
                if self.fd2conn.get(fd).and_then(Option::as_ref).is_none() {
                    continue;
                }
                if events & POLLIN != 0 {
                    self.handle_read(fd);
                }
                let can_write = self
                    .fd2conn
                    .get(fd)
                    .and_then(Option::as_ref)
                    .is_some_and(|conn| !conn.want_close);
                if events & POLLOUT != 0 && can_write {
                    self.handle_write(fd);
                }
                let close = self
                    .fd2conn
                    .get(fd)
                    .and_then(Option::as_ref)
                    .is_some_and(|conn| events & POLLERR != 0 || conn.want_close);
                if close {
                    self.conn_destroy(fd);
                    continue;
                }
                // Refresh AFTER the IO work (the book refreshes before, so a slow request
                // could get its connection reaped).
                let idle_node = self.fd2conn[fd].as_mut().map(|conn| {
                    conn.last_active_ms = monotonic_ms();
                    conn.idle_node
                });
                if let Some(idle_node) = idle_node {
                    self.idle.move_to_back(idle_node);
                }
            }

            self.process_timers();
        }
    }
}
