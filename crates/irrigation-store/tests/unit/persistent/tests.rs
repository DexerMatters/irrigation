//! Unit tests for `src/persistent.rs`.

use super::*;

#[test]
fn map_operations_preserve_order_and_length() {
    let map = PMap::new();
    let (map, previous) = map.insert(3, "c");
    assert_eq!(previous, None);
    let (map, _) = map.insert(1, "a");
    let (map, _) = map.insert(2, "b");
    assert_eq!(map.len(), 3);
    assert_eq!(map.get(&2), Some(&"b"));
    assert_eq!(
        map.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );

    let (map, removed) = map.remove(&2);
    assert_eq!(removed, Some("b"));
    assert_eq!(map.len(), 2);
    assert_eq!(map.get(&2), None);
    assert_eq!(map.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![1, 3]);
}

#[test]
fn map_shares_structure_with_previous_version() {
    let base: PMap<u32, u32> = (0..64).map(|i| (i, i)).collect();
    let (edited, _) = base.insert(10, 999);
    assert_eq!(base.get(&10), Some(&10));
    assert_eq!(edited.get(&10), Some(&999));
    assert_eq!(base.len(), edited.len());
}

#[test]
fn map_insert_if_changed_reuses_equal_values() {
    let map: PMap<u32, u32> = [(1, 1)].into_iter().collect();
    let (same, changed) = map.insert_if_changed(1, 1);
    assert!(!changed);
    assert_eq!(same.get(&1), Some(&1));
    let (different, changed) = map.insert_if_changed(1, 2);
    assert!(changed);
    assert_eq!(different.get(&1), Some(&2));
}

#[test]
fn sequence_supports_insert_remove_move_and_indexing() {
    let seq: PSeq<u32> = [1, 2, 3, 4].into_iter().collect();
    assert_eq!(seq.len(), 4);
    assert_eq!(seq.get(2), Some(&3));

    let inserted = seq.insert(0, 0);
    assert_eq!(inserted.to_vec(), vec![0, 1, 2, 3, 4]);
    assert_eq!(seq.to_vec(), vec![1, 2, 3, 4]);

    let (removed, value) = inserted.remove(0);
    assert_eq!(value, Some(0));
    assert_eq!(removed.to_vec(), vec![1, 2, 3, 4]);

    let moved = removed.move_element(0, 3);
    assert_eq!(moved.to_vec(), vec![2, 3, 4, 1]);
}

#[test]
fn sequence_handles_large_sizes() {
    let seq: PSeq<u32> = (0..1000).collect();
    assert_eq!(seq.len(), 1000);
    for index in [0, 1, 499, 500, 998, 999] {
        assert_eq!(seq.get(index), Some(&(index as u32)));
    }
    let inserted = seq.insert(500, 10_000);
    assert_eq!(inserted.get(500), Some(&10_000));
    assert_eq!(inserted.get(501), Some(&500));
    assert_eq!(inserted.len(), 1001);
}

#[test]
fn set_tracks_membership() {
    let set: PSet<u32> = [3, 1, 2, 1].into_iter().collect();
    assert_eq!(set.len(), 3);
    assert!(set.contains(&2));
    assert_eq!(set.iter().copied().collect::<Vec<_>>(), vec![1, 2, 3]);
    let (removed, was_present) = set.remove(&2);
    assert!(was_present);
    assert!(!removed.contains(&2));
    assert_eq!(removed.len(), 2);
}
