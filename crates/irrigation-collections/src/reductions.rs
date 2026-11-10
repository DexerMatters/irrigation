//! Lawful reductions for the common algebras.
//!
//! A reduction states its identity and an associative combine; the runtime
//! caches the aggregate. `Sum` is restricted to integers because IEEE addition
//! is not associative and the cutoff contract must not be weakened by a
//! convenience reduction.

use std::collections::BTreeSet;
use std::marker::PhantomData;
use std::ops::Add;

use crate::reduce::Reduction;

/// Counts elements of `T`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Count<T>(PhantomData<fn() -> T>);

impl<T: Clone + PartialEq + Send + Sync + 'static> Reduction for Count<T> {
    type Item = T;
    type Aggregate = usize;

    fn leaf(_item: &Self::Item) -> Self::Aggregate {
        1
    }

    fn combine(left: &Self::Aggregate, right: &Self::Aggregate) -> Self::Aggregate {
        left + right
    }

    fn identity() -> Self::Aggregate {
        0
    }
}

/// Adds integer elements of `T`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sum<T>(PhantomData<fn() -> T>);

macro_rules! integer_sum {
    ($($ty:ty),* $(,)?) => {
        $(
            impl Reduction for Sum<$ty> {
                type Item = $ty;
                type Aggregate = $ty;

                fn leaf(item: &Self::Item) -> Self::Aggregate {
                    *item
                }

                fn combine(left: &Self::Aggregate, right: &Self::Aggregate) -> Self::Aggregate {
                    left.add(*right)
                }

                fn identity() -> Self::Aggregate {
                    0
                }
            }
        )*
    };
}

integer_sum!(
    u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize
);

/// Keeps the largest element of `T`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Max<T>(PhantomData<fn() -> T>);

impl<T: Ord + Clone + Send + Sync + 'static> Reduction for Max<T> {
    type Item = T;
    type Aggregate = Option<T>;

    fn leaf(item: &Self::Item) -> Self::Aggregate {
        Some(item.clone())
    }

    fn combine(left: &Self::Aggregate, right: &Self::Aggregate) -> Self::Aggregate {
        match (left, right) {
            (Some(left), Some(right)) => Some(left.clone().max(right.clone())),
            (Some(value), None) | (None, Some(value)) => Some(value.clone()),
            (None, None) => None,
        }
    }

    fn identity() -> Self::Aggregate {
        None
    }
}

/// Keeps the smallest element of `T`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Min<T>(PhantomData<fn() -> T>);

impl<T: Ord + Clone + Send + Sync + 'static> Reduction for Min<T> {
    type Item = T;
    type Aggregate = Option<T>;

    fn leaf(item: &Self::Item) -> Self::Aggregate {
        Some(item.clone())
    }

    fn combine(left: &Self::Aggregate, right: &Self::Aggregate) -> Self::Aggregate {
        match (left, right) {
            (Some(left), Some(right)) => Some(left.clone().min(right.clone())),
            (Some(value), None) | (None, Some(value)) => Some(value.clone()),
            (None, None) => None,
        }
    }

    fn identity() -> Self::Aggregate {
        None
    }
}

/// Collects elements of `T` into an ordered set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Union<T>(PhantomData<fn() -> T>);

impl<T: Ord + Clone + Send + Sync + 'static> Reduction for Union<T> {
    type Item = T;
    type Aggregate = BTreeSet<T>;

    fn leaf(item: &Self::Item) -> Self::Aggregate {
        std::iter::once(item.clone()).collect()
    }

    fn combine(left: &Self::Aggregate, right: &Self::Aggregate) -> Self::Aggregate {
        let mut union = left.clone();
        union.extend(right.iter().cloned());
        union
    }

    fn identity() -> Self::Aggregate {
        BTreeSet::new()
    }
}
