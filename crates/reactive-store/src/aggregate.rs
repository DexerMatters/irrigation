//! Persistent sequences with cached lawful aggregates.
//!
//! A balanced sequence stores, at every node, the reduction of its subtree
//! under a declared associative operation. A point edit recomputes only the
//! ancestor path, so aggregate updates cost `O(log n)` combines rather than
//! `O(n)`.
//!
//! The operation must be lawful: an explicit identity and an associative
//! combine. The plan forbids silently reassociating floating-point addition or
//! an arbitrary user closure; the caller declares both pieces here.

use std::sync::Arc;

type Link<A, T> = Option<Arc<AggNode<A, T>>>;

#[derive(Debug)]
struct AggNode<A, T> {
    value: T,
    aggregate: A,
    left: Link<A, T>,
    right: Link<A, T>,
    height: u16,
    size: usize,
}

fn height<A, T>(link: &Link<A, T>) -> u16 {
    link.as_ref().map_or(0, |node| node.height)
}

fn size<A, T>(link: &Link<A, T>) -> usize {
    link.as_ref().map_or(0, |node| node.size)
}

/// A persistent sequence with a cached lawful reduction.
pub struct AggSeq<A, T> {
    root: Link<A, T>,
    len: usize,
    leaf: fn(&T) -> A,
    combine: fn(&A, &A) -> A,
    identity: A,
}

impl<A: Clone, T> Clone for AggSeq<A, T> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            len: self.len,
            leaf: self.leaf,
            combine: self.combine,
            identity: self.identity.clone(),
        }
    }
}

impl<A: std::fmt::Debug + Clone, T: std::fmt::Debug + Clone> std::fmt::Debug for AggSeq<A, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AggSeq")
            .field("len", &self.len)
            .field("aggregate", &self.aggregate())
            .finish_non_exhaustive()
    }
}

impl<A: Clone + PartialEq, T: Clone + PartialEq> PartialEq for AggSeq<A, T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<A: Clone + PartialEq + Eq, T: Clone + PartialEq + Eq> Eq for AggSeq<A, T> {}

impl<A: Clone, T: Clone> AggSeq<A, T> {
    /// Creates an empty sequence with the declared reduction.
    #[must_use]
    pub fn new(leaf: fn(&T) -> A, combine: fn(&A, &A) -> A, identity: A) -> Self {
        Self {
            root: None,
            len: 0,
            leaf,
            combine,
            identity,
        }
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` when the sequence is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The reduction of the whole sequence.
    #[must_use]
    pub fn aggregate(&self) -> A {
        self.aggregate_of(&self.root)
    }

    fn aggregate_of(&self, link: &Link<A, T>) -> A {
        match link {
            Some(node) => node.aggregate.clone(),
            None => self.identity.clone(),
        }
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "`Link` is the AVL's option-typed child pointer; building a node always yields one"
    )]
    fn make(&self, value: T, left: Link<A, T>, right: Link<A, T>) -> Link<A, T> {
        let aggregate = (self.combine)(
            &(self.combine)(&self.aggregate_of(&left), &(self.leaf)(&value)),
            &self.aggregate_of(&right),
        );
        Some(Arc::new(AggNode {
            value,
            aggregate,
            height: 1 + height(&left).max(height(&right)),
            size: 1 + size(&left) + size(&right),
            left,
            right,
        }))
    }

    fn rebalance(&self, node: Arc<AggNode<A, T>>) -> Link<A, T> {
        let balance = i32::from(height(&node.left)) - i32::from(height(&node.right));
        if balance > 1 {
            let left = node.left.as_ref().expect("left child");
            if height(&left.left) < height(&left.right) {
                let new_left = self.rotate_left(Arc::clone(left));
                let rebuilt = self
                    .make(node.value.clone(), new_left, node.right.clone())
                    .expect("rebuilt node");
                return self.rotate_right(rebuilt);
            }
            self.rotate_right(node)
        } else if balance < -1 {
            let right = node.right.as_ref().expect("right child");
            if height(&right.right) < height(&right.left) {
                let new_right = self.rotate_right(Arc::clone(right));
                let rebuilt = self
                    .make(node.value.clone(), node.left.clone(), new_right)
                    .expect("rebuilt node");
                return self.rotate_left(rebuilt);
            }
            self.rotate_left(node)
        } else {
            Some(node)
        }
    }

    fn rotate_right(&self, node: Arc<AggNode<A, T>>) -> Link<A, T> {
        let left = node.left.as_ref().expect("left child");
        self.make(
            left.value.clone(),
            left.left.clone(),
            self.make(node.value.clone(), left.right.clone(), node.right.clone()),
        )
    }

    fn rotate_left(&self, node: Arc<AggNode<A, T>>) -> Link<A, T> {
        let right = node.right.as_ref().expect("right child");
        self.make(
            right.value.clone(),
            self.make(node.value.clone(), node.left.clone(), right.left.clone()),
            right.right.clone(),
        )
    }
}

impl<A: Clone, T: Clone> AggSeq<A, T> {
    /// Returns the element at `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&T> {
        let mut current = self.root.as_deref();
        let mut index = index;
        while let Some(node) = current {
            let left = size(&node.left);
            match index.cmp(&left) {
                std::cmp::Ordering::Less => current = node.left.as_deref(),
                std::cmp::Ordering::Greater => {
                    index -= left + 1;
                    current = node.right.as_deref();
                }
                std::cmp::Ordering::Equal => return Some(&node.value),
            }
        }
        None
    }

    /// Iterates in order.
    pub fn iter(&self) -> AggIter<'_, A, T> {
        let mut stack = Vec::new();
        let mut current = self.root.as_deref();
        while let Some(node) = current {
            stack.push(node);
            current = node.left.as_deref();
        }
        AggIter { stack }
    }

    /// Collects the sequence into a vector.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        self.iter().cloned().collect()
    }

    /// Inserts at `index`, recomputing the ancestor path only.
    ///
    /// # Panics
    ///
    /// Panics when `index > len`.
    #[must_use]
    pub fn insert(&self, index: usize, value: T) -> Self {
        assert!(index <= self.len, "insert index {index} out of bounds");
        Self {
            root: self.insert_at(self.root.clone(), index, value),
            len: self.len + 1,
            leaf: self.leaf,
            combine: self.combine,
            identity: self.identity.clone(),
        }
    }

    fn insert_at(&self, link: Link<A, T>, index: usize, value: T) -> Link<A, T> {
        match link {
            None => self.make(value, None, None),
            Some(node) => {
                let left_size = size(&node.left);
                if index <= left_size {
                    let left = self.insert_at(node.left.clone(), index, value);
                    let rebuilt = self
                        .make(node.value.clone(), left, node.right.clone())
                        .expect("rebuilt node");
                    self.rebalance(rebuilt)
                } else {
                    let right = self.insert_at(node.right.clone(), index - left_size - 1, value);
                    let rebuilt = self
                        .make(node.value.clone(), node.left.clone(), right)
                        .expect("rebuilt node");
                    self.rebalance(rebuilt)
                }
            }
        }
    }

    /// Removes and returns the element at `index`.
    #[must_use]
    pub fn remove(&self, index: usize) -> (Self, Option<T>) {
        if index >= self.len {
            return (self.clone(), None);
        }
        let (root, removed) = self.remove_at(self.root.clone(), index);
        (
            Self {
                root,
                len: self.len - 1,
                leaf: self.leaf,
                combine: self.combine,
                identity: self.identity.clone(),
            },
            removed,
        )
    }

    fn remove_at(&self, link: Link<A, T>, index: usize) -> (Link<A, T>, Option<T>) {
        match link {
            None => (None, None),
            Some(node) => {
                let left_size = size(&node.left);
                match index.cmp(&left_size) {
                    std::cmp::Ordering::Less => {
                        let (left, removed) = self.remove_at(node.left.clone(), index);
                        let rebuilt = self
                            .make(node.value.clone(), left, node.right.clone())
                            .expect("rebuilt node");
                        (self.rebalance(rebuilt), removed)
                    }
                    std::cmp::Ordering::Greater => {
                        let (right, removed) =
                            self.remove_at(node.right.clone(), index - left_size - 1);
                        let rebuilt = self
                            .make(node.value.clone(), node.left.clone(), right)
                            .expect("rebuilt node");
                        (self.rebalance(rebuilt), removed)
                    }
                    std::cmp::Ordering::Equal => {
                        let removed = node.value.clone();
                        match (node.left.clone(), node.right.clone()) {
                            (None, right) => (right, Some(removed)),
                            (left, None) => (left, Some(removed)),
                            (left, Some(right)) => {
                                let (new_right, successor) = self.remove_first(right);
                                (
                                    self.make(successor.value.clone(), left, new_right),
                                    Some(removed),
                                )
                            }
                        }
                    }
                }
            }
        }
    }

    fn remove_first(&self, node: Arc<AggNode<A, T>>) -> (Link<A, T>, Arc<AggNode<A, T>>) {
        match node.left.clone() {
            None => (node.right.clone(), node),
            Some(left) => {
                let (new_left, first) = self.remove_first(left);
                let rebuilt = self
                    .make(node.value.clone(), new_left, node.right.clone())
                    .expect("rebuilt node");
                (self.rebalance(rebuilt), first)
            }
        }
    }

    /// Replaces the element at `index`, recomputing the ancestor path only.
    #[must_use]
    pub fn set(&self, index: usize, value: T) -> Self {
        assert!(index < self.len, "set index {index} out of bounds");
        Self {
            root: self.set_at(self.root.clone(), index, value),
            len: self.len,
            leaf: self.leaf,
            combine: self.combine,
            identity: self.identity.clone(),
        }
    }

    fn set_at(&self, link: Link<A, T>, index: usize, value: T) -> Link<A, T> {
        let node = link.expect("set requires an existing node");
        let left_size = size(&node.left);
        match index.cmp(&left_size) {
            std::cmp::Ordering::Less => self.make(
                node.value.clone(),
                self.set_at(node.left.clone(), index, value),
                node.right.clone(),
            ),
            std::cmp::Ordering::Greater => self.make(
                node.value.clone(),
                node.left.clone(),
                self.set_at(node.right.clone(), index - left_size - 1, value),
            ),
            std::cmp::Ordering::Equal => self.make(value, node.left.clone(), node.right.clone()),
        }
    }

    /// Appends an element.
    #[must_use]
    pub fn push(&self, value: T) -> Self {
        self.insert(self.len, value)
    }
}

/// In-order iterator over an [`AggSeq`].
pub struct AggIter<'a, A, T> {
    stack: Vec<&'a AggNode<A, T>>,
}

impl<A, T> std::fmt::Debug for AggIter<'_, A, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AggIter").finish_non_exhaustive()
    }
}

impl<'a, A, T> Iterator for AggIter<'a, A, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        let mut current = node.right.as_deref();
        while let Some(node) = current {
            self.stack.push(node);
            current = node.left.as_deref();
        }
        Some(&node.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COMBINES: AtomicU64 = AtomicU64::new(0);

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
}
