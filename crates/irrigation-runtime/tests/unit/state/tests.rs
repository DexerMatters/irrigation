//! Unit tests for `src/state.rs`.

use super::*;
use irrigation_core::descriptor::UnitKind;
use irrigation_core::key::any_key;
use std::hash::{Hash, Hasher};

/// Two different keys that deliberately share one hash.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Colliding(u64);

impl Hash for Colliding {
    fn hash<H: Hasher>(&self, state: &mut H) {
        0u64.hash(state);
    }
}

#[test]
fn keyed_child_scopes_disambiguate_equal_hashes() {
    let mut table = ScopeTable::default();
    let first = table.child_keyed(ScopeId::ROOT, any_key(Colliding(1)));
    let second = table.child_keyed(ScopeId::ROOT, any_key(Colliding(2)));
    assert_ne!(first, second, "equal hashes must not share a scope");
    assert_eq!(
        table.child_keyed(ScopeId::ROOT, any_key(Colliding(1))),
        first
    );
    let other_parent = table.child(ScopeId::ROOT, 1);
    assert_ne!(
        table.child_keyed(other_parent, any_key(Colliding(1))),
        first,
        "the parent is part of the scope identity"
    );
}

#[test]
fn scope_ids_are_allocated_not_hashed() {
    let mut table = ScopeTable::default();
    let first = table.child(ScopeId::ROOT, 7);
    let second = table.child(ScopeId::ROOT, 8);
    assert_ne!(first, second);
    assert_ne!(first, ScopeId::ROOT);
    assert_eq!(table.child(ScopeId::ROOT, 7), first);
    assert_eq!(table.len(), 2);
}

#[test]
fn field_claim_conflicts_with_singleton_claim_in_either_order() {
    let view = ViewId(3);
    let field = FieldId(1);
    let singleton = UnitKey::field(view, field, UnitKind::Value);
    let field_scope = WriteScope::Field(view, field);
    let singleton_scope = WriteScope::entity(&singleton);

    for field_first in [true, false] {
        let mut state = State::default();
        let first = state.alloc(NodeRecord::unit("first", singleton.clone()));
        let second = state.alloc(NodeRecord::unit("second", singleton.clone()));
        let (first_scope, second_scope) = if field_first {
            (field_scope.clone(), singleton_scope.clone())
        } else {
            (singleton_scope.clone(), field_scope.clone())
        };
        state.register_claim(first, 1, first_scope, "first", None);

        let conflict = state
            .conflicting_claim(&second_scope, second)
            .expect("a whole-field claim and its singleton are one unit");
        assert_eq!(conflict.owner, first);
        assert_eq!(
            state.claim_covering(&singleton).map(|claim| claim.owner),
            Some(first)
        );
    }
}

#[test]
fn one_entity_is_one_claim_across_unit_kinds() {
    let view = ViewId(3);
    let field = FieldId(1);
    let value = UnitKey::entity(view, field, UnitKind::Value, any_key(9u32));
    let membership = UnitKey::entity(view, field, UnitKind::Membership, any_key(9u32));
    let mut state = State::default();
    let first = state.alloc(NodeRecord::unit("first", value.clone()));
    let second = state.alloc(NodeRecord::unit("second", membership.clone()));
    state.register_claim(first, 1, WriteScope::Entity(value), "first", None);

    let conflict = state.conflicting_claim(&WriteScope::Entity(membership), second);
    assert_eq!(conflict.map(|claim| claim.owner), Some(first));
}

#[test]
fn exhausted_generation_retires_the_slot_instead_of_wrapping() {
    let unit = UnitKey::field(ViewId(3), FieldId(1), UnitKind::Value);
    let mut state = State::default();
    let first = state.alloc(NodeRecord::unit("first", unit.clone()));
    assert_eq!(first.index(), 0);
    state.nodes[0].generation = NodeId::MAX_GENERATION;
    state.nodes[0].freed = true;
    state.free_nodes.push(0);

    let second = state.alloc(NodeRecord::unit("second", unit.clone()));
    assert_eq!(second.index(), 1, "an exhausted slot is retired");
    assert_eq!(second.generation(), 0);
    assert_eq!(state.nodes[0].generation, NodeId::MAX_GENERATION);
    assert_eq!(state.free_nodes, vec![0], "the retired slot stays reserved");

    let third = state.alloc(NodeRecord::unit("third", unit));
    assert_eq!(third.index(), 2, "the retired slot is never handed out");
    assert_eq!(state.free_nodes, vec![0]);
}

#[test]
fn claim_group_conflicts_without_partial_claims() {
    let view = ViewId(9);
    let field = FieldId(0);
    let first = WriteScope::Entity(UnitKey::entity(view, field, UnitKind::Value, any_key(1u32)));
    let second_unit = UnitKey::entity(view, field, UnitKind::Value, any_key(2u32));
    let second = WriteScope::Entity(second_unit.clone());
    let mut state = State::default();
    let owner = state.alloc(NodeRecord::unit(
        "owner",
        UnitKey::field(view, field, UnitKind::Value),
    ));
    let other = state.alloc(NodeRecord::unit(
        "other",
        UnitKey::field(view, field, UnitKind::Value),
    ));
    let third = state.alloc(NodeRecord::unit(
        "third",
        UnitKey::field(view, field, UnitKind::Value),
    ));
    state.register_claim(owner, 0, first.clone(), "first", None);

    let conflict = state
        .claim_group(other, 1, &[first, second.clone()], "group", None)
        .expect_err("overlap must reject the whole group");
    assert!(!conflict.0.is_empty());
    assert!(
        state.claim_covering(&second_unit).is_none(),
        "the rejected group left a partial claim"
    );
    state
        .claim_group(third, 2, std::slice::from_ref(&second), "third", None)
        .expect("the uncontested scope is still free");
    assert!(state.claim_covering(&second_unit).is_some());
}

#[test]
fn a_plan_is_visible_until_its_owner_commits() {
    let view = ViewId(9);
    let field = FieldId(0);
    let unit = UnitKey::entity(view, field, UnitKind::Value, any_key(7u32));
    let scope = WriteScope::Entity(unit.clone());
    let mut state = State::default();
    let owner = state.alloc(NodeRecord::unit("owner", unit.clone()));
    state
        .plan_group(owner, 0, &[scope], "plan", None)
        .expect("plan");
    assert!(state.planned_covering(&unit).is_some());
    state.release_claims(owner);
    assert!(
        state.planned_covering(&unit).is_none(),
        "release_claims must retire the plan too"
    );
}

#[test]
fn a_plan_can_be_withdrawn_before_its_claim() {
    let view = ViewId(9);
    let field = FieldId(0);
    let planned = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
    let kept = UnitKey::entity(view, field, UnitKind::Value, any_key(2u32));
    let mut state = State::default();
    let owner = state.alloc(NodeRecord::unit("owner", planned.clone()));
    let scopes = [
        WriteScope::Entity(planned.clone()),
        WriteScope::Entity(kept.clone()),
    ];
    state
        .plan_group(owner, 0, &scopes, "plan", None)
        .expect("plan");
    state.unplan_group(owner, std::slice::from_ref(&scopes[0]));
    assert!(state.planned_covering(&planned).is_none());
    assert!(state.planned_covering(&kept).is_some());
}

#[test]
fn a_claim_group_dedupes_the_ownership_pair() {
    let view = ViewId(9);
    let field = FieldId(0);
    let value = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
    let membership = UnitKey::entity(view, field, UnitKind::Membership, any_key(1u32));
    let other = UnitKey::entity(view, field, UnitKind::Value, any_key(2u32));
    let mut state = State::default();
    let owner = state.alloc(NodeRecord::unit("owner", value.clone()));
    let scopes = [
        WriteScope::entity(&value),
        WriteScope::entity(&membership),
        WriteScope::entity(&other),
    ];
    let claimed = state
        .claim_group(owner, 0, &scopes, "group", None)
        .expect("claim group");
    assert_eq!(claimed, 2, "one entity is one claim across its unit kinds");
    assert_eq!(state.node(owner).claims.len(), 2);
    assert!(state.claim_covering(&value).is_some());
    assert!(state.claim_covering(&other).is_some());
}

#[test]
fn a_broad_claim_still_blocks_one_entity() {
    let view = ViewId(9);
    let field = FieldId(0);
    let unit = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
    let mut state = State::default();
    let first = state.alloc(NodeRecord::unit("first", unit.clone()));
    let second = state.alloc(NodeRecord::unit("second", unit.clone()));
    state.register_claim(first, 0, WriteScope::Field(view, field), "field", None);
    let conflict = state.conflicting_claim(&WriteScope::entity(&unit), second);
    assert_eq!(conflict.map(|claim| claim.owner), Some(first));
    state.release_claims(first);
    assert!(
        state
            .conflicting_claim(&WriteScope::entity(&unit), second)
            .is_none()
    );
}

#[test]
fn overlapping_plans_conflict_before_any_claim() {
    let view = ViewId(9);
    let field = FieldId(0);
    let unit = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
    let scope = WriteScope::entity(&unit);
    let mut state = State::default();
    let first = state.alloc(NodeRecord::unit("first", unit.clone()));
    let second = state.alloc(NodeRecord::unit("second", unit));
    state
        .plan_group(first, 0, std::slice::from_ref(&scope), "first", None)
        .expect("first plan");
    let conflict = state
        .plan_group(second, 1, std::slice::from_ref(&scope), "second", None)
        .expect_err("overlap must conflict");
    assert!(!conflict.0.is_empty());
    state.unplan_group(first, std::slice::from_ref(&scope));
    state
        .plan_group(second, 1, std::slice::from_ref(&scope), "second", None)
        .expect("handover after withdrawal");
}
