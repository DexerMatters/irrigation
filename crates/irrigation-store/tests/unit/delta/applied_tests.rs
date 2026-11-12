//! Unit tests for `src/delta.rs`.

use super::*;

#[test]
fn applied_delta_iterates_unit_changes() {
    let applied = AppliedDelta {
        changed: vec![1u32, 2],
        removed: vec![3],
        content_changed: true,
    };
    let units: Vec<UnitChange<u32>> = applied.into_units().collect();
    assert_eq!(
        units,
        vec![
            UnitChange::Changed(1),
            UnitChange::Changed(2),
            UnitChange::Removed(3),
        ]
    );
}

#[test]
fn field_store_contract_is_implemented_for_persistent_families() {
    fn assert_field_store<T: FieldStore>() {}
    assert_field_store::<PMap<u32, u32>>();
    assert_field_store::<PSet<u32>>();
}

#[test]
fn map_delta_store_reports_exact_keys() {
    let base: PMap<u32, u32> = [(1, 10), (2, 20)].into_iter().collect();
    let delta = MapDelta::from_edits(vec![
        MapEdit::Update { key: 1, value: 11 },
        MapEdit::Update { key: 2, value: 20 },
        MapEdit::Insert { key: 3, value: 30 },
        MapEdit::Remove { key: 9 },
    ]);
    let (next, applied) = base.apply_delta(&delta);
    assert_eq!(applied.changed, vec![1, 3]);
    assert!(applied.removed.is_empty());
    assert_eq!(next.get(&1), Some(&11));
    assert_eq!(next.get(&3), Some(&30));
}

#[test]
fn set_delta_store_reports_membership_moves() {
    let base: PSet<u32> = [1, 2].into_iter().collect();
    let delta = SetDelta::from_edits(vec![
        SetEdit::Insert { value: 2 },
        SetEdit::Insert { value: 3 },
        SetEdit::Remove { value: 1 },
        SetEdit::Remove { value: 9 },
    ]);
    let (next, applied) = base.apply_delta(&delta);
    assert_eq!(applied.changed, vec![3]);
    assert_eq!(applied.removed, vec![1]);
    assert!(next.contains(&2));
    assert!(next.contains(&3));
    assert!(!next.contains(&1));
}
