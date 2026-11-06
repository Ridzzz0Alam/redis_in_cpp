//! Sorted set: name -> node hashtable plus an AVL tree ordered by
//! `(score, name)`, ported from `zset.h` / `zset.cpp`.
//!
//! Both indexes address the same `HMap<ZNode>` arena, so a `ZNode` is in both
//! containers at once with one allocation — the same payoff as the C++
//! intrusive design.

use crate::avl::{avl_del, avl_fix, avl_init, avl_offset, AvlNode};
use crate::common::str_hash;
use crate::hashtable::{HMap, HashNode};

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

impl ZNode {
    fn new(name: &[u8], score: f64) -> Self {
        Self {
            name: name.to_vec(),
            score,
            hcode: 0,
            hnext: None,
            parent: None,
            left: None,
            right: None,
            height: 1,
            cnt: 1,
        }
    }

    pub fn name(&self) -> &[u8] {
        &self.name
    }
}

impl HashNode for ZNode {
    fn key(&self) -> &[u8] {
        &self.name
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

impl AvlNode for ZNode {
    fn parent(&self) -> Option<usize> {
        self.parent
    }
    fn set_parent(&mut self, parent: Option<usize>) {
        self.parent = parent;
    }
    fn left(&self) -> Option<usize> {
        self.left
    }
    fn set_left(&mut self, left: Option<usize>) {
        self.left = left;
    }
    fn right(&self) -> Option<usize> {
        self.right
    }
    fn set_right(&mut self, right: Option<usize>) {
        self.right = right;
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn set_height(&mut self, height: u32) {
        self.height = height;
    }
    fn cnt(&self) -> u32 {
        self.cnt
    }
    fn set_cnt(&mut self, cnt: u32) {
        self.cnt = cnt;
    }
}

/// Compare a node with a `(score, name)` key.
fn less_key(node: &ZNode, score: f64, name: &[u8]) -> bool {
    if node.score != score {
        node.score < score
    } else {
        node.name.as_slice() < name
    }
}

fn less_node(lhs: &ZNode, rhs: &ZNode) -> bool {
    less_key(lhs, rhs.score, &rhs.name)
}

#[derive(Default)]
pub struct ZSet {
    pub hmap: HMap<ZNode>,
    root: Option<usize>,
}

impl ZSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.hmap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hmap.is_empty()
    }

    /// Add a new `(score, name)` tuple, or update an existing member's score.
    /// Returns `true` if a new member was added.
    pub fn insert(&mut self, name: &[u8], score: f64) -> bool {
        if let Some(idx) = self.lookup(name) {
            self.update(idx, score);
            return false;
        }
        let idx = self.hmap.insert(ZNode::new(name, score));
        self.tree_insert(idx);
        true
    }

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

    fn update(&mut self, idx: usize, score: f64) {
        if self.hmap.get(idx).score == score {
            return;
        }
        self.root = avl_del(&mut self.hmap, idx);
        avl_init(self.hmap.get_mut(idx));
        self.hmap.get_mut(idx).score = score;
        self.tree_insert(idx);
    }

    /// Look a member up by name.
    pub fn lookup(&mut self, name: &[u8]) -> Option<usize> {
        self.root?;
        let hcode = str_hash(name);
        self.hmap.lookup(name, hcode)
    }

    /// Look a member up by name without triggering rehashing.
    pub fn lookup_no_rehash(&self, name: &[u8]) -> Option<usize> {
        self.root?;
        let hcode = str_hash(name);
        self.hmap.lookup_no_rehash(name, hcode)
    }

    pub fn node(&self, idx: usize) -> &ZNode {
        self.hmap.get(idx)
    }

    pub fn score(&self, idx: usize) -> f64 {
        self.hmap.get(idx).score
    }

    /// Delete a member by arena index, keeping both indexes in sync.
    pub fn delete(&mut self, idx: usize) {
        self.root = avl_del(&mut self.hmap, idx);
        self.hmap.remove_index(idx);
    }

    /// First member >= `(score, name)`.
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

    /// Jump `offset` members forwards/backwards from `idx`.
    pub fn node_offset(&self, idx: usize, offset: i64) -> Option<usize> {
        avl_offset(&self.hmap, idx, offset)
    }

    pub fn clear(&mut self) {
        self.hmap.clear();
        self.root = None;
    }
}
