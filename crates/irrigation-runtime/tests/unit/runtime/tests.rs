//! Unit tests for `src/runtime.rs`.

#[cfg(feature = "observe")]
use irrigation_core::descriptor::{UnitKey, UnitKind};
use irrigation_core::frame::Dep;
#[cfg(feature = "observe")]
use irrigation_core::ids::{FieldId, ViewId};

use crate::state::{NodeKind, NodeRecord};

/// A body that still defines units is retained while the observations it
/// recorded are reclaimed; explaining it must report the loss, not panic.
#[test]
fn explaining_tolerates_a_reclaimed_observation() {
    let runtime = crate::Runtime::new();
    let (body, dependency) = {
        let mut state = runtime.inner.lock();
        let dependency = state.alloc(NodeRecord::empty("site", NodeKind::Site));
        let mut stale = NodeRecord::empty("body", NodeKind::InvocationBody);
        stale.trace.push(Dep {
            node: dependency,
            version: 3,
            routing: 1,
            validity: 2,
        });
        let body = state.alloc(stale);
        // `collect` retains a provider for the definitions it installed but
        // frees the nodes its trace names, exactly as this does.
        state.node_mut(dependency).freed = true;
        (body, dependency)
    };
    assert!(
        runtime.try_explain_node(dependency).is_none(),
        "a reclaimed identity explains to nothing"
    );
    assert!(
        runtime.try_explain_node(body).is_none(),
        "a live node with a reclaimed observation explains to nothing"
    );
}

/// The same trace with its observation intact still explains.
#[test]
fn explaining_a_live_observation_still_explains() {
    let runtime = crate::Runtime::new();
    let (body, dependency) = {
        let mut state = runtime.inner.lock();
        let dependency = state.alloc(NodeRecord::empty("site", NodeKind::Site));
        let mut record = NodeRecord::empty("body", NodeKind::InvocationBody);
        record.trace.push(Dep {
            node: dependency,
            version: 3,
            routing: 1,
            validity: 2,
        });
        (state.alloc(record), dependency)
    };
    let explanation = runtime
        .try_explain_node(body)
        .expect("a live node with a live observation explains");
    assert_eq!(explanation.dependencies.len(), 1);
    assert_eq!(explanation.dependencies[0].node, dependency);
}

#[test]
#[cfg(feature = "observe")]
fn a_host_edited_unit_has_one_change_version() {
    let runtime = crate::Runtime::new();
    let unit = UnitKey::field(ViewId(0x5151), FieldId(0), UnitKind::Value);
    // A host edit installs the unit in the global table, not in its region.
    runtime
        .edit(|edit| edit.set(unit.clone(), 5u32))
        .expect("host edit");
    assert_eq!(
        runtime.unit_changed_at(&unit),
        Some(1),
        "the host edit installed the unit"
    );
    assert_eq!(
        runtime.unit_versions(std::slice::from_ref(&unit)),
        vec![Some(1)],
        "the two questions about one unit must agree"
    );
}
