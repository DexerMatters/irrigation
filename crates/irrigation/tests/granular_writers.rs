//! Two components writing different fields of one derived View.
//!
//! Goal #2 asks for granular concurrency: components touching the same group of Views must
//! cooperate at data granularity rather than being ordered as components. The existing
//! claim tests all prove the *blocking* half -- a reader waits for a writer. This proves the
//! other half: two writers of different fields proceed at the same time, and both of their
//! writes land without clobbering each other.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use irrigation::{Runtime, RuntimeConfig, ViewSchema, view};

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

#[irrigation::irrigation]
mod writers {
    #[component]
    pub fn left(tag: u32) -> u32 {
        irrigation::ensure_view(<super::Pair as irrigation::ViewSchema>::DESCRIPTOR);
        super::slow();
        // The typed handle, not a literal `FieldId`: the field's identity is the
        // generated constant, so a writer cannot name a field that does not exist.
        irrigation::define_unit(super::Pair::LEFT.unit(), u64::from(tag), "left writer");
        0
    }

    #[component]
    pub fn right(tag: u32) -> u32 {
        irrigation::ensure_view(<super::Pair as irrigation::ViewSchema>::DESCRIPTOR);
        super::slow();
        irrigation::define_unit(super::Pair::RIGHT.unit(), u64::from(tag), "right writer");
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

/// A field's typed handle must be the field the descriptor declares at that position.
///
/// The handle carries `FieldId(index)` and the descriptor carries fields in declaration
/// order, so the two are one fact spelled twice. If the macro ever numbered a handle
/// differently from the descriptor, `Pair::LEFT.unit()` would name another field and every
/// writer using the typed path would write the wrong unit -- with no compile error.
#[test]
fn a_typed_handle_is_the_field_its_descriptor_declares() {
    for (handle, name) in [(Pair::LEFT, "left"), (Pair::RIGHT, "right")] {
        let declared = <Pair as ViewSchema>::DESCRIPTOR
            .fields
            .get(handle.id().0 as usize)
            .unwrap_or_else(|| panic!("the descriptor declares a field at {}", handle.id().0));
        assert_eq!(
            declared.name,
            name,
            "the handle at {} names `{name}`",
            handle.id().0
        );
        assert_eq!(
            handle.unit(),
            irrigation::UnitKey::field(
                <Pair as ViewSchema>::view_id(),
                handle.id(),
                irrigation::UnitKind::Value
            ),
            "the handle's unit is built from its own id"
        );
    }
}

/// An entity-bearing unit can be named from the field handle, without the caller spelling
/// the kind or the entity's shape. It must be the same unit the framework builds internally.
#[test]
fn a_membership_unit_is_named_from_the_field_handle() {
    use std::sync::Arc;

    let member = irrigation::any_key(7u64);
    let from_handle = Pair::LEFT.member_unit(Arc::clone(&member));
    let built_by_hand = irrigation::UnitKey::entity(
        <Pair as ViewSchema>::view_id(),
        Pair::LEFT.id(),
        irrigation::UnitKind::Membership,
        Arc::clone(&member),
    );
    assert_eq!(from_handle, built_by_hand, "the handle names the same unit");

    let entry = Pair::LEFT.entry_unit(member);
    assert_eq!(
        entry.kind,
        irrigation::UnitKind::Value,
        "the entry unit carries a payload"
    );
    assert!(
        from_handle.entity.is_some(),
        "the membership unit names its member"
    );
    assert_eq!(
        from_handle.field,
        Pair::LEFT.id(),
        "the unit names the field whose handle built it"
    );
}
