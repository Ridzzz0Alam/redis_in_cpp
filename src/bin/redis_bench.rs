//! A small redis-benchmark-style load generator, ported from
//! `bench/redis_bench.cpp`.
//!
//! Each client thread sends `pipeline` requests per round trip and records the
//! round-trip latency. Throughput = total requests / wall time.

use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use redis_rust::net;
use redis_rust::protocol::encode_request;
use redis_rust::Buffer;

#[derive(Clone)]
struct Options {
    port: u16,
    clients: usize,
    requests: usize,
    pipeline: usize,
    cmd: String,
    value_size: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            port: 1234,
            clients: 50,
            requests: 200_000,
            pipeline: 1,
            cmd: "set".to_string(),
            value_size: 16,
        }
    }
}

fn parse(args: &[String], o: &mut Options) -> bool {
    if !args.len().is_multiple_of(2) {
        return false;
    }
    let mut i = 0;
    while i + 1 < args.len() {
        let value = args[i + 1].as_str();
        let ok = match args[i].as_str() {
            "--port" => value.parse().map(|v| o.port = v).is_ok(),
            "--clients" => value
                .parse()
                .map(|v| {
                    o.clients = v;
                    v > 0
                })
                .unwrap_or(false),
            "--requests" => value
                .parse()
                .map(|v| {
                    o.requests = v;
                    v > 0
                })
                .unwrap_or(false),
            "--pipeline" => value
                .parse()
                .map(|v| {
                    o.pipeline = v;
                    v > 0
                })
                .unwrap_or(false),
            "--cmd" => {
                o.cmd = value.to_string();
                o.cmd == "set" || o.cmd == "get" || o.cmd == "zadd"
            }
            "--value-size" => value.parse().map(|v| o.value_size = v).is_ok(),
            _ => false,
        };
        if !ok {
            return false;
        }
        i += 2;
    }
    true
}

fn make_cmd(o: &Options, client: usize, i: usize, value: &[u8]) -> Vec<Vec<u8>> {
    let key = format!("key:{}", (client * 7919 + i) % 100_000);
    if o.cmd == "get" {
        return vec![b"get".to_vec(), key.into_bytes()];
    }
    if o.cmd == "zadd" {
        return vec![
            b"zadd".to_vec(),
            b"bench_zset".to_vec(),
            (i % 1000).to_string().into_bytes(),
            key.into_bytes(),
        ];
    }
    vec![b"set".to_vec(), key.into_bytes(), value.to_vec()]
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut o = Options::default();
    if !parse(&args, &mut o) {
        eprintln!(
            "usage: redis_bench [--port N] [--clients N] [--requests N] [--pipeline N] \
             [--cmd set|get|zadd] [--value-size N]"
        );
        return ExitCode::FAILURE;
    }

    if o.requests < o.clients {
        eprintln!("--requests must be at least --clients");
        return ExitCode::FAILURE;
    }
    let value: Vec<u8> = vec![b'v'; o.value_size];

    let errors = Arc::new(AtomicUsize::new(0));
    let connect_errors = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let start = Instant::now();
    let mut handles = Vec::new();

    for client in 0..o.clients {
        let o = o.clone();
        let value = value.clone();
        let errors = Arc::clone(&errors);
        let connect_errors = Arc::clone(&connect_errors);
        let completed = Arc::clone(&completed);
        // Spread the remainder so exactly `requests` are sent in total.
        let mine = o.requests / o.clients + usize::from(client < o.requests % o.clients);
        handles.push(std::thread::spawn(move || -> Vec<u32> {
            let mut latencies = Vec::with_capacity(mine.div_ceil(o.pipeline));
            let mut stream = match net::connect_tcp("127.0.0.1", o.port) {
                Ok(stream) => stream,
                Err(_) => {
                    connect_errors.fetch_add(1, Ordering::Relaxed);
                    return latencies;
                }
            };

            let mut batch: Buffer = Buffer::new();
            let mut sent = 0;
            while sent < mine {
                let this_round = o.pipeline.min(mine - sent);
                batch.clear();
                for k in 0..this_round {
                    let req = encode_request(&make_cmd(&o, client, sent + k, &value));
                    batch.extend_from_slice(&req);
                }
                let t0 = Instant::now();
                if !net::write_all(&mut stream, &batch) {
                    errors.fetch_add(1, Ordering::Relaxed);
                    break;
                }
                let mut failed = false;
                for _ in 0..this_round {
                    if net::read_response(&mut stream).is_err() {
                        errors.fetch_add(1, Ordering::Relaxed);
                        failed = true;
                        break;
                    }
                }
                if failed {
                    break;
                }
                latencies.push(t0.elapsed().as_micros() as u32);
                sent += this_round;
                completed.fetch_add(this_round, Ordering::Relaxed);
            }
            latencies
        }));
    }

    let mut all = Vec::new();
    for handle in handles {
        if let Ok(latencies) = handle.join() {
            all.extend(latencies);
        }
    }
    let secs = start.elapsed().as_secs_f64();

    let connect_count = connect_errors.load(Ordering::Relaxed);
    if connect_count > 0 {
        eprintln!(
            "benchmark failed: {connect_count} clients could not connect - is the server running?"
        );
        return ExitCode::FAILURE;
    }
    let error_count = errors.load(Ordering::Relaxed);
    if error_count > 0 {
        eprintln!("benchmark failed: {error_count} clients hit a read/write error");
        return ExitCode::FAILURE;
    }

    all.sort_unstable();
    let pct = |p: f64| all[((p * (all.len() - 1) as f64) as usize).min(all.len() - 1)];
    let total = completed.load(Ordering::Relaxed);
    println!(
        "{:<5} clients={:<4} pipeline={:<3} requests={:<8} | {:>10.0} ops/s | \
         round-trip p50 {} us, p99 {} us, max {} us",
        o.cmd,
        o.clients,
        o.pipeline,
        total,
        total as f64 / secs,
        pct(0.50),
        pct(0.99),
        all.last().copied().unwrap_or(0)
    );
    ExitCode::SUCCESS
}
