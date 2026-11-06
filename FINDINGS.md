# Code Review Findings

A senior review of the Rust Redis-like server as originally delivered. Every finding
below has been fixed in the working tree unless it is marked **Not fixed**. Each
entry says what was wrong, why it matters, how it was fixed, and which test now
guards it.

The original code compiled, passed all 55 tests, and was clean under
`cargo clippy -D warnings` and `cargo fmt --check`. **None of the problems below
were caught by that tooling.** Green CI showed that the code did what the tests
asked. It did not show that the server was safe to run. Most findings are about
behaviour under hostile, unusual or heavy load, which the tests never exercised.

Severity: **High** means one client or the environment can take the server down.
**Medium** means wrong behaviour, undefined behaviour, or a false claim in the
docs. **Low** means an edge case, robustness gap or tooling problem.

---

## High severity

### H1. One client could make the server allocate unbounded reply memory
- **Where:** `server.rs`, `handle_read` / `try_one_request`.
- **What was wrong:** after each read, the loop ran every complete request in the
  input buffer and appended every reply to `outgoing`, with no limit.
- **Impact (reproduced):** I stored a 100 KB value, then sent one 64 KB write of
  pipelined `get k` and never read the replies. Server memory went from
  **2 MB to 314 MB**. With an 8 MB value the same 64 KB would need about 26 GB.
  Any client could crash the server this way.
- **Fix:**
  - The new `ServerOptions::max_pending_output` (default 4 MiB) caps pending
    replies. `process_incoming` stops running requests while that much output is
    queued.
  - `handle_write` resumes the held-back requests once the output drains.
  - `Conn::update_interest` is now the single place that derives poll interest
    from buffer state.
  - After the fix, the same test peaks at **7 MB**.
- **Test:** `test_server::pending_output_is_bounded_and_resumes`. It also
  checks that the held-back requests are answered, because a naive fix would
  leave them stuck in the input buffer forever.

### H2. 100% CPU busy-loop when file descriptors run out
- **Where:** `server.rs`, `handle_accept`.
- **What was wrong:** on an `accept()` error such as `EMFILE`, the code logged and
  broke out of the loop. The pending connection stays in the kernel backlog, so the
  listener is still readable and `poll()` returns at once, forever.
- **Impact (reproduced):** with `ulimit -n 16` and 30 clients, the server used a
  full CPU core indefinitely.
- **Fix:**
  - Resource errors now pause accepting for 100 ms (`accept_paused_until`): the
    listener is polled with `events = 0`, and the pause counts as a timer.
  - `ConnectionAborted` is treated as transient.
  - A `max_clients` limit (default 10,000) closes extra connections.
  - After the fix, the same test uses **0** CPU ticks.
- **Test:** `test_server::max_clients_is_enforced`. `EMFILE` itself was checked by
  hand, because exhausting fds inside the test process is not practical.

### H3. Listened on every network interface with no authentication
- **Where:** `Server::new` bound `0.0.0.0` unconditionally.
- **Impact:** anyone who could reach the port could read and write all data, and,
  combined with H1, crash the server.
- **Fix:** the new `ServerOptions::bind_addr` defaults to `127.0.0.1`. Exposing
  the server takes an explicit `--bind 0.0.0.0`.
- **Test:** `test_server::default_bind_is_loopback`.

---

## Medium severity

### M1. Undefined behaviour in the `signal()` FFI declaration
- **Where:** `bin/redis_server.rs`.
- **What was wrong:** `signal` was declared to return `extern "C" fn(c_int)`, a
  non-nullable Rust function pointer. The C function returns `SIG_DFL`, which is
  null, or `SIG_ERR`, which is -1. Producing an invalid function pointer is UB even
  if the value is ignored. The result was also never checked.
- **Fix:** `sighandler_t` is now declared as `usize` and the result is compared
  with `SIG_ERR`. On failure the binary exits with an error.
- **Lesson:** an FFI signature must say exactly what the C side can return. A
  function that can return null cannot be typed as a Rust `fn`.

### M2. Expired keys could still be read
- **Where:** `database.rs`. No command checked a key's deadline.
- **What was wrong:** keys were removed only by `process_expired`, at most 2,000
  per event-loop pass. Until then `GET` returned the value, `PTTL` returned 0,
  `KEYS` listed the key, and `DEL` returned 1. Anyone using `Database` without the
  server loop never saw keys expire.
- **Fix:**
  - `Database::lookup` now deletes a due key and reports it as missing (lazy
    expiry, which real Redis also does).
  - `DEL` goes through `lookup`.
  - `KEYS` skips due keys.
- **Test:** `test_database::expired_keys_are_invisible_before_background_expiry`.

### M3. Command behaviour diverged from Redis
| Problem | Fix | Test |
|---|---|---|
| `SET` on an existing key kept its old TTL | `do_set` clears the TTL | `set_discards_previous_ttl` |
| `ZREM` of the last member left an empty zset key | the key is deleted when the set becomes empty | `removing_last_member_deletes_the_zset` |
| Command names were case-sensitive (`GET` failed) | the name is lowercased before dispatch; keys stay case-sensitive | `command_names_are_case_insensitive` |
| `zquery` `limit` counted name and score separately, so `limit=1` and `limit=2` both returned one member | `limit` now counts members | `zquery_limit_counts_members` |
| `zquery` with a negative offset returned nothing when the seek key was past the end | counts back from the last member (`ZSet::last`) | `zquery_negative_offset_past_the_end` |

The `limit` quirk came from the book. The original agent didn't fix it; it wrote a
test (`zquery_limit_counts_name_and_score`) that locked the quirk in. **A test
that enshrines surprising behaviour should come with a comment saying why it is
intentional, or the behaviour should be fixed.**

### M4. "Bounded work" claim was false for `KEYS` and `ZQUERY`
- **What was wrong:** both commands built the entire reply first. If it was over
  32 MiB, `response_end` then threw it away and sent an error. A big keyspace meant
  a long event-loop stall plus a huge allocation, for a reply that could never be
  sent.
- **Fix:** both commands check the reply size while building it and stop with
  `ERR_TOO_BIG` as soon as it passes 32 MiB (`abort_too_big`).
- **Test:** `test_database::oversized_keys_reply_is_cut_short`.
- **Not fixed:** the work up to 32 MiB still runs on the event loop. The real fix
  is an incremental `SCAN`, which is listed as future work.

### M5. The hashtable arena reintroduced the pause rehashing was meant to avoid
- **Where:** `hashtable.rs`.
- **What was wrong:** the main reason for this hashtable is that it never pauses to
  rehash everything at once. But its nodes lived in one `Vec<Option<V>>`, which
  copies **every node** whenever it grows. New slot tables were also filled element
  by element (`vec![None; n]` of `Option<usize>`).
- **Fix:**
  - Nodes now live in a chunked arena (`Arena`, 1,024 nodes per chunk), so chunks
    never move.
  - Slots are `Option<NonZeroUsize>` (index + 1), so new tables come from zeroed
    memory and their pages are only touched when used.
  - The map releases all its memory when it becomes empty.
- **Measured** (`bench_hashtable 3000000`): worst-case insert went from 531 µs to
  154 µs. `std::HashMap`'s worst case is about 210 ms.
- **Not fixed (by design):** p99 is still about 10 µs, against std's 0.6 µs. That
  is the cost of moving up to 128 nodes per insert while rehashing, the trade-off
  the design chose. Arena slots of deleted keys are reused but only released when
  the map empties. Compacting the arena would break the stable indices the heap and
  AVL tree depend on.
- **Test:** `test_hashtable::many_chunks_drain_and_reuse`.

### M6. Some `poll` results were ignored
- **What was wrong:** only `POLLIN`, `POLLOUT` and `POLLERR` were handled. A
  connection reporting only `POLLHUP` or `POLLNVAL` was never closed, and because
  any event counted as activity, its idle timer kept resetting. The loop would spin
  on it forever.
- **Fix:** `POLLERR` and `POLLNVAL` close the connection immediately. `POLLHUP`
  closes it after a final read. The useless `POLLERR` in the requested events was
  removed, since the kernel always reports it.

### M7. Quadratic buffer handling
- **What was wrong:** `buf_consume` (`Vec::drain(..n)`) ran after every pipelined
  request and every partial write, copying all remaining bytes each time. Sending an
  8 MB reply copied hundreds of MB.
- **Fix:** the new `buffer::ByteQueue` keeps a read offset and compacts only when the
  dead prefix is at least half the buffer, so consuming is O(1) amortized.
- **Test:** `buffer::tests::byte_queue_fifo`.

---

## Low severity

| # | Problem | Fix | Test |
|---|---|---|---|
| L1 | `next_timer_ms` returned 0 at `deadline == now`, but `process_timers` only expired on a strict `<`, so the loop busy-polled for up to 1 ms | both use `<=` | — |
| L2 | If the idle list's front node had no connection, `process_timers` called a no-op `conn_destroy` and looped forever. Only a `debug_assert` guarded against it, and release builds remove that check. | `DList::front_id` was added, and a stale node is detached | `test_list::front_id_and_reuse` |
| L3 | A malformed or oversized request closed the connection at once, losing replies already queued for earlier pipelined requests | `close_after_flush`: send what is queued, then close | `replies_before_a_bad_request_are_flushed` |
| L4 | The client's `decode_value` recursed once per nesting level with no limit, so a hostile server could overflow the client's stack. The comment claimed message size bounded it, which isn't true. The `#[allow(clippy::only_used_in_recursion)]` on it was also wrong. | depth limit `K_MAX_DEPTH = 64`; allow and comment removed | `deeply_nested_response_is_rejected_not_a_stack_overflow` |
| L5 | `redis_cli` with no command sent an empty request; a trailing `--port` was sent as the command; exit code was 0 on error replies | usage errors exit 2, error replies exit 1 | e2e script checks exit codes |
| L6 | `bench_hashtable 0` panicked | validated, usage message | manual |
| L7 | `redis_bench`: if `requests < clients` it reported "is the server running?"; requests that didn't divide evenly were silently dropped; connect errors and I/O errors were reported the same way | exact request distribution, separate error messages | manual |
| L8 | A panicking thread-pool job killed its worker permanently, so the pool could shrink to zero while jobs piled up | `catch_unwind` around each job | `panicking_job_does_not_kill_the_worker` |
| L9 | Every heap sift step cloned an `Arc` (two atomic operations per level) | sift with `swap`; removed items are marked `NO_POS` | existing heap tests |
| L10 | Accepted sockets didn't set `TCP_NODELAY` | `set_nodelay(true)` | — |
| L11 | `pexpire` deadline used `+`, which can overflow | `saturating_add` | — |

---

## Docs, tests and project hygiene

- **Inaccurate docs.**
  - ARCHITECTURE.md said there were "only two `unsafe` sites"; there were five.
  - `lib.rs` referred to "the C++23 project in this repository", which had been
    deleted.
  - The test count, the trade-off table and the "bounded work" claims didn't match
    the code.
  - All of these are corrected. `build_guide.md` now notes that its excerpts show
    the original version of each function.
- **No minimum Rust version.** The code needs Rust 1.87 (`is_multiple_of`) but
  `Cargo.toml` didn't say so. `rust-version = "1.87"` has been added.
- **No version control.** The project wasn't a git repository even though it
  shipped a CI workflow.
- **Tests that depend on timing.** `idle_connections_are_closed` slept a fixed
  1,200 ms and hoped. It now blocks on the read until the server closes the
  connection.
- **Racy e2e script.** It picked a "free" port and then started the server, so
  another process could take the port in between, and it failed unclearly if the
  server didn't start. The server now binds port 0, the script reads the real port
  from its output, and startup failure is reported clearly.
- **Missing tests for what mattered.** There were no tests for output limits,
  connection limits, expiry visibility, Redis command behaviour or protocol-error
  handling. There are now 70 Rust tests (up from 55) and 20 e2e commands
  (up from 15).

### Known limitations (documented, not bugs)
- `poll()` is O(connections) per loop. `epoll` would be the next step.
- Integers on the wire use host byte order.
- No hash seed, so an attacker could craft colliding keys (hash flooding).
- No persistence.

---

## Lessons for next time

1. **Passing tests show only what you tested.** Before calling a server done,
   ask what one hostile client, one slow client, or exhausted fds or memory would
   do. H1, H2 and M6 all fail that check.
2. **Every buffer that grows from input needs a limit.** Look for each
   `extend`/`push` driven by network data and ask what stops it.
3. **Check the error paths in event loops.** A level-triggered `poll` plus an
   error branch that just logs and breaks almost always means a busy-loop.
4. **Safe defaults.** A server without auth must bind to loopback by default.
5. **FFI types must cover every value C can return**, including null and error
   sentinels. Add a `// SAFETY:` comment on every `unsafe` block.
6. **Make sure a design actually delivers what it claims.** "Progressive rehashing
   avoids pauses" was undercut by a `Vec` that copied everything on growth. Measure
   the property you claim (the benchmark existed but nobody read the max column
   critically).
7. **Don't use tests to lock in quirks.** If behaviour is surprising, fix it or
   explain in a comment why it's intentional.
8. **Keep the docs true.** A wrong claim in an "interview guide" (like the unsafe
   count) is worse than no claim.
9. **Avoid fixed sleeps in tests.** Block on the event you expect, with a generous
   timeout as a safety net.
10. **When you move work aside (backpressure, deferral), make sure something
    resumes it.** Held-back requests need a code path that runs them later, and a
    test that proves it.

---

## How everything was verified

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test             # 70 passed
cargo test --release   # 70 passed
cargo build --release --bins
python3 tests/e2e/test_cmds.py --server target/release/redis_server --cli target/release/redis_cli
# all 20 commands passed
```

The memory reproduction for H1 (314 MB → 7 MB), the fd-exhaustion reproduction for
H2 (100% of a core → 0) and the `bench_hashtable` numbers for M5 were re-run
against the fixed release binary.
