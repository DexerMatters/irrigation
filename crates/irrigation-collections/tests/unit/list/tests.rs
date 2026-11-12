//! Unit tests for `src/list.rs`.

use super::*;

#[test]
fn source_list_field_tracks_identity() {
    let storage = <SourceList<u32> as ViewField>::empty();
    let key = UnitKey::entity(ViewId(1), FieldId(0), UnitKind::Value, any_key(9u64));
    let storage = <SourceList<u32> as ViewField>::define(
        &storage,
        key.entity_key().expect("entity").as_ref(),
        value_of(5u32),
    );
    assert_eq!(storage.len(), 1);
    assert_eq!(storage.get(0), Some(&(9, 5)));
    let storage = <SourceList<u32> as ViewField>::retract(
        &storage,
        key.entity_key().expect("entity").as_ref(),
    );
    assert!(storage.is_empty());
}

#[test]
fn derived_list_field_replaces_the_whole_value() {
    let storage = <DerivedList<u32> as ViewField>::empty();
    let key = UnitKey::field(ViewId(1), FieldId(0), UnitKind::Value);
    let storage = <DerivedList<u32> as ViewField>::define(
        &storage,
        key.entity_key().map_or(&(), |key| key.as_ref()),
        value_of(DerivedListStorage::Whole(
            [1u32, 2, 3].into_iter().collect::<PSeq<u32>>(),
        )),
    );
    assert_eq!(storage.to_vec(), vec![1, 2, 3]);
}

#[test]
fn derived_list_unit_eq_compares_one_identity() {
    let view = ViewId(1);
    let field = FieldId(0);
    let value = UnitKey::entity(view, field, UnitKind::Value, any_key(1u64));
    let membership = UnitKey::entity(view, field, UnitKind::Membership, any_key(1u64));
    let old = DerivedListStorage::from_keyed([(1u64, 10u32), (2, 20)]);
    let same = DerivedListStorage::from_keyed([(1u64, 10u32), (2, 20)]);
    let next = DerivedListStorage::from_keyed([(1u64, 11u32), (2, 20)]);
    assert_eq!(
        <DerivedList<u32> as ViewField>::unit_eq(&old, &same, &value),
        Some(true)
    );
    assert_eq!(
        <DerivedList<u32> as ViewField>::unit_eq(&old, &next, &value),
        Some(false)
    );
    assert_eq!(
        <DerivedList<u32> as ViewField>::unit_eq(&old, &next, &membership),
        Some(true)
    );
}

#[test]
fn derived_list_define_many_batches_payload_identities() {
    let storage = <DerivedList<u32> as ViewField>::empty();
    let view = ViewId(1);
    let field = FieldId(0);
    let drafts = vec![
        (
            UnitKey::entity(view, field, UnitKind::Value, any_key(2u64)),
            value_of(20u32),
        ),
        (
            UnitKey::entity(view, field, UnitKind::Value, any_key(1u64)),
            value_of(10u32),
        ),
    ];
    let storage = <DerivedList<u32> as ViewField>::define_many(&storage, &drafts);
    assert_eq!(storage.ids().to_vec(), vec![2, 1]);
    assert_eq!(storage.get(1), Some(&10));
    assert_eq!(storage.to_vec(), vec![20, 10]);
}

#[test]
fn keyed_list_storage_is_a_field_store() {
    fn assert_field_store<T: irrigation_store::delta::FieldStore>() {}
    assert_field_store::<DerivedListStorage<u32>>();
}

#[test]
fn keyed_list_delta_reports_identity_moves() {
    let storage = DerivedListStorage::from_keyed([(1u64, 10u32), (2, 20)]);
    let delta = KeyedListDelta::from_edits(vec![
        KeyedListEdit::Set { id: 1, value: 11 },
        KeyedListEdit::Set { id: 2, value: 20 },
        KeyedListEdit::Set { id: 3, value: 30 },
        KeyedListEdit::Remove { id: 9 },
    ]);
    let (storage, applied) = storage.apply_delta(&delta);
    assert_eq!(applied.changed, vec![1, 3]);
    assert!(applied.removed.is_empty());
    assert_eq!(storage.get(1), Some(&11));
    assert_eq!(storage.get(3), Some(&30));

    let delta = KeyedListDelta::from_edits(vec![KeyedListEdit::Remove { id: 1 }]);
    let (storage, applied) = storage.apply_delta(&delta);
    assert_eq!(applied.removed, vec![1]);
    assert_eq!(storage.get(1), None);
}

#[test]
fn keyed_storage_separates_order_from_payload_index() {
    let storage = DerivedListStorage::from_keyed((0..10_000u64).map(|id| (id, id * 2)));
    assert_eq!(storage.len(), 10_000);
    assert_eq!(storage.get(4_999), Some(&9_998));

    let updated = storage.with_entity(4_999, 7);
    assert_eq!(updated.get(4_999), Some(&7));
    assert_eq!(updated.ids().get(4_999), Some(&4_999));
    assert_eq!(updated.len(), 10_000);

    let removed = updated.without_entity(4_999);
    assert_eq!(removed.get(4_999), None);
    assert_eq!(removed.len(), 9_999);

    let reordered = removed.reorder(&[3u64, 1, 2].into_iter().collect());
    assert_eq!(reordered.ids().to_vec(), vec![3, 1, 2]);
    assert_eq!(reordered.to_vec(), vec![6, 2, 4]);
}

#[test]
fn derived_list_field_keyed_entities_keep_identity() {
    let mut storage = <DerivedList<u32> as ViewField>::empty();
    for (id, value) in [(2u64, 20u32), (1, 10)] {
        storage = <DerivedList<u32> as ViewField>::define(
            &storage,
            any_key(id).as_ref(),
            value_of(value),
        );
    }
    let key = UnitKey::field(ViewId(1), FieldId(0), UnitKind::Value);
    storage = <DerivedList<u32> as ViewField>::define(
        &storage,
        key.entity_key().map_or(&(), |key| key.as_ref()),
        value_of(DerivedListStorage::from_keyed([(2u64, 20u32), (1, 10)])),
    );
    assert_eq!(storage.to_vec(), vec![20, 10]);
    assert_eq!(storage.ids().to_vec(), vec![2, 1]);
    assert_eq!(storage.get(1), Some(&10));
    let structure = <DerivedList<u32> as ViewField>::structure(&storage).expect("structure");
    let ids = structure.value.downcast_ref::<PSeq<u64>>().expect("ids");
    assert_eq!(ids.to_vec(), vec![2, 1]);
}
