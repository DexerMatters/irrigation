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

#[cfg(test)]
#[path = "../tests/unit/change/tests.rs"]
mod tests;
