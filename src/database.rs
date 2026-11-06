//! The key space and command execution, ported from `database.h` /
//! `database.cpp`. Commands and their semantics are unchanged from the book's
//! chapter 14 (the `g_data` globals are now a `Database` object).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::clock::monotonic_ms;
use crate::common::str_hash;
use crate::hashtable::{HMap, HashNode};
use crate::heap::{Heap, HeapItem, HeapRef, NO_POS};
use crate::protocol::{
    out_arr, out_begin_arr, out_dbl, out_end_arr, out_err, out_int, out_nil, out_str, ERR_BAD_ARG,
    ERR_BAD_TYP, ERR_TOO_BIG, ERR_UNKNOWN, K_MAX_MSG,
};
use crate::thread_pool::ThreadPool;
use crate::zset::ZSet;
use crate::Buffer;

/// Clock used for TTL decisions; injectable so tests can be deterministic.
pub type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

enum DbValue {
    Str(Vec<u8>),
    ZSet(ZSet),
}

/// KV pair for the top-level hashtable.
struct Entry {
    key: Vec<u8>,
    hcode: u64,
    hnext: Option<usize>,
    ttl: Option<HeapRef>,
    value: DbValue,
}

impl HashNode for Entry {
    fn key(&self) -> &[u8] {
        &self.key
    }

    fn hcode(&self) -> u64 {
        self.hcode
    }

    fn set_hcode(&mut self, hcode: u64) {
        self.hcode = hcode;
    }

    fn hnext(&self) -> Option<usize> {
        self.hnext
    }

    fn set_hnext(&mut self, next: Option<usize>) {
        self.hnext = next;
    }
}

impl Entry {
    fn new(key: Vec<u8>, value: DbValue) -> Self {
        Self {
            key,
            hcode: 0,
            hnext: None,
            ttl: None,
            value,
        }
    }
}

pub struct Database {
    map: HMap<Entry>,
    heap: Heap,
    pool: Option<Arc<ThreadPool>>,
    clock: Clock,
}

fn str2int(s: &[u8], out: &mut i64) -> bool {
    let Ok(text) = std::str::from_utf8(s) else {
        return false;
    };
    match text.parse::<i64>() {
        Ok(v) => {
            *out = v;
            true
        }
        Err(_) => false,
    }
}

fn str2dbl(s: &[u8], out: &mut f64) -> bool {
    let Ok(text) = std::str::from_utf8(s) else {
        return false;
    };
    match text.parse::<f64>() {
        Ok(v) if !v.is_nan() => {
            *out = v;
            true
        }
        _ => false,
    }
}

/// True if `ent` has a TTL that is due at `now`.
fn entry_expired(heap: &Heap, ent: &Entry, now: u64) -> bool {
    ent.ttl.as_ref().is_some_and(|pos_ref| {
        heap.get(pos_ref.load(Ordering::Relaxed))
            .is_some_and(|item| item.val <= now)
    })
}

/// Replace everything written since `start` with an "too big" error. Used by
/// commands whose output size depends on the data, so they stop as soon as
/// the response can no longer be sent instead of building it in full.
fn abort_too_big(out: &mut Buffer, start: usize) {
    out.truncate(start);
    out_err(out, ERR_TOO_BIG, "response is too big.");
}

impl Default for Database {
    fn default() -> Self {
        Self::new()
    }
}

impl Database {
    /// Values larger than this (zset members) are freed on the thread pool.
    pub const K_LARGE_CONTAINER_SIZE: usize = 1000;

    pub fn new() -> Self {
        Self::with_clock(None, Box::new(monotonic_ms))
    }

    pub fn with_pool(pool: Option<Arc<ThreadPool>>) -> Self {
        Self::with_clock(pool, Box::new(monotonic_ms))
    }

    pub fn with_clock(pool: Option<Arc<ThreadPool>>, clock: Clock) -> Self {
        Self {
            map: HMap::new(),
            heap: Heap::new(),
            pool,
            clock,
        }
    }

    /// Number of keys.
    pub fn size(&self) -> usize {
        self.map.len()
    }

    /// Earliest key-expiration deadline, if any key has a TTL.
    pub fn next_expiry_ms(&self) -> Option<u64> {
        self.heap.top_val()
    }

    /// Delete expired keys, at most `max_work` per call.
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
            // The heap item was already removed; don't try to delete it again.
            ent.ttl = None;
            self.entry_del(ent);
            removed += 1;
        }
        removed
    }

    /// Execute one parsed command, appending exactly one serialized value.
    pub fn execute(&mut self, cmd: &mut [Vec<u8>], out: &mut Buffer) {
        // Command names are case-insensitive, as in Redis.
        let name = cmd
            .first()
            .map(|c| c.to_ascii_lowercase())
            .unwrap_or_default();
        match (cmd.len(), name.as_slice()) {
            (2, b"get") => self.do_get(cmd, out),
            (3, b"set") => self.do_set(cmd, out),
            (2, b"del") => self.do_del(cmd, out),
            (3, b"pexpire") => self.do_expire(cmd, out),
            (2, b"pttl") => self.do_ttl(cmd, out),
            (1, b"keys") => self.do_keys(out),
            (4, b"zadd") => self.do_zadd(cmd, out),
            (3, b"zrem") => self.do_zrem(cmd, out),
            (3, b"zscore") => self.do_zscore(cmd, out),
            (6, b"zquery") => self.do_zquery(cmd, out),
            _ => out_err(out, ERR_UNKNOWN, "unknown command."),
        }
    }

    /// Find a live key. A key whose TTL is already due is deleted here
    /// ("lazy expiry"), so commands never observe expired data even if the
    /// background `process_expired` pass has not reached it yet.
    fn lookup(&mut self, key: &[u8]) -> Option<usize> {
        let hcode = str_hash(key);
        let idx = self.map.lookup(key, hcode)?;
        let now = (self.clock)();
        if entry_expired(&self.heap, self.map.get(idx), now) {
            let ent = self.map.remove_index(idx).expect("expired entry");
            self.entry_del(ent);
            return None;
        }
        Some(idx)
    }

    fn insert_new(&mut self, key: Vec<u8>, value: DbValue) -> usize {
        self.map.insert(Entry::new(key, value))
    }

    fn do_get(&mut self, cmd: &mut [Vec<u8>], out: &mut Buffer) {
        let Some(idx) = self.lookup(&cmd[1]) else {
            return out_nil(out);
        };
        match &self.map.get(idx).value {
            DbValue::Str(s) => out_str(out, s),
            DbValue::ZSet(_) => out_err(out, ERR_BAD_TYP, "not a string value"),
        }
    }

    fn do_set(&mut self, cmd: &mut [Vec<u8>], out: &mut Buffer) {
        if let Some(idx) = self.lookup(&cmd[1]) {
            match &mut self.map.get_mut(idx).value {
                DbValue::Str(s) => *s = std::mem::take(&mut cmd[2]),
                DbValue::ZSet(_) => {
                    return out_err(out, ERR_BAD_TYP, "a non-string value exists");
                }
            }
            // SET replaces the key, so any previous TTL is discarded (Redis).
            self.set_ttl(idx, -1);
            return out_nil(out);
        }
        let key = std::mem::take(&mut cmd[1]);
        let value = std::mem::take(&mut cmd[2]);
        self.insert_new(key, DbValue::Str(value));
        out_nil(out);
    }

    fn do_del(&mut self, cmd: &mut [Vec<u8>], out: &mut Buffer) {
        // `lookup` already treats an expired key as absent (DEL returns 0).
        let removed = self
            .lookup(&cmd[1])
            .and_then(|idx| self.map.remove_index(idx));
        if let Some(ent) = removed {
            self.entry_del(ent);
            out_int(out, 1);
        } else {
            out_int(out, 0);
        }
    }

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
                    let created = Arc::new(std::sync::atomic::AtomicUsize::new(NO_POS));
                    self.map.get_mut(idx).ttl = Some(Arc::clone(&created));
                    created
                }
            };
            let pos = match pos_ref.load(Ordering::Relaxed) {
                NO_POS => None,
                p => Some(p),
            };
            let expire_at = (self.clock)().saturating_add(ttl_ms as u64);
            self.heap.upsert(
                pos,
                HeapItem {
                    val: expire_at,
                    pos: pos_ref,
                    data: idx,
                },
            );
        }
    }

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

    // PEXPIRE key ttl_ms
    fn do_expire(&mut self, cmd: &mut [Vec<u8>], out: &mut Buffer) {
        let mut ttl_ms = 0i64;
        if !str2int(&cmd[2], &mut ttl_ms) {
            return out_err(out, ERR_BAD_ARG, "expect int64");
        }
        if let Some(idx) = self.lookup(&cmd[1]) {
            self.set_ttl(idx, ttl_ms);
            out_int(out, 1);
        } else {
            out_int(out, 0);
        }
    }

    // PTTL key
    fn do_ttl(&mut self, cmd: &mut [Vec<u8>], out: &mut Buffer) {
        let Some(idx) = self.lookup(&cmd[1]) else {
            return out_int(out, -2); // not found
        };
        let Some(pos_ref) = &self.map.get(idx).ttl else {
            return out_int(out, -1); // no TTL
        };
        let pos = pos_ref.load(Ordering::Relaxed);
        let Some(item) = self.heap.get(pos) else {
            return out_int(out, -1);
        };
        let expire_at = item.val;
        let now_ms = (self.clock)();
        out_int(
            out,
            if expire_at > now_ms {
                (expire_at - now_ms) as i64
            } else {
                0
            },
        );
    }

    fn do_keys(&mut self, out: &mut Buffer) {
        let now = (self.clock)();
        let start = out.len();
        let ctx = out_begin_arr(out);
        let mut n = 0u32;
        let mut too_big = false;
        let heap = &self.heap;
        self.map.for_each(|_, ent| {
            if entry_expired(heap, ent, now) {
                return true; // logically gone; process_expired will free it
            }
            out_str(out, &ent.key);
            n += 1;
            too_big = out.len() - start > K_MAX_MSG;
            !too_big
        });
        if too_big {
            return abort_too_big(out, start);
        }
        out_end_arr(out, ctx, n);
    }

    // ZADD zset score name
    fn do_zadd(&mut self, cmd: &mut [Vec<u8>], out: &mut Buffer) {
        let mut score = 0.0f64;
        if !str2dbl(&cmd[2], &mut score) {
            return out_err(out, ERR_BAD_ARG, "expect float");
        }
        let idx = match self.lookup(&cmd[1]) {
            Some(idx) => idx,
            None => {
                let key = std::mem::take(&mut cmd[1]);
                self.insert_new(key, DbValue::ZSet(ZSet::new()))
            }
        };
        match &mut self.map.get_mut(idx).value {
            DbValue::ZSet(zset) => {
                let added = zset.insert(&cmd[3], score);
                out_int(out, i64::from(added));
            }
            DbValue::Str(_) => out_err(out, ERR_BAD_TYP, "expect zset"),
        }
    }

    // ZREM zset name
    fn do_zrem(&mut self, cmd: &mut [Vec<u8>], out: &mut Buffer) {
        let Some(idx) = self.lookup(&cmd[1]) else {
            return out_int(out, 0);
        };
        let member = match &mut self.map.get_mut(idx).value {
            DbValue::ZSet(zset) => zset.lookup(&cmd[2]),
            DbValue::Str(_) => return out_err(out, ERR_BAD_TYP, "expect zset"),
        };
        if let Some(member_idx) = member {
            let now_empty = match &mut self.map.get_mut(idx).value {
                DbValue::ZSet(zset) => {
                    zset.delete(member_idx);
                    zset.is_empty()
                }
                DbValue::Str(_) => false,
            };
            if now_empty {
                // Redis never keeps an empty sorted set around.
                let ent = self.map.remove_index(idx).expect("zset entry");
                self.entry_del(ent);
            }
            out_int(out, 1);
        } else {
            out_int(out, 0);
        }
    }

    // ZSCORE zset name
    fn do_zscore(&mut self, cmd: &mut [Vec<u8>], out: &mut Buffer) {
        let Some(idx) = self.lookup(&cmd[1]) else {
            return out_nil(out);
        };
        match &mut self.map.get_mut(idx).value {
            DbValue::ZSet(zset) => match zset.lookup(&cmd[2]) {
                Some(member) => out_dbl(out, zset.score(member)),
                None => out_nil(out),
            },
            DbValue::Str(_) => out_err(out, ERR_BAD_TYP, "expect zset"),
        }
    }

    // ZQUERY zset score name offset limit
    //
    // Returns up to `limit` members (each as a name, score pair) starting
    // `offset` ranks after the first member >= (score, name). A negative
    // offset with no such member counts back from the last member.
    fn do_zquery(&mut self, cmd: &mut [Vec<u8>], out: &mut Buffer) {
        let mut score = 0.0f64;
        if !str2dbl(&cmd[2], &mut score) {
            return out_err(out, ERR_BAD_ARG, "expect fp number");
        }
        let mut offset = 0i64;
        let mut limit = 0i64;
        if !str2int(&cmd[4], &mut offset) || !str2int(&cmd[5], &mut limit) {
            return out_err(out, ERR_BAD_ARG, "expect int");
        }
        if limit <= 0 {
            return out_arr(out, 0);
        }
        let Some(idx) = self.lookup(&cmd[1]) else {
            return out_arr(out, 0);
        };
        let zset = match &self.map.get(idx).value {
            DbValue::ZSet(zset) => zset,
            DbValue::Str(_) => return out_err(out, ERR_BAD_TYP, "expect zset"),
        };

        let mut member = match zset.seekge(score, &cmd[3]) {
            Some(start) => zset.node_offset(start, offset),
            // Past the end: offset -1 is the last member, -2 the one before...
            None if offset < 0 => zset
                .last()
                .and_then(|last| zset.node_offset(last, offset + 1)),
            None => None,
        };

        let start = out.len();
        let ctx = out_begin_arr(out);
        let mut n = 0i64;
        while let Some(m) = member {
            if n >= limit {
                break;
            }
            out_str(out, zset.node(m).name());
            out_dbl(out, zset.score(m));
            if out.len() - start > K_MAX_MSG {
                return abort_too_big(out, start);
            }
            member = zset.node_offset(m, 1);
            n += 1;
        }
        out_end_arr(out, ctx, (2 * n) as u32);
    }
}
