//! Port of `tests/test_avl.cpp` (the book's test_avl + test_offset).

use redis_rust::avl::{
    avl_cnt, avl_del, avl_fix, avl_height, avl_init, avl_offset, AvlArena, AvlNode,
};

struct TestNode {
    val: u32,
    parent: Option<usize>,
    left: Option<usize>,
    right: Option<usize>,
    height: u32,
    cnt: u32,
}

impl AvlNode for TestNode {
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

#[derive(Default)]
struct Container {
    arena: Vec<TestNode>,
    root: Option<usize>,
}

impl AvlArena for Container {
    type Node = TestNode;

    fn avl_node(&self, idx: usize) -> &TestNode {
        &self.arena[idx]
    }

    fn avl_node_mut(&mut self, idx: usize) -> &mut TestNode {
        &mut self.arena[idx]
    }
}

impl Container {
    fn add(&mut self, val: u32) {
        let mut node = TestNode {
            val,
            parent: None,
            left: None,
            right: None,
            height: 0,
            cnt: 0,
        };
        avl_init(&mut node);
        self.arena.push(node);
        let idx = self.arena.len() - 1;

        let mut cur = None;
        let mut from = self.root;
        while let Some(c) = from {
            cur = Some(c);
            from = if val < self.arena[c].val {
                self.arena[c].left
            } else {
                self.arena[c].right
            };
        }
        match cur {
            Some(c) => {
                if val < self.arena[c].val {
                    self.arena[c].left = Some(idx);
                } else {
                    self.arena[c].right = Some(idx);
                }
            }
            None => self.root = Some(idx),
        }
        self.arena[idx].parent = cur;
        let root = avl_fix(self, idx);
        self.root = Some(root);
    }

    fn del(&mut self, val: u32) -> bool {
        let mut cur = self.root;
        while let Some(c) = cur {
            if val == self.arena[c].val {
                break;
            }
            cur = if val < self.arena[c].val {
                self.arena[c].left
            } else {
                self.arena[c].right
            };
        }
        let Some(idx) = cur else {
            return false;
        };
        let root = avl_del(self, idx);
        self.root = root;
        true
    }
}

fn verify_node(arena: &Container, parent: Option<usize>, node: Option<usize>) {
    let Some(idx) = node else { return };
    assert_eq!(arena.arena[idx].parent, parent);
    verify_node(arena, Some(idx), arena.arena[idx].left);
    verify_node(arena, Some(idx), arena.arena[idx].right);
    assert_eq!(
        arena.arena[idx].cnt,
        1 + avl_cnt(arena, arena.arena[idx].left) + avl_cnt(arena, arena.arena[idx].right)
    );
    let l = avl_height(arena, arena.arena[idx].left);
    let r = avl_height(arena, arena.arena[idx].right);
    assert!(l == r || l + 1 == r || l == r + 1, "unbalanced");
    assert_eq!(arena.arena[idx].height, 1 + l.max(r));
    let val = arena.arena[idx].val;
    if let Some(left) = arena.arena[idx].left {
        assert!(arena.arena[left].val <= val);
    }
    if let Some(right) = arena.arena[idx].right {
        assert!(arena.arena[right].val >= val);
    }
}

fn extract(arena: &Container, node: Option<usize>, out: &mut Vec<u32>) {
    let Some(idx) = node else { return };
    extract(arena, arena.arena[idx].left, out);
    out.push(arena.arena[idx].val);
    extract(arena, arena.arena[idx].right, out);
}

fn verify(arena: &Container, reference: &[u32]) {
    verify_node(arena, None, arena.root);
    assert_eq!(avl_cnt(arena, arena.root) as usize, reference.len());
    let mut got = Vec::new();
    extract(arena, arena.root, &mut got);
    let mut expected = reference.to_vec();
    expected.sort_unstable();
    got.sort_unstable();
    assert_eq!(got, expected);
}

#[test]
fn empty_and_single() {
    let mut c = Container::default();
    verify(&c, &[]);
    c.add(123);
    verify(&c, &[123]);
    assert!(!c.del(124));
    assert!(c.del(123));
    verify(&c, &[]);
}

#[test]
fn sequential_insert_stays_balanced() {
    let mut c = Container::default();
    let mut reference = Vec::new();
    for i in (0..1000).step_by(3) {
        c.add(i);
        reference.push(i);
        verify(&c, &reference);
    }
}

#[test]
fn random_insert_and_delete() {
    let mut c = Container::default();
    let mut reference: Vec<u32> = Vec::new();
    let mut rng = 7u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % 1000) as u32
    };

    for _ in 0..300 {
        let val = next();
        c.add(val);
        reference.push(val);
        verify(&c, &reference);
    }
    for _ in 0..600 {
        let val = next();
        if let Some(pos) = reference.iter().position(|&v| v == val) {
            assert!(c.del(val));
            reference.remove(pos);
        } else {
            assert!(!c.del(val));
        }
        verify(&c, &reference);
    }
}

#[test]
fn insert_and_remove_at_every_position() {
    for sz in 0..100u32 {
        for val in 0..sz {
            {
                let mut c = Container::default();
                let mut reference = Vec::new();
                for i in 0..sz {
                    if i != val {
                        c.add(i);
                        reference.push(i);
                    }
                }
                verify(&c, &reference);
                c.add(val);
                reference.push(val);
                verify(&c, &reference);
            }
            {
                let mut c = Container::default();
                let mut reference = Vec::new();
                for i in 0..sz {
                    c.add(i);
                    reference.push(i);
                }
                assert!(c.del(val));
                reference.retain(|&v| v != val);
                verify(&c, &reference);
            }
        }
    }
}

#[test]
fn offset_reaches_every_rank() {
    for sz in 1..100u32 {
        let mut c = Container::default();
        for i in 0..sz {
            c.add(i);
        }
        let mut min = c.root.unwrap();
        while let Some(left) = c.arena[min].left {
            min = left;
        }
        for i in 0..sz {
            let node = avl_offset(&c, min, i64::from(i)).unwrap();
            assert_eq!(c.arena[node].val, i);
            for j in 0..sz {
                let offset = i64::from(j) - i64::from(i);
                let n2 = avl_offset(&c, node, offset).unwrap();
                assert_eq!(c.arena[n2].val, j);
            }
            assert_eq!(avl_offset(&c, node, -i64::from(i) - 1), None);
            assert_eq!(avl_offset(&c, node, i64::from(sz - i)), None);
        }
    }
}
