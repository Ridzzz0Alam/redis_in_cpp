//! Binary min-heap for key TTLs, ported from `heap.h` / `heap.cpp`.
//!
//! `HeapItem::pos` is a shared back-reference: every time an item moves, the
//! heap stores the new index through it, so the owner always knows where its
//! timer lives. (`Rc<Cell>` would be the natural choice, but `Database`
//! entries can be moved to the thread pool, so this uses `Arc<AtomicUsize>`.)

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Sentinel meaning "not in the heap".
pub const NO_POS: usize = usize::MAX;

/// Shared back-reference to an item's heap index.
pub type HeapRef = Arc<AtomicUsize>;

#[derive(Clone)]
pub struct HeapItem {
    pub val: u64,     // expiration time (ms)
    pub pos: HeapRef, // points at the owner's heap index
    pub data: usize,  // owner's arena index (Entry)
}

pub fn heap_parent(i: usize) -> usize {
    i.div_ceil(2) - 1
}

pub fn heap_left(i: usize) -> usize {
    i * 2 + 1
}

pub fn heap_right(i: usize) -> usize {
    i * 2 + 2
}

#[derive(Default)]
pub struct Heap {
    items: Vec<HeapItem>,
}

impl Heap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn peek(&self) -> Option<&HeapItem> {
        self.items.first()
    }

    /// The minimum expiration value, if the heap is non-empty.
    pub fn top_val(&self) -> Option<u64> {
        self.items.first().map(|item| item.val)
    }

    /// The item at `pos`, if it exists.
    pub fn get(&self, pos: usize) -> Option<&HeapItem> {
        self.items.get(pos)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    // Sifts move items with `swap`, so no `Arc` is cloned (two atomic RMWs)
    // per level; each moved item's back-reference is updated as it moves.
    fn set_pos(&self, pos: usize) {
        self.items[pos].pos.store(pos, Ordering::Relaxed);
    }

    fn up(&mut self, mut pos: usize) {
        while pos > 0 {
            let parent = heap_parent(pos);
            if self.items[parent].val <= self.items[pos].val {
                break;
            }
            self.items.swap(pos, parent);
            self.set_pos(pos);
            pos = parent;
        }
        self.set_pos(pos);
    }

    fn down(&mut self, mut pos: usize) {
        loop {
            let l = heap_left(pos);
            let r = heap_right(pos);
            let mut min_pos = pos;
            if l < self.items.len() && self.items[l].val < self.items[min_pos].val {
                min_pos = l;
            }
            if r < self.items.len() && self.items[r].val < self.items[min_pos].val {
                min_pos = r;
            }
            if min_pos == pos {
                break;
            }
            self.items.swap(pos, min_pos);
            self.set_pos(pos);
            pos = min_pos;
        }
        self.set_pos(pos);
    }

    /// Restore the heap property starting at `pos`.
    pub fn update(&mut self, pos: usize) {
        if pos > 0 && self.items[heap_parent(pos)].val > self.items[pos].val {
            self.up(pos);
        } else {
            self.down(pos);
        }
    }

    /// Remove the item at `pos` (swap-with-last, then re-sift).
    pub fn delete(&mut self, pos: usize) {
        let removed = self.items.swap_remove(pos);
        removed.pos.store(NO_POS, Ordering::Relaxed);
        if pos < self.items.len() {
            self.update(pos);
        }
    }

    /// Update an existing item or append a new one.
    pub fn upsert(&mut self, pos: Option<usize>, item: HeapItem) {
        let pos = match pos {
            Some(p) if p < self.items.len() => {
                self.items[p] = item;
                p
            }
            _ => {
                let p = self.items.len();
                self.items.push(item);
                p
            }
        };
        self.update(pos);
    }

    /// Remove and return the minimum item.
    pub fn pop_min(&mut self) -> Option<HeapItem> {
        if self.items.is_empty() {
            return None;
        }
        let item = self.items.swap_remove(0);
        item.pos.store(NO_POS, Ordering::Relaxed);
        if !self.items.is_empty() {
            self.update(0);
        }
        Some(item)
    }
}
