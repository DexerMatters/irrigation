//! Unit tests for `src/tree.rs`.

use super::*;

/// Presence must mean readability for every slot the reader knows.
///
/// `tree_contains` used to handle only `ROOT`, `TAG` and `PARENT`, so a `FIELD`,
/// `CHILDREN` or `MEMBERSHIP` unit that `tree_get` read was reported absent.
#[test]
fn a_readable_unit_is_present() {
    let mut storage = TreeStorage::default();
    let root = build_tree_node(
        &TreeBuild {
            id: Some(1),
            type_index: 0,
            variant: 0,
            fields: vec![TreeValue::Int(7)],
            children: vec![BuildSlot::One(Box::new(TreeBuild::leaf(1, 0)))],
        },
        None,
        &mut storage,
    );
    let content: Arc<dyn Any + Send + Sync> = Arc::new(storage);
    let field = UnitKey::entity(
        ViewId(0),
        FieldId(slots::FIELD),
        UnitKind::Structure,
        irrigation_core::key::any_key((root, 0u16)),
    );
    assert!(
        tree_get(&content, &field).is_some(),
        "the reader knows the field unit"
    );
    assert!(
        tree_contains(&content, &field),
        "so presence must report it"
    );
}
