//! Port of `tests/test_hashtable.cpp`.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use redis_rust::hashtable::{HMap, HashNode};

struct Item {
    key: Vec<u8>,
    value: i32,
    hcode: u64,
    hnext: Option<usize>,
}

impl Item {
    fn new(key: &str, value: i32) -> Self {
        Self {
            key: key.as_bytes().to_vec(),
            value,
            hcode: 0,
            hnext: None,
        }
    }
}

impl HashNode for Item {
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

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn lookup<'a>(map: &'a mut HMap<Item>, key: &str) -> Option<&'a Item> {
    let idx = map.lookup(key.as_bytes(), redis_rust::common::str_hash(key.as_bytes()))?;
    Some(map.get(idx))
}

fn erase(map: &mut HMap<Item>, key: &str) -> bool {
    let hcode = redis_rust::common::str_hash(key.as_bytes());
    map.delete(key.as_bytes(), hcode).is_some()
}

#[test]
fn empty_table() {
    let mut map: HMap<Item> = HMap::new();
    assert_eq!(map.len(), 0);
    assert!(lookup(&mut map, "missing").is_none());
    assert!(!erase(&mut map, "missing"));
}

#[test]
fn insert_lookup_delete() {
    let mut map = HMap::new();
    map.insert(Item::new("a", 1));
    map.insert(Item::new("b", 2));
    assert_eq!(lookup(&mut map, "a").unwrap().value, 1);
    assert_eq!(lookup(&mut map, "b").unwrap().value, 2);
    assert_eq!(map.len(), 2);

    assert!(erase(&mut map, "a"));
    assert!(lookup(&mut map, "a").is_none());
    assert!(!erase(&mut map, "a"));
    assert_eq!(map.len(), 1);
}

// Enough keys to trigger many progressive rehashes; every key must stay
// reachable while keys are split between the old and new tables.
#[test]
fn matches_std_hash_map_through_rehashing() {
    let mut map: HMap<Item> = HMap::new();
    let mut oracle: HashMap<String, i32> = HashMap::new();
    let mut rng = Rng(42);

    for i in 0..20_000 {
        let key = format!("k{}", rng.next() % 10_000);
        if rng.next().is_multiple_of(4) {
            assert_eq!(
                erase(&mut map, &key),
                oracle.remove(&key).is_some(),
                "{key}"
            );
        } else if let Entry::Vacant(slot) = oracle.entry(key.clone()) {
            map.insert(Item::new(&key, i));
            slot.insert(i);
        }
        assert_eq!(map.len(), oracle.len());
    }

    for (key, value) in &oracle {
        let item = lookup(&mut map, key).expect(key);
        assert_eq!(item.value, *value);
    }
}

#[test]
fn foreach_visits_every_node_once() {
    let mut map = HMap::new();
    for i in 0..1000 {
        map.insert(Item::new(&i.to_string(), i));
    }
    let mut seen = vec![0u8; 1000];
    map.for_each(|_, item| {
        seen[item.value as usize] += 1;
        true
    });
    assert!(seen.iter().all(|&count| count == 1));
}

#[test]
fn foreach_stops_early() {
    let mut map = HMap::new();
    for i in 0..100 {
        map.insert(Item::new(&i.to_string(), i));
    }
    let mut visits = 0;
    map.for_each(|_, _| {
        visits += 1;
        visits < 10
    });
    assert_eq!(visits, 10);
}
