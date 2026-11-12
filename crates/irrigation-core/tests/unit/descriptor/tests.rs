//! Unit tests for `src/descriptor.rs`.

use super::*;
use crate::key::any_key;

#[test]
fn unit_keys_distinguish_entities_and_kinds() {
    let view = ViewId(1);
    let field = FieldId(0);
    let a = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
    let b = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
    let c = UnitKey::entity(view, field, UnitKind::Value, any_key(2u32));
    let structure = UnitKey::field(view, field, UnitKind::Structure);

    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_ne!(a, structure);
}

#[test]
fn unit_keys_with_different_types_are_distinct() {
    let view = ViewId(1);
    let field = FieldId(0);
    let int_key = UnitKey::entity(view, field, UnitKind::Membership, any_key(1u64));
    let str_key = UnitKey::entity(
        view,
        field,
        UnitKind::Membership,
        any_key(String::from("1")),
    );
    assert_ne!(int_key, str_key);
}

#[test]
fn coverage_matches_overlap_for_every_claim_shape() {
    let view = ViewId(1);
    let field = FieldId(0);
    let structure = UnitKey::field(view, field, UnitKind::Structure);
    let content = UnitKey::field(view, field, UnitKind::Content);
    let value = UnitKey::entity(view, field, UnitKind::Value, any_key(7u32));
    let membership = UnitKey::entity(view, field, UnitKind::Membership, any_key(7u32));

    let structure_claim = WriteScope::entity(&structure);
    let content_claim = WriteScope::entity(&content);
    assert!(structure_claim.covers(&structure));
    assert!(
        !structure_claim.covers(&content),
        "a singleton claim names one kind"
    );
    assert!(!structure_claim.overlaps(&content_claim));

    let value_claim = WriteScope::Entity(value.clone());
    let membership_claim = WriteScope::Entity(membership.clone());
    assert!(value_claim.covers(&membership));
    assert!(membership_claim.covers(&value));
    assert!(
        value_claim.overlaps(&membership_claim),
        "one entity is one ownership pair regardless of the observed kind"
    );

    let field_claim = WriteScope::Field(view, field);
    assert!(field_claim.covers(&structure));
    assert!(field_claim.overlaps(&structure_claim));
    assert!(structure_claim.overlaps(&field_claim));
}
