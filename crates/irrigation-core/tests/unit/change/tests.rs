//! Unit tests for `src/change.rs`.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Counter(u32);

#[derive(Clone, Debug, PartialEq, Eq)]
enum CounterDelta {
    Empty,
    Add(u32),
}

impl Delta for CounterDelta {
    type Target = Counter;

    fn empty() -> Self {
        Self::Empty
    }

    fn is_empty(&self) -> bool {
        matches!(self, Self::Empty) || matches!(self, Self::Add(0))
    }

    fn apply(&self, target: &Counter) -> Counter {
        match self {
            Self::Empty => target.clone(),
            Self::Add(amount) => Counter(target.0 + amount),
        }
    }

    fn compose(&self, later: &Self) -> Self {
        match (self, later) {
            (Self::Empty, other) => other.clone(),
            (other, Self::Empty) => other.clone(),
            (Self::Add(a), Self::Add(b)) => Self::Add(a + b),
        }
    }
}

#[test]
fn delta_laws_hold_for_a_trivial_change_structure() {
    let base = Counter(1);
    let d1 = CounterDelta::Add(2);
    let d2 = CounterDelta::Add(3);

    assert_eq!(d1.apply(&base), Counter(3));
    assert_eq!(
        d1.apply(&CounterDelta::empty().apply(&base)),
        d1.apply(&base)
    );
    assert_eq!(d2.apply(&d1.apply(&base)), d1.compose(&d2).apply(&base));
}
