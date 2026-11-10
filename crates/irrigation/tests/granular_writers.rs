//! Two components writing different fields of one derived View.
//!
//! Goal #2 asks for granular concurrency: components touching the same group of Views must
//! cooperate at data granularity rather than being ordered as components. The existing
//! claim tests all prove the *blocking* half -- a reader waits for a writer. This proves the
//! other half: two writers of different fields proceed at the same time, and both of their
//! writes land without clobbering each other.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use irrigation::{FieldId, Runtime, RuntimeConfig, UnitKey, UnitKind, ViewSchema, view};

const BODY_NANOS: u64 = 150_000_000;

static IN_FLIGHT: AtomicU32 = AtomicU32::new(0);
static MAX_IN_FLIGHT: AtomicU32 = AtomicU32::new(0);

fn slow() {
    let now = IN_FLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
    MAX_IN_FLIGHT.fetch_max(now, Ordering::SeqCst);
    std::thread::sleep(Duration::from_nanos(BODY_NANOS));
    IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
}

#[view(derived)]
pub struct Pair {
    pub left: u64,
    pub right: u64,
}

fn field(index: u16) -> UnitKey {
    UnitKey::field(
        <Pair as ViewSchema>::view_id(),
        FieldId(index),
        UnitKind::Value,
    )
}

#[irrigation::irrigation]
mod writers {
    #[component]
    pub fn left(tag: u32) -> u32 {
        irrigation::ensure_view(<super::Pair as irrigation::ViewSchema>::DESCRIPTOR);
        super::slow();
        irrigation::define_unit(super::field(0), u64::from(tag), "left writer");
        0
    }

    #[component]
    pub fn right(tag: u32) -> u32 {
        irrigation::ensure_view(<super::Pair as irrigation::ViewSchema>::DESCRIPTOR);
        super::slow();
        irrigation::define_unit(super::field(1), u64::from(tag), "right writer");
        0
    }
}

#[test]
fn two_components_write_different_fields_of_one_derived_view() {
    let runtime = Runtime::with_config(RuntimeConfig {
        workers: 2,
        ..RuntimeConfig::default()
    });
    runtime.register_view(<Pair as ViewSchema>::DESCRIPTOR);

    let started = Instant::now();
    let left = {
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            runtime
                .call::<writers::__LeftSpec>(writers::__LeftArgs { tag: 1 })
                .map_err(|error| format!("{error:?}"))
        })
    };
    let right = {
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            runtime
                .call::<writers::__RightSpec>(writers::__RightArgs { tag: 2 })
                .map_err(|error| format!("{error:?}"))
        })
    };
    let results = (
        left.join().unwrap_or_else(|_| Err("panic".into())),
        right.join().unwrap_or_else(|_| Err("panic".into())),
    );
    let elapsed = started.elapsed();
    assert!(
        results.0.is_ok(),
        "the left writer succeeded: {:?}",
        results.0
    );
    assert!(
        results.1.is_ok(),
        "the right writer succeeded: {:?}",
        results.1
    );
    assert_eq!(
        MAX_IN_FLIGHT.load(Ordering::SeqCst),
        2,
        "both bodies were inside their slow section at once, in {} ms",
        elapsed.as_millis()
    );

    // Both writes landed, and each field holds only its own writer's value: two
    // components writing different fields of one View do not clobber each other.
    let snapshot = runtime.snapshot();
    let pair = snapshot.view::<Pair>();
    assert_eq!(pair.left(), 1, "the left writer's value");
    assert_eq!(pair.right(), 2, "the right writer's value");
}
