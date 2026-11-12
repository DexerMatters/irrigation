//! Unit tests for `src/scheduler.rs`.

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
