//! Unit tests for `src/inner/definitions.rs`.

use std::sync::Arc;

use irrigation_core::descriptor::{
    ContentVtable, FieldDescriptor, FieldKind, ViewDescriptor, ViewKind,
};

use super::*;

const VIEW: ViewId = ViewId(0x7171);
const FIELDS: &[FieldDescriptor] = &[FieldDescriptor {
    name: "value",
    kind: FieldKind::Scalar,
}];

static DESCRIPTOR: ViewDescriptor = ViewDescriptor {
    id: VIEW,
    name: "RetractionProbe",
    kind: ViewKind::Derived,
    fields: FIELDS,
    content: ContentVtable {
        empty: || Arc::new(0u64),
        define: |content, _key, value| match value.downcast_ref::<u64>() {
            Some(value) => Arc::new(*value),
            None => Arc::clone(content),
        },
        define_many: |content, drafts| match drafts.first() {
            Some((_, value)) => match value.downcast_ref::<u64>() {
                Some(value) => Arc::new(*value),
                None => Arc::clone(content),
            },
            None => Arc::clone(content),
        },
        retract: |_content, _key| Arc::new(0u64),
        contains: |_content, key| key.kind == UnitKind::Value,
        get: |content, _key| content.downcast_ref::<u64>().map(|value| value_of(*value)),
        field_storage: |content, field| {
            (field == FieldId(0))
                .then(|| content.downcast_ref::<u64>().map(|value| value_of(*value)))
                .flatten()
        },
        structure: |_, _| None,
        len: |_, _| None,
        moves_content: |_| true,
        changed_units_equal: |_, _, _| None,
        summary: |_, _| None,
        eq: |left, right| left.downcast_ref::<u64>() == right.downcast_ref::<u64>(),
        field_equal: |_, left, right| match (
            left.downcast_ref::<u64>(),
            right.downcast_ref::<u64>(),
        ) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        },
        describe: |_| None,
        variant_label: |_, _| None,
    },
};

/// Presence answers for every entity unit the committed content spells, not
/// only for a membership: a payload port with no cell is present when the
/// content projects it, which is what every read of that unit does.
#[test]
fn a_content_seeded_payload_port_is_present() {
    let inner = Inner::new(RuntimeConfig::default());
    inner.register_view(&DESCRIPTOR);
    let state = inner.lock();
    let entity = UnitKey::entity(
        VIEW,
        FieldId(0),
        UnitKind::Value,
        irrigation_core::key::any_key(5u32),
    );
    // No cell exists for this unit; the descriptor's `get` projects every
    // Value unit from the content.
    assert!(
        state.units.get(&entity).is_none(),
        "the premise is that nothing materialized the cell"
    );
    assert!(
        unit_present(&state, &entity),
        "the content spells this payload unit, so it is present"
    );
    // A field-level unit with no cell is still absent: only an entity has a
    // content projection to fall back to.
    let field_level = UnitKey::field(VIEW, FieldId(0), UnitKind::Value);
    assert!(!unit_present(&state, &field_level));
}

/// A provider that retracts a whole-field definition whose cell already holds
/// the field's empty value must not move that cell's version: the value did
/// not change, and every unit subscription would otherwise be woken for it.
///
/// This branch is reached by no integration test in the workspace, which is
/// why the public-API gates written for it passed with the cutoff disabled.
#[test]
fn retracting_an_equal_whole_field_value_keeps_its_version() {
    let inner = Inner::new(RuntimeConfig::default());
    inner.register_view(&DESCRIPTOR);
    let mut state = inner.lock();
    state.revision = 4;
    let provider = state.alloc(NodeRecord::empty("provider", NodeKind::Unit));
    let unit = UnitKey::field(VIEW, FieldId(0), UnitKind::Value);
    let mut record = NodeRecord::unit("value", unit.clone());
    // The cell already holds the field's empty value.
    record.install_value(Some(value_of(0u64)), 3, 3);
    let node = state.alloc(record);
    state.units.insert(Arc::new(unit.clone()), node);
    state.record_cell(&unit, node);
    {
        let region = state.regions.get_mut(&VIEW).expect("registered");
        region
            .units
            .insert(any_key(unit.clone()), UnitSlot { node });
        region.owners.insert(
            any_key(owner_key(&unit)),
            ProviderRef {
                node: provider,
                label: "provider",
                location: None,
            },
        );
    }
    let before = state.node(node);
    let (version, stamp) = (before.value_version, before.changed_at);

    let mut changed = Vec::new();
    Inner::with_taken_region(&mut state, VIEW, |state, region| {
        Inner::retract_unit(state, region, provider, &unit, 9, &mut changed, false);
    })
    .expect("the region is registered");

    let after = state.node(node);
    assert_eq!(
        after.value_version, version,
        "an equal payload must keep its change version"
    );
    assert_eq!(after.changed_at, stamp, "and its stamp");
    assert!(
        changed.is_empty(),
        "and must not be reported as a changed node"
    );
}
