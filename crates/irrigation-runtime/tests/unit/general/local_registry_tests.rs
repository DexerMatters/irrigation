//! Unit tests for `src/general.rs`.

use super::*;

#[derive(Debug, Clone)]
struct Counter;

impl LocalGeneralComponent for Counter {
    type State = u64;
    type Output = u64;

    const ID: irrigation_core::ComponentId = irrigation_core::ComponentId(0x00C0_00F1);
    const NAME: &'static str = "general::local_registry_tests::Counter";

    fn initialize(&self, _cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)> {
        Ok((0, 0))
    }

    fn update(
        &self,
        _cx: &mut GeneralCx,
        state: &Self::State,
        _changes: &ChangeBatch,
    ) -> Result<(Self::State, Self::Output)> {
        Ok((state + 1, state + 1))
    }
}

/// Stages one slot for `runtime` at `node`, returning its registry key.
fn stage_slot(runtime: u64, node: irrigation_core::ids::NodeId) -> u64 {
    let key = NEXT_LOCAL_SLOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    LOCAL_SLOTS.with(|slots| {
        slots.borrow_mut().insert(
            key,
            Box::new(Slot {
                component: Counter,
                state: Some(0),
                staged: Some(1),
                node: Some((runtime, node)),
            }),
        );
    });
    key
}

/// Whether the slot still holds a staged, unpromoted state.
///
/// The slot itself returns to the registry either way -- `SlotGuard` restores
/// it after promoting -- so the observable is whether it still names a
/// pending node.
fn waiting(key: u64) -> Option<bool> {
    LOCAL_SLOTS.with(|slots| {
        let slots = slots.borrow();
        slots.get(&key).map(|slot| slot.pending_node().is_some())
    })
}

/// The registry is thread-global and a `NodeId` is only unique inside one
/// arena: a commit in one runtime must not promote another's staged state.
#[test]
fn a_promotion_only_matches_its_own_runtime() {
    let node = irrigation_core::ids::NodeId::new(7, 1);
    let key = stage_slot(1, node);

    // Another runtime on this thread commits the same node id: nothing here.
    promote_local_state(2, node, true);
    assert_eq!(
        waiting(key),
        Some(true),
        "another runtime's commit must leave this staged state alone"
    );

    // Its own runtime commits: the slot is consumed.
    promote_local_state(1, node, true);
    assert_eq!(
        waiting(key),
        Some(false),
        "the owning runtime's commit promotes it"
    );
}
