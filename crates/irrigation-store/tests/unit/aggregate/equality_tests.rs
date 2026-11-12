//! Unit tests for `src/aggregate.rs`.

use super::*;

fn sum(sequence: &[u64]) -> AggSeq<u64, u64> {
    let mut agg = AggSeq::new(|item: &u64| *item, |a, b| a + b, 0);
    for item in sequence {
        agg = agg.push(*item);
    }
    agg
}

fn max(sequence: &[u64]) -> AggSeq<u64, u64> {
    let mut agg = AggSeq::new(|item: &u64| *item, |a, b| (*a).max(*b), 0);
    for item in sequence {
        agg = agg.push(*item);
    }
    agg
}

/// The same items under different reductions are not equal: their aggregate differs.
#[test]
fn a_different_reduction_is_not_equal() {
    let summed = sum(&[1, 2, 3]);
    let maximized = max(&[1, 2, 3]);
    assert_eq!(summed.aggregate(), 6);
    assert_eq!(maximized.aggregate(), 3);
    assert_ne!(
        summed, maximized,
        "equality may not ignore the aggregate they disagree on"
    );
}

#[test]
fn the_same_items_under_the_same_reduction_are_equal() {
    assert_eq!(sum(&[1, 2, 3]), sum(&[1, 2, 3]));
    assert_ne!(sum(&[1, 2, 3]), sum(&[1, 2]));
}
