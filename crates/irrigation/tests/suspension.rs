//! Gates for suspended demand: a waiting task yields its worker.
//!
//! These tests force the exact shape the plan describes: two workers, one
//! demand blocked inside a computation it owns, one demand waiting on that
//! computation, and third-party work queued behind the waiter. If the waiter
//! parks its worker the third demand cannot start, the blocked computation
//! never observes it, and the test fails. If the waiter suspends, its worker
//! takes the third demand immediately.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use irrigation::{Runtime, RuntimeConfig, TaskCounts, irrigation};

/// The probes below are process-wide, so the gates that use them take turns.
static SERIAL: Mutex<()> = Mutex::new(());

/// Set once the blocking computation started.
static SLOW_STARTED: AtomicBool = AtomicBool::new(false);
/// Set by the third-party demand the blocked computation is waiting for.
static FAST_DONE: AtomicBool = AtomicBool::new(false);
/// Set by the waiting demand just before it demands the blocking computation.
static READER_STARTED: AtomicBool = AtomicBool::new(false);
/// Number of mutually recursive demands that reached their rendezvous.
static PAIR_STARTED: AtomicUsize = AtomicUsize::new(0);
/// Releases the writer holding the claim the cancelled demand waits on.
static HELD_WRITER_RELEASE: OnceLock<Mutex<Option<mpsc::Receiver<()>>>> = OnceLock::new();

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

    /// Takes the claim on its node and holds it until the gate releases it.
    pub fn hold(_tag: u64) -> u64 {
        SLOW_STARTED.store(true, Ordering::Release);
        let receiver = super::HELD_WRITER_RELEASE
            .get()
            .and_then(|slot| slot.lock().ok().and_then(|mut slot| slot.take()));
        if let Some(receiver) = receiver {
            let _ = receiver.recv_timeout(Duration::from_secs(10));
        }
        1
    }

    /// Holds both mutually recursive demands inside their bodies until each
    /// has started, so both wait edges are recorded concurrently.
    pub fn rendezvous(_tag: u64) -> u64 {
        super::PAIR_STARTED.fetch_add(1, Ordering::AcqRel);
        let deadline = Instant::now() + Duration::from_secs(5);
        while super::PAIR_STARTED.load(Ordering::Acquire) < 2 {
            assert!(Instant::now() < deadline, "the pair never met");
            std::thread::sleep(Duration::from_millis(1));
        }
        0
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

#[irrigation]
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

    /// A two-component value cycle, one demand per component.
    #[component]
    pub fn left(tag: u64) -> u64 {
        let mark = super::probe::rendezvous(tag);
        let value = right(tag);
        value + mark
    }

    /// The other half of the cycle.
    #[component]
    pub fn right(tag: u64) -> u64 {
        let mark = super::probe::rendezvous(tag);
        let value = left(tag);
        value + mark
    }

    /// Takes a claim and holds it until the gate releases it.
    #[component]
    pub fn held_once(tag: u64) -> u64 {
        super::probe::hold(tag)
    }

    /// Touches the node `held_once` owns.
    #[component]
    pub fn held_reader(tag: u64) -> u64 {
        let mark = super::probe::mark_reader(tag);
        let value = held_once(tag);
        value + mark
    }
}

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn reset_probes() {
    SLOW_STARTED.store(false, Ordering::Release);
    FAST_DONE.store(false, Ordering::Release);
    READER_STARTED.store(false, Ordering::Release);
}

/// Waits, with a deadline, until the runtime reports the cancelled demand's
/// task suspended on the live claim.
fn wait_for_suspended_task(runtime: &Runtime) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let stats = runtime.stats();
        if stats.tasks.waiting >= 1 && runtime.explain().waiting_tasks >= 1 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the waiting demand never suspended: {stats:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
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
    let (release_tx, release_rx) = mpsc::channel();
    let _ = HELD_WRITER_RELEASE.set(Mutex::new(Some(release_rx)));
    std::thread::scope(|scope| {
        let slow = scope.spawn(|| {
            runtime
                .call::<yielding::__HeldOnceSpec>(yielding::__HeldOnceArgs { tag: 9 })
                .expect("held demand")
        });
        probe::wait_until(&SLOW_STARTED);

        let (reader_tx, reader_rx) = mpsc::channel();
        let reader_runtime = runtime.clone();
        let reader = scope.spawn(move || {
            let error = reader_runtime
                .call::<yielding::__HeldReaderSpec>(yielding::__HeldReaderArgs { tag: 9 })
                .expect_err("a cancelled demand cannot publish");
            let _ = reader_tx.send(error);
        });
        probe::wait_until(&READER_STARTED);
        wait_for_suspended_task(&runtime);

        runtime.cancel();
        let outcome = reader_rx.recv_timeout(Duration::from_secs(2));
        let writer_finished = slow.is_finished();
        // Release the writer in every outcome, so a failing gate does not hold
        // the scope open behind it.
        let _ = release_tx.send(());
        let error = outcome.expect("a cancelled demand must be woken, not resumed by the writer");
        assert!(error.is_cancelled(), "expected cancellation, got {error}");
        assert!(
            !writer_finished,
            "the reader resumed on the writer's commit instead of the cancellation"
        );
        assert_eq!(slow.join().expect("slow thread"), 1);
        reader.join().expect("reader thread");
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

#[test]
fn progress_accounting_shows_and_drains_in_flight_demands() {
    let _serial = serial();
    reset_probes();
    let runtime = two_workers();
    std::thread::scope(|scope| {
        let slow = scope.spawn(|| {
            runtime
                .call::<yielding::__SleepyOnceSpec>(yielding::__SleepyOnceArgs { tag: 11 })
                .expect("sleeping demand")
        });
        probe::wait_until(&SLOW_STARTED);
        let running = runtime.stats().tasks;
        assert!(
            running.running >= 1,
            "a demand blocked in its own computation must be counted as running, got {running:?}"
        );
        assert_eq!(slow.join().expect("slow thread"), 1);
    });
    assert_eq!(
        runtime.stats().tasks,
        TaskCounts::default(),
        "every demand released its task"
    );
}

#[test]
fn a_cycle_between_two_tasks_is_reported_not_hung() {
    let _serial = serial();
    PAIR_STARTED.store(0, Ordering::Release);
    let runtime = two_workers();
    std::thread::scope(|scope| {
        let left = scope.spawn(|| {
            runtime
                .call::<yielding::__LeftSpec>(yielding::__LeftArgs { tag: 13 })
                .expect_err("a cycle cannot produce a value")
        });
        let right = scope.spawn(|| {
            runtime
                .call::<yielding::__RightSpec>(yielding::__RightArgs { tag: 13 })
                .expect_err("a cycle cannot produce a value")
        });
        let first = left.join().expect("left thread");
        let second = right.join().expect("right thread");
        assert!(
            matches!(first, irrigation::ComputeError::Cycle { .. }),
            "left demand: {first}"
        );
        assert!(
            matches!(second, irrigation::ComputeError::Cycle { .. }),
            "right demand: {second}"
        );
    });
}

#[test]
fn a_workerless_runtime_runs_demands_inline() {
    let _serial = serial();
    reset_probes();
    FAST_DONE.store(true, Ordering::Release);
    let runtime = Runtime::with_config(RuntimeConfig {
        workers: 0,
        stack_size: 16 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    let value = runtime
        .call::<yielding::__FastOnceSpec>(yielding::__FastOnceArgs { tag: 21 })
        .expect("inline demand");
    assert_eq!(value, 2);
    assert_eq!(runtime.stats().tasks, TaskCounts::default());
}

// ---------------------------------------------------------------------------
// A parked async component is still cancellable (runtime-general-07)
// ---------------------------------------------------------------------------

/// Set once the parked component future was polled.
static PARKED_STARTED: AtomicBool = AtomicBool::new(false);

struct ParkedAsync;

impl irrigation::AsyncGeneralComponent for ParkedAsync {
    type State = ();
    type Output = u64;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00C0_0020);
    const NAME: &'static str = "tests::ParkedAsync";

    fn initialize<'a>(
        &'a self,
        _cx: &'a mut irrigation::GeneralCx,
    ) -> irrigation::ComponentFuture<'a, (Self::State, Self::Output)> {
        Box::pin(async move {
            PARKED_STARTED.store(true, Ordering::Release);
            // No executor and no wakeup: only cancellation can end this wait.
            std::future::pending::<()>().await;
            Ok(((), 0))
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a mut irrigation::GeneralCx,
        _state: &'a Self::State,
        _changes: &'a irrigation::ChangeBatch,
    ) -> irrigation::ComponentFuture<'a, (Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_cancelled_async_component_returns_instead_of_parking_forever() {
    let _serial = serial();
    PARKED_STARTED.store(false, Ordering::Release);
    let runtime = Runtime::with_config(RuntimeConfig {
        workers: 1,
        stack_size: 16 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    runtime
        .install_general_root(irrigation::AsyncAdapter::new(ParkedAsync))
        .expect("install");

    let (sender, receiver) = std::sync::mpsc::channel();
    {
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            let result = runtime.read_outputs_general::<irrigation::AsyncAdapter<ParkedAsync>>();
            let _ = sender.send(result.is_err());
        });
    }
    probe::wait_until(&PARKED_STARTED);
    runtime.cancel();

    let cancelled = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("a cancelled async component must not park the demand forever");
    assert!(cancelled, "a cancelled demand cannot publish");
}
