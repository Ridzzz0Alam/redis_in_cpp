//! A Rust port of the C++23 "Redis clone" in this repository.
//!
//! The module layout mirrors the C++ project one-for-one:
//!
//! * `hashtable` — chained hashtable with bounded progressive rehashing
//! * `avl`       — AVL tree with subtree counts (rank queries)
//! * `zset`      — sorted set = hashtable + AVL tree over one node arena
//! * `heap`      — binary min-heap with back-references for key TTLs
//! * `list`      — intrusive-style circular list for idle-connection timers
//! * `protocol`  — request parser and tagged (TLV) response codec
//! * `database`  — key space and command handlers
//! * `net`       — blocking client helpers
//! * `server`    — non-blocking `poll()` event loop
//! * `thread_pool` — producer/consumer pool for freeing large values

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
