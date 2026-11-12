//! Unit tests for `src/delta.rs`.

use super::*;

#[test]
fn list_delta_composition_matches_sequential_application() {
    let base: PSeq<u32> = [1, 2, 3].into_iter().collect();
    let first = ListDelta::from_edits(vec![
        ListEdit::Insert { index: 0, value: 0 },
        ListEdit::Replace {
            index: 2,
            value: 20,
        },
    ]);
    let second = ListDelta::from_edits(vec![ListEdit::Move { from: 0, to: 3 }]);

    let sequential = second.apply(&first.apply(&base));
    let composed = first.compose(&second).apply(&base);
    assert_eq!(sequential, composed);
    assert_eq!(sequential.to_vec(), vec![1, 20, 3, 0]);
}

#[test]
fn empty_delta_is_identity() {
    let base: PMap<u32, u32> = [(1, 1)].into_iter().collect();
    let delta: MapDelta<u32, u32> = MapDelta::new();
    assert!(delta.is_empty());
    assert_eq!(delta.apply(&base), base);
}

#[test]
fn set_delta_insert_then_remove_is_observably_empty() {
    let base: PSet<u32> = [1, 2].into_iter().collect();
    let delta = SetDelta::from_edits(vec![
        SetEdit::Insert { value: 9 },
        SetEdit::Remove { value: 9 },
    ]);
    assert_eq!(delta.apply(&base), base);
}
