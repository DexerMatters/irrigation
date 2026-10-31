//! Worker pool for demand execution.
//!
//! Derived computation runs on pool workers rather than on the caller's stack:
//! a worker has a large stack, so deep structural recursion does not overflow,
//! and several demands can run concurrently. A caller that is itself a worker
//! executes inline, which keeps nested demand from deadlocking on its own pool.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

type Job = Box<dyn FnOnce() + Send + 'static>;

thread_local! {
    static IS_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Returns `true` when the current thread is one of this runtime's workers.
#[must_use]
pub fn on_worker() -> bool {
    IS_WORKER.with(Cell::get)
}

struct PoolInner {
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
    shutdown: AtomicBool,
}

impl PoolInner {
    fn push(&self, job: Job) {
        let mut queue = self.queue.lock().unwrap_or_else(|poison| poison.into_inner());
        queue.push_back(job);
        drop(queue);
        self.wake.notify_one();
    }

    fn pop(&self) -> Option<Job> {
        let mut queue = self.queue.lock().unwrap_or_else(|poison| poison.into_inner());
        loop {
            if let Some(job) = queue.pop_front() {
                return Some(job);
            }
            if self.shutdown.load(Ordering::Acquire) {
                return None;
            }
            queue = self
                .wake
                .wait(queue)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }
}

/// A fixed-size worker pool.
pub struct Pool {
    inner: Arc<PoolInner>,
    handles: Mutex<Vec<JoinHandle<()>>>,
    workers: usize,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool")
            .field("workers", &self.workers)
            .finish_non_exhaustive()
    }
}

impl Pool {
    /// Starts `workers` threads, each with `stack_size` bytes of stack.
    #[must_use]
    pub fn new(workers: usize, stack_size: usize) -> Self {
        let inner = Arc::new(PoolInner {
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        let mut handles = Vec::new();
        for index in 0..workers {
            let worker_inner = Arc::clone(&inner);
            let handle = std::thread::Builder::new()
                .name(format!("reactive-worker-{index}"))
                .stack_size(stack_size)
                .spawn(move || {
                    IS_WORKER.with(|flag| flag.set(true));
                    while let Some(job) = worker_inner.pop() {
                        // A panicking job loses its own result; it must not lose
                        // the worker for every later demand.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                    }
                })
                .expect("spawn reactive worker");
            handles.push(handle);
        }
        Self {
            inner,
            handles: Mutex::new(handles),
            workers,
        }
    }

    /// Number of worker threads.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Queues `job` for a worker without waiting for it.
    pub fn submit(&self, job: impl FnOnce() + Send + 'static) {
        self.inner.push(Box::new(job));
    }

    /// Runs one queued job on the calling thread, if any is waiting.
    ///
    /// A worker that is about to park on a dependency calls this first, so
    /// queued demand is not stranded behind blocked workers.
    pub fn try_run_one(&self) -> bool {
        let job = {
            let mut queue = self
                .inner
                .queue
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            queue.pop_front()
        };
        match job {
            Some(job) => {
                job();
                true
            }
            None => false,
        }
    }

    /// Runs `job` on a worker and waits for its result.
    ///
    /// If the caller is already a worker, or no workers exist, the job runs
    /// inline. That rule is what prevents a nested demand from waiting on a
    /// pool that is entirely busy with its own callers.
    pub fn run<T: Send + 'static>(&self, job: impl FnOnce() -> T + Send + 'static) -> T {
        if self.workers == 0 || on_worker() {
            return job();
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.inner.push(Box::new(move || {
            let result = job();
            let _ = sender.send(result);
        }));
        match receiver.recv() {
            Ok(value) => value,
            Err(error) => panic!("reactive worker dropped a demand result: {error}"),
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::Release);
        self.inner.wake.notify_all();
        let mut handles = self.handles.lock().unwrap_or_else(|poison| poison.into_inner());
        for handle in handles.drain(..) {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_jobs_and_returns_results() {
        let pool = Pool::new(2, 1024 * 1024);
        let value = pool.run(|| 6 * 7);
        assert_eq!(value, 42);
        assert_eq!(pool.workers(), 2);
    }

    #[test]
    fn nested_runs_do_not_deadlock() {
        let pool = Arc::new(Pool::new(1, 1024 * 1024));
        let clone = Arc::clone(&pool);
        let value = pool.run(move || {
            // Already on a worker: this must execute inline.
            clone.run(|| 1) + 1
        });
        assert_eq!(value, 2);
    }
}
