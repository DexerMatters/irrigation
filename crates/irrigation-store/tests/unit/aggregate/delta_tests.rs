//! Unit tests for `src/aggregate.rs`.

use super::*;

fn sum() -> AggSeq<u64, u64> {
    AggSeq::new(|item| *item, |left, right| *left + *right, 0)
}

#[test]
fn reduced_delta_reports_positional_moves() {
    let sequence = sum().push(1).push(2);
    let delta = ReducedDelta::from_edits(vec![
        ReducedEdit::Set { index: 0, item: 9 },
        ReducedEdit::Insert { index: 1, item: 5 },
        ReducedEdit::Remove { index: 2 },
    ]);
    let (sequence, applied) = sequence.apply_delta(&delta);
    assert_eq!(sequence.to_vec(), vec![9, 5]);
    assert!(applied.content_changed);
    assert_eq!(applied.changed, vec![0, 1]);
    assert_eq!(applied.removed, vec![2]);
}

#[test]
fn reduced_sequence_is_a_field_store() {
    fn assert_field_store<T: crate::delta::FieldStore>() {}
    assert_field_store::<AggSeq<u64, u64>>();
}
