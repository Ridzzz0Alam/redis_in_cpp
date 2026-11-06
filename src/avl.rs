//! AVL tree with subtree counts, ported from `avl.h` / `avl.cpp`.
//!
//! The tree is generic over a node arena; `HMap<V>` implements [`AvlArena`] for
//! any `V` that is also an [`AvlNode`], so a sorted set can keep its nodes in
//! the hashtable arena and index them from both structures at once.

use std::cmp::max;

/// Link and augmentation fields of an AVL node.
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

/// An arena of AVL nodes addressed by stable indices.
pub trait AvlArena {
    type Node: AvlNode;
    fn avl_node(&self, idx: usize) -> &Self::Node;
    fn avl_node_mut(&mut self, idx: usize) -> &mut Self::Node;
}

/// Reset a node to a fresh leaf.
pub fn avl_init<N: AvlNode>(node: &mut N) {
    node.set_left(None);
    node.set_right(None);
    node.set_parent(None);
    node.set_height(1);
    node.set_cnt(1);
}

pub fn avl_height<A: AvlArena>(arena: &A, idx: Option<usize>) -> u32 {
    idx.map_or(0, |i| arena.avl_node(i).height())
}

pub fn avl_cnt<A: AvlArena>(arena: &A, idx: Option<usize>) -> u32 {
    idx.map_or(0, |i| arena.avl_node(i).cnt())
}

fn update<A: AvlArena>(arena: &mut A, idx: usize) {
    let left = arena.avl_node(idx).left();
    let right = arena.avl_node(idx).right();
    let height = 1 + max(avl_height(arena, left), avl_height(arena, right));
    let cnt = 1 + avl_cnt(arena, left) + avl_cnt(arena, right);
    let node = arena.avl_node_mut(idx);
    node.set_height(height);
    node.set_cnt(cnt);
}

fn rot_left<A: AvlArena>(arena: &mut A, node: usize) -> usize {
    let parent = arena.avl_node(node).parent();
    let new_node = arena.avl_node(node).right().expect("right child");
    let inner = arena.avl_node(new_node).left();

    // node <-> inner
    arena.avl_node_mut(node).set_right(inner);
    if let Some(inner) = inner {
        arena.avl_node_mut(inner).set_parent(Some(node));
    }
    // parent <- new_node
    arena.avl_node_mut(new_node).set_parent(parent);
    // new_node <-> node
    arena.avl_node_mut(new_node).set_left(Some(node));
    arena.avl_node_mut(node).set_parent(Some(new_node));

    update(arena, node);
    update(arena, new_node);
    new_node
}

fn rot_right<A: AvlArena>(arena: &mut A, node: usize) -> usize {
    let parent = arena.avl_node(node).parent();
    let new_node = arena.avl_node(node).left().expect("left child");
    let inner = arena.avl_node(new_node).right();

    // node <-> inner
    arena.avl_node_mut(node).set_left(inner);
    if let Some(inner) = inner {
        arena.avl_node_mut(inner).set_parent(Some(node));
    }
    // parent <- new_node
    arena.avl_node_mut(new_node).set_parent(parent);
    // new_node <-> node
    arena.avl_node_mut(new_node).set_right(Some(node));
    arena.avl_node_mut(node).set_parent(Some(new_node));

    update(arena, node);
    update(arena, new_node);
    new_node
}

fn fix_left<A: AvlArena>(arena: &mut A, node: usize) -> usize {
    let left = arena.avl_node(node).left().expect("left child");
    let left_left = arena.avl_node(left).left();
    let left_right = arena.avl_node(left).right();
    if avl_height(arena, left_left) < avl_height(arena, left_right) {
        let rotated = rot_left(arena, left);
        arena.avl_node_mut(node).set_left(Some(rotated));
    }
    rot_right(arena, node)
}

fn fix_right<A: AvlArena>(arena: &mut A, node: usize) -> usize {
    let right = arena.avl_node(node).right().expect("right child");
    let right_right = arena.avl_node(right).right();
    let right_left = arena.avl_node(right).left();
    if avl_height(arena, right_right) < avl_height(arena, right_left) {
        let rotated = rot_right(arena, right);
        arena.avl_node_mut(node).set_right(Some(rotated));
    }
    rot_left(arena, node)
}

/// Restore the AVL invariant from `node` up to the root; returns the new root.
pub fn avl_fix<A: AvlArena>(arena: &mut A, mut node: usize) -> usize {
    loop {
        let parent = arena.avl_node(node).parent();
        update(arena, node);

        let left = arena.avl_node(node).left();
        let right = arena.avl_node(node).right();
        let l = avl_height(arena, left);
        let r = avl_height(arena, right);
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

fn del_easy<A: AvlArena>(arena: &mut A, node: usize) -> Option<usize> {
    let left = arena.avl_node(node).left();
    let right = arena.avl_node(node).right();
    debug_assert!(left.is_none() || right.is_none());
    let child = if left.is_some() { left } else { right };
    let parent = arena.avl_node(node).parent();

    if let Some(child) = child {
        arena.avl_node_mut(child).set_parent(parent);
    }
    let Some(parent) = parent else {
        return child; // removing the root node
    };
    if arena.avl_node(parent).left() == Some(node) {
        arena.avl_node_mut(parent).set_left(child);
    } else {
        arena.avl_node_mut(parent).set_right(child);
    }
    Some(avl_fix(arena, parent))
}

/// Detach `node` and return the new root of the tree.
pub fn avl_del<A: AvlArena>(arena: &mut A, node: usize) -> Option<usize> {
    let node_left = arena.avl_node(node).left();
    let node_right = arena.avl_node(node).right();
    if node_left.is_none() || node_right.is_none() {
        return del_easy(arena, node);
    }

    // Find the successor (leftmost node of the right subtree).
    let mut victim = node_right.expect("right child");
    while let Some(left) = arena.avl_node(victim).left() {
        victim = left;
    }

    let root = del_easy(arena, victim);

    // Move the successor into the deleted node's place: copy the link fields
    // only (the payload stays in the arena, exactly like the intrusive C++).
    let parent = arena.avl_node(node).parent();
    let left = arena.avl_node(node).left();
    let right = arena.avl_node(node).right();
    let height = arena.avl_node(node).height();
    let cnt = arena.avl_node(node).cnt();
    {
        let v = arena.avl_node_mut(victim);
        v.set_parent(parent);
        v.set_left(left);
        v.set_right(right);
        v.set_height(height);
        v.set_cnt(cnt);
    }
    if let Some(left) = left {
        arena.avl_node_mut(left).set_parent(Some(victim));
    }
    if let Some(right) = right {
        arena.avl_node_mut(right).set_parent(Some(victim));
    }

    match parent {
        Some(parent) => {
            if arena.avl_node(parent).left() == Some(node) {
                arena.avl_node_mut(parent).set_left(Some(victim));
            } else {
                arena.avl_node_mut(parent).set_right(Some(victim));
            }
            root
        }
        None => Some(victim),
    }
}

/// Jump `offset` ranks forwards (or backwards if negative) from `node`.
/// Worst case O(log n), independent of how large the offset is.
pub fn avl_offset<A: AvlArena>(arena: &A, mut node: usize, offset: i64) -> Option<usize> {
    let mut pos: i64 = 0;
    while offset != pos {
        let right = arena.avl_node(node).right();
        let left = arena.avl_node(node).left();
        let right_cnt = i64::from(avl_cnt(arena, right));
        let left_cnt = i64::from(avl_cnt(arena, left));

        if pos < offset && pos + right_cnt >= offset {
            // The target is inside the right subtree.
            node = right.expect("right subtree");
            let new_left = arena.avl_node(node).left();
            pos += i64::from(avl_cnt(arena, new_left)) + 1;
        } else if pos > offset && pos - left_cnt <= offset {
            // The target is inside the left subtree.
            node = left.expect("left subtree");
            let new_right = arena.avl_node(node).right();
            pos -= i64::from(avl_cnt(arena, new_right)) + 1;
        } else {
            // Go to the parent.
            let parent = arena.avl_node(node).parent()?;
            if arena.avl_node(parent).right() == Some(node) {
                let left_cnt = i64::from(avl_cnt(arena, left));
                pos -= left_cnt + 1;
            } else {
                let right_cnt = i64::from(avl_cnt(arena, right));
                pos += right_cnt + 1;
            }
            node = parent;
        }
    }
    Some(node)
}
