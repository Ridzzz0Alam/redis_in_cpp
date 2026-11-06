//! Chained hashtable with bounded progressive rehashing, ported from
//! `hashtable.h` / `hashtable.cpp`.
//!
//! The C++ version is intrusive: it stores an `HNode` inside the payload and
//! recovers the payload with `container_of`. The Rust version keeps the same
//! algorithm but stores payloads in a stable arena and links them by index.
//! A `ZNode` can therefore still live in a hashtable and an AVL tree at the
//! same time without copying.

use std::num::NonZeroUsize;

use crate::avl::{AvlArena, AvlNode};
use crate::common::str_hash;

/// A payload that can be stored in an [`HMap`].
pub trait HashNode {
    fn key(&self) -> &[u8];
    fn hcode(&self) -> u64;
    fn set_hcode(&mut self, hcode: u64);
    fn hnext(&self) -> Option<usize>;
    fn set_hnext(&mut self, next: Option<usize>);
}

/// A slot link: arena index + 1, so that `None` is all-zero bytes. That lets
/// `vec![None; n]` use a zeroed (lazily-paged) allocation instead of writing
/// every slot, which keeps `trigger_rehashing` cheap for huge tables.
type Link = Option<NonZeroUsize>;

fn to_link(idx: usize) -> Link {
    NonZeroUsize::new(idx + 1)
}

fn from_link(link: Link) -> Option<usize> {
    link.map(|l| l.get() - 1)
}

#[derive(Clone, Default)]
struct Table {
    slots: Vec<Link>,
    mask: usize,
    size: usize,
}

impl Table {
    fn new(n: usize) -> Self {
        debug_assert!(n > 0 && (n - 1) & n == 0);
        Self {
            slots: vec![None; n],
            mask: n - 1,
            size: 0,
        }
    }
}

const REHASHING_WORK: usize = 128;
const MAX_LOAD_FACTOR: usize = 8;

/// Node storage split into fixed-size chunks.
///
/// A single `Vec<Option<V>>` reallocates and copies *every* node when it
/// grows, which is the same O(n) pause progressive rehashing exists to avoid.
/// Chunks never move once allocated; only the small vector of chunk handles
/// grows. The first chunk grows normally (bounded by `CHUNK`) so tiny maps,
/// such as small sorted sets, don't pay for a full chunk up front.
struct Arena<V> {
    chunks: Vec<Vec<Option<V>>>,
    len: usize,
}

const CHUNK_BITS: usize = 10;
const CHUNK: usize = 1 << CHUNK_BITS;

impl<V> Arena<V> {
    fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }

    fn slot(&self, idx: usize) -> Option<&Option<V>> {
        if idx >= self.len {
            return None;
        }
        Some(&self.chunks[idx >> CHUNK_BITS][idx & (CHUNK - 1)])
    }

    fn slot_mut(&mut self, idx: usize) -> &mut Option<V> {
        assert!(idx < self.len, "arena index out of range");
        &mut self.chunks[idx >> CHUNK_BITS][idx & (CHUNK - 1)]
    }

    fn node(&self, idx: usize) -> &V {
        self.slot(idx)
            .and_then(Option::as_ref)
            .expect("live hashtable node")
    }

    fn node_mut(&mut self, idx: usize) -> &mut V {
        self.slot_mut(idx).as_mut().expect("live hashtable node")
    }

    fn push(&mut self, value: V) -> usize {
        match self.chunks.last_mut() {
            Some(chunk) if chunk.len() < CHUNK => chunk.push(Some(value)),
            _ => {
                let mut chunk = if self.chunks.is_empty() {
                    Vec::new()
                } else {
                    Vec::with_capacity(CHUNK)
                };
                chunk.push(Some(value));
                self.chunks.push(chunk);
            }
        }
        self.len += 1;
        self.len - 1
    }

    fn clear(&mut self) {
        self.chunks = Vec::new();
        self.len = 0;
    }
}

/// Hashtable with two tables for progressive rehashing.
pub struct HMap<V> {
    nodes: Arena<V>,
    free: Vec<usize>,
    newer: Table,
    older: Option<Table>,
    len: usize,
    migrate_pos: usize,
}

impl<V> Default for HMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> HMap<V> {
    pub fn new() -> Self {
        Self {
            nodes: Arena::new(),
            free: Vec::new(),
            newer: Table::default(),
            older: None,
            len: 0,
            migrate_pos: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Immutable access to an arena slot. Panics if `idx` is not live.
    pub fn get(&self, idx: usize) -> &V {
        self.nodes.node(idx)
    }

    /// Mutable access to an arena slot.
    pub fn get_mut(&mut self, idx: usize) -> &mut V {
        self.nodes.node_mut(idx)
    }

    pub fn contains_index(&self, idx: usize) -> bool {
        self.nodes.slot(idx).is_some_and(Option::is_some)
    }

    /// Iterate over every live node.
    pub fn for_each(&self, mut f: impl FnMut(usize, &V) -> bool)
    where
        V: HashNode,
    {
        for table in [Some(&self.newer), self.older.as_ref()]
            .into_iter()
            .flatten()
        {
            for slot in &table.slots {
                let mut cur = from_link(*slot);
                while let Some(idx) = cur {
                    let node = self.nodes.node(idx);
                    if !f(idx, node) {
                        return;
                    }
                    cur = node.hnext();
                }
            }
        }
    }

    /// Drop every node and release all memory.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.free = Vec::new();
        self.newer = Table::default();
        self.older = None;
        self.len = 0;
        self.migrate_pos = 0;
    }
}

fn h_insert<V: HashNode>(nodes: &mut Arena<V>, table: &mut Table, idx: usize) {
    let hcode = nodes.node(idx).hcode();
    let pos = hcode as usize & table.mask;
    let head = from_link(table.slots[pos]);
    nodes.node_mut(idx).set_hnext(head);
    table.slots[pos] = to_link(idx);
    table.size += 1;
}

fn h_lookup<V: HashNode>(nodes: &Arena<V>, table: &Table, key: &[u8], hcode: u64) -> Option<usize> {
    if table.slots.is_empty() {
        return None;
    }
    let pos = hcode as usize & table.mask;
    let mut cur = from_link(table.slots[pos]);
    while let Some(idx) = cur {
        let node = nodes.node(idx);
        if node.hcode() == hcode && node.key() == key {
            return Some(idx);
        }
        cur = node.hnext();
    }
    None
}

fn unlink<V: HashNode>(nodes: &mut Arena<V>, table: &mut Table, idx: usize, hcode: u64) -> bool {
    if table.slots.is_empty() {
        return false;
    }
    let pos = hcode as usize & table.mask;
    if from_link(table.slots[pos]) == Some(idx) {
        table.slots[pos] = to_link_opt(nodes.node(idx).hnext());
        table.size -= 1;
        return true;
    }
    let mut cur = from_link(table.slots[pos]);
    while let Some(c) = cur {
        let next = nodes.node(c).hnext();
        if next == Some(idx) {
            let nn = nodes.node(idx).hnext();
            nodes.node_mut(c).set_hnext(nn);
            table.size -= 1;
            return true;
        }
        cur = next;
    }
    false
}

fn to_link_opt(idx: Option<usize>) -> Link {
    idx.and_then(to_link)
}

impl<V: HashNode> HMap<V> {
    fn alloc_node(&mut self, node: V) -> usize {
        if let Some(idx) = self.free.pop() {
            *self.nodes.slot_mut(idx) = Some(node);
            idx
        } else {
            self.nodes.push(node)
        }
    }

    fn trigger_rehashing(&mut self) {
        debug_assert!(self.older.is_none());
        let old = std::mem::take(&mut self.newer);
        let new_size = (old.mask + 1) * 2;
        self.older = Some(old);
        self.newer = Table::new(new_size);
        self.migrate_pos = 0;
    }

    fn help_rehashing(&mut self) {
        let Some(mut older) = self.older.take() else {
            return;
        };
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
            let idx = from_link(older.slots[pos]).expect("non-empty slot");
            older.slots[pos] = to_link_opt(self.nodes.node(idx).hnext());
            older.size -= 1;
            self.migrate_pos = pos;
            h_insert(&mut self.nodes, &mut self.newer, idx);
            nwork += 1;
        }
        if older.size > 0 {
            self.older = Some(older);
        }
        // else: dropping `older` frees its slot array, like the C++ free().
    }

    /// Insert a node (its key is hashed here) and return its stable arena index.
    pub fn insert(&mut self, mut node: V) -> usize {
        let hcode = str_hash(node.key());
        node.set_hcode(hcode);
        if self.newer.slots.is_empty() {
            self.newer = Table::new(4);
        }
        let idx = self.alloc_node(node);
        h_insert(&mut self.nodes, &mut self.newer, idx);
        self.len += 1;

        if self.older.is_none() {
            let threshold = (self.newer.mask + 1) * MAX_LOAD_FACTOR;
            if self.newer.size >= threshold {
                self.trigger_rehashing();
            }
        }
        self.help_rehashing();
        idx
    }

    /// Look a node up by key. Returns its arena index.
    pub fn lookup(&mut self, key: &[u8], hcode: u64) -> Option<usize> {
        self.help_rehashing();
        self.lookup_no_rehash(key, hcode)
    }

    /// Look a node up by key without triggering rehashing.
    pub fn lookup_no_rehash(&self, key: &[u8], hcode: u64) -> Option<usize> {
        if let Some(idx) = h_lookup(&self.nodes, &self.newer, key, hcode) {
            return Some(idx);
        }
        self.older
            .as_ref()
            .and_then(|t| h_lookup(&self.nodes, t, key, hcode))
    }

    /// Delete by key, returning the removed payload.
    pub fn delete(&mut self, key: &[u8], hcode: u64) -> Option<V> {
        let idx = self.lookup(key, hcode)?;
        self.remove_index(idx)
    }

    /// Unlink and remove a node by its arena index, returning the payload.
    pub fn remove_index(&mut self, idx: usize) -> Option<V> {
        let hcode = self.nodes.slot(idx)?.as_ref()?.hcode();
        let removed = unlink(&mut self.nodes, &mut self.newer, idx, hcode)
            || match self.older.as_mut() {
                Some(table) => unlink(&mut self.nodes, table, idx, hcode),
                None => false,
            };
        if !removed {
            return None;
        }
        let value = self.nodes.slot_mut(idx).take();
        self.len -= 1;
        if self.len == 0 {
            // Nothing references the arena any more: give the memory back
            // instead of keeping the high-water mark forever.
            self.clear();
        } else {
            self.free.push(idx);
        }
        value
    }
}

impl<V: HashNode + AvlNode> AvlArena for HMap<V> {
    type Node = V;

    fn avl_node(&self, idx: usize) -> &V {
        self.get(idx)
    }

    fn avl_node_mut(&mut self, idx: usize) -> &mut V {
        self.get_mut(idx)
    }
}
