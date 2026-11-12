//! Unit tests for `src/tree.rs`.

use super::*;

/// `TreePatch::removed` removes the listed identities that the new root no longer
/// reaches, and reports them. The doc used to promise that subtrees went with their
/// roots; the code removes exactly what is listed, and the `reachable` guard keeps it
/// from deleting a node the patch re-attached.
#[test]
fn a_patch_removes_listed_nodes_that_the_new_root_cannot_reach() {
    let mut storage = TreeStorage::default();
    let root = build_tree_node(
        &TreeBuild {
            id: Some(1),
            type_index: 0,
            variant: 0,
            fields: Vec::new(),
            children: vec![BuildSlot::One(Box::new(TreeBuild {
                id: Some(2),
                type_index: 1,
                variant: 0,
                fields: Vec::new(),
                children: Vec::new(),
            }))],
        },
        None,
        &mut storage,
    );
    assert!(storage.node(2).is_some(), "the child is installed");

    // A patch that drops the child from the root's slot must also be told to remove it.
    let patch = TreePatch {
        root: Some(root),
        nodes: vec![TreePatchNode {
            id: root,
            type_index: 0,
            variant: 0,
            fields: Vec::new(),
            children: vec![ChildSlot::None],
            parent: None,
        }],
        removed: vec![2],
    };
    let (next, applied) = storage.apply_patch(&patch);
    assert!(
        next.node(2).is_none(),
        "a listed, unreachable node is removed"
    );
    assert!(applied.removed.contains(&2), "and the delta reports it");
}

/// A listed node the new root still reaches is kept: the guard exists so a patch that
/// re-parents a node cannot delete it in the same step.
#[test]
fn a_patch_keeps_a_listed_node_that_is_still_reachable() {
    let mut storage = TreeStorage::default();
    let root = build_tree_node(
        &TreeBuild {
            id: Some(1),
            type_index: 0,
            variant: 0,
            fields: Vec::new(),
            children: vec![BuildSlot::One(Box::new(TreeBuild {
                id: Some(2),
                type_index: 1,
                variant: 0,
                fields: Vec::new(),
                children: Vec::new(),
            }))],
        },
        None,
        &mut storage,
    );
    let patch = TreePatch {
        root: Some(root),
        nodes: Vec::new(),
        removed: vec![2],
    };
    let (next, applied) = storage.apply_patch(&patch);
    assert!(
        next.node(2).is_some(),
        "a reachable node survives being listed"
    );
    assert!(applied.removed.is_empty());
}
