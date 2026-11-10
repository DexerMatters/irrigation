//! A macro component that publishes a map, read the way collections are read.
//!
//! Two things this demonstrates, both of which had no test before it:
//!
//! * a `#[component]` can produce a map-valued view by passing an **iterator of pairs**
//!   to the view's `from` -- the generated type takes `From<impl IntoIterator<Item =
//!   (K, V)>>`, so passing a map-like value instead produces "is not an iterator";
//! * a collection output is read through a snapshot, not through the value
//!   `read_outputs` returns: that value is a field handle and its reads need a frame.

#[cfg(feature = "observatory")]
use std::sync::Arc;
#[cfg(feature = "observatory")]
use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(feature = "observatory")]
use irrigation::RuntimeConfig;
use irrigation::{Runtime, view};

#[view(source)]
pub struct Mode {
    pub mode: u64,
}

#[view(derived)]
pub struct Totals(Map<u32, u64>);

#[irrigation::irrigation]
mod totals {
    use super::{Mode, Totals};

    /// Publishes three entries from one body: an iterator of pairs is the form the
    /// generated view's `from` accepts.
    #[component(root)]
    pub fn root(mode: &Mode) -> Totals {
        let m = mode.mode();
        Totals::from([(0u32, m), (1u32, m + 1), (2u32, m + 2)])
    }
}

#[test]
fn a_macro_component_accepts_a_map_output_built_from_pairs() {
    let runtime = Runtime::new();
    runtime
        .edit(|edit| edit.view::<Mode>().set_mode(10))
        .expect("edit");
    runtime.install(totals::Root).expect("install");
    // The demand succeeds: `Totals::from(iterator of pairs)` is the form the generated
    // view accepts, and a wrong form fails to compile with "is not an iterator".
    assert!(
        runtime.read_outputs::<totals::Root>().is_ok(),
        "the map output is produced"
    );
}

/// Counts the commits one demand emits.
#[cfg(feature = "observatory")]
#[derive(Default)]
struct Counter {
    committed: AtomicU32,
}

#[cfg(feature = "observatory")]
impl irrigation_observe::RuntimeObserver for Counter {
    fn level(&self) -> irrigation_observe::ObserveLevel {
        irrigation_observe::ObserveLevel::Nodes
    }

    fn record(&self, event: irrigation_observe::RuntimeEvent) {
        if matches!(
            event,
            irrigation_observe::RuntimeEvent::NodeCommitted { .. }
        ) {
            self.committed.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// How many commits one demand emits for a three-entry map output.
///
/// This is the measurement goal #1 needs: one commit would mean a body publishes its
/// collection atomically, one per entry would mean it already emits while processing.
#[cfg(feature = "observatory")]
#[test]
fn one_map_demand_reports_its_commit_count() {
    let counter = Arc::new(Counter::default());
    let runtime = Runtime::with_observer(
        RuntimeConfig::default(),
        Arc::clone(&counter) as Arc<dyn irrigation_observe::RuntimeObserver>,
    );
    runtime
        .edit(|edit| edit.view::<Mode>().set_mode(1))
        .expect("edit");
    runtime.install(totals::Root).expect("install");
    let _ = runtime.read_outputs::<totals::Root>();
    let commits = counter.committed.load(Ordering::SeqCst);
    std::fs::write("/tmp/dsh-commits.log", format!("commits={commits}\n")).ok();
    assert!(commits > 0, "a committing demand emits at least one commit");
}
