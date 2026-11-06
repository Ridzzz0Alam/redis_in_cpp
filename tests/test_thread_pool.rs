//! Port of `tests/test_thread_pool.cpp`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use redis_rust::thread_pool::ThreadPool;

#[test]
fn runs_all_queued_work_before_destruction() {
    let count = Arc::new(AtomicUsize::new(0));
    {
        let pool = ThreadPool::new(4);
        for _ in 0..10_000 {
            let count = Arc::clone(&count);
            pool.queue(move || {
                count.fetch_add(1, Ordering::Relaxed);
            });
        }
    } // destructor drains the queue
    assert_eq!(count.load(Ordering::Relaxed), 10_000);
}

#[test]
fn uses_multiple_threads() {
    let ids = Arc::new(Mutex::new(HashSet::new()));
    {
        let pool = ThreadPool::new(4);
        for _ in 0..64 {
            let ids = Arc::clone(&ids);
            pool.queue(move || {
                std::thread::sleep(Duration::from_millis(2));
                ids.lock().unwrap().insert(std::thread::current().id());
            });
        }
    }
    let ids = ids.lock().unwrap();
    assert!(ids.len() > 1);
    assert!(!ids.contains(&std::thread::current().id()));
}

#[test]
fn idle_pool_shuts_down_promptly() {
    let start = Instant::now();
    {
        let _pool = ThreadPool::new(8);
    }
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn panicking_job_does_not_kill_the_worker() {
    let count = Arc::new(AtomicUsize::new(0));
    {
        let pool = ThreadPool::new(1);
        pool.queue(|| panic!("boom (expected by this test)"));
        let count = Arc::clone(&count);
        pool.queue(move || {
            count.fetch_add(1, Ordering::Relaxed);
        });
    }
    assert_eq!(count.load(Ordering::Relaxed), 1);
}
