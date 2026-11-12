//! Unit tests for `src/inner/ops.rs`.

use super::*;

/// Bookkeeping written per invocation scope must be dropped with the scope,
/// or the map grows by an entry per invocation forever.
#[test]
fn pruning_drops_only_unreachable_scopes() {
    let mut state = State::default();
    let live = ScopeId(1);
    let dead = ScopeId(2);
    let component = irrigation_core::ComponentId(0x00C0_00A1);
    state.scope_components.insert(live, component);
    state.scope_components.insert(dead, component);
    state
        .declared_views
        .entry(dead)
        .or_default()
        .insert(ViewId(3));
    let mut live_scopes = irrigation_core::hash::FxHashSet::default();
    live_scopes.insert(live);

    Inner::retain_live_scopes(&mut state, &live_scopes);

    assert!(
        state.scope_components.contains_key(&live),
        "a reachable scope keeps its component"
    );
    assert!(
        !state.scope_components.contains_key(&dead),
        "an unreachable scope's component mapping is dropped"
    );
    assert!(
        !state.declared_views.contains_key(&dead),
        "and so are its declared views"
    );
}
