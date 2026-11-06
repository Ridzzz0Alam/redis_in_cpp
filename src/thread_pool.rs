//! Producer/consumer thread pool, ported from `thread_pool.h` / `thread_pool.cpp`.
//!
//! Same design as the C++ version: a mutex-protected queue, a condition
//! variable, and N workers that unlock before running a job. The destructor
//! drains queued work and joins every worker.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

type Job = Box<dyn FnOnce() + Send + 'static>;

struct State {
    jobs: VecDeque<Job>,
    stopping: bool,
}

struct Inner {
    state: Mutex<State>,
    not_empty: Condvar,
}

pub struct ThreadPool {
    inner: Arc<Inner>,
    threads: Vec<JoinHandle<()>>,
}

impl ThreadPool {
    pub fn new(num_threads: usize) -> Self {
        assert!(num_threads > 0);
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                jobs: VecDeque::new(),
                stopping: false,
            }),
            not_empty: Condvar::new(),
        });
        let mut threads = Vec::with_capacity(num_threads);
        for _ in 0..num_threads {
            let inner = Arc::clone(&inner);
            threads.push(thread::spawn(move || worker(inner)));
        }
        Self { inner, threads }
    }

    /// Queue a job. The pool takes ownership of the closure.
    pub fn queue(&self, work: impl FnOnce() + Send + 'static) {
        {
            let mut state = self.inner.state.lock().expect("thread pool mutex");
            state.jobs.push_back(Box::new(work));
        }
        self.inner.not_empty.notify_one();
    }

    /// Number of queued jobs (mostly useful in tests).
    pub fn queued(&self) -> usize {
        self.inner
            .state
            .lock()
            .expect("thread pool mutex")
            .jobs
            .len()
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        {
            let mut state = self.inner.state.lock().expect("thread pool mutex");
            state.stopping = true;
        }
        self.inner.not_empty.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn worker(inner: Arc<Inner>) {
    loop {
        let job = {
            let mut state = inner.state.lock().expect("thread pool mutex");
            loop {
                if let Some(job) = state.jobs.pop_front() {
                    break job;
                }
                if state.stopping {
                    return;
                }
                state = inner.not_empty.wait(state).expect("thread pool condvar");
            }
        };
        job(); // run without holding the lock
    }
}
