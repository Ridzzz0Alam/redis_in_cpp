//! Per-insert latency of the progressive-rehashing HMap vs std::collections::HashMap,
//! ported from `bench/bench_hashtable.cpp`.
//!
//! std HashMap rehashes every key at once when it grows (a latency spike that
//! grows with the table); HMap moves at most 128 keys per operation.

use std::collections::HashMap;
use std::process::ExitCode;
use std::time::Instant;

use redis_rust::hashtable::{HMap, HashNode};

struct Item {
    key: Vec<u8>,
    hcode: u64,
    hnext: Option<usize>,
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

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

struct Stats {
    total_ms: f64,
    p50_us: f64,
    p99_us: f64,
    p999_us: f64,
    max_us: f64,
}

fn summarize(mut ns: Vec<u32>, total: std::time::Duration) -> Stats {
    ns.sort_unstable();
    let pct = |p: f64| -> f64 {
        let idx = (p * (ns.len() - 1) as f64) as usize;
        f64::from(ns[idx]) / 1000.0
    };
    Stats {
        total_ms: total.as_secs_f64() * 1000.0,
        p50_us: pct(0.50),
        p99_us: pct(0.99),
        p999_us: pct(0.999),
        max_us: f64::from(*ns.last().unwrap()) / 1000.0,
    }
}

fn print_row(name: &str, s: &Stats) {
    println!(
        "| {:<26} | {:>10.1} | {:>8.2} | {:>8.2} | {:>9.2} | {:>11.1} |",
        name, s.total_ms, s.p50_us, s.p99_us, s.p999_us, s.max_us
    );
}

fn main() -> ExitCode {
    let n: usize = match std::env::args().nth(1) {
        None => 5_000_000,
        Some(arg) => match arg.parse() {
            Ok(n) if n > 0 => n,
            _ => {
                eprintln!("usage: bench_hashtable [N > 0]");
                return ExitCode::FAILURE;
            }
        },
    };

    let keys: Vec<Vec<u8>> = (0..n)
        .map(|i| mix(i as u64).to_le_bytes().to_vec())
        .collect();

    let mut hmap: HMap<Item> = HMap::new();
    let mut ns = vec![0u32; n];
    let start = Instant::now();
    for (i, slot) in ns.iter_mut().enumerate() {
        let key = keys[i].clone();
        let t0 = Instant::now();
        hmap.insert(Item {
            key,
            hcode: 0,
            hnext: None,
        });
        *slot = t0.elapsed().as_nanos().min(u128::from(u32::MAX)) as u32;
    }
    let hm = summarize(ns, start.elapsed());

    let mut umap: HashMap<Vec<u8>, u64> = HashMap::new();
    let mut ns = vec![0u32; n];
    let start = Instant::now();
    for (i, slot) in ns.iter_mut().enumerate() {
        let t0 = Instant::now();
        umap.insert(keys[i].clone(), i as u64);
        *slot = t0.elapsed().as_nanos().min(u128::from(u32::MAX)) as u32;
    }
    let um = summarize(ns, start.elapsed());

    println!("Inserting {} keys (per-insert latency)\n", n);
    println!(
        "| {:<26} | {:>10} | {:>8} | {:>8} | {:>9} | {:>11} |",
        "container", "total ms", "p50 us", "p99 us", "p99.9 us", "max us"
    );
    println!(
        "|{:-<28}|{:->12}|{:->10}|{:->10}|{:->11}|{:->13}|",
        "", "", "", "", "", ""
    );
    print_row("HMap (progressive rehash)", &hm);
    print_row("std::collections::HashMap", &um);
    ExitCode::SUCCESS
}
