//! Does a commit ever land while the body that produces it is still running?
//!
//! The commit count alone cannot answer goal #1's question: a dozen commits could all
//! happen after the body returns, once its projections are built. This measures *when*
//! they land, by timestamping every `NodeCommitted` against the instant the body started,
//! and asserts at least one lands inside the body's own window.
//!
//! Note what this does and does not say: the framework interleaves commits with
//! processing, but the *body's own output* still cannot commit before it returns, because
//! the output is the return value. A body that wanted to publish partway through has no
//! verb for it.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use irrigation::{Runtime, RuntimeConfig, view};

/// How long the body spends before publishing, so a commit during it is unambiguous.
const BODY_NANOS: u64 = 200_000_000;

static BODY_START: Mutex<Option<Instant>> = Mutex::new(None);

/// Marks the body as started; a free function because a component body cannot assign
/// through a static.
fn mark_start() {
    *BODY_START.lock().expect("start") = Some(Instant::now());
}

#[view(source)]
pub struct Mode {
    pub mode: u64,
}

#[view(derived)]
pub struct Totals(Map<u32, u64>);

#[irrigation::irrigation]
mod totals {
    use super::{BODY_NANOS, Mode, Totals};

    /// Reads the mode, waits, then publishes three entries.
    #[component(root)]
    pub fn root(mode: &Mode) -> Totals {
        let m = mode.mode();
        super::mark_start();
        std::thread::sleep(std::time::Duration::from_nanos(BODY_NANOS));
        Totals::from([(0u32, m), (1u32, m + 1), (2u32, m + 2)])
    }
}

/// Records every commit's offset from the body's start.
#[derive(Default)]
struct Timeline {
    offsets: Mutex<Vec<u128>>,
}

impl irrigation_observe::RuntimeObserver for Timeline {
    fn level(&self) -> irrigation_observe::ObserveLevel {
        irrigation_observe::ObserveLevel::Nodes
    }

    fn record(&self, event: irrigation_observe::RuntimeEvent) {
        if !matches!(
            event,
            irrigation_observe::RuntimeEvent::NodeCommitted { .. }
        ) {
            return;
        }
        let start = *BODY_START.lock().expect("start");
        if let Some(start) = start {
            self.offsets
                .lock()
                .expect("offsets")
                .push(start.elapsed().as_nanos());
        }
    }
}

#[test]
fn commits_land_while_the_body_is_still_running() {
    let timeline = Arc::new(Timeline::default());
    let runtime = Runtime::with_observer(
        RuntimeConfig::default(),
        Arc::clone(&timeline) as Arc<dyn irrigation_observe::RuntimeObserver>,
    );
    runtime
        .edit(|edit| edit.view::<Mode>().set_mode(1))
        .expect("edit");
    runtime.install(totals::Root).expect("install");
    let _ = runtime.read_outputs::<totals::Root>();

    let offsets = timeline.offsets.lock().expect("offsets").clone();
    let during = offsets
        .iter()
        .filter(|offset| **offset < u128::from(BODY_NANOS))
        .count();
    assert!(
        !offsets.is_empty(),
        "the demand committed at least one node"
    );
    assert!(
        during > 0,
        "some commits land while the body is still running: publication is per node, \
         not batched at the end (commits={}, during_body={during})",
        offsets.len()
    );
}
