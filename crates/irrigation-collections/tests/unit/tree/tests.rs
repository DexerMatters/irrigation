//! Unit tests for `src/tree.rs`.

use super::*;

fn sample() -> TreeBuild {
    TreeBuild {
        id: None,
        type_index: 0,
        variant: 0,
        fields: vec![TreeValue::Text(String::from("root"))],
        children: vec![BuildSlot::Many(vec![
            TreeBuild::leaf(1, 0).with_field(TreeValue::Int(1)),
            TreeBuild {
                id: None,
                type_index: 1,
                variant: 1,
                fields: vec![TreeValue::Text(String::from("call"))],
                children: vec![
                    BuildSlot::One(Box::new(
                        TreeBuild::leaf(2, 0).with_field(TreeValue::Text(String::from("x"))),
                    )),
                    BuildSlot::Many(vec![
                        TreeBuild::leaf(2, 1).with_field(TreeValue::Int(2)),
                        TreeBuild::leaf(2, 2).with_field(TreeValue::Int(3)),
                    ]),
                ],
            },
        ])],
    }
}

fn insert(storage: &mut TreeStorage, id: TreeNodeId, node: TreeNode) {
    storage.nodes = storage.nodes.insert(id, node).0;
}

#[test]
fn snapshot_refs_read_tags_fields_and_children() {
    // Build a storage by hand mirroring what the editor produces.
    let mut storage = TreeStorage::default();
    insert(
        &mut storage,
        0,
        TreeNode {
            type_index: 0,
            variant: 0,
            fields: vec![TreeValue::Text(String::from("root"))],
            children: vec![ChildSlot::Many(vec![1, 2])],
            parent: None,
        },
    );
    insert(
        &mut storage,
        1,
        TreeNode {
            type_index: 1,
            variant: 0,
            fields: vec![TreeValue::Int(1)],
            children: vec![],
            parent: Some(0),
        },
    );
    insert(
        &mut storage,
        2,
        TreeNode {
            type_index: 1,
            variant: 1,
            fields: vec![TreeValue::Text(String::from("call"))],
            children: vec![ChildSlot::One(3), ChildSlot::Many(vec![4, 5])],
            parent: Some(0),
        },
    );
    for (id, parent) in [(3, 2), (4, 2), (5, 2)] {
        insert(
            &mut storage,
            id,
            TreeNode {
                type_index: 2,
                variant: 0,
                fields: vec![TreeValue::UInt(id)],
                children: vec![],
                parent: Some(parent),
            },
        );
    }
    storage.root = Some(0);
    storage.next_id = 6;

    let storage = Arc::new(storage);
    let root_ref = TreeRef::snapshot(Arc::clone(&storage), 0);
    assert_eq!(root_ref.tag(), Some((0, 0)));
    assert_eq!(
        root_ref
            .field(0)
            .and_then(|v| v.as_text().map(str::to_string)),
        Some(String::from("root"))
    );
    let children = root_ref.child_slot(0);
    assert_eq!(children.ids(), vec![1, 2]);
    let call = root_ref.child(2);
    assert_eq!(call.tag(), Some((1, 1)));
    assert_eq!(call.parent().map(|parent| parent.id()), Some(0));
    assert_eq!(call.child_slot(1).ids(), vec![4, 5]);
    assert_eq!(storage.preorder(), vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn tree_storage_is_a_field_store() {
    fn assert_field_store<T: irrigation_store::delta::FieldStore>() {}
    assert_field_store::<TreeStorage>();
}

#[test]
fn tree_changed_units_compare_per_node_fields() {
    let old = TreeStorage::default();
    let patch = TreePatch {
        root: Some(1),
        nodes: vec![TreePatchNode {
            id: 1,
            parent: None,
            type_index: 0,
            variant: 0,
            fields: vec![TreeValue::Int(1)],
            children: vec![],
        }],
        removed: vec![],
    };
    let (old, _) = TreeStorage::apply_patch(&old, &patch);
    let (same, _) = TreeStorage::apply_patch(&TreeStorage::default(), &patch);
    let patch = TreePatch {
        root: Some(1),
        nodes: vec![TreePatchNode {
            id: 1,
            parent: None,
            type_index: 0,
            variant: 0,
            fields: vec![TreeValue::Int(2)],
            children: vec![],
        }],
        removed: vec![],
    };
    let (next, _) = TreeStorage::apply_patch(&TreeStorage::default(), &patch);
    let view = ViewId(1);
    let field = FieldId(slots::FIELD);
    let unit = UnitKey::entity(view, field, UnitKind::Value, any_key((1u64, 0u16)));
    assert_eq!(
        tree_changed_units_equal(
            &old_as_content(&old),
            &old_as_content(&same),
            std::slice::from_ref(&unit)
        ),
        Some(true)
    );
    assert_eq!(
        tree_changed_units_equal(
            &old_as_content(&old),
            &old_as_content(&next),
            std::slice::from_ref(&unit)
        ),
        Some(false)
    );
    let root = UnitKey::field(view, FieldId(slots::ROOT), UnitKind::Value);
    assert_eq!(
        tree_changed_units_equal(
            &old_as_content(&old),
            &old_as_content(&same),
            std::slice::from_ref(&root)
        ),
        Some(true)
    );
}

fn old_as_content(storage: &TreeStorage) -> Content {
    Arc::new(storage.clone())
}

#[test]
fn tree_storage_patch_is_a_field_delta() {
    let storage = TreeStorage::default();
    let patch = TreePatch {
        root: Some(1),
        nodes: vec![
            TreePatchNode {
                id: 1,
                parent: None,
                type_index: 0,
                variant: 0,
                fields: vec![TreeValue::Text(String::from("root"))],
                children: vec![ChildSlot::Many(vec![2])],
            },
            TreePatchNode {
                id: 2,
                parent: Some(1),
                type_index: 1,
                variant: 0,
                fields: vec![TreeValue::Int(7)],
                children: vec![],
            },
        ],
        removed: vec![],
    };
    let (storage, applied) = TreeStorage::apply_patch(&storage, &patch);
    assert!(applied.content_changed);
    assert_eq!(storage.len(), 2);
    assert_eq!(storage.preorder(), vec![1, 2]);

    let patch = TreePatch {
        root: Some(1),
        nodes: vec![TreePatchNode {
            id: 1,
            parent: None,
            type_index: 0,
            variant: 0,
            fields: vec![TreeValue::Text(String::from("root"))],
            children: vec![],
        }],
        removed: vec![2],
    };
    let (storage, applied) = TreeStorage::apply_patch(&storage, &patch);
    assert_eq!(storage.len(), 1);
    assert_eq!(applied.removed, vec![2]);
}

#[test]
fn tree_build_construction_is_structural() {
    let build = sample();
    assert_eq!(build.children.len(), 1);
    match &build.children[0] {
        BuildSlot::Many(children) => {
            assert_eq!(children.len(), 2);
            assert_eq!(children[1].fields[0], TreeValue::Text(String::from("call")));
        }
        other => panic!("unexpected slot {other:?}"),
    }
}

#[test]
fn vtable_round_trips_root_and_membership() {
    let content = tree_empty();
    let key = UnitKey::field(ViewId(1), FieldId(slots::ROOT), UnitKind::Value);
    assert!(tree_get(&content, &key).is_some());
    assert!(tree_eq(&content, &content));
}
