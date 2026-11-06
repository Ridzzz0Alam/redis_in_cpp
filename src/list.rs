//! Intrusive-style circular doubly-linked list, ported from `list.h`.
//!
//! The C++ list embeds `DList` nodes inside the payload and uses
//! `container_of` to recover the payload. Here each node stores a `usize`
//! handle (for the server that is the connection's fd), and node ids are
//! stable indices. Insert, detach and front are all O(1), just like the
//! original.

#[derive(Clone, Copy)]
struct Node {
    prev: usize,
    next: usize,
    value: usize,
}

/// Circular list whose first node (index 0) is a sentinel.
pub struct DList {
    nodes: Vec<Node>,
    free: Vec<usize>,
    len: usize,
}

impl DList {
    pub fn new() -> Self {
        Self {
            nodes: vec![Node {
                prev: 0,
                next: 0,
                value: usize::MAX,
            }],
            free: Vec::new(),
            len: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes[0].next == 0
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Append a value to the tail (insert before the sentinel). Returns the
    /// stable node id needed by [`detach`](Self::detach).
    pub fn push_back(&mut self, value: usize) -> usize {
        let id = if let Some(id) = self.free.pop() {
            self.nodes[id] = Node {
                prev: 0,
                next: 0,
                value,
            };
            id
        } else {
            self.nodes.push(Node {
                prev: 0,
                next: 0,
                value,
            });
            self.nodes.len() - 1
        };
        let head = 0;
        let prev = self.nodes[head].prev;
        self.nodes[id].prev = prev;
        self.nodes[id].next = head;
        self.nodes[prev].next = id;
        self.nodes[head].prev = id;
        self.len += 1;
        id
    }

    /// Remove a node from the list. Safe to call once per id.
    pub fn detach(&mut self, id: usize) {
        let prev = self.nodes[id].prev;
        let next = self.nodes[id].next;
        self.nodes[prev].next = next;
        self.nodes[next].prev = prev;
        self.free.push(id);
        self.len -= 1;
    }

    /// The value at the front (the next deadline), if any.
    pub fn front(&self) -> Option<usize> {
        if self.is_empty() {
            None
        } else {
            Some(self.nodes[self.nodes[0].next].value)
        }
    }

    /// Move `id` to the tail. Equivalent to detach + push_back with the same id.
    pub fn move_to_back(&mut self, id: usize) {
        let head = 0;
        let prev = self.nodes[id].prev;
        let next = self.nodes[id].next;
        self.nodes[prev].next = next;
        self.nodes[next].prev = prev;

        let tail = self.nodes[head].prev;
        self.nodes[id].prev = tail;
        self.nodes[id].next = head;
        self.nodes[tail].next = id;
        self.nodes[head].prev = id;
    }

    /// Values in list order (front to back), for tests.
    pub fn values(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.len);
        let mut cur = self.nodes[0].next;
        while cur != 0 {
            out.push(self.nodes[cur].value);
            cur = self.nodes[cur].next;
        }
        out
    }
}

impl Default for DList {
    fn default() -> Self {
        Self::new()
    }
}
