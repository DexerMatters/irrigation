//! Change contracts for Views and incremental operators.
//!
//! A View supplies a valid-edit type `ΔV`, an application operation, and
//! normalization. The laws the plan requires are:
//!
//! ```text
//! apply(v, empty) = v
//! apply(apply(v, d1), d2) = apply(v, compose(d1, d2))
//! apply(v, normalize(v, d)) = apply(v, d)
//! ```
//!
//! Composition is ordered: tree edits and list splices are not assumed
//! commutative or invertible.

/// A lawful, composable change to a value of type [`Delta::Target`].
pub trait Delta: Clone + Send + Sync + 'static {
    /// The state this delta applies to.
    type Target: Clone + Send + Sync + 'static;

    /// The identity delta.
    fn empty() -> Self;

    /// Returns `true` when applying this delta cannot change any observation.
    fn is_empty(&self) -> bool;

    /// Applies the delta to a base state.
    fn apply(&self, target: &Self::Target) -> Self::Target;

    /// Composes two sequential deltas: `self` first, then `later`.
    fn compose(&self, later: &Self) -> Self;
}

/// Normalizes a delta against a concrete base state.
///
/// Implementations drop operations that cannot change the final value. The
/// default keeps the delta unchanged, which is always lawful.
pub fn normalize<D: Delta>(delta: &D, _base: &D::Target) -> D {
    delta.clone()
}

#[cfg(test)]
mod tests {
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
        assert_eq!(d1.apply(&CounterDelta::empty().apply(&base)), d1.apply(&base));
        assert_eq!(
            d2.apply(&d1.apply(&base)),
            d1.compose(&d2).apply(&base)
        );
        assert_eq!(normalize(&d1, &base).apply(&base), d1.apply(&base));
    }
}
