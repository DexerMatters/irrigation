//! Gates for suspended demand: a waiting task yields its worker.
//!
//! These tests force the exact shape the plan describes: two workers, one
//! demand blocked inside a computation it owns, one demand waiting on that
//! computation, and third-party work queued behind the waiter. If the waiter
//! parks its worker the third demand cannot start, the blocked computation
//! never observes it, and the test fails. If the waiter suspends, its worker
//! takes the third demand immediately.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use reactive::{Runtime, RuntimeConfig, reactive};

/// The probes below are process-wide, so the gates that use them take turns.
static SERIAL: Mutex<()> = Mutex::new(());

/// Set once the blocking computation started.
static SLOW_STARTED: AtomicBool = AtomicBool::new(false);
/// Set by the third-party demand the blocked computation is waiting for.
static FAST_DONE: AtomicBool = AtomicBool::new(false);
/// Set by the waiting demand just before it demands the blocking computation.
static READER_STARTED: AtomicBool = AtomicBool::new(false);

mod probe {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::{FAST_DONE, READER_STARTED, SLOW_STARTED};

    /// Blocks a worker until [`fast`] runs, or until `budget` elapses.
    pub fn slow(_tag: u64, budget: Duration) -> u64 {
        SLOW_STARTED.store(true, Ordering::Release);
        let deadline = Instant::now() + budget;
        while !FAST_DONE.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return 0;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        1
    }

    /// Runs on the worker the waiter yields, and releases [`slow`].
    pub fn fast(_tag: u64) -> u64 {
        FAST_DONE.store(true, Ordering::Release);
        2
    }

    /// Runs before the waiting demand blocks.
    pub fn mark_reader(_tag: u64) -> u64 {
        READER_STARTED.store(true, Ordering::Release);
        0
    }

    /// Perturbs the schedule so two demands on one node overlap differently.
    pub fn jitter(_tag: u64) {
        std::thread::sleep(Duration::from_millis(1));
    }

    /// Blocks a worker for a fixed time, with nothing to release it.
    pub fn sleepy(_tag: u64, span: Duration) -> u64 {
        SLOW_STARTED.store(true, Ordering::Release);
        std::thread::sleep(span);
        1
    }

    /// Waits for a probe flag, failing rather than hanging the suite.
    pub fn wait_until(flag: &AtomicBool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !flag.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "a probe was never reached; the demand did not start"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[reactive]
mod yielding {
    use std::time::Duration;

    /// Blocks for up to two seconds unless `fast_once` runs in between.
    #[component]
    pub fn slow_once(tag: u64) -> u64 {
        super::probe::slow(tag, Duration::from_secs(2))
    }

    /// The unrelated demand that must run while `slow_once` is blocked.
    #[component]
    pub fn fast_once(tag: u64) -> u64 {
        super::probe::fast(tag)
    }

    /// Touches a node owned by `slow_once`, so this demand has to wait.
    #[component]
    pub fn reader(tag: u64) -> u64 {
        let mark = super::probe::mark_reader(tag);
        let value = slow_once(tag);
        value + mark
    }

    /// Sleeps for half a second, giving its waiter time to be cancelled.
    #[component]
    pub fn sleepy_once(tag: u64) -> u64 {
        super::probe::sleepy(tag, Duration::from_millis(500))
    }

    /// A shared invocation with a slightly variable schedule.
    #[component]
    pub fn perturbed(tag: u64) -> u64 {
        super::probe::jitter(tag);
        tag * 3 + 1
    }

    /// Touches a node owned by `sleepy_once`.
    #[component]
    pub fn sleepy_reader(tag: u64) -> u64 {
        let mark = super::probe::mark_reader(tag);
        let value = sleepy_once(tag);
        value + mark
    }
}

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn reset_probes() {
    SLOW_STARTED.store(false, Ordering::Release);
    FAST_DONE.store(false, Ordering::Release);
    READER_STARTED.store(false, Ordering::Release);
}

fn two_workers() -> Runtime {
    Runtime::with_config(RuntimeConfig {
        workers: 2,
        stack_size: 16 * 1024 * 1024,
        // The blocking implementation re-checks a node only after this slice,
        // so a long slice makes a parked worker starve the queued demand.
        wait_slice: Duration::from_secs(10),
        ..RuntimeConfig::default()
    })
}

#[test]
fn a_waiting_demand_yields_its_worker_to_queued_work() {
    let _serial = serial();
    reset_probes();
    let runtime = two_workers();
    std::thread::scope(|scope| {
        let slow = scope.spawn(|| {
            runtime
                .call::<yielding::__SlowOnceSpec>(yielding::__SlowOnceArgs { tag: 7 })
                .expect("slow demand")
        });
        probe::wait_until(&SLOW_STARTED);

        let reader = scope.spawn(|| {
            runtime
                .call::<yielding::__ReaderSpec>(yielding::__ReaderArgs { tag: 7 })
                .expect("waiting demand")
        });
        probe::wait_until(&READER_STARTED);
        // Let the waiting demand reach the node owned by the slow demand.
        std::thread::sleep(Duration::from_millis(200));

        let fast = scope.spawn(|| {
            runtime
                .call::<yielding::__FastOnceSpec>(yielding::__FastOnceArgs { tag: 7 })
                .expect("third demand")
        });

        assert_eq!(
            slow.join().expect("slow thread"),
            1,
            "the blocked demand never observed the yielded worker"
        );
        assert_eq!(reader.join().expect("reader thread"), 1);
        assert_eq!(fast.join().expect("fast thread"), 2);
    });
}

#[test]
fn a_suspended_demand_can_be_cancelled() {
    let _serial = serial();
    reset_probes();
    let runtime = two_workers();
    std::thread::scope(|scope| {
        let slow = scope.spawn(|| {
            runtime
                .call::<yielding::__SleepyOnceSpec>(yielding::__SleepyOnceArgs { tag: 9 })
                .expect("sleeping demand")
        });
        probe::wait_until(&SLOW_STARTED);

        let reader = scope.spawn(|| {
            runtime
                .call::<yielding::__SleepyReaderSpec>(yielding::__SleepyReaderArgs { tag: 9 })
                .expect_err("a cancelled demand cannot publish")
        });
        probe::wait_until(&READER_STARTED);
        // Let the waiting demand reach the node owned by the sleeping demand.
        std::thread::sleep(Duration::from_millis(200));

        runtime.cancel();
        let error = reader.join().expect("reader thread");
        assert!(error.is_cancelled(), "expected cancellation, got {error}");
        assert_eq!(slow.join().expect("slow thread"), 1);
    });
}

#[test]
fn concurrent_demands_share_an_invocation_across_suspensions() {
    let _serial = serial();
    reset_probes();
    let runtime = Runtime::with_config(RuntimeConfig {
        workers: 4,
        stack_size: 16 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let runtime = &runtime;
            scope.spawn(move || {
                for round in 0..20u64 {
                    for tag in 0..6u64 {
                        let value = runtime
                            .call::<yielding::__PerturbedSpec>(yielding::__PerturbedArgs { tag })
                            .expect("shared demand");
                        assert_eq!(value, tag * 3 + 1, "tag {tag}, round {round}");
                    }
                }
            });
        }
    });
}
