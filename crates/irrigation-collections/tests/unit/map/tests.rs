//! Unit tests for `src/map.rs`.

use super::*;
use irrigation_core::descriptor::{FieldDescriptor, UnitKey};

#[test]
fn map_field_installs_and_retracts_units() {
    let storage = <Map<u32, String> as ViewField>::empty();
    let key = UnitKey::entity(ViewId(1), FieldId(0), UnitKind::Value, any_key(7u32));
    let storage = <Map<u32, String> as ViewField>::define(
        &storage,
        key.entity_key().expect("entity").as_ref(),
        value_of(String::from("seven")),
    );
    assert_eq!(storage.get(&7), Some(&String::from("seven")));
    assert!(<Map<u32, String> as ViewField>::contains(
        &storage,
        key.entity_key().expect("entity").as_ref()
    ));
    let storage = <Map<u32, String> as ViewField>::retract(
        &storage,
        key.entity_key().expect("entity").as_ref(),
    );
    assert!(storage.is_empty());
}

#[test]
fn map_storage_tracks_the_key_projection_with_the_entries() {
    let storage = MapStorage::from_entries([(2u32, 20u32), (1, 10)]);
    assert_eq!(storage.len(), 2);
    assert_eq!(storage.keys().len(), 2);
    assert_eq!(storage.get(&1), Some(&10));

    let (storage, previous) = storage.insert(3, 30);
    assert_eq!(previous, None);
    assert_eq!(storage.keys().len(), 3);
    let (storage, previous) = storage.remove(&2);
    assert_eq!(previous, Some(20));
    assert_eq!(storage.keys().len(), 2);
    assert!(!storage.contains_key(&2));

    let structure = <Map<u32, u32> as ViewField>::structure(&storage).expect("structure");
    let keys = structure
        .value
        .downcast_ref::<PMap<u32, ()>>()
        .expect("key projection");
    assert_eq!(keys.len(), 2);
}

#[test]
fn map_only_payload_units_move_content() {
    let view = ViewId(1);
    let field = FieldId(0);
    let value = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
    let membership = UnitKey::entity(view, field, UnitKind::Membership, any_key(1u32));
    let structure = UnitKey::field(view, field, UnitKind::Structure);
    assert!(<Map<u32, u32> as ViewField>::unit_moves_content(&value));
    assert!(!<Map<u32, u32> as ViewField>::unit_moves_content(
        &membership
    ));
    assert!(!<Map<u32, u32> as ViewField>::unit_moves_content(
        &structure
    ));
}

#[test]
fn map_storage_is_a_field_store() {
    fn assert_field_store<T: irrigation_store::delta::FieldStore>() {}
    assert_field_store::<MapStorage<u32, u32>>();
}

#[test]
fn map_delta_reports_only_moved_keys() {
    let storage = MapStorage::from_entries([(1u32, 10u32), (2u32, 20u32)]);
    let delta = MapDelta::from_edits(vec![
        MapEdit::Update { key: 1, value: 11 },
        MapEdit::Update { key: 2, value: 20 },
        MapEdit::Insert { key: 3, value: 30 },
        MapEdit::Remove { key: 9 },
    ]);
    let (storage, applied) = storage.apply_delta(&delta);
    assert_eq!(applied.changed, vec![1, 3]);
    assert!(applied.removed.is_empty());
    assert!(applied.content_changed);
    assert_eq!(storage.get(&1), Some(&11));
    assert_eq!(storage.get(&3), Some(&30));

    let delta = MapDelta::from_edits(vec![MapEdit::Remove { key: 1 }, MapEdit::Remove { key: 1 }]);
    let (storage, applied) = storage.apply_delta(&delta);
    assert_eq!(applied.removed, vec![1]);
    assert!(!storage.contains_key(&1));
}

#[test]
fn map_define_many_applies_one_payload_batch() {
    let storage = <Map<u32, u32> as ViewField>::empty();
    let drafts = vec![
        (
            UnitKey::entity(ViewId(1), FieldId(0), UnitKind::Value, any_key(1u32)),
            value_of(10u32),
        ),
        (
            UnitKey::entity(ViewId(1), FieldId(0), UnitKind::Value, any_key(2u32)),
            value_of(20u32),
        ),
    ];
    let storage = <Map<u32, u32> as ViewField>::define_many(&storage, &drafts);
    assert_eq!(storage.len(), 2);
    assert_eq!(storage.get(&1), Some(&10));
    assert_eq!(storage.get(&2), Some(&20));
}

#[test]
fn map_descriptor_kind_is_stable() {
    assert_eq!(<Map<u32, u32> as ViewField>::KIND, FieldKind::Map);
    let _ = FieldDescriptor {
        name: "entries",
        kind: <Map<u32, u32> as ViewField>::KIND,
    };
}
