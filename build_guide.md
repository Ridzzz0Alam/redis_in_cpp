# Build Your Own Redis in Rust — Build Guide

A step-by-step reference for rebuilding this repository's Rust Redis clone from an
empty Cargo project, while understanding every concept along the way.

The algorithms and command semantics come from *Build Your Own Redis with C/C++* by
James Smith, via this repository's former C++23 port. The C++ tree has been removed;
this guide and `src/` are now the source of truth.

---

## 0. How to use this guide

### 0.1 Code markers

| Marker | Meaning |
|---|---|
| `// PORTED FROM C++ (...)` | The algorithm, names and structure come from the original C++ file. |
| `// RUST CHANGE` | A deliberate difference forced by Rust's ownership/safety model. |
| `// ADDED` | Code written for this Rust port: helpers, tests, CLI/CI, benchmarks. |
| `// SAFETY:` | Justification for the few `unsafe` blocks (`poll`, signals). |

The port is deliberately conservative about **algorithms** and deliberately
different about **memory layout**:

- The C++ structures were intrusive: an `HNode` lived *inside* the payload and
  `container_of()` recovered the owner with pointer arithmetic.
- Rust cannot safely store arbitrary self-referential back-pointers. Every
  container here stores nodes in a stable `Vec<Option<T>>` **arena** and links them
  by `usize` index. The chaining, rotations, progressive rehashing and heap
  back-references are unchanged.

This preserves the lesson — you still build the tables, trees and heaps yourself —
without unsafe pointer arithmetic in the data structures.

### 0.2 Everything here was compiled and tested

The crate was built and checked with a recent stable Rust toolchain:

- `cargo test`: 55 tests pass (unit + integration).
- `cargo test --release`: the same 55 pass optimized.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo fmt --check`: clean.
- End-to-end transcript (`tests/e2e/test_cmds.py`): 15 commands pass.
- Extra checks: 2,000 pipelined requests in one write, an 8 MiB value, TTL expiry,
  the 500 ms idle timeout (tests), and deleting a 1,500-member sorted set through the
  thread pool.

### 0.3 Map: book chapters → guide stages

| Book folder | Stage | What you build | New concepts |
|---|---|---|---|
| `03/` | 1 | Hello-world TCP server/client | sockets, `TcpListener`, `TcpStream` |
| `04/` | 2 | Length-prefixed protocol | TCP is a byte stream, framing, partial reads |
| `06/` | 3 | Event loop (echo) | non-blocking IO, `poll(2)`, pipelining |
| `07/` | 4 | `get/set/del` | request/response serialization |
| `08/` | 5 | Own hashtable | chaining, arena nodes, progressive rehashing |
| `09/` | 6 | Typed responses + `keys` | TLV serialization, recursive parsing |
| `10/` | 7 | AVL tree | balanced BSTs, rotations, subtree counts |
| `11/` | 8 | Sorted sets | dual indexing, rank queries in O(log n) |
| `12/` | 9 | Idle-connection timeouts | monotonic clocks, timer lists |
| `13/` | 10 | Key TTLs | binary heaps with back-references |
| `14/` | 11 | Async deletion | thread pools, mutex + condition variable |

The book has no `05/` folder; chapter 5 is event-loop theory, covered in Stage 3.

### 0.4 Where the files are

```
.
├── Cargo.toml
├── src/
│   ├── lib.rs             module exports
│   ├── common.rs          FNV-1 hash
│   ├── buffer.rs          Buffer + append helpers
│   ├── clock.rs           monotonic milliseconds
│   ├── list.rs            index-based circular list (idle timers)
│   ├── hashtable.rs       chained hashtable, progressive rehash
│   ├── avl.rs             AVL tree with subtree counts
│   ├── zset.rs            sorted set = hashtable + AVL arena
│   ├── heap.rs            binary min-heap with back-references
│   ├── thread_pool.rs     mutex + condvar worker pool
│   ├── protocol.rs        request parser + TLV codec
│   ├── database.rs        key space and command handlers
│   ├── net.rs             blocking client helpers
│   ├── server.rs          non-blocking poll() event loop
│   └── bin/
│       ├── redis_server.rs
│       ├── redis_cli.rs
│       ├── redis_bench.rs
│       └── bench_hashtable.rs
└── tests/                 one file per C++ test suite + e2e transcript
```

The package lives at the repository root, so every command in this guide can be
run from there.

---

## 1. Setup

### 1.1 Toolchain

This project uses POSIX sockets and `poll(2)`, so build on **Linux** (WSL2 also
works). You need a recent stable Rust toolchain; the port was tested with
`rustc 1.99`:

```bash
rustup toolchain install stable
rustup component add rustfmt clippy
```

There are **no third-party crates**. The event loop declares `poll(2)` with a tiny
`unsafe extern "C"` block; everything else is `std`.

### 1.2 Project layout

`Cargo.toml` (repository root):

```toml
[package]
name = "redis-rust"
version = "1.0.0"
edition = "2021"

[lib]
name = "redis_rust"
path = "src/lib.rs"

[[bin]]
name = "redis_server"
path = "src/bin/redis_server.rs"

[[bin]]
name = "redis_cli"
path = "src/bin/redis_cli.rs"

[[bin]]
name = "redis_bench"
path = "src/bin/redis_bench.rs"

[[bin]]
name = "bench_hashtable"
path = "src/bin/bench_hashtable.rs"

[dependencies]
```

The release profile at the bottom of that file is:

```toml
[profile.release]
debug = true
```

`src/lib.rs`:

```rust
pub mod avl;
pub mod buffer;
pub mod clock;
pub mod common;
pub mod database;
pub mod hashtable;
pub mod heap;
pub mod list;
pub mod net;
pub mod protocol;
pub mod server;
pub mod thread_pool;
pub mod zset;

pub use buffer::Buffer;
```

### 1.3 Building and running

```bash
cargo build --release
./target/release/redis_server --port 1234 &
./target/release/redis_cli set k v
./target/release/redis_cli get k
```

Tests and quality gates:

```bash
cargo test
cargo test --release
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run --release --bin bench_hashtable -- 5000000
```

End-to-end:

```bash
python3 tests/e2e/test_cmds.py \
  --server target/release/redis_server \
  --cli target/release/redis_cli
```

**Tip — checking memory.** The data structures are ordinary safe Rust, so the
borrow checker already rules out most lifetime bugs. The only `unsafe` code is the
`poll(2)` declaration and the signal handler's `write(2)` call. `cargo miri test`
can be used to check those code paths (Miri does not currently execute the real
`poll` syscall, so run the non-server tests under it).

---

## 2. The big picture

The final server is a single-threaded event loop (plus helper threads for one job)
that owns an in-memory database:

```text
            clients (TCP, port 1234)
                    │
         ┌──────────▼───────────┐
         │   poll() event loop  │  one thread, non-blocking sockets
         │  ┌────────────────┐  │
         │  │ Conn per client│  │  incoming/outgoing byte buffers
         │  └───────┬────────┘  │
         │  parse → execute → serialize
         │          │           │
         │   ┌──────▼───────┐   │
         │   │ HMap<Entry>  │   │  key → Entry { bytes | ZSet }
         │   └──────────────┘   │
         │   ZSet = HMap<ZNode> + AVL root index
         │   timers: DList (idle conns) + Heap (key TTLs)
         └──────────┬───────────┘
                    │ big deletes
             ┌──────▼──────┐
             │ thread pool │  frees large sorted sets off the main thread
             └─────────────┘
```

Key design rule: **the main thread must never block or do unbounded work**, because
while it is busy, *every* client waits. That rule explains non-blocking IO (Stage
3), progressive rehashing (Stage 5), bounded timer processing (Stage 10), and the
thread pool (Stage 11).

---

## 3. Why the Rust design differs: arenas instead of intrusive pointers

This section is the mental bridge from the C++ guide; read it before Stage 5.

The C++ payloads looked like this:

```cpp
struct Entry {
    HNode node;          // links live inside the payload
    std::string key;
    std::string val;
};
```

and `container_of(node, Entry, node)` recovered the `Entry*`. In Rust, moving
`Entry` values into a `Vec` would invalidate any pointer to an embedded `HNode`, and
safe Rust has no sanctioned `container_of`.

The port instead keeps every payload in a **stable arena**:

```rust
pub struct HMap<V> {
    nodes: Vec<Option<V>>,     // stable indices; some slots are free
    free: Vec<usize>,          // recycled slots
    newer: Table,
    older: Option<Table>,
    len: usize,
    migrate_pos: usize,
}
```

A link is an `Option<usize>` index rather than a pointer:

```rust
pub trait HashNode {
    fn key(&self) -> &[u8];
    fn hcode(&self) -> u64;
    fn set_hcode(&mut self, hcode: u64);
    fn hnext(&self) -> Option<usize>;
    fn set_hnext(&mut self, next: Option<usize>);
}
```

The important payoff survives: **one node can be indexed by two containers**. A
`ZNode` implements both `HashNode` and `AvlNode`, and the sorted set keeps both the
hashtable and the AVL root pointing into the same `HMap<ZNode>` arena. That is what
the C++ intrusive design bought, expressed safely.

Offsets (`container_of`) become ordinary index lookups:

```text
C++:  container_of(hnode, Entry, node)        -> Entry*
Rust: the index you already have              -> &Entry via map.get(idx)
```

---

## Stage 1 — Sockets 101

### Concepts

**A socket is a file descriptor.** On Linux a TCP connection is an integer handle
that you read and write. Rust's `std::net` wraps the same syscalls:

1. `TcpListener::bind(addr)` — create + bind the socket (`SO_REUSEADDR` is set by
   std).
2. `listener.listen` happens as part of `bind`; the OS queues completed handshakes.
3. `listener.accept()` — take one connection; returns a `TcpStream` for the client
   while the listener keeps listening.
4. The client uses `TcpStream::connect(addr)`.

**Byte order.** Network protocols use big-endian. `u16::to_be_bytes()` /
`u16::from_be_bytes()` state that intent. This project keeps the book's shortcut on
the wire (native byte order for lengths), documented in Appendix C.

**Addresses.** Rust's `SocketAddr` replaces `sockaddr_in` + pointer casts:

```rust
let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 1234)); // 0.0.0.0:1234
```

### A minimal server and client

```rust
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};

fn server() -> std::io::Result<()> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 1234)))?;
    loop {
        let (mut stream, _addr) = listener.accept()?;
        let mut buf = [0u8; 64];
        let n = stream.read(&mut buf)?;
        eprintln!("client says: {}", String::from_utf8_lossy(&buf[..n]));
        stream.write_all(b"world")?;
    }
}

fn client() -> std::io::Result<()> {
    let mut stream = TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, 1234)))?;
    stream.write_all(b"hello")?;
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf)?;
    println!("server says: {}", String::from_utf8_lossy(&buf[..n]));
    Ok(())
}
```

### What is wrong with it (motivates Stage 2)

`read()` returns *whatever bytes have arrived so far*. Localhost usually delivers
the whole message, but nothing guarantees it. TCP is a byte stream.

---

## Stage 2 — A protocol: length-prefixed messages

### Concepts

**TCP is a byte stream, not a message stream.** Two `write`s may arrive as one
`read`, or one `write` may need many `read`s. Message boundaries are the
application's job.

**Framing.** Every message gets a 4-byte length header:

```text
+-----+------+-----+------+--------
| len | msg1 | len | msg2 | more...
+-----+------+-----+------+--------
```

**Partial reads and writes.** `Read::read` and `Write::write` may transfer fewer
bytes than requested, so loop until the requested count is reached. Rust's
`write_all` already loops; `read_full` is written explicitly.

**Limits.** `K_MAX_MSG` caps a message at 32 MiB. Never trust a client-supplied
length uncapped.

`src/net.rs`:

```rust
pub fn read_full(stream: &mut TcpStream, buf: &mut [u8]) -> bool {
    let mut pos = 0usize;
    while pos < buf.len() {
        match stream.read(&mut buf[pos..]) {
            Ok(0) => return false, // EOF
            Ok(n) => pos += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    true
}

pub fn write_all(stream: &mut TcpStream, buf: &[u8]) -> bool {
    stream.write_all(buf).is_ok()
}
```

A blocking server reads one framed message at a time:

```rust
fn one_request(stream: &mut TcpStream) -> bool {
    let mut header = [0u8; 4];
    if !read_full(stream, &mut header) {
        return false;
    }
    let len = u32::from_ne_bytes(header) as usize;
    if len > K_MAX_MSG {
        return false;
    }
    let mut body = vec![0u8; len];
    if !read_full(stream, &mut body) {
        return false;
    }
    // ... handle body, then write a framed reply ...
    true
}
```

### Limitation

`one_request` blocks in `read()`. Accepting and serving one client at a time means
a second client waits until the first disconnects. Stage 3 fixes that.

---

## Stage 3 — The event loop

### Concepts

**Ways to serve many clients:**

| Approach | Idea | Cost |
|---|---|---|
| Process/thread per connection | One blocking thread per client | Memory, context switches, locking |
| **Event loop** (Redis, nginx, Node.js) | One thread, non-blocking sockets, ask the OS who is ready | Code must never block |
| Hybrid | Event loop + workers for slow tasks | Stage 11 |

**Non-blocking mode.** `TcpStream::set_nonblocking(true)` makes reads/writes return
`ErrorKind::WouldBlock` instead of waiting. A slow client can no longer freeze the
loop.

**Readiness — `poll(2)`.** `std::net` does not expose `poll`, so the port declares
the C ABI directly:

```rust
#[repr(C)]
struct PollFd {
    fd: RawFd,
    events: i16,
    revents: i16,
}

const POLLIN: i16 = 0x001;
const POLLOUT: i16 = 0x004;
const POLLERR: i16 = 0x008;

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
}
```

`PollFd` matches Linux's `struct pollfd`; the raw fd comes from
`AsRawFd::as_raw_fd()`.

**The `Conn` state machine.** A request may arrive in pieces, so each connection
keeps:

- `incoming` — bytes read but not yet parsed,
- `outgoing` — responses generated but not yet written,
- `want_read` / `want_write` / `want_close` — the application's intention, which the
  loop converts into `poll` flags.

`src/server.rs`:

```rust
struct Conn {
    stream: TcpStream,
    want_read: bool,
    want_write: bool,
    want_close: bool,
    incoming: Buffer,
    outgoing: Buffer,
    last_active_ms: u64,
    idle_node: usize,
}
```

**Pipelining.** After reading, process requests in a loop. Each handled request
removes **only its own bytes** (`buf_consume(4 + len)`); the next request may
already be buffered behind it.

**Optimistic write.** After generating a response, call `handle_write` immediately:
the socket is almost always writable in a request/response protocol.

**`fd2conn`.** Linux hands out the lowest free fd, so `Vec<Option<Conn>>` indexed by
fd is an O(1) map from fd to connection.

`try_one_request` from `server.rs`:

```rust
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

    buf_consume(&mut conn.incoming, 4 + len);
    true
}
```

The loop, simplified:

```rust
let mut pollfds = Vec::new();
loop {
    pollfds.clear();
    pollfds.push(PollFd { fd: listener.as_raw_fd(), events: POLLIN, revents: 0 });
    pollfds.push(PollFd { fd: wake_read.as_raw_fd(), events: POLLIN, revents: 0 });
    for (fd, slot) in self.fd2conn.iter().enumerate() {
        let Some(conn) = slot else { continue };
        let mut events = POLLERR;
        if conn.want_read { events |= POLLIN; }
        if conn.want_write { events |= POLLOUT; }
        pollfds.push(PollFd { fd: fd as RawFd, events, revents: 0 });
    }

    let timeout = self.next_timer_ms();
    let rv = unsafe { poll(pollfds.as_mut_ptr(), pollfds.len() as c_ulong, timeout) };
    if rv < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == ErrorKind::Interrupted { continue; }
        return Err(err);
    }
    // ... accept, dispatch ready fds, process timers ...
}
```

---

## Stage 4 — A real protocol: commands

### Concepts

A request body is a list of byte strings:

```text
+------+-----+------+-----+------+-----+-----+------+
| nstr | len | str1 | len | str2 | ... | len | strn |
+------+-----+------+-----+------+-----+-----+------+
```

**Binary safety.** C++ `std::string` is a byte string, so this port uses
`Vec<Vec<u8>>`, not `Vec<String>`. Keys, values and sorted-set member names may
contain NUL or invalid UTF-8; only status/error text is UTF-8.

**Defensive parsing.** Every length is bounds-checked with `checked_add`, and
trailing bytes are rejected: a mismatch means the peer disagrees about the format,
so closing is safer.

`src/protocol.rs`:

```rust
pub fn parse_req(req: &[u8], out: &mut Vec<Vec<u8>>) -> bool {
    let mut cur = 0usize;
    let Some(nstr) = read_u32(req, &mut cur) else { return false };
    if nstr as usize > K_MAX_ARGS {
        return false; // safety limit
    }
    while out.len() < nstr as usize {
        let Some(len) = read_u32(req, &mut cur) else { return false };
        let len = len as usize;
        let Some(end) = cur.checked_add(len) else { return false };
        if end > req.len() {
            return false;
        }
        out.push(req[cur..end].to_vec());
        cur = end;
    }
    cur == req.len() // reject trailing garbage
}
```

`encode_request` (used by the client and benchmarks) writes the same layout:

```rust
pub fn encode_request(cmd: &[Vec<u8>]) -> Buffer {
    let mut len = 4usize;
    for s in cmd {
        len += 4 + s.len();
    }
    let mut buf = Buffer::with_capacity(4 + len);
    buf_append_u32(&mut buf, len as u32);
    buf_append_u32(&mut buf, cmd.len() as u32);
    for s in cmd {
        buf_append_u32(&mut buf, s.len() as u32);
        buf_append(&mut buf, s);
    }
    buf
}
```

Command dispatch in `database.rs` matches on `(arity, name)` and appends exactly one
serialized value:

```rust
pub fn execute(&mut self, cmd: &mut [Vec<u8>], out: &mut Buffer) {
    let name: &[u8] = cmd.first().map_or(&[], Vec::as_slice);
    match (cmd.len(), name) {
        (2, b"get") => self.do_get(cmd, out),
        (3, b"set") => self.do_set(cmd, out),
        (2, b"del") => self.do_del(cmd, out),
        // ... pexpire, pttl, keys, zadd, zrem, zscore, zquery ...
        _ => out_err(out, ERR_UNKNOWN, "unknown command."),
    }
}
```

---

## Stage 5 — Your own hashtable

### Concepts

**Why not `std::collections::HashMap`?** It is excellent, but when it grows it
rehashes every key at once — an O(n) pause during which no client is served with a
large table. Redis uses **progressive rehashing**, and so does this code.

**Chaining.** A table is an array of slots; each slot is the head of a singly linked
list of nodes whose hash lands there. Lookup = hash → slot → walk the short list.

**Power-of-two sizes.** With `n = 2^k`, `hash % n == hash & (n - 1)`. Store
`mask = n - 1` and use `hcode & mask`.

**Load factor.** The table tolerates up to 8 keys per slot on average before growing
(`MAX_LOAD_FACTOR`).

**Progressive rehashing.**

1. The current table becomes `older`; a new double-size table becomes `newer`.
2. Every lookup/insert/delete calls `help_rehashing()`, moving at most 128 nodes
   from `older` to `newer`.
3. Lookups/deletes check both tables; inserts always go to `newer`.
4. When `older` is empty, it is dropped.

`src/hashtable.rs`, the core:

```rust
const REHASHING_WORK: usize = 128;
const MAX_LOAD_FACTOR: usize = 8;

#[derive(Clone, Default)]
struct Table {
    slots: Vec<Option<usize>>, // slot -> arena index
    mask: usize,
    size: usize,
}

impl Table {
    fn new(n: usize) -> Self {
        debug_assert!(n > 0 && (n - 1) & n == 0);
        Self { slots: vec![None; n], mask: n - 1, size: 0 }
    }
}
```

Insert into a single table (arena indices instead of `HNode*`):

```rust
fn h_insert<V: HashNode>(nodes: &mut [Option<V>], table: &mut Table, idx: usize) {
    let hcode = nodes[idx].as_ref().expect("live node").hcode();
    let pos = hcode as usize & table.mask;
    let head = table.slots[pos];
    nodes[idx].as_mut().expect("live node").set_hnext(head);
    table.slots[pos] = Some(idx);
    table.size += 1;
}
```

Lookup compares the hash first (cheap filter) and then the key bytes:

```rust
fn h_lookup<V: HashNode>(
    nodes: &[Option<V>],
    table: &Table,
    key: &[u8],
    hcode: u64,
) -> Option<usize> {
    if table.slots.is_empty() {
        return None;
    }
    let pos = hcode as usize & table.mask;
    let mut cur = table.slots[pos];
    while let Some(idx) = cur {
        let node = nodes[idx].as_ref().expect("live node");
        if node.hcode() == hcode && node.key() == key {
            return Some(idx);
        }
        cur = node.hnext();
    }
    None
}
```

Migrating at most 128 nodes per call:

```rust
fn help_rehashing(&mut self) {
    let Some(mut older) = self.older.take() else { return };
    let mut nwork = 0;
    while nwork < REHASHING_WORK && older.size > 0 {
        let mut pos = self.migrate_pos;
        while pos < older.slots.len() && older.slots[pos].is_none() {
            pos += 1;
        }
        if pos >= older.slots.len() {
            self.migrate_pos = pos;
            break;
        }
        let idx = older.slots[pos].expect("non-empty slot");
        let next = self.nodes[idx].as_ref().expect("live node").hnext();
        older.slots[pos] = next;
        older.size -= 1;
        self.migrate_pos = pos;
        h_insert(&mut self.nodes, &mut self.newer, idx);
        nwork += 1;
    }
    if older.size == 0 {
        // dropping `older` frees its slot array
    } else {
        self.older = Some(older);
    }
}
```

**Trait-based payloads.** `HashNode` is the Rust replacement for "the payload embeds
an `HNode`". `HMap` never knows whether `V` is an `Entry` or a `ZNode`:

```rust
pub trait HashNode {
    fn key(&self) -> &[u8];
    fn hcode(&self) -> u64;
    fn set_hcode(&mut self, hcode: u64);
    fn hnext(&self) -> Option<usize>;
    fn set_hnext(&mut self, next: Option<usize>);
}
```

**FNV hash.** `common.rs` matches the C++ bit-for-bit:

```rust
pub fn str_hash(data: &[u8]) -> u64 {
    let mut h: u32 = 0x811C_9DC5;
    for &byte in data {
        h = h.wrapping_add(u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    u64::from(h)
}
```

---

## Stage 6 — Typed responses: serialization

### Concepts

A status plus raw bytes cannot express "an array of strings" or "a double". The
project uses a compact tagged binary format (TLV). Every value starts with a 1-byte
tag:

| Tag | Wire format | Used for |
|---|---|---|
| `TAG_NIL` 0 | `tag` | missing key |
| `TAG_ERR` 1 | `tag, code:u32, len:u32, msg` | errors |
| `TAG_STR` 2 | `tag, len:u32, bytes` | strings |
| `TAG_INT` 3 | `tag, i64` | counts, TTLs |
| `TAG_DBL` 4 | `tag, f64` | scores |
| `TAG_ARR` 5 | `tag, n:u32, n × value` | arrays (nest recursively) |

Rust models the response as an enum instead of a `std::variant`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Error { code: u32, msg: String },
    Str(Vec<u8>),
    Int(i64),
    Dbl(f64),
    Arr(Vec<Value>),
}
```

**Writing now, patching the length later.** A response begins with a 4-byte total
length, but you do not know it until the body is serialized. `response_begin`
reserves the header; `response_end` backfills it. `out_begin_arr`/`out_end_arr` do
the same for arrays of unknown length (used by `zquery`).

```rust
pub fn response_end(out: &mut Buffer, header: usize) {
    let mut msg_size = out.len() - header - 4;
    if msg_size > K_MAX_MSG {
        out.truncate(header + 4);
        out_err(out, ERR_TOO_BIG, "response is too big.");
        msg_size = out.len() - header - 4;
    }
    let len = msg_size as u32;
    out[header..header + 4].copy_from_slice(&len.to_ne_bytes());
}
```

**`keys` and iteration.** `HMap::for_each` visits every slot in both tables and
invokes a closure:

```rust
pub fn for_each(&self, mut f: impl FnMut(usize, &V) -> bool)
where
    V: HashNode,
{
    for table in [Some(&self.newer), self.older.as_ref()].into_iter().flatten() {
        for slot in &table.slots {
            let mut cur = *slot;
            while let Some(idx) = cur {
                let node = self.nodes[idx].as_ref().expect("live node");
                if !f(idx, node) {
                    return;
                }
                cur = node.hnext();
            }
        }
    }
}
```

**Recursive decoding.** The client-side decoder returns the value and how many
bytes it consumed. Rust's `Result<(Value, usize), String>` replaces the C++
`std::expected`:

```rust
pub fn decode_value(data: &[u8]) -> Result<(Value, usize), String> {
    let Some(&tag) = data.first() else {
        return Err("bad response".to_string());
    };
    match tag {
        TAG_NIL => Ok((Value::Nil, 1)),
        TAG_ARR => {
            // read n, then recursively decode n items from data[5..]
            // ...
            Ok((Value::Arr(items), pos))
        }
        // TAG_ERR, TAG_STR, TAG_INT, TAG_DBL ...
        _ => Err("bad response".to_string()),
    }
}
```

---

## Stage 7 — The AVL tree

### Concepts

**Why a tree?** Sorted sets need order: "first member with score ≥ X", "the next 10
by `(score, name)`". Hashtables have no order. A plain BST degenerates into a list
on sorted input; a **balanced** BST guarantees O(log n) height.

**AVL invariant.** For every node, the heights of its left and right subtrees differ
by at most 1. Each node stores `height` (checked in O(1)) and `cnt` (subtree size).

**Rotations.** A left rotation preserves in-order order while moving a right child
up. The four imbalance cases collapse into two helpers (`fix_left`, `fix_right`).

**`avl_fix`.** After an insert or delete, walk up via `parent`, update `height` and
`cnt`, rotate where needed, re-attach the rotated subtree to the parent, and return
the new root. Callers always use `root = avl_fix(...)`.

**Deletion.** 0 or 1 child: splice the child in. 2 children: find the successor
(leftmost node of the right subtree), detach it, and copy the deleted node's link
fields into the successor's node — in Rust, that means copying the AVL fields
between two indices in the same arena. The payload stays where it is.

**Rank queries (`cnt`).** `avl_offset(node, k)` jumps to the k-th successor (or
predecessor when negative) in **O(log n)** using subtree counts. `zquery` uses it for
pagination.

**Trait-based nodes.** The tree is generic over an arena:

```rust
pub trait AvlNode {
    fn parent(&self) -> Option<usize>;
    fn set_parent(&mut self, parent: Option<usize>);
    fn left(&self) -> Option<usize>;
    fn set_left(&mut self, left: Option<usize>);
    fn right(&self) -> Option<usize>;
    fn set_right(&mut self, right: Option<usize>);
    fn height(&self) -> u32;
    fn set_height(&mut self, height: u32);
    fn cnt(&self) -> u32;
    fn set_cnt(&mut self, cnt: u32);
}

pub trait AvlArena {
    type Node: AvlNode;
    fn avl_node(&self, idx: usize) -> &Self::Node;
    fn avl_node_mut(&mut self, idx: usize) -> &mut Self::Node;
}
```

`HMap<V>` implements `AvlArena` for any `V: HashNode + AvlNode`, which is why a
sorted set can reuse the hashtable's arena:

```rust
impl<V: HashNode + AvlNode> AvlArena for HMap<V> {
    type Node = V;
    fn avl_node(&self, idx: usize) -> &V { self.get(idx) }
    fn avl_node_mut(&mut self, idx: usize) -> &mut V { self.get_mut(idx) }
}
```

A rotation (`avl.rs`):

```rust
fn rot_left<A: AvlArena>(arena: &mut A, node: usize) -> usize {
    let parent = arena.avl_node(node).parent();
    let new_node = arena.avl_node(node).right().expect("right child");
    let inner = arena.avl_node(new_node).left();

    arena.avl_node_mut(node).set_right(inner);
    if let Some(inner) = inner {
        arena.avl_node_mut(inner).set_parent(Some(node));
    }
    arena.avl_node_mut(new_node).set_parent(parent);
    arena.avl_node_mut(new_node).set_left(Some(node));
    arena.avl_node_mut(node).set_parent(Some(new_node));

    update(arena, node);
    update(arena, new_node);
    new_node
}
```

`avl_fix` climbs to the root:

```rust
pub fn avl_fix<A: AvlArena>(arena: &mut A, mut node: usize) -> usize {
    loop {
        let parent = arena.avl_node(node).parent();
        update(arena, node);

        let l = avl_height(arena, arena.avl_node(node).left());
        let r = avl_height(arena, arena.avl_node(node).right());
        let fixed = if l == r + 2 {
            fix_left(arena, node)
        } else if l + 2 == r {
            fix_right(arena, node)
        } else {
            node
        };

        match parent {
            Some(parent) => {
                if arena.avl_node(parent).left() == Some(node) {
                    arena.avl_node_mut(parent).set_left(Some(fixed));
                } else {
                    arena.avl_node_mut(parent).set_right(Some(fixed));
                }
                node = parent;
            }
            None => return fixed,
        }
    }
}
```

`avl_offset` uses `cnt` to skip whole subtrees:

```rust
pub fn avl_offset<A: AvlArena>(arena: &A, mut node: usize, offset: i64) -> Option<usize> {
    let mut pos: i64 = 0;
    while offset != pos {
        let right = arena.avl_node(node).right();
        let left = arena.avl_node(node).left();
        let right_cnt = i64::from(avl_cnt(arena, right));
        let left_cnt = i64::from(avl_cnt(arena, left));

        if pos < offset && pos + right_cnt >= offset {
            node = right.expect("right subtree");
            pos += i64::from(avl_cnt(arena, arena.avl_node(node).left())) + 1;
        } else if pos > offset && pos - left_cnt <= offset {
            node = left.expect("left subtree");
            pos -= i64::from(avl_cnt(arena, arena.avl_node(node).right())) + 1;
        } else {
            let parent = arena.avl_node(node).parent()?;
            if arena.avl_node(parent).right() == Some(node) {
                pos -= i64::from(avl_cnt(arena, left)) + 1;
            } else {
                pos += i64::from(avl_cnt(arena, right)) + 1;
            }
            node = parent;
        }
    }
    Some(node)
}
```

---

## Stage 8 — Sorted sets

### Concepts

A Redis **sorted set** maps unique member names to scores and keeps them ordered by
`(score, name)`. It needs two query styles:

| Query | Best structure |
|---|---|
| score of member X (`zscore`, `zrem`, updates) | hashtable by name → O(1) |
| members with score ≥ S, offset/limit (`zquery`) | ordered tree → O(log n) |

A `ZSet` keeps **both at once**. In C++ one `ZNode` was embedded in both containers.
Here the `ZNode` lives in the hashtable arena and the AVL tree stores its index; the
node implements both traits:

```rust
pub struct ZSet {
    pub hmap: HMap<ZNode>,
    root: Option<usize>,
}

pub struct ZNode {
    name: Vec<u8>,
    pub score: f64,

    // hashtable links
    hcode: u64,
    hnext: Option<usize>,

    // AVL links
    parent: Option<usize>,
    left: Option<usize>,
    right: Option<usize>,
    height: u32,
    cnt: u32,
}

impl HashNode for ZNode { /* key, hcode, hnext ... */ }
impl AvlNode for ZNode { /* parent, left, right, height, cnt ... */ }
```

**Tuple ordering.** `zless` compares score first, then the name bytes. Rust's slice
comparison is lexicographic on bytes, exactly like the C++ `memcmp`-then-length:

```rust
fn less_key(node: &ZNode, score: f64, name: &[u8]) -> bool {
    if node.score != score {
        node.score < score
    } else {
        node.name.as_slice() < name
    }
}
```

**Insertion.** Look up in the hashtable; if absent, allocate a node and insert into
both indexes. Search the tree with the user-defined comparison, attach, then
rebalance:

```rust
fn tree_insert(&mut self, idx: usize) {
    let mut parent = None;
    let mut cur = self.root;
    while let Some(node) = cur {
        parent = Some(node);
        cur = if less_node(self.hmap.get(idx), self.hmap.get(node)) {
            self.hmap.get(node).left()
        } else {
            self.hmap.get(node).right()
        };
    }
    match parent {
        Some(parent) => {
            if less_node(self.hmap.get(idx), self.hmap.get(parent)) {
                self.hmap.get_mut(parent).set_left(Some(idx));
            } else {
                self.hmap.get_mut(parent).set_right(Some(idx));
            }
        }
        None => self.root = Some(idx),
    }
    self.hmap.get_mut(idx).set_parent(parent);
    self.root = Some(avl_fix(&mut self.hmap, idx));
}
```

**Updating a score** cannot happen in place: detach the tree node, change the
score, reinsert. The hashtable entry is untouched because the name did not change.

```rust
fn update(&mut self, idx: usize, score: f64) {
    if self.hmap.get(idx).score == score {
        return;
    }
    self.root = avl_del(&mut self.hmap, idx);
    avl_init(self.hmap.get_mut(idx));
    self.hmap.get_mut(idx).score = score;
    self.tree_insert(idx);
}
```

**`seekge`** is a lower-bound search on `(score, name)`: when the current node is
≥ the key, remember it and go left; otherwise go right.

```rust
pub fn seekge(&self, score: f64, name: &[u8]) -> Option<usize> {
    let mut found = None;
    let mut cur = self.root;
    while let Some(idx) = cur {
        let node = self.hmap.get(idx);
        if less_key(node, score, name) {
            cur = node.right();
        } else {
            found = Some(idx);
            cur = node.left();
        }
    }
    found
}
```

**Deletion** removes the node from both indexes. `avl_del` detaches the tree node;
`remove_index` unlinks it from the hashtable and frees the arena slot:

```rust
pub fn delete(&mut self, idx: usize) {
    self.root = avl_del(&mut self.hmap, idx);
    self.hmap.remove_index(idx);
}
```

**Type checks.** An `Entry` holds either bytes or a `ZSet`. A zset command on a
string key returns `ERR_BAD_TYP`; a *missing* key behaves like an empty zset.

---

## Stage 9 — Timers: kicking idle connections

### Concepts

A client that connects and never sends anything holds a socket and memory forever.
The server closes connections idle for 5 seconds.

**Timers in an event loop.** Never `sleep()`: that blocks every client. Instead
compute the time until the nearest timer and pass it as `poll`'s timeout. `poll`
returns when a socket is ready *or* when the timeout expires; then run expired
timers.

**Monotonic clock.** Wall-clock time can jump backwards (NTP, manual changes). Use a
monotonic source. `clock.rs` uses `Instant`:

```rust
pub fn monotonic_ms() -> u64 {
    start().elapsed().as_millis() as u64
}
```

**Why a list is enough.** All idle timers have the same duration, so ordering
connections by last activity keeps the list sorted by deadline. The front is always
next to expire; activity moves a connection to the back. O(1) with a doubly linked
list.

**Index-based list.** C++ embedded a `DList` in `Conn` and used `container_of`. Here
`DList` stores values (connection fds) and stable node ids:

```rust
pub struct DList {
    nodes: Vec<Node>, // index 0 is the sentinel
    free: Vec<usize>,
    len: usize,
}

impl DList {
    pub fn push_back(&mut self, value: usize) -> usize { /* return node id */ }
    pub fn detach(&mut self, id: usize) { /* unlink + recycle id */ }
    pub fn front(&self) -> Option<usize> { /* value at the head */ }
    pub fn move_to_back(&mut self, id: usize) { /* detach + relink at tail */ }
}
```

**Timer extraction.** `next_timer_ms` asks the idle list and the TTL heap which
deadline is nearest:

```rust
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
    if next_ms == u64::MAX { return -1; }
    if next_ms <= now_ms { return 0; }
    (next_ms - now_ms).min(i32::MAX as u64) as i32
}
```

**Refresh after IO.** The C++ book refreshed a connection's idle timer *before*
processing its IO. That lets a request slower than the timeout get its connection
reaped mid-processing. This port refreshes **after** the IO work:

```rust
if let Some(conn) = self.fd2conn[fd].as_mut() {
    conn.last_active_ms = monotonic_ms();
}
self.idle.move_to_back(idle_node);
```

`process_timers` walks the front of the idle list, destroying expired connections,
then lets the database expire keys:

```rust
fn process_timers(&mut self) {
    let now_ms = monotonic_ms();
    while let Some(fd) = self.idle.front() {
        let expired = self.fd2conn.get(fd).and_then(Option::as_ref)
            .is_none_or(|conn| conn.last_active_ms + self.opts.idle_timeout_ms < now_ms);
        if !expired {
            break; // the list is ordered
        }
        self.conn_destroy(fd);
    }
    let _ = self.db.process_expired(2000);
}
```

---

## Stage 10 — Key expiration with a heap

### Concepts

`pexpire key ms` sets a deadline; `pttl key` reports the remaining time (`-1` = no
TTL, `-2` = no key). Expired keys must disappear on their own.

**Why a heap now?** Every key can have a different TTL, so a FIFO list no longer
stays sorted. A binary min-heap gives O(1) minimum and O(log n) insert/update/delete.

**Array layout.** Children of `i` are `2i+1`, `2i+2`; the parent is `(i+1)/2 - 1`;
the minimum is at index 0.

```rust
pub fn heap_parent(i: usize) -> usize { i.div_ceil(2) - 1 }
pub fn heap_left(i: usize) -> usize { i * 2 + 1 }
pub fn heap_right(i: usize) -> usize { i * 2 + 2 }
```

**The back-reference problem.** To delete a key's timer you need its current heap
position — but items move during every sift. Each `HeapItem` holds a shared
reference to the owner's position; every move writes the new index through it:

```rust
pub type HeapRef = Arc<AtomicUsize>;

pub struct HeapItem {
    pub val: u64,      // expiration time (ms)
    pub pos: HeapRef,  // owner's heap index, kept in sync on every move
    pub data: usize,   // owner's arena index (Entry)
}
```

`Arc<AtomicUsize>` rather than `Rc<Cell<usize>>` is deliberate: an `Entry` may be
moved to the thread pool for large-delete work, so it must be `Send + Sync`.

A sift writes the new position through the back-reference:

```rust
fn up(&mut self, mut pos: usize) {
    let t = self.items[pos].clone();
    while pos > 0 && self.items[heap_parent(pos)].val > t.val {
        self.items[pos] = self.items[heap_parent(pos)].clone();
        self.items[pos].pos.store(pos, Ordering::Relaxed);
        pos = heap_parent(pos);
    }
    self.items[pos] = t;
    self.items[pos].pos.store(pos, Ordering::Relaxed);
}
```

**Setting a TTL.** Removing a TTL deletes the heap item; creating/updating one
allocates the shared back-reference on first use and upserts:

```rust
fn set_ttl(&mut self, idx: usize, ttl_ms: i64) {
    let ttl_ref = self.map.get(idx).ttl.clone();
    if ttl_ms < 0 {
        if let Some(pos_ref) = ttl_ref {
            let pos = pos_ref.load(Ordering::Relaxed);
            if pos != NO_POS {
                self.heap.delete(pos);
            }
            self.map.get_mut(idx).ttl = None;
        }
    } else {
        let pos_ref = match ttl_ref {
            Some(existing) => existing,
            None => {
                let created = Arc::new(AtomicUsize::new(NO_POS));
                self.map.get_mut(idx).ttl = Some(Arc::clone(&created));
                created
            }
        };
        let pos = match pos_ref.load(Ordering::Relaxed) {
            NO_POS => None,
            p => Some(p),
        };
        let expire_at = (self.clock)() + ttl_ms as u64;
        self.heap.upsert(pos, HeapItem { val: expire_at, pos: pos_ref, data: idx });
    }
}
```

**Bounded work.** If a million keys expire at once, deleting them all in one pass
would stall the event loop. `process_expired` removes at most 2000 by default; the
heap top is still overdue, so the next `poll` returns immediately and the rest are
handled on later iterations.

```rust
pub fn process_expired(&mut self, max_work: usize) -> usize {
    let now = (self.clock)();
    let mut removed = 0;
    while removed < max_work {
        match self.heap.top_val() {
            Some(val) if val <= now => {}
            _ => break,
        }
        let item = self.heap.pop_min().expect("expired heap item");
        let mut ent = self.map.remove_index(item.data).expect("expired entry");
        ent.ttl = None; // already unlinked from the heap
        self.entry_del(ent);
        removed += 1;
    }
    removed
}
```

**Cleanup order.** `entry_del` removes an entry's timer *before* freeing the value,
otherwise the heap keeps a dangling back-reference and later writes through freed
memory. The Rust version drops the `Arc` on the floor only after the heap item is
gone.

---

## Stage 11 — A thread pool for large deletions

### Concepts

Deleting a sorted set with a million members means a million deallocations — a
visible pause on the main thread. The value is already unlinked from every shared
structure and nobody else can reach it, so freeing it can safely happen on another
thread. Real Redis does this with `UNLINK` / lazy-free.

**Only when worth it.** Thread handoff costs locking, a wakeup and cache misses, so
`entry_del` goes async only for values larger than 1000 members.

**Producer/consumer.** The main thread produces jobs; workers consume them. A mutex
protects the queue; a condition variable sleeps idle workers instead of spinning;
workers unlock before running a job.

`src/thread_pool.rs`:

```rust
type Job = Box<dyn FnOnce() + Send + 'static>;

struct State {
    jobs: VecDeque<Job>,
    stopping: bool,
}

struct Inner {
    state: Mutex<State>,
    not_empty: Condvar,
}

pub struct ThreadPool {
    inner: Arc<Inner>,
    threads: Vec<JoinHandle<()>>,
}
```

The worker waits for work or shutdown, then runs outside the lock:

```rust
fn worker(inner: Arc<Inner>) {
    loop {
        let job = {
            let mut state = inner.state.lock().expect("thread pool mutex");
            loop {
                if let Some(job) = state.jobs.pop_front() {
                    break job;
                }
                if state.stopping {
                    return;
                }
                state = inner.not_empty.wait(state).expect("thread pool condvar");
            }
        };
        job(); // run without holding the lock
    }
}
```

The destructor asks workers to stop, wakes them, and joins. Workers only exit when
the queue is empty, so queued work is drained first:

```rust
impl Drop for ThreadPool {
    fn drop(&mut self) {
        {
            let mut state = self.inner.state.lock().expect("thread pool mutex");
            state.stopping = true;
        }
        self.inner.not_empty.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
```

`entry_del` decides between inline and pool deletion:

```rust
fn entry_del(&mut self, mut ent: Entry) {
    if let Some(pos_ref) = ent.ttl.take() {
        let pos = pos_ref.load(Ordering::Relaxed);
        if pos != NO_POS {
            self.heap.delete(pos);
        }
    }
    let set_size = match &ent.value {
        DbValue::ZSet(zset) => zset.len(),
        DbValue::Str(_) => 0,
    };
    if set_size > Self::K_LARGE_CONTAINER_SIZE {
        if let Some(pool) = &self.pool {
            pool.queue(move || drop(ent)); // large: free off-thread
            return;
        }
    }
    drop(ent);
}
```

**Is it thread-safe?** Workers only touch the `Entry` they were given, which the
main thread has already removed from the hashtable and the heap. The only shared
state is the queue, protected by the mutex. Allocation/deallocation is thread-safe.

---

## Appendix A — The final event loop

The full implementation is `src/server.rs`. The shape of `run()`:

```rust
pub fn run(&mut self) -> io::Result<()> {
    let mut pollfds: Vec<PollFd> = Vec::new();

    loop {
        pollfds.clear();
        pollfds.push(PollFd { fd: self.listener.as_raw_fd(), events: POLLIN, revents: 0 });
        pollfds.push(PollFd { fd: self.wake_read.as_raw_fd(), events: POLLIN, revents: 0 });
        for (fd, slot) in self.fd2conn.iter().enumerate() {
            let Some(conn) = slot else { continue };
            let mut events = POLLERR;
            if conn.want_read { events |= POLLIN; }
            if conn.want_write { events |= POLLOUT; }
            pollfds.push(PollFd { fd: fd as RawFd, events, revents: 0 });
        }

        let rv = unsafe {
            poll(pollfds.as_mut_ptr(), pollfds.len() as c_ulong, self.next_timer_ms())
        };
        if rv < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted { continue; }
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
            let can_write = self.fd2conn[fd]
                .as_ref()
                .is_some_and(|conn| !conn.want_close);
            if events & POLLOUT != 0 && can_write {
                self.handle_write(fd);
            }
            let close = self.fd2conn[fd]
                .as_ref()
                .is_some_and(|conn| events & POLLERR != 0 || conn.want_close);
            if close {
                self.conn_destroy(fd);
                continue;
            }
            // Refresh the idle timer AFTER the IO work.
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
```

Graceful shutdown uses a self-pipe (`UnixStream::pair`): `stop()` writes one byte to
the wake fd, which `poll` reports as readable. The binary installs a SIGINT/SIGTERM
handler that performs the same async-signal-safe one-byte `write(2)`.

---

## Appendix B — Test checklist

```bash
cargo test                       # all 55 tests
cargo test --release             # optimized run
cargo clippy --all-targets -- -D warnings
cargo fmt --check

# The book's transcript against the real binaries:
python3 tests/e2e/test_cmds.py \
  --server target/release/redis_server \
  --cli target/release/redis_cli

# Manual smoke tests:
./target/release/redis_server --port 1234 &
./target/release/redis_cli set k v
./target/release/redis_cli get k
./target/release/redis_cli pexpire k 300
sleep 0.5
./target/release/redis_cli get k            # (nil)
./target/release/redis_cli zadd k 1 x       # (err) 3 expect zset
```

The test suites mirror the C++ ones:

| Rust test | Covers |
|---|---|
| `test_hashtable.rs` | progressive rehash, differential vs `HashMap`, `for_each` |
| `test_avl.rs` | invariants, every insert/delete position, rank offsets |
| `test_zset.rs` | ordering, seek/offset, differential vs `BTreeSet` |
| `test_heap.rs` | heap property + back-reference synchronization |
| `test_list.rs` | FIFO order, detach, move-to-back |
| `test_thread_pool.rs` | drains on drop, multiple workers, prompt shutdown |
| `test_protocol.rs` | round-trip, truncation, random input, oversized responses |
| `test_database.rs` | command semantics, deterministic TTLs, async delete |
| `test_server.rs` | real TCP server: pipelining, 8 MiB values, idle/expiry |

---

## Appendix C — Known limitations & exercises

1. **`buf_consume()` is O(n).** `Vec::drain(..n)` shifts every remaining byte. With
   many pipelined requests this becomes quadratic. *Exercise:* keep a read offset
   and compact occasionally, or implement a ring buffer.
2. **`poll()` is O(number of connections) per iteration.** Every loop rebuilds and
   scans the fd list. *Exercise:* port to `epoll`/`io_uring` so the kernel tracks the
   interest set and returns only ready fds.
3. **Host byte order on the wire.** Lengths and numbers use `to_ne_bytes`. *Exercise:*
   use `to_le_bytes`/`from_le_bytes` (or network order) everywhere.
4. **Hash flooding.** FNV with no secret seed lets an attacker craft keys that land
   in one slot. *Exercise:* mix a random per-process seed into `str_hash`, or use
   SipHash as real Redis does.
5. **`process_expired` bound.** The loop condition allows exactly `max_work`
   deletions here (the C++ original allowed one extra). A good comparison exercise.
6. **Number parsing differs slightly.** Rust's `f64::from_str` accepts `inf` and
   rejects a few forms differently from `strtod`; `str2dbl` rejects NaN to match the
   C++ guard.
7. **No persistence, replication or multiple databases.** *Exercises:* add a list
   type (reuse `list.rs`), `EXPIRE` in seconds, `INCR`, or an RDB-style snapshot.
8. **Generic key types.** `HashNode::key` returns `&[u8]` and `HMap` is specialized
   to byte-string keys. *Exercise:* make the map generic over
   `K: Borrow<Q>` and a `Hash`/`Eq` bound.
9. **Unsafe surface.** There are only two `unsafe` blocks: the `poll` FFI declaration
   and the signal handler's `write(2)`. Keep it that way; Miri can check the rest.

---

## Appendix D — Rust features used, and what they replace

| Rust feature | Where | C++ counterpart |
|---|---|---|
| `Result<T, String>` | protocol/net/server | `std::expected` / error codes |
| `Option<usize>` arena links | hashtable, AVL, list | intrusive pointers + `container_of` |
| Traits (`HashNode`, `AvlNode`, `AvlArena`) | data structures | embedded node structs / function pointers |
| `Vec<Vec<u8>>` | protocol, database | `std::vector<std::string>` (binary strings) |
| `std::sync::Arc<AtomicUsize>` | heap back-references | `size_t *ref` |
| `std::thread`, `Mutex`, `Condvar` | thread pool | `pthread_*` |
| `std::net::{TcpListener, TcpStream}` | server, net | POSIX `socket`/`accept`/`read`/`write` |
| `AsRawFd` + `unsafe extern "C"` `poll` | event loop | `<poll.h>` |
| `Instant` | clock | `clock_gettime(CLOCK_MONOTONIC)` |
| `is_none_or`, `is_some_and`, `div_ceil`, `fill_with` | various | hand-written checks/loops |
| `Drop` | maps, zsets, thread pool, server | destructors / `free` / join loops |

Deliberately **not** used: `unsafe` pointer-based intrusive containers, a
third-party async runtime, `mio`/`tokio`, or `DashMap`. The point is to build the
mechanisms yourself. A good follow-up is to replace the crate's `poll` FFI with
`epoll` (still dependency-free via `unsafe extern "C"`).

---

## Appendix E — Glossary

- **fd (file descriptor):** integer handle to a kernel object (file, socket, pipe).
- **Framing:** marking where one message ends and the next begins in a byte stream.
- **Pipelining:** sending several requests before reading any responses.
- **Event loop:** a single thread repeatedly waiting for readiness events and
  dispatching callbacks.
- **Arena:** a `Vec` of slots addressed by stable indices; nodes can be linked
  without pointers.
- **Intrusive container:** a container whose link fields live inside the payload.
  The C++ original used this; the Rust port emulates it with traits + arena indices.
- **Load factor:** average items per hashtable slot.
- **Progressive rehashing:** migrating a hashtable to a bigger array a little at a
  time.
- **AVL tree:** a BST where sibling subtree heights differ by at most 1.
- **Order-statistic / augmented tree:** a tree storing subtree sizes to answer rank
  queries in O(log n).
- **Min-heap:** array-based tree where every parent ≤ its children; minimum at 0.
- **TTL:** time to live, a per-key expiration deadline.
- **Monotonic clock:** a clock that never goes backwards.
- **Condition variable:** lets a thread sleep until another thread signals that a
  condition may have changed.

---

## Appendix F — C++ → Rust migration map

| C++ construct | Rust replacement |
|---|---|
| `struct HNode { HNode *next; }` embedded in payload | `Option<usize>` link + `HashNode` trait |
| `container_of(ptr, Type, member)` | you already have the arena index |
| `HMap` owning raw `HNode*` | `HMap<V>` owning `Vec<Option<V>>` |
| `std::map` placeholder | custom `HMap` |
| `std::string` as bytes | `Vec<u8>` |
| `HNode **from` delete trick | walk the chain and relink the previous node/slot |
| `AVLNode *parent/left/right` | `Option<usize>` fields |
| `*victim = *node` (move link fields) | copy the AVL fields between two arena indices |
| `size_t *ref` heap back-reference | `Arc<AtomicUsize>` |
| `pthread_create/mutex/cond` | `std::thread`, `std::sync::{Mutex, Condvar}` |
| `std::jthread` + stop token | `JoinHandle` + a `stopping` flag |
| `Conn*` in `vector<Conn*>` | `Vec<Option<Conn>>` indexed by fd |
| `DList` embedded in `Conn` | `DList` of node ids + `Conn::idle_node` |
| `pollfd`, `poll()` | `#[repr(C)] PollFd` + `unsafe extern "C"` |
| `clock_gettime(CLOCK_MONOTONIC)` | `std::time::Instant` |
| `printf`/`fprintf` | `println!`/`eprintln!` (or raw bytes to stdout) |
| `abort()` on fatal error | return `Result` / `ExitCode::FAILURE` |
| RAII destructor freeing nodes | `Drop` on the arena/`ZSet` |

---

With that you have the complete Rust server: sockets and framing, an event loop
with pipelining and timers, custom hash/AVL/heap structures, sorted sets with rank
queries, TTLs, and a thread pool that keeps large deletes off the hot path.
