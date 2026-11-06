//! Port of `tests/test_database.cpp`; command expectations mirror the book's
//! `test_cmds.py`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use redis_rust::database::Database;
use redis_rust::protocol::{
    decode_value, Value, ERR_BAD_ARG, ERR_BAD_TYP, ERR_TOO_BIG, ERR_UNKNOWN,
};
use redis_rust::thread_pool::ThreadPool;
use redis_rust::Buffer;

struct Harness {
    db: Database,
    now: Arc<AtomicU64>,
}

impl Harness {
    fn new() -> Self {
        let now = Arc::new(AtomicU64::new(1_000_000));
        let clock_now = Arc::clone(&now);
        let db = Database::with_clock(None, Box::new(move || clock_now.load(Ordering::Relaxed)));
        Self { db, now }
    }

    fn run(&mut self, cmd: &[&str]) -> Value {
        let mut cmd: Vec<Vec<u8>> = cmd.iter().map(|s| s.as_bytes().to_vec()).collect();
        let mut out = Buffer::new();
        self.db.execute(&mut cmd, &mut out);
        let (value, consumed) = decode_value(&out).expect("decode");
        assert_eq!(consumed, out.len(), "exactly one value per command");
        value
    }

    fn now(&self) -> u64 {
        self.now.load(Ordering::Relaxed)
    }

    fn advance(&self, by: u64) {
        self.now.fetch_add(by, Ordering::Relaxed);
    }
}

fn s(v: &str) -> Value {
    Value::Str(v.as_bytes().to_vec())
}
fn num(v: i64) -> Value {
    Value::Int(v)
}
fn dbl(v: f64) -> Value {
    Value::Dbl(v)
}
fn nil() -> Value {
    Value::Nil
}
fn arr(v: Vec<Value>) -> Value {
    Value::Arr(v)
}
fn err_code(v: &Value) -> u32 {
    match v {
        Value::Error { code, .. } => *code,
        _ => 0,
    }
}

#[test]
fn get_set_del() {
    let mut h = Harness::new();
    assert_eq!(h.run(&["get", "k"]), nil());
    assert_eq!(h.run(&["set", "k", "v"]), nil());
    assert_eq!(h.run(&["get", "k"]), s("v"));
    assert_eq!(h.run(&["set", "k", "v2"]), nil());
    assert_eq!(h.run(&["get", "k"]), s("v2"));
    assert_eq!(h.run(&["del", "k"]), num(1));
    assert_eq!(h.run(&["del", "k"]), num(0));
    assert_eq!(h.run(&["get", "k"]), nil());
    assert_eq!(h.db.size(), 0);
}

#[test]
fn unknown_commands_and_wrong_arity() {
    let mut h = Harness::new();
    assert_eq!(err_code(&h.run(&["nope"])), ERR_UNKNOWN);
    assert_eq!(err_code(&h.run(&["get"])), ERR_UNKNOWN);
    assert_eq!(err_code(&h.run(&["set", "k"])), ERR_UNKNOWN);
    assert_eq!(err_code(&h.run(&[])), ERR_UNKNOWN);
}

#[test]
fn keys() {
    let mut h = Harness::new();
    assert_eq!(h.run(&["keys"]), arr(vec![]));
    h.run(&["set", "a", "1"]);
    h.run(&["zadd", "z", "1", "m"]);
    match h.run(&["keys"]) {
        Value::Arr(items) => {
            assert_eq!(items.len(), 2);
            assert!(
                (items[0] == s("a") && items[1] == s("z"))
                    || (items[0] == s("z") && items[1] == s("a"))
            );
        }
        other => panic!("expected array, got {other:?}"),
    }
}

#[test]
fn type_errors() {
    let mut h = Harness::new();
    h.run(&["set", "s", "v"]);
    h.run(&["zadd", "z", "1", "m"]);
    assert_eq!(err_code(&h.run(&["zadd", "s", "1", "m"])), ERR_BAD_TYP);
    assert_eq!(err_code(&h.run(&["zscore", "s", "m"])), ERR_BAD_TYP);
    assert_eq!(err_code(&h.run(&["get", "z"])), ERR_BAD_TYP);
    assert_eq!(err_code(&h.run(&["set", "z", "v"])), ERR_BAD_TYP);
}

#[test]
fn bad_numeric_arguments() {
    let mut h = Harness::new();
    assert_eq!(err_code(&h.run(&["zadd", "z", "abc", "m"])), ERR_BAD_ARG);
    assert_eq!(err_code(&h.run(&["zadd", "z", "nan", "m"])), ERR_BAD_ARG);
    assert_eq!(err_code(&h.run(&["zadd", "z", "1x", "m"])), ERR_BAD_ARG);
    assert_eq!(
        err_code(&h.run(&["zquery", "z", "1", "", "x", "1"])),
        ERR_BAD_ARG
    );
    assert_eq!(err_code(&h.run(&["pexpire", "z", "soon"])), ERR_BAD_ARG);
    assert_eq!(h.run(&["zadd", "z", "+1.5", "m"]), num(1)); // leading '+' like strtod
}

#[test]
fn sorted_set_transcript() {
    let mut h = Harness::new();
    assert_eq!(h.run(&["zscore", "asdf", "n1"]), nil());
    assert_eq!(
        h.run(&["zquery", "xxx", "1", "asdf", "1", "10"]),
        arr(vec![])
    );
    assert_eq!(h.run(&["zadd", "zset", "1", "n1"]), num(1));
    assert_eq!(h.run(&["zadd", "zset", "2", "n2"]), num(1));
    assert_eq!(h.run(&["zadd", "zset", "1.1", "n1"]), num(0));
    assert_eq!(h.run(&["zscore", "zset", "n1"]), dbl(1.1));
    assert_eq!(
        h.run(&["zquery", "zset", "1", "", "0", "10"]),
        arr(vec![s("n1"), dbl(1.1), s("n2"), dbl(2.0)])
    );
    assert_eq!(
        h.run(&["zquery", "zset", "1.1", "", "1", "10"]),
        arr(vec![s("n2"), dbl(2.0)])
    );
    assert_eq!(
        h.run(&["zquery", "zset", "1.1", "", "2", "10"]),
        arr(vec![])
    );
    assert_eq!(h.run(&["zrem", "zset", "adsf"]), num(0));
    assert_eq!(h.run(&["zrem", "zset", "n1"]), num(1));
    assert_eq!(
        h.run(&["zquery", "zset", "1", "", "0", "10"]),
        arr(vec![s("n2"), dbl(2.0)])
    );
}

#[test]
fn zquery_limit_counts_members() {
    let mut h = Harness::new();
    for i in 0..5 {
        h.run(&["zadd", "z", &i.to_string(), &format!("m{i}")]);
    }
    assert_eq!(
        h.run(&["zquery", "z", "0", "", "0", "2"]),
        arr(vec![s("m0"), dbl(0.0), s("m1"), dbl(1.0)])
    );
    assert_eq!(
        h.run(&["zquery", "z", "0", "", "0", "1"]),
        arr(vec![s("m0"), dbl(0.0)])
    );
    assert_eq!(h.run(&["zquery", "z", "0", "", "0", "0"]), arr(vec![]));
}

#[test]
fn zquery_negative_offset_past_the_end() {
    let mut h = Harness::new();
    for i in 0..5 {
        h.run(&["zadd", "z", &i.to_string(), &format!("m{i}")]);
    }
    // Nothing is >= 100, so offsets count back from the last member.
    assert_eq!(
        h.run(&["zquery", "z", "100", "", "-1", "10"]),
        arr(vec![s("m4"), dbl(4.0)])
    );
    assert_eq!(
        h.run(&["zquery", "z", "100", "", "-2", "10"]),
        arr(vec![s("m3"), dbl(3.0), s("m4"), dbl(4.0)])
    );
    assert_eq!(h.run(&["zquery", "z", "100", "", "0", "10"]), arr(vec![]));
    assert_eq!(h.run(&["zquery", "z", "100", "", "-9", "10"]), arr(vec![]));
}

#[test]
fn command_names_are_case_insensitive() {
    let mut h = Harness::new();
    assert_eq!(h.run(&["SET", "k", "v"]), nil());
    assert_eq!(h.run(&["Get", "k"]), s("v"));
    assert_eq!(h.run(&["get", "K"]), nil()); // keys stay case-sensitive
}

#[test]
fn expired_keys_are_invisible_before_background_expiry() {
    let mut h = Harness::new();
    h.run(&["set", "k", "v"]);
    h.run(&["zadd", "z", "1", "m"]);
    h.run(&["pexpire", "k", "100"]);
    h.run(&["pexpire", "z", "100"]);
    h.advance(100); // due, but process_expired() has not run
    assert_eq!(h.run(&["keys"]), arr(vec![]));
    assert_eq!(h.run(&["get", "k"]), nil());
    assert_eq!(h.run(&["pttl", "k"]), num(-2));
    assert_eq!(h.run(&["zscore", "z", "m"]), nil());
    assert_eq!(h.run(&["del", "z"]), num(0));
    assert_eq!(h.db.size(), 0);
    assert_eq!(h.db.next_expiry_ms(), None);
    assert_eq!(h.db.process_expired(2000), 0);
}

#[test]
fn set_discards_previous_ttl() {
    let mut h = Harness::new();
    h.run(&["set", "k", "v"]);
    h.run(&["pexpire", "k", "100"]);
    h.run(&["set", "k", "v2"]);
    assert_eq!(h.run(&["pttl", "k"]), num(-1));
    assert_eq!(h.db.next_expiry_ms(), None);
    h.advance(1000);
    assert_eq!(h.run(&["get", "k"]), s("v2"));
}

#[test]
fn removing_last_member_deletes_the_zset() {
    let mut h = Harness::new();
    h.run(&["zadd", "z", "1", "a"]);
    h.run(&["zadd", "z", "2", "b"]);
    assert_eq!(h.run(&["zrem", "z", "a"]), num(1));
    assert_eq!(h.db.size(), 1);
    assert_eq!(h.run(&["zrem", "z", "b"]), num(1));
    assert_eq!(h.db.size(), 0);
    assert_eq!(h.run(&["keys"]), arr(vec![]));
    // The name is free for any type again.
    assert_eq!(h.run(&["set", "z", "v"]), nil());
}

#[test]
fn oversized_keys_reply_is_cut_short() {
    let mut h = Harness::new();
    let big = "k".repeat(8 << 20);
    for i in 0..5 {
        h.run(&["set", &format!("{big}{i}"), "v"]); // 40 MiB of key names
    }
    assert_eq!(err_code(&h.run(&["keys"])), ERR_TOO_BIG);
}

#[test]
fn ttl_lifecycle() {
    let mut h = Harness::new();
    assert_eq!(h.run(&["pttl", "k"]), num(-2));
    assert_eq!(h.run(&["pexpire", "k", "100"]), num(0));
    h.run(&["set", "k", "v"]);
    assert_eq!(h.run(&["pttl", "k"]), num(-1));
    assert_eq!(h.run(&["pexpire", "k", "100"]), num(1));
    assert_eq!(h.run(&["pttl", "k"]), num(100));
    assert_eq!(h.db.next_expiry_ms(), Some(h.now() + 100));

    h.advance(99);
    assert_eq!(h.db.process_expired(2000), 0);
    assert_eq!(h.run(&["get", "k"]), s("v"));

    h.advance(1);
    assert_eq!(h.db.process_expired(2000), 1);
    assert_eq!(h.run(&["get", "k"]), nil());
    assert_eq!(h.db.next_expiry_ms(), None);
}

#[test]
fn removing_ttl_and_deleting_keys_updates_heap() {
    let mut h = Harness::new();
    h.run(&["set", "a", "1"]);
    h.run(&["set", "b", "2"]);
    h.run(&["pexpire", "a", "50"]);
    h.run(&["pexpire", "b", "10"]);
    assert_eq!(h.db.next_expiry_ms(), Some(h.now() + 10));
    h.run(&["pexpire", "b", "-1"]); // negative = persist
    assert_eq!(h.run(&["pttl", "b"]), num(-1));
    assert_eq!(h.db.next_expiry_ms(), Some(h.now() + 50));
    h.run(&["del", "a"]); // deleting must also drop its timer
    assert_eq!(h.db.next_expiry_ms(), None);
}

#[test]
fn expiry_work_is_bounded() {
    let mut h = Harness::new();
    for i in 0..100 {
        let key = format!("k{i}");
        h.run(&["set", &key, "v"]);
        h.run(&["pexpire", &key, "5"]);
    }
    h.advance(10);
    assert_eq!(h.db.process_expired(30), 30);
    assert_eq!(h.db.size(), 70);
    assert_eq!(h.db.process_expired(2000), 70);
    assert_eq!(h.db.size(), 0);
}

#[test]
fn large_zset_freed_on_thread_pool() {
    let pool = Arc::new(ThreadPool::new(2));
    let mut db = Database::with_pool(Some(Arc::clone(&pool)));
    for i in 0..(Database::K_LARGE_CONTAINER_SIZE + 500) {
        let mut cmd: Vec<Vec<u8>> = vec![
            b"zadd".to_vec(),
            b"big".to_vec(),
            i.to_string().into_bytes(),
            format!("m{i}").into_bytes(),
        ];
        let mut out = Buffer::new();
        db.execute(&mut cmd, &mut out);
    }
    let mut del: Vec<Vec<u8>> = vec![b"del".to_vec(), b"big".to_vec()];
    let mut out = Buffer::new();
    db.execute(&mut del, &mut out);
    assert_eq!(db.size(), 0);
    drop(db);
    drop(pool); // drains and joins the workers
}

#[test]
fn destructor_frees_everything() {
    let mut db = Database::new();
    for i in 0..1000 {
        let mut a: Vec<Vec<u8>> =
            vec![b"set".to_vec(), format!("s{i}").into_bytes(), b"v".to_vec()];
        let mut b: Vec<Vec<u8>> = vec![
            b"zadd".to_vec(),
            format!("z{}", i % 10).into_bytes(),
            b"1".to_vec(),
            format!("m{i}").into_bytes(),
        ];
        let mut c: Vec<Vec<u8>> = vec![
            b"pexpire".to_vec(),
            format!("s{i}").into_bytes(),
            b"100000".to_vec(),
        ];
        let mut out = Buffer::new();
        db.execute(&mut a, &mut out);
        db.execute(&mut b, &mut out);
        db.execute(&mut c, &mut out);
    }
    assert_eq!(db.size(), 1010);
}
