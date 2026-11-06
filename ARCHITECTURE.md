# Architecture — Interview Guide

A short, explainable overview of the Rust Redis-like server. Read this before an
interview; for the full walkthrough see [`build_guide.md`](build_guide.md).

---

## 1. The 30-second pitch

> I built an in-memory Redis-like server in Rust. It uses a single-threaded,
> non-blocking `poll()` event loop, like Redis. Clients speak a binary,
> length-prefixed protocol. I implemented the data structures myself: a
> progressively rehashing hashtable for the key space, an AVL tree plus a hashtable
> for sorted sets, a min-heap for key TTLs, and a linked list for idle-connection
> timeouts. Large sorted sets are freed on a small worker pool so the event loop
> never stalls.

The three ideas to remember:

1. **One thread handles all clients** (event loop), so it must never block.
2. **Everything expensive is bounded** (rehashing, expiry, large deletes).
3. **Data structures are hand-written**, not `std::collections`.

---

## 2. High-level picture

```text
Clients ──TCP──► poll() event loop ──► Database
                      │                  ├── HMap: key → Entry (string or sorted set)
                      │                  ├── ZSet: HMap(name) + AVL(score, name)
                      │                  └── Min-heap: TTL deadlines
                      ├── list of idle connections
                      └── worker pool: free large values off-thread
```

- **Main thread:** owns the database, all connections, and all timers.
- **Worker threads:** only drop detached values (no shared database access).

---

## 3. Components

| Component | File | What it does |
|---|---|---|
| Event loop | `src/server.rs` | `poll()`, connections, read/write buffers, timers, shutdown |
| Protocol | `src/protocol.rs` | Parse requests, encode/decode responses |
| Database | `src/database.rs` | Key space, commands, TTLs, async deletion |
| Hashtable | `src/hashtable.rs` | Chained map with progressive rehashing |
| AVL tree | `src/avl.rs` | Balanced tree with subtree counts (rank queries) |
| Sorted set | `src/zset.rs` | Hashtable + AVL over the same nodes |
| TTL heap | `src/heap.rs` | Min-heap with back-references |
| Idle list | `src/list.rs` | FIFO list ordered by last activity |
| Thread pool | `src/thread_pool.rs` | Mutex + condition variable + workers |
| Client library | `src/net.rs` | Blocking connect/call/read helpers |
| Binaries | `src/bin/` | `redis_server`, `redis_cli`, `redis_bench`, `bench_hashtable` |

---

## 4. How a request flows

1. `poll()` reports a client fd is readable.
2. `handle_read` reads up to 64 KiB into `Conn::incoming`.
3. `try_one_request` runs in a loop:
   - read the 4-byte length,
   - check bounds and the 32 MiB limit,
   - parse the request into `Vec<Vec<u8>>` (binary-safe),
   - call `Database::execute`,
   - serialize one response into `Conn::outgoing`,
   - consume **only** the handled request's bytes.
4. `handle_write` writes as much of `outgoing` as the socket accepts.
5. If part of the response remains, `poll` waits for `POLLOUT` and writes again.

**Pipelining:** because the loop consumes only one request at a time, multiple
requests sent in one write are handled in order without losing data.

**Why `Vec<Vec<u8>>`?** Keys and values are arbitrary bytes, not necessarily UTF-8.

**Connection flags:** each connection has `want_read`, `want_write`, and
`want_close`; the loop turns those into `poll` events.

---

## 5. Data structures

### Key space: `HMap<Entry>`

- Chained hashtable: array of slots, each slot heads a linked list.
- Power-of-two size, so `hash & (size - 1)` picks the slot.
- An `Entry` is either a string (`Vec<u8>`) or a sorted set.

### Progressive rehashing

A normal hashtable rehashes every key at once, causing a pause. This one keeps two
tables (`newer`, `older`) and moves at most **128 nodes per operation**. Lookups
check both; inserts go to `newer`; when `older` empties it is dropped. The cost is
spread out, so the event loop stays responsive.

### Sorted sets: `ZSet`

A sorted set needs two things:

| Query | Structure |
|---|---|
| score/update/delete by member name | hashtable → O(1) |
| range by `(score, name)` | AVL tree → O(log n) |

The same `ZNode` is in both indexes. Each node has hash links **and** AVL links, and
both structures refer to it by an arena index. This is the Rust-safe version of the
C++ intrusive design.

The AVL tree stores a subtree size (`cnt`) per node. That enables `avl_offset`:
jump forward/backward by rank in **O(log n)**, used by `zquery ... offset limit`.

### TTLs: min-heap

- Each item stores an absolute expiry time.
- The smallest deadline is at the root.
- A **back-reference** (`Arc<AtomicUsize>`) lets an entry find its current heap
  position; every sift updates it.
- `process_expired` removes at most **2,000 keys per loop**, so a mass expiry does
  not freeze the server.

`Arc<AtomicUsize>` rather than `Rc<Cell>` because entries can move to the worker
pool and therefore must be `Send`.

### Idle connections: list

All connections share the same idle timeout, so a list ordered by last activity is
already sorted by deadline. The front is next to expire; activity moves a connection
to the back in O(1).

---

## 6. Concurrency model

- **Event-loop thread** does all database and connection work. No locks needed.
- **Worker pool** (`Mutex<VecDeque<Job>>` + `Condvar`) only frees large values.
- When a value is deleted, it is first unlinked from the database. Then:
  - ≤ 1,000 sorted-set members → drop inline,
  - over 1,000 → `pool.queue(move || drop(entry))`.
- Workers never touch shared database state; the only shared state is the job queue.
- On shutdown, workers finish queued jobs, then join.

**Graceful shutdown:** a self-pipe wakes `poll()`. `stop()`, a `StopHandle`, or a
SIGINT/SIGTERM handler writes one byte to it, so `run()` returns cleanly.

---

## 7. Memory and safety

- **Arena indices instead of pointers.** Nodes live in `Vec<Option<T>>`; links are
  `Option<usize>`. A node's index is stable until it is removed.
- This avoids `container_of` and unsafe pointer arithmetic in every data structure.
- **Only two `unsafe` sites:** the `poll(2)` FFI declaration and the signal handler's
  one-byte `write(2)`. Everything else is safe Rust.
- **Ownership:** `Database` owns the map and heap; `ZSet` owns its nodes; `Server`
  owns the database and connections. Dropping the top owner frees everything.

---

## 8. Key numbers

| Number | Meaning |
|---|---|
| 32 MiB | Max message size |
| 200,000 | Max arguments per request |
| 128 | Nodes moved per rehash step |
| 2,000 | Keys expired per loop iteration |
| 1,000 | Sorted-set size that triggers async deletion |
| 5,000 ms | Default idle timeout |
| 4 | Default worker threads |

---

## 9. Testing

- 55 Rust tests: each data structure, protocol, database layer, server, thread pool.
- Randomized differential tests against `std::collections` containers.
- Exhaustive AVL/heap insert/delete position tests.
- Real TCP tests: pipelining, 8 MiB values, malformed requests, idle timeouts,
  background expiry.
- End-to-end transcript against the real binaries.
- `cargo fmt --check`, `cargo clippy -D warnings`, debug and release test runs.

---

## 10. Trade-offs and limitations

| Decision | Why | Cost |
|---|---|---|
| Arena indices | Safe Rust, stable identity | Extra indirection |
| Progressive rehash | Avoid large pauses | More complex lookup/delete |
| `Vec<u8>` keys | Binary-safe | Less ergonomic than `String` |
| Single event loop | No locking on the hot path | One slow command blocks everyone |
| Worker pool only for large deletes | Keeps hot path fast | Queue overhead (avoided by threshold) |
| `poll(2)` | No third-party deps | O(connections) per loop, Linux-only |
| `Vec::drain` for buffers | Simple | O(n) front removal |
| Host-endian integers | Matches the book | Not portable across endianness |

If I had more time: `epoll`/`io_uring`, ring buffers, a hash seed against collision
attacks, and persistence (RDB/AOF).

---

## 11. Likely interview questions

**Why not just use `std::collections::HashMap`?**
It is a good map, but growth rehashes every key at once, which can pause the event
loop. I implemented progressive rehashing to keep each operation bounded. It was also
the point of the project: build the structures.

**Why both a hashtable and an AVL tree for sorted sets?**
The hashtable gives O(1) lookup/update by member name. The AVL tree gives ordered
range queries and, with subtree counts, O(log n) rank offsets. Different queries need
different indexes.

**How can one node be in two containers in safe Rust?**
The node lives in an arena (`Vec<Option<ZNode>>`). The hashtable and the AVL tree
both store its `usize` index. `ZNode` implements both the hash-node and AVL-node
traits. No unsafe pointers are needed.

**How does TTL deletion find the right heap element?**
Each heap item holds a shared `Arc<AtomicUsize>` that points at the owning entry's
heap index. Every time an item moves during a sift, the new index is written through
that reference.

**Why `Arc<AtomicUsize>` instead of `Rc<Cell<usize>>`?**
Entries can be moved to the worker pool for large deletes, so they must be `Send +
Sync`. `Rc` is not `Send`.

**How is the thread pool safe if the database is single-threaded?**
A value is first removed from every shared structure; only then is its ownership
moved into a job. Workers only drop their own detached value. The only shared state
is the queue, protected by a mutex.

**How does the loop stay responsive?**
Non-blocking sockets, bounded rehashing, bounded expiry, and large deallocation moved
off-thread. The main rule was: never do unbounded work on the event-loop thread.

**How does pipelining work?**
After a read, the server processes requests in a loop, consuming only the bytes of
each handled request. Requests already buffered behind it remain for the next
iteration.

**What was the trickiest part?**
AVL deletion with two children (moving link fields to the successor), keeping the heap
back-references in sync on every sift, and making the borrow checker accept an
index-based tree that needs to mutate several nodes per rotation.

**What would you improve next?**
Replace `poll` with `epoll` (O(1) readiness instead of O(n) scan), use a read offset or
ring buffer instead of `Vec::drain` on the front, add a hash seed, and add persistence.

---

## 12. Files to mention

```text
src/server.rs       poll event loop, connections, timers
src/protocol.rs     request parser + response TLV codec
src/database.rs     commands, key space, TTLs, async delete
src/hashtable.rs    progressive-rehash hashtable
src/avl.rs          AVL tree with subtree counts
src/zset.rs         sorted set (hashtable + AVL)
src/heap.rs         TTL min-heap with back-references
src/list.rs         idle-connection list
src/thread_pool.rs  worker pool for large frees
```
