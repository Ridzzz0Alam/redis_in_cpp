//! The TCP server: non-blocking sockets + `poll()` event loop + timers,
//! ported from `server.h` / `server.cpp`.
//!
//! The server is a struct instead of `g_data`; the idle list is index based
//! rather than intrusive. The event-loop shape, pipelining and timer ordering
//! follow the C++ implementation, including refreshing a connection's idle
//! timer *after* its IO is processed.
//!
//! Resource limits (none of which the original had):
//! * a connection's pending output is capped (`max_pending_output`): once it
//!   is exceeded, buffered requests wait until the client reads its replies,
//!   so one client cannot make the server allocate unbounded reply memory;
//! * at most `max_clients` connections are kept;
//! * when `accept()` fails for lack of resources (e.g. `EMFILE`), accepting is
//!   paused briefly instead of busy-looping on a permanently readable listener.

use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::os::fd::{AsRawFd, RawFd};
use std::os::raw::{c_int, c_ulong};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use crate::buffer::ByteQueue;
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
const POLLHUP: i16 = 0x010;
const POLLNVAL: i16 = 0x020;
const INT32_MAX: u64 = i32::MAX as u64;

/// How long to stop accepting after `accept()` fails for lack of resources.
const ACCEPT_BACKOFF_MS: u64 = 100;
/// Keys expired per event-loop iteration.
const EXPIRE_WORK: usize = 2000;

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
}

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// Address to listen on. Defaults to loopback: the server has no
    /// authentication, so exposing it on every interface must be explicit.
    pub bind_addr: IpAddr,
    /// 0 lets the OS pick an ephemeral port (see [`Server::port`]).
    pub port: u16,
    /// Close connections idle for this long.
    pub idle_timeout_ms: u64,
    /// Thread pool size for freeing large values.
    pub worker_threads: usize,
    /// Log connections to stderr.
    pub verbose: bool,
    /// Maximum simultaneous client connections; extra ones are closed.
    pub max_clients: usize,
    /// Stop executing a client's buffered requests while this many reply
    /// bytes are waiting to be sent to it (one reply may overshoot it).
    pub max_pending_output: usize,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 1234,
            idle_timeout_ms: 5 * 1000,
            worker_threads: 4,
            verbose: true,
            max_clients: 10_000,
            max_pending_output: 4 << 20, // 4 MiB
        }
    }
}

struct Conn {
    stream: std::net::TcpStream,
    // application's intention, for the event loop
    want_read: bool,
    want_write: bool,
    want_close: bool,
    /// Protocol error seen: send what is already queued, then close.
    close_after_flush: bool,
    // buffered input and output
    incoming: ByteQueue,
    outgoing: ByteQueue,
    // idle timer
    last_active_ms: u64,
    idle_node: usize,
}

impl Conn {
    /// Derive the poll interest from the buffers. While replies are pending
    /// we stop reading (backpressure); once they are flushed we either read
    /// again or, after a protocol error, close.
    fn update_interest(&mut self) {
        if self.outgoing.is_empty() {
            self.want_write = false;
            self.want_read = !self.close_after_flush;
            if self.close_after_flush {
                self.want_close = true;
            }
        } else {
            self.want_write = true;
            self.want_read = false;
        }
    }
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
    num_conns: usize,
    idle: DList,
    /// Accepting is paused until this time after a resource error.
    accept_paused_until: Option<u64>,
}

impl Server {
    /// Create, bind and start listening. Use port 0 for an ephemeral port.
    pub fn new(opts: ServerOptions) -> io::Result<Self> {
        let listener = TcpListener::bind((opts.bind_addr, opts.port))?;
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
            num_conns: 0,
            idle: DList::new(),
            accept_paused_until: None,
        })
    }

    /// The actual listening port (useful with port 0).
    pub fn port(&self) -> u16 {
        self.bound_port
    }

    /// Number of open client connections.
    pub fn num_clients(&self) -> usize {
        self.num_conns
    }

    /// Ask `run()` to return by writing one byte to the wake pipe.
    pub fn stop(&self) {
        let mut stream = &self.wake_write;
        let _ = stream.write(&[1u8]);
    }

    /// Raw fd written by [`stop`](Self::stop). A signal handler may
    /// `write(2)` one byte to it (that call is async-signal-safe).
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
                    if self.num_conns >= self.opts.max_clients {
                        if self.opts.verbose {
                            eprintln!("rejecting {addr}: max clients reached");
                        }
                        continue; // dropping `stream` closes it
                    }
                    if let Err(e) = stream.set_nonblocking(true) {
                        if self.opts.verbose {
                            eprintln!("set_nonblocking() error: {e}");
                        }
                        continue;
                    }
                    // Replies are written in one go; don't let Nagle hold back
                    // the tail of a reply waiting for the client's ACK.
                    let _ = stream.set_nodelay(true);
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
                        close_after_flush: false,
                        incoming: ByteQueue::new(),
                        outgoing: ByteQueue::new(),
                        last_active_ms: monotonic_ms(),
                        idle_node,
                    };
                    if self.fd2conn.len() <= fd {
                        self.fd2conn.resize_with(fd + 1, || None);
                    }
                    debug_assert!(self.fd2conn[fd].is_none());
                    self.fd2conn[fd] = Some(conn);
                    self.num_conns += 1;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e)
                    if e.kind() == ErrorKind::Interrupted
                        || e.kind() == ErrorKind::ConnectionAborted =>
                {
                    continue
                }
                Err(e) => {
                    // EMFILE / ENFILE / ENOBUFS / ENOMEM: the connection stays
                    // in the backlog, so the listener remains readable and
                    // poll() would return immediately forever. Back off.
                    if self.opts.verbose {
                        eprintln!("accept() error: {e}; pausing accepts");
                    }
                    self.accept_paused_until = Some(monotonic_ms() + ACCEPT_BACKOFF_MS);
                    break;
                }
            }
        }
    }

    fn conn_destroy(&mut self, fd: usize) {
        if let Some(conn) = self.fd2conn[fd].take() {
            self.idle.detach(conn.idle_node);
            self.num_conns -= 1;
            drop(conn); // closes the socket
        }
    }

    // Process one request if there is enough data.
    fn try_one_request(&mut self, fd: usize) -> bool {
        let Some(conn) = self.fd2conn[fd].as_mut() else {
            return false;
        };
        let data = conn.incoming.data();
        if data.len() < 4 {
            return false; // want read
        }
        let mut header = [0u8; 4];
        header.copy_from_slice(&data[..4]);
        let len = u32::from_ne_bytes(header) as usize;
        if len > K_MAX_MSG {
            conn.close_after_flush = true;
            return false;
        }
        if 4 + len > data.len() {
            return false; // want read
        }

        let mut cmd = Vec::new();
        if !parse_req(&data[4..4 + len], &mut cmd) {
            conn.close_after_flush = true; // bad request
            return false;
        }

        let out = conn.outgoing.writer();
        let header_pos = response_begin(out);
        self.db.execute(&mut cmd, out);
        response_end(out, header_pos);

        // remove only this request: more may follow (pipelining)
        conn.incoming.consume(4 + len);
        true
    }

    /// Execute buffered requests until input runs out, the connection is
    /// closing, or too much output is pending; then update poll interest.
    fn process_incoming(&mut self, fd: usize) {
        loop {
            let Some(conn) = self.fd2conn[fd].as_ref() else {
                return;
            };
            if conn.want_close
                || conn.close_after_flush
                || conn.outgoing.len() >= self.opts.max_pending_output
            {
                break;
            }
            if !self.try_one_request(fd) {
                break;
            }
        }
        if let Some(conn) = self.fd2conn[fd].as_mut() {
            conn.update_interest();
        }
    }

    fn handle_write(&mut self, fd: usize) {
        let Some(conn) = self.fd2conn[fd].as_mut() else {
            return;
        };
        if conn.outgoing.is_empty() {
            return;
        }
        match conn.stream.write(conn.outgoing.data()) {
            Ok(0) => conn.want_close = true,
            Ok(n) => {
                conn.outgoing.consume(n);
                if conn.outgoing.is_empty() {
                    // Requests may have been held back by the output limit.
                    self.process_incoming(fd);
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
        let Some(conn) = self.fd2conn[fd].as_mut() else {
            return;
        };
        match result {
            Ok(0) => {
                conn.want_close = true; // EOF
                return;
            }
            Ok(n) => conn.incoming.append(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {
                return;
            }
            Err(_) => {
                conn.want_close = true;
                return;
            }
        }

        self.process_incoming(fd);

        let has_output = self.fd2conn[fd]
            .as_ref()
            .is_some_and(|conn| !conn.outgoing.is_empty());
        if has_output {
            // Optimistic: the socket is probably writable immediately.
            self.handle_write(fd);
        }
    }

    fn accept_paused(&self, now_ms: u64) -> bool {
        self.accept_paused_until.is_some_and(|until| now_ms < until)
    }

    // poll() timeout until the nearest timer, or -1 for none.
    fn next_timer_ms(&mut self) -> i32 {
        let now_ms = monotonic_ms();
        let mut next_ms = u64::MAX;

        if let Some(fd) = self.idle.front() {
            if let Some(conn) = self.fd2conn.get(fd).and_then(Option::as_ref) {
                next_ms = conn.last_active_ms + self.opts.idle_timeout_ms;
            } else {
                next_ms = now_ms; // stale node: let process_timers drop it
            }
        }
        if let Some(expiry) = self.db.next_expiry_ms() {
            next_ms = next_ms.min(expiry);
        }
        if let Some(until) = self.accept_paused_until {
            next_ms = next_ms.min(until);
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
        while let (Some(node), Some(fd)) = (self.idle.front_id(), self.idle.front()) {
            let Some(conn) = self.fd2conn.get(fd).and_then(Option::as_ref) else {
                // A node without a connection should not exist; drop it rather
                // than looping forever on it.
                self.idle.detach(node);
                continue;
            };
            // `<=` matches next_timer_ms(), which returns 0 once the deadline
            // is reached; `<` would busy-poll until the next millisecond.
            if conn.last_active_ms + self.opts.idle_timeout_ms > now_ms {
                break; // the list is ordered: nothing else has expired
            }
            if self.opts.verbose {
                eprintln!("removing idle connection: {fd}");
            }
            self.conn_destroy(fd);
        }
        if !self.accept_paused(now_ms) {
            self.accept_paused_until = None;
        }
        let _ = self.db.process_expired(EXPIRE_WORK);
    }

    /// Run the event loop until [`stop`](Self::stop) is called.
    pub fn run(&mut self) -> io::Result<()> {
        let mut pollfds: Vec<PollFd> = Vec::new();

        loop {
            pollfds.clear();
            pollfds.push(PollFd {
                fd: self.listener.as_raw_fd(),
                events: if self.accept_paused(monotonic_ms()) {
                    0
                } else {
                    POLLIN
                },
                revents: 0,
            });
            pollfds.push(PollFd {
                fd: self.wake_read.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            });
            for (fd, slot) in self.fd2conn.iter().enumerate() {
                let Some(conn) = slot else { continue };
                let mut events = 0;
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
            // SAFETY: `pollfds` is a valid, exclusively borrowed array of
            // `#[repr(C)]` pollfd structs whose length is passed alongside.
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
            if pollfds[0].revents & POLLIN != 0 {
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
                if events & (POLLERR | POLLNVAL) != 0 {
                    self.conn_destroy(fd);
                    continue;
                }
                // On Linux, EOF is reported as POLLIN (read returns 0) and a
                // fully closed peer as POLLHUP; reading handles both.
                if events & (POLLIN | POLLHUP) != 0 {
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
                // POLLHUP: the peer can no longer receive anything we send.
                let close = self
                    .fd2conn
                    .get(fd)
                    .and_then(Option::as_ref)
                    .is_some_and(|conn| events & POLLHUP != 0 || conn.want_close);
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
