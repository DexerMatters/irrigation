//! Worker pool for demand execution.
//!
//! Derived computation runs on pool workers rather than on the caller's stack:
//! a worker has a large stack, so deep structural recursion does not overflow,
//! and several demands can run concurrently. A caller that is itself a worker
//! executes inline, which keeps nested demand from deadlocking on its own pool.

use parking_lot::{Condvar, Mutex};
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Runs one job under the pool's panic boundary.
///
/// A panicking job loses its own result and nothing else: the worker stays
/// alive for every later demand, and a thread that helped the job on its way to
/// a park gets that task's failure through the job's own channel rather than
/// the other task's panic unwinding its stack. Every path that executes a job
/// goes through here for that reason.
fn execute(job: Job) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
}

struct PoolInner {
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
    shutdown: AtomicBool,
}

impl PoolInner {
    fn push(&self, job: Job) {
        let mut queue = self.queue.lock();
        queue.push_back(job);
        drop(queue);
        self.wake.notify_one();
    }

    fn pop(&self) -> Option<Job> {
        let mut queue = self.queue.lock();
        loop {
            if let Some(job) = queue.pop_front() {
                return Some(job);
            }
            if self.shutdown.load(Ordering::Acquire) {
                return None;
            }
            self.wake.wait(&mut queue);
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
    ///
    /// # Panics
    ///
    /// Panics when a worker thread cannot be spawned -- no memory for its stack, or the
    /// process is out of threads. There is no degraded mode: a pool missing a worker
    /// would make progress depend on which threads happened to start.
    #[must_use]
    pub fn new(workers: usize, stack_size: usize) -> Self {
        // Large stacks are the deep-recursion mode. Reserve a small fixed headroom
        // for runtime frames and debug instrumentation so the configured depth
        // budget is not consumed entirely by the scheduler's own bookkeeping. The
        // addition is contractual rather than silent: `RuntimeConfig::stack_size`
        // says so, and no test here exercises a stack near the threshold, so
        // removing it would be an untested bet either way.
        const HEADROOM: usize = 16 * 1024 * 1024;
        let stack_size = if stack_size >= 64 * 1024 * 1024 {
            stack_size.saturating_add(HEADROOM)
        } else {
            stack_size
        };
        let inner = Arc::new(PoolInner {
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        let mut handles = Vec::new();
        for index in 0..workers {
            let worker_inner = Arc::clone(&inner);
            let handle = std::thread::Builder::new()
                .name(format!("irrigation-worker-{index}"))
                .stack_size(stack_size)
                .spawn(move || {
                    IS_WORKER.with(|flag| flag.set(true));
                    while let Some(job) = worker_inner.pop() {
                        execute(job);
                    }
                })
                .expect("spawn irrigation worker");
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
    /// queued demand is not stranded behind blocked workers. The job belongs to
    /// another demand, so its panic boundary is not the caller's: a failing job
    /// must not turn the helping demand's read into a foreign failure.
    pub fn try_run_one(&self) -> bool {
        let job = {
            let mut queue = self.inner.queue.lock();
            queue.pop_front()
        };
        match job {
            Some(job) => {
                execute(job);
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
    ///
    /// # Panics
    ///
    /// Inline work propagates the job's panic to the caller, because the caller is where the
    /// work ran. Pooled work re-raises it on this thread after joining the worker.
    pub fn run<T: Send + 'static>(&self, job: impl FnOnce() -> T + Send + 'static) -> T {
        if self.workers == 0 || on_worker() {
            // Inline work is the caller's own: it takes the value back and must
            // see its panic too, so it stays outside the pool's boundary.
            return job();
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.inner.push(Box::new(move || {
            let result = job();
            let _ = sender.send(result);
        }));
        match receiver.recv() {
            Ok(value) => value,
            Err(error) => panic!("irrigation worker dropped a demand result: {error}"),
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // Store the flag while holding the queue lock. A worker checks the flag
        // and then waits under that same lock, so this ordering closes the
        // lost-wakeup window that would otherwise leave a worker parked and
        // make `join` hang for ever.
        {
            let _queue = self.inner.queue.lock();
            self.inner.shutdown.store(true, Ordering::Release);
        }
        self.inner.wake.notify_all();
        let mut handles = self.handles.lock();
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

    #[test]
    fn a_helped_job_does_not_panic_the_helping_thread() {
        // No workers: the only thread that can run the job is the helper.
        let pool = Pool::new(0, 1024 * 1024);
        pool.submit(|| panic!("a job belonging to another demand"));
        assert!(pool.try_run_one(), "one queued job was helped");
        assert_eq!(pool.run(|| 6 * 7), 42, "the pool still runs jobs");
    }

    #[test]
    fn a_panicking_job_does_not_retire_the_worker() {
        let pool = Pool::new(1, 1024 * 1024);
        pool.submit(|| panic!("a job belonging to another demand"));
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        pool.submit(move || {
            let _ = sender.send(());
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the worker survived the panicking job");
    }
}
