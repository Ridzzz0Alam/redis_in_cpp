//! Chained hashtable with bounded progressive rehashing, ported from
//! `hashtable.h` / `hashtable.cpp`.
//!
//! The C++ version is intrusive: it stores an `HNode` inside the payload and
//! recovers the payload with `container_of`. The Rust version keeps the same
//! algorithm but stores payloads in a stable arena and links them by index.
//! A `ZNode` can therefore still live in a hashtable and an AVL tree at the
//! same time without copying.

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

#[derive(Clone, Default)]
struct Table {
    slots: Vec<Option<usize>>,
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

/// Hashtable with two tables for progressive rehashing.
pub struct HMap<V> {
    nodes: Vec<Option<V>>,
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
            nodes: Vec::new(),
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
        self.nodes[idx].as_ref().expect("live hashtable node")
    }

    /// Mutable access to an arena slot.
    pub fn get_mut(&mut self, idx: usize) -> &mut V {
        self.nodes[idx].as_mut().expect("live hashtable node")
    }

    pub fn contains_index(&self, idx: usize) -> bool {
        self.nodes.get(idx).is_some_and(Option::is_some)
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
            if table.slots.is_empty() {
                continue;
            }
            for slot in &table.slots {
                let mut cur = *slot;
                while let Some(idx) = cur {
                    let node = self.nodes[idx].as_ref().expect("live hashtable node");
                    if !f(idx, node) {
                        return;
                    }
                    cur = node.hnext();
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
        self.free.clear();
        self.newer = Table::default();
        self.older = None;
        self.len = 0;
        self.migrate_pos = 0;
    }
}

fn h_insert<V: HashNode>(nodes: &mut [Option<V>], table: &mut Table, idx: usize) {
    let hcode = nodes[idx].as_ref().expect("live node").hcode();
    let pos = hcode as usize & table.mask;
    let head = table.slots[pos];
    nodes[idx].as_mut().expect("live node").set_hnext(head);
    table.slots[pos] = Some(idx);
    table.size += 1;
}

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

fn unlink<V: HashNode>(nodes: &mut [Option<V>], table: &mut Table, idx: usize, hcode: u64) -> bool {
    if table.slots.is_empty() {
        return false;
    }
    let pos = hcode as usize & table.mask;
    if table.slots[pos] == Some(idx) {
        let next = nodes[idx].as_ref().expect("live node").hnext();
        table.slots[pos] = next;
        table.size -= 1;
        return true;
    }
    let mut cur = table.slots[pos];
    while let Some(c) = cur {
        let next = nodes[c].as_ref().expect("live node").hnext();
        if next == Some(idx) {
            let nn = nodes[idx].as_ref().expect("live node").hnext();
            nodes[c].as_mut().expect("live node").set_hnext(nn);
            table.size -= 1;
            return true;
        }
        cur = next;
    }
    false
}

impl<V: HashNode> HMap<V> {
    fn alloc_node(&mut self, node: V) -> usize {
        if let Some(idx) = self.free.pop() {
            self.nodes[idx] = Some(node);
            idx
        } else {
            self.nodes.push(Some(node));
            self.nodes.len() - 1
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
            let idx = older.slots[pos].expect("non-empty slot");
            let next = self.nodes[idx].as_ref().expect("live node").hnext();
            older.slots[pos] = next;
            older.size -= 1;
            self.migrate_pos = pos;
            h_insert(&mut self.nodes, &mut self.newer, idx);
            nwork += 1;
        }
        if older.size == 0 {
            // Dropping `older` frees its slot array, like the C++ free().
        } else {
            self.older = Some(older);
        }
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
        if let Some(idx) = h_lookup(&self.nodes, &self.newer, key, hcode) {
            return Some(idx);
        }
        self.older
            .as_ref()
            .and_then(|t| h_lookup(&self.nodes, t, key, hcode))
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
        let hcode = self.nodes.get(idx)?.as_ref()?.hcode();
        let removed = unlink(&mut self.nodes, &mut self.newer, idx, hcode)
            || match self.older.as_mut() {
                Some(table) => unlink(&mut self.nodes, table, idx, hcode),
                None => false,
            };
        if !removed {
            return None;
        }
        let value = self.nodes[idx].take();
        self.free.push(idx);
        self.len -= 1;
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
