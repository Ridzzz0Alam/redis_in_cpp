//! Port of `tests/test_zset.cpp`.

use std::collections::BTreeSet;

use redis_rust::zset::ZSet;

fn range(z: &ZSet, score: f64, name: &[u8], offset: i64, limit: usize) -> Vec<(Vec<u8>, f64)> {
    let mut out = Vec::new();
    let mut node = z.seekge(score, name);
    if let Some(start) = node {
        node = z.node_offset(start, offset);
    }
    while let Some(idx) = node {
        if out.len() >= limit {
            break;
        }
        out.push((z.node(idx).name().to_vec(), z.score(idx)));
        node = z.node_offset(idx, 1);
    }
    out
}

#[test]
fn insert_reports_new_vs_updated() {
    let mut z = ZSet::new();
    assert!(z.insert(b"a", 1.0));
    assert!(!z.insert(b"a", 2.0));
    let idx = z.lookup(b"a").unwrap();
    assert_eq!(z.score(idx), 2.0);
    assert!(z.lookup(b"b").is_none());
}

#[test]
fn ordered_by_score_then_name() {
    let mut z = ZSet::new();
    z.insert(b"c", 1.0);
    z.insert(b"a", 2.0);
    z.insert(b"b", 1.0);
    z.insert(b"", 1.0);
    let all = range(&z, -1e300, b"", 0, 100);
    let expected = vec![
        (b"".to_vec(), 1.0),
        (b"b".to_vec(), 1.0),
        (b"c".to_vec(), 1.0),
        (b"a".to_vec(), 2.0),
    ];
    assert_eq!(all, expected);
}

#[test]
fn seek_ge_and_offset() {
    let mut z = ZSet::new();
    for i in 0..10 {
        z.insert(format!("n{i}").as_bytes(), f64::from(i));
    }
    let mut r = range(&z, 4.5, b"", 0, 3);
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].0, b"n5");

    r = range(&z, 4.5, b"", 2, 100);
    assert_eq!(r.len(), 3); // n7, n8, n9
    assert_eq!(r[0].0, b"n7");

    r = range(&z, 4.5, b"", -2, 1); // offsets can go backwards
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, b"n3");

    assert!(range(&z, 100.0, b"", 0, 10).is_empty());
}

#[test]
fn update_moves_node_in_order() {
    let mut z = ZSet::new();
    z.insert(b"x", 1.0);
    z.insert(b"y", 2.0);
    z.insert(b"x", 3.0);
    let all = range(&z, 0.0, b"", 0, 10);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].0, b"y");
    assert_eq!(all[1].0, b"x");
}

#[test]
fn delete_keeps_both_indexes_in_sync() {
    let mut z = ZSet::new();
    for i in 0..100 {
        z.insert(format!("m{i}").as_bytes(), f64::from(i % 7));
    }
    for i in (0..100).step_by(2) {
        let node = z.lookup(format!("m{i}").as_bytes()).unwrap();
        z.delete(node);
    }
    assert_eq!(range(&z, -1.0, b"", 0, 1000).len(), 50);
    assert!(z.lookup(b"m0").is_none());
    assert!(z.lookup(b"m1").is_some());
}

#[test]
fn randomized_against_std_btree_set() {
    let mut z = ZSet::new();
    let mut oracle: BTreeSet<(i64, String)> = BTreeSet::new();
    let mut rng = 3u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };

    for _ in 0..3000 {
        let name = format!("k{}", next() % 300);
        let score = (next() % 50) as i64;
        let existing = oracle.iter().find(|(_, n)| *n == name).cloned();
        if next() % 3 == 0 {
            let node = z.lookup(name.as_bytes());
            assert_eq!(node.is_some(), existing.is_some());
            if let Some(node) = node {
                z.delete(node);
                let old = existing.unwrap();
                oracle.remove(&old);
            }
        } else {
            let added = z.insert(name.as_bytes(), score as f64);
            assert_eq!(added, existing.is_none());
            if let Some(old) = existing {
                oracle.remove(&old);
            }
            oracle.insert((score, name));
        }
    }

    let all = range(&z, -1.0, b"", 0, 100_000);
    assert_eq!(all.len(), oracle.len());
    for (got, expected) in all.iter().zip(oracle.iter()) {
        assert_eq!(got.0, expected.1.as_bytes());
        assert_eq!(got.1, expected.0 as f64);
    }
}
