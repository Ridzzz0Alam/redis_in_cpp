//! Port of `tests/test_heap.cpp` (the book's test_heap).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use redis_rust::heap::{heap_left, heap_parent, heap_right, Heap, HeapItem, HeapRef, NO_POS};

struct Container {
    heap: Heap,
    map: Vec<(u64, HeapRef)>,
}

impl Container {
    fn new() -> Self {
        Self {
            heap: Heap::new(),
            map: Vec::new(),
        }
    }

    fn add(&mut self, val: u64) {
        let reference = Arc::new(AtomicUsize::new(NO_POS));
        self.map.push((val, Arc::clone(&reference)));
        let pos = self.heap.len();
        self.heap.upsert(
            None,
            HeapItem {
                val,
                pos: reference,
                data: 0,
            },
        );
        assert!(pos < self.heap.len());
    }

    fn del(&mut self, val: u64) {
        let idx = self
            .map
            .iter()
            .position(|(v, _)| *v == val)
            .expect("value in map");
        let pos = self.map[idx].1.load(Ordering::Relaxed);
        assert_eq!(self.heap.get(pos).unwrap().val, val);
        assert!(Arc::ptr_eq(
            &self.heap.get(pos).unwrap().pos,
            &self.map[idx].1
        ));
        self.heap.delete(pos);
        self.map.remove(idx);
    }

    fn verify(&self) {
        assert_eq!(self.heap.len(), self.map.len());
        for i in 0..self.heap.len() {
            let l = heap_left(i);
            let r = heap_right(i);
            assert!(
                l >= self.heap.len()
                    || self.heap.get(l).unwrap().val >= self.heap.get(i).unwrap().val
            );
            assert!(
                r >= self.heap.len()
                    || self.heap.get(r).unwrap().val >= self.heap.get(i).unwrap().val
            );
            assert_eq!(self.heap.get(i).unwrap().pos.load(Ordering::Relaxed), i);
            if i > 0 {
                assert!(heap_parent(i) < i);
            }
        }
    }
}

#[test]
fn insert_anywhere() {
    for sz in 0..120u32 {
        for j in 0..(2 + sz * 2) {
            let mut c = Container::new();
            for i in 0..sz {
                c.add(1 + u64::from(i) * 2);
            }
            c.verify();
            c.add(u64::from(j));
            c.verify();
        }
    }
}

#[test]
fn delete_anywhere() {
    for sz in 0..120u32 {
        for j in 0..sz {
            let mut c = Container::new();
            for i in 0..sz {
                c.add(u64::from(i));
            }
            c.verify();
            c.del(u64::from(j));
            c.verify();
        }
    }
}

#[test]
fn minimum_is_always_at_the_top() {
    let mut c = Container::new();
    for v in [50u64, 10, 40, 5, 99, 7] {
        c.add(v);
        c.verify();
    }
    assert_eq!(c.heap.peek().unwrap().val, 5);
    c.del(5);
    assert_eq!(c.heap.peek().unwrap().val, 7);
}
