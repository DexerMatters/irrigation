//! Unit tests for `src/ids.rs`.

use super::*;

#[test]
fn full_width_ids_do_not_alias_where_truncation_did() {
    // These two names collided under the old `as u32` truncation
    // (0x335f71be). Full-width ViewId/ComponentId values do not.
    let first = const_hash("audit_hash_view::InkOwcyHFoDQS");
    let second = const_hash("audit_hash_view::MOlqwVtqzJ");
    assert_eq!(first as u32, second as u32);
    assert_ne!(first, second);
}

#[test]
fn const_hash_is_deterministic_and_distinguishes_names() {
    assert_eq!(const_hash("a::b"), const_hash("a::b"));
    assert_ne!(const_hash("a::b"), const_hash("a::c"));
    assert_ne!(const_hash(""), const_hash("x"));
}

#[test]
fn const_hash_is_fnv1a_64() {
    // FNV-1a 64 of the one-byte string "a".
    assert_eq!(const_hash("a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(const_hash(""), 0xcbf2_9ce4_8422_2325);
}

#[test]
fn node_id_try_new_rejects_out_of_range_halves() {
    assert_eq!(NodeId::try_new(7, 3), Some(NodeId::new(7, 3)));
    assert_eq!(
        NodeId::try_new(NodeId::INDEX_MASK, 0),
        Some(NodeId::new(NodeId::INDEX_MASK, 0))
    );
    assert_eq!(
        NodeId::try_new(0, NodeId::MAX_GENERATION),
        Some(NodeId::new(0, NodeId::MAX_GENERATION))
    );
    assert_eq!(NodeId::try_new(NodeId::INDEX_MASK + 1, 0), None);
    assert_eq!(NodeId::try_new(0, NodeId::MAX_GENERATION + 1), None);
}

#[test]
fn node_id_none_is_distinct_from_real_indices() {
    assert_eq!(NodeId::NONE.index(), NodeId::INDEX_MASK as usize);
    assert_eq!(NodeId(7).index(), 7);
    assert_eq!(NodeId(7).generation(), 0);
    assert_eq!(NodeId::new(7, 3).index(), 7);
    assert_eq!(NodeId::new(7, 3).generation(), 3);
}
