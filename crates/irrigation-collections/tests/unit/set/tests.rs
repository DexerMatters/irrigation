//! Unit tests for `src/set.rs`.

use super::*;

#[test]
fn set_unit_eq_compares_membership_only() {
    let view = ViewId(1);
    let field = FieldId(0);
    let unit = UnitKey::entity(view, field, UnitKind::Membership, any_key(1u32));
    let old: PSet<u32> = [1, 2].into_iter().collect();
    let same: PSet<u32> = [1, 2].into_iter().collect();
    let next: PSet<u32> = [2, 3].into_iter().collect();
    assert_eq!(
        <SourceSet<u32> as ViewField>::unit_eq(&old, &same, &unit),
        Some(true)
    );
    assert_eq!(
        <SourceSet<u32> as ViewField>::unit_eq(&old, &next, &unit),
        Some(false)
    );
    let structure = UnitKey::field(view, field, UnitKind::Structure);
    assert_eq!(
        <SourceSet<u32> as ViewField>::unit_eq(&old, &next, &structure),
        Some(false)
    );
}

#[test]
fn derived_set_storage_is_a_field_store() {
    fn assert_field_store<T: irrigation_store::delta::FieldStore>() {}
    assert_field_store::<DerivedSetStorage<u32>>();
}

#[test]
fn set_define_many_batches_membership() {
    let storage = <SourceSet<u32> as ViewField>::empty();
    let view = ViewId(1);
    let field = FieldId(0);
    let drafts = vec![
        (
            UnitKey::entity(view, field, UnitKind::Membership, any_key(1u32)),
            value_of(true),
        ),
        (
            UnitKey::entity(view, field, UnitKind::Membership, any_key(2u32)),
            value_of(true),
        ),
    ];
    let storage = <SourceSet<u32> as ViewField>::define_many(&storage, &drafts);
    assert!(storage.contains(&1));
    assert!(storage.contains(&2));
    assert_eq!(storage.len(), 2);
}

#[test]
fn set_define_many_preserves_a_whole_field_draft() {
    let storage = <DerivedSet<u32> as ViewField>::empty();
    let view = ViewId(1);
    let field = FieldId(0);
    let whole = DerivedSetStorage::Keyed([1u32, 2].into_iter().collect());
    let drafts = vec![(
        UnitKey::field(view, field, UnitKind::Value),
        value_of(whole),
    )];
    let storage = <DerivedSet<u32> as ViewField>::define_many(&storage, &drafts);
    assert_eq!(storage.to_vec(), vec![1, 2]);
}
