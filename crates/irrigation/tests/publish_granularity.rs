//! Does the commit count grow with the number of entries a body publishes?
//!
//! For a whole-map publish it does not: one entry and three entries cost the same number
//! of commits, because the commits are the demand's node set rather than one per entry.
//! This guards the granularity in the other direction -- per-entry commits for a whole-map
//! replacement would be wasted invalidation work.

// These tests observe the runtime through `irrigation-observe`, which is an optional dependency of
// the facade behind the `observatory` feature. Without that gate this file fails to compile under the
// default feature set, which is how `cargo test --workspace --locked` builds -- so the whole suite
// stopped compiling while a grep for "test result: FAILED" kept reporting zero failures.
#![cfg(feature = "observatory")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use irrigation::{Runtime, RuntimeConfig, view};

#[view(source)]
pub struct Mode {
    pub mode: u64,
}

#[view(derived)]
pub struct Totals(irrigation::Map<u32, u64>);

#[irrigation::irrigation]
mod totals {
    use super::{Mode, Totals};

    #[component(root)]
    pub fn root(mode: &Mode) -> Totals {
        let n = mode.mode();
        // One entry for n == 1, three for n == 3.
        Totals::from(
            [(0u32, 10u64), (1u32, 11u64), (2u32, 12u64)]
                .into_iter()
                .take(n as usize),
        )
    }
}

#[derive(Default)]
struct Counter {
    committed: AtomicU32,
}

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

fn commits_for(entries: u64) -> u32 {
    let counter = Arc::new(Counter::default());
    let runtime = Runtime::with_observer(
        RuntimeConfig::default(),
        Arc::clone(&counter) as Arc<dyn irrigation_observe::RuntimeObserver>,
    );
    runtime
        .edit(|edit| edit.view::<Mode>().set_mode(entries))
        .expect("edit");
    runtime.install(totals::Root).expect("install");
    let _ = runtime.read_outputs::<totals::Root>();
    counter.committed.load(Ordering::SeqCst)
}

/// Publishing a whole map costs the same however many entries it has: the commits are the
/// demand's node set, not one per entry. A per-entry commit here would make every
/// collection publish scale its invalidation bookkeeping with the data.
#[test]
fn a_whole_map_publish_costs_the_same_for_one_entry_and_three() {
    let one = commits_for(1);
    let three = commits_for(3);
    assert!(one > 0, "the demand commits its nodes");
    assert_eq!(
        one, three,
        "entry count must not change the commit count (one={one}, three={three})"
    );
}
