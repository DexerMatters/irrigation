//! Unit tests for `src/tree.rs`.

use super::*;

/// A supplied identity is used as-is and the allocator moves past it, so the next
/// anonymous node cannot take it.
#[test]
fn a_supplied_identity_is_honoured_and_not_handed_out_again() {
    let mut storage = TreeStorage::default();
    assert_eq!(
        resolve_build_identity(Some(41), &mut storage),
        41,
        "the supplied id is the id"
    );
    assert!(
        storage.next_id > 41,
        "and the allocator cannot reach it again (next_id = {})",
        storage.next_id
    );
    assert_eq!(
        resolve_build_identity(None, &mut storage),
        storage.next_id - 1,
        "an anonymous node takes the next free id"
    );
}

/// Two nodes under one identity would alias each other's units, so it is refused.
#[test]
#[should_panic(expected = "reuses identity 7")]
fn a_duplicate_identity_is_refused() {
    let mut storage = TreeStorage::default();
    storage.nodes = storage
        .nodes
        .insert(
            7,
            TreeNode {
                type_index: 0,
                variant: 0,
                fields: Vec::new(),
                children: Vec::new(),
                parent: None,
            },
        )
        .0;
    resolve_build_identity(Some(7), &mut storage);
}
