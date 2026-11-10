//! Panic isolation for host observers.
//!
//! Observer code is host code: the kernel must survive a panicking observer
//! without losing a wake, wedging the demand gate, or aborting the process.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use irrigation::{ComputeError, Runtime, RuntimeConfig, irrigation, view};
use irrigation_observe::{ObserveLevel, RuntimeEvent, RuntimeObserver};

/// How long one host operation may take before the test reports the wedge.
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// A panicking wake observer cannot strand the demand it woke
// ---------------------------------------------------------------------------

/// Set once the first demand is inside the root body.
static STARTED: AtomicBool = AtomicBool::new(false);
/// Set by the test once the second demand registered its wait.
static RELEASED: AtomicBool = AtomicBool::new(false);

#[view(source)]
pub struct SlowMode {
    pub value: u64,
}

#[irrigation]
mod slow_root {
    use super::{RELEASED, STARTED, SlowMode};

    fn block_until_released() {
        STARTED.store(true, std::sync::atomic::Ordering::SeqCst);
        while !RELEASED.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::yield_now();
        }
    }

    #[component]
    pub fn root(mode: &SlowMode) -> u64 {
        block_until_released();
        mode.value()
    }
}

struct PanicsOnTaskWoken;

impl RuntimeObserver for PanicsOnTaskWoken {
    fn level(&self) -> ObserveLevel {
        ObserveLevel::Full
    }

    fn record(&self, event: RuntimeEvent) {
        if matches!(event, RuntimeEvent::TaskWoken { .. }) {
            panic!("the observer refused the wake");
        }
    }
}

/// Releases the blocked root body even when an assertion panics.
struct Release;

impl Drop for Release {
    fn drop(&mut self) {
        RELEASED.store(true, Ordering::SeqCst);
    }
}

#[test]
fn a_panicking_wake_observer_does_not_strand_the_demand_it_woke() {
    STARTED.store(false, Ordering::SeqCst);
    RELEASED.store(false, Ordering::SeqCst);
    let observer: Arc<dyn RuntimeObserver> = Arc::new(PanicsOnTaskWoken);
    let runtime = Runtime::with_observer(
        RuntimeConfig {
            workers: 2,
            ..RuntimeConfig::default()
        },
        observer,
    );
    runtime.install(slow_root::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<SlowMode>().set_value(7))
        .expect("edit");
    let _release = Release;

    // The first demand blocks inside the body, so it owns the root node.
    let (owner_tx, owner_rx) = mpsc::channel();
    {
        let runtime = runtime.clone();
        let _owner = std::thread::spawn(move || {
            let _ = owner_tx.send(runtime.read_outputs::<slow_root::Root>());
        });
    }
    let deadline = Instant::now() + RECOVERY_TIMEOUT;
    while !STARTED.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "the first demand never entered the root body"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    // The second demand suspends on the node the first one owns.
    let (woken_tx, woken_rx) = mpsc::channel();
    {
        let runtime = runtime.clone();
        let _woken = std::thread::spawn(move || {
            let _ = woken_tx.send(runtime.read_outputs::<slow_root::Root>());
        });
    }
    while runtime.stats().suspensions == 0 {
        assert!(
            Instant::now() < deadline,
            "the second demand never waited on the first"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    // Completing the owner wakes the waiter; the observer panics on that wake.
    RELEASED.store(true, Ordering::SeqCst);
    let owner = owner_rx
        .recv_timeout(RECOVERY_TIMEOUT)
        .expect("the demand that owned the node hung");
    assert!(
        matches!(owner, Err(ComputeError::Panicked { .. })),
        "the host must see the observer panic: {owner:?}"
    );
    let woken = woken_rx
        .recv_timeout(RECOVERY_TIMEOUT)
        .expect("the panicking observer stranded the demand it woke");
    assert_eq!(
        woken.expect("the woken demand must still read the committed value"),
        7
    );
}

// ---------------------------------------------------------------------------
// A panicking observer cannot wedge the demand gate (r5-demand-panic-holds-gate)
// ---------------------------------------------------------------------------

mod panicking_retry_observer {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    use irrigation::{ComputeError, Runtime, RuntimeConfig, ViewSchema, irrigation, view};
    use irrigation_observe::{ObserveLevel, RuntimeEvent, RuntimeObserver};

    #[view(source)]
    pub struct Marker {
        pub value: u64,
    }

    #[view(derived)]
    pub struct Table(irrigation::Map<u32, u32>);

    #[irrigation]
    mod retrying {
        use super::Table;

        #[irrigation::component]
        pub fn reader() -> u32 {
            Table.get(&1).unwrap_or(0)
        }

        #[irrigation::component]
        pub fn writer() -> u32 {
            Table.define(1, 42);
            0
        }

        /// Reads the table before defining it, so the attempt's trace is stale
        /// at commit and its demand restarts once.
        #[irrigation::component]
        pub fn root() -> u32 {
            let first = reader();
            let _ = writer();
            first
        }
    }

    struct PanicsOnRetry;

    impl RuntimeObserver for PanicsOnRetry {
        fn level(&self) -> ObserveLevel {
            ObserveLevel::Full
        }

        fn record(&self, event: RuntimeEvent) {
            if let RuntimeEvent::Arbitration { kind, .. } = event {
                if kind == "demand_retry" {
                    panic!("the observer refused the retry arbitration");
                }
            }
        }
    }

    /// How long one host operation may take before the test reports the wedge.
    const RECOVERY_TIMEOUT: Duration = Duration::from_secs(5);

    fn read_root(runtime: &Runtime) -> Option<irrigation::Result<u32>> {
        let (sender, receiver) = mpsc::channel();
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            let _ = sender.send(runtime.read_outputs::<retrying::Root>());
        });
        receiver.recv_timeout(RECOVERY_TIMEOUT).ok()
    }

    fn edit_marker(runtime: &Runtime) -> bool {
        let (sender, receiver) = mpsc::channel();
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            let _ = sender.send(runtime.edit(|edit| edit.view::<Marker>().set_value(1)));
        });
        matches!(receiver.recv_timeout(RECOVERY_TIMEOUT), Ok(Ok(())))
    }

    #[test]
    fn a_panicking_observer_cannot_wedge_the_gate() {
        let observer: Arc<dyn RuntimeObserver> = Arc::new(PanicsOnRetry);
        let runtime = Runtime::with_observer(
            RuntimeConfig {
                workers: 2,
                ..RuntimeConfig::default()
            },
            observer,
        );
        runtime.register_view(<Table as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<Marker as ViewSchema>::DESCRIPTOR);
        runtime
            .install(retrying::Root)
            .expect("install the retrying root");

        // The first attempt reads the table before it is defined, so the demand
        // restarts; the observer panics when it is told about the retry.
        let host = read_root(&runtime);
        let running = runtime.stats().tasks.running;
        let edited = edit_marker(&runtime);

        assert!(
            matches!(host, Some(Err(ComputeError::Panicked { .. }))),
            "the host must see the panicked demand instead of parking: {host:?}; \
             {running} task(s) left running; a later edit succeeded: {edited}"
        );
        assert_eq!(running, 0, "the panicked attempt left its task running");
        assert!(edited, "the panicked attempt kept its gate read permit");
    }

    /// Fails every demand as soon as the runtime reports it started.
    struct PanicsOnDemandStart;

    impl RuntimeObserver for PanicsOnDemandStart {
        fn level(&self) -> ObserveLevel {
            ObserveLevel::Summary
        }

        fn record(&self, event: RuntimeEvent) {
            if matches!(event, RuntimeEvent::DemandStarted { .. }) {
                panic!("the observer refused the demand start");
            }
        }
    }

    #[test]
    fn a_panicking_observer_during_an_inline_demand_releases_the_gate() {
        let observer: Arc<dyn RuntimeObserver> = Arc::new(PanicsOnDemandStart);
        let runtime = Runtime::with_observer(
            RuntimeConfig {
                workers: 0,
                ..RuntimeConfig::default()
            },
            observer,
        );
        runtime.register_view(<Table as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<Marker as ViewSchema>::DESCRIPTOR);
        runtime
            .install(retrying::Root)
            .expect("install the retrying root");

        // An inline demand runs on the caller's thread, so its observer panic
        // surfaces there; the task it allocated must still be closed.
        let host = {
            let runtime = runtime.clone();
            std::thread::spawn(move || runtime.read_outputs::<retrying::Root>())
        }
        .join();
        assert!(
            host.is_err(),
            "the caller's thread must see the observer panic"
        );
        assert_eq!(
            runtime.stats().tasks.running,
            0,
            "the panicked inline demand left its task running"
        );
        assert!(
            edit_marker(&runtime),
            "the panicked inline demand kept its gate read permit"
        );
    }
}
