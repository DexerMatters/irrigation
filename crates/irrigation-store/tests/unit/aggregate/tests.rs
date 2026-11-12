//! Unit tests for `src/aggregate.rs`.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static COMBINES: AtomicU64 = AtomicU64::new(0);

/// Walks the tree asserting the AVL invariant at every node, and returns
/// the subtree's `(height, size)` so the caller can check its own node.
fn assert_balanced<A, T>(link: &Link<A, T>) -> (u16, usize) {
    let Some(node) = link else {
        return (0, 0);
    };
    let (left_height, left_size) = assert_balanced(&node.left);
    let (right_height, right_size) = assert_balanced(&node.right);
    let balance = i32::from(left_height) - i32::from(right_height);
    assert!(
        balance.abs() <= 1,
        "node is unbalanced: {left_height} vs {right_height}"
    );
    assert_eq!(
        node.height,
        1 + left_height.max(right_height),
        "stored height does not match the subtree"
    );
    assert_eq!(
        node.size,
        1 + left_size + right_size,
        "stored size does not match the subtree"
    );
    (node.height, node.size)
}

fn assert_sequence_invariant(seq: &AggSeq<u64, u64>, model: &[u64]) {
    let (_, size) = assert_balanced(&seq.root);
    assert_eq!(size, model.len(), "tree size");
    assert_eq!(seq.len(), model.len(), "sequence length");
    assert_eq!(seq.to_vec(), model, "in-order contents");
    assert_eq!(seq.aggregate(), model.iter().sum::<u64>(), "aggregate");
}

/// Deterministic churn: a small LCG keeps the mix reproducible.
struct Churn(u64);

impl Churn {
    fn below(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % bound.max(1)
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "`AggSeq::new` takes `fn(&T) -> A` and `fn(&A, &A) -> A`"
)]
fn leaf(value: &u64) -> u64 {
    *value
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "`AggSeq::new` takes `fn(&A, &A) -> A`"
)]
fn add(left: &u64, right: &u64) -> u64 {
    COMBINES.fetch_add(1, Ordering::Relaxed);
    left + right
}

fn sum_seq(values: impl IntoIterator<Item = u64>) -> AggSeq<u64, u64> {
    let mut seq = AggSeq::new(leaf, add, 0);
    for value in values {
        seq = seq.push(value);
    }
    seq
}

#[test]
fn aggregate_tracks_insertions_and_removals() {
    let mut seq = sum_seq(0..100);
    assert_eq!(seq.aggregate(), 4950);
    seq = seq.insert(0, 1000);
    assert_eq!(seq.aggregate(), 5950);
    let (seq, removed) = seq.remove(0);
    assert_eq!(removed, Some(1000));
    assert_eq!(seq.aggregate(), 4950);
    let seq = seq.set(50, 500);
    assert_eq!(seq.aggregate(), 4950 - 50 + 500);
    assert_eq!(seq.get(50), Some(&500));
    assert_eq!(seq.len(), 100);
}

#[test]
fn point_edits_recombine_only_a_logarithmic_path() {
    let size = 16_384usize;
    let mut seq = sum_seq(0..size as u64);
    let expected: u64 = (0..size as u64).sum();
    assert_eq!(seq.aggregate(), expected);

    COMBINES.store(0, Ordering::Relaxed);
    seq = seq.set(size / 2, 1);
    let combines = COMBINES.load(Ordering::Relaxed);
    assert!(
        combines <= 64,
        "a point edit performed {combines} combines at size {size}"
    );
    assert!(combines > 0);

    COMBINES.store(0, Ordering::Relaxed);
    seq = seq.insert(size / 2, 7);
    let combines = COMBINES.load(Ordering::Relaxed);
    assert!(
        combines <= 96,
        "an insertion performed {combines} combines at size {size}"
    );
    assert_eq!(seq.len(), size + 1);
    assert_eq!(seq.aggregate(), expected - (size as u64 / 2) + 1 + 7);
}

#[test]
fn sequence_order_is_preserved() {
    let seq: AggSeq<u64, u64> = {
        let mut seq = AggSeq::new(leaf, add, 0);
        for value in [5, 1, 4, 2, 3] {
            seq = seq.push(value);
        }
        seq
    };
    assert_eq!(seq.to_vec(), vec![5, 1, 4, 2, 3]);
    assert_eq!(seq.aggregate(), 15);
    let moved = seq.remove(0).0;
    assert_eq!(moved.to_vec(), vec![1, 4, 2, 3]);
    assert_eq!(moved.aggregate(), 10);
}

#[test]
fn deletion_keeps_the_avl_invariant() {
    // The two-child case replaces the removed node with its in-order
    // successor; that rebuild has to be rebalanced like every other path.
    let base = sum_seq(0..5);
    assert_sequence_invariant(&base, &[0, 1, 2, 3, 4]);
    for first in 0..5 {
        for second in 0..4 {
            let removed = base.remove(first).0;
            let mut model = vec![0, 1, 2, 3, 4];
            model.remove(first);
            let (shrunk, found) = removed.remove(second);
            assert_eq!(found, Some(model[second]));
            model.remove(second);
            assert_sequence_invariant(&shrunk, &model);
        }
    }
}

#[test]
fn random_churn_keeps_the_avl_invariant() {
    let mut churn = Churn(0x5eed);
    let mut seq = AggSeq::new(leaf, add, 0);
    for _ in 0..512 {
        let mut model = seq.to_vec();
        match churn.below(4) {
            0 => {
                let value = churn.below(1000) as u64;
                seq = seq.push(value);
                model.push(value);
            }
            1 if !model.is_empty() => {
                let index = churn.below(model.len());
                let value = churn.below(1000) as u64;
                seq = seq.insert(index, value);
                model.insert(index, value);
            }
            2 if !model.is_empty() => {
                let index = churn.below(model.len());
                let (next, removed) = seq.remove(index);
                assert_eq!(removed, Some(model.remove(index)));
                seq = next;
            }
            _ if !model.is_empty() => {
                let index = churn.below(model.len());
                let value = churn.below(1000) as u64;
                seq = seq.set(index, value);
                model[index] = value;
            }
            _ => {}
        }
        assert_sequence_invariant(&seq, &model);
    }
}
