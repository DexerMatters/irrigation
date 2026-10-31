//! Verification support: deterministic generators, oracle comparison, and
//! execution accounting.
//!
//! The plan requires randomized edit sequences checked against a separate
//! from-scratch evaluator. This crate supplies the pieces that are shared by
//! such tests without pulling test-only dependencies into the runtime.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

use reactive_runtime::{Runtime, RuntimeStats};

/// A deterministic 64-bit linear congruential generator.
///
/// Reproducibility matters more than statistical quality here: a failing
/// transaction sequence must be replayable from its seed.
#[derive(Clone, Debug)]
pub struct Lcg(u64);

impl Lcg {
    /// Creates a generator from a seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Returns the next 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    /// Returns a value below `bound` (`0` when `bound` is zero).
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() as usize) % bound
        }
    }

    /// Returns a value in `low..high` (`low` when the range is empty).
    pub fn range(&mut self, low: u64, high: u64) -> u64 {
        if high <= low {
            low
        } else {
            low + self.next_u64() % (high - low)
        }
    }

    /// Returns `true` with probability `1 / n`.
    pub fn one_in(&mut self, n: u64) -> bool {
        n > 0 && self.next_u64() % n == 0
    }
}

/// A from-scratch evaluator used as the semantic oracle.
pub trait Oracle {
    /// The value the runtime must agree with.
    type Output: PartialEq + std::fmt::Debug;

    /// Evaluates the model from scratch.
    fn evaluate(&self) -> Self::Output;
}

/// Counters observed by one transaction, separated from lifetime totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatsDelta {
    /// Node executions caused by the transaction and the following read.
    pub executions: u64,
    /// Successful trace validations.
    pub validations: u64,
    /// Revisions published.
    pub revisions: u64,
}

/// Captures a baseline so a test can measure one transaction.
#[derive(Clone, Debug)]
pub struct StatsProbe {
    baseline: RuntimeStats,
}

impl StatsProbe {
    /// Starts measuring at the runtime's current counters.
    #[must_use]
    pub fn start(runtime: &Runtime) -> Self {
        Self {
            baseline: runtime.stats(),
        }
    }

    /// Returns the counters accumulated since [`StatsProbe::start`].
    #[must_use]
    pub fn delta(&self, runtime: &Runtime) -> StatsDelta {
        let current = runtime.stats();
        StatsDelta {
            executions: current.executions.saturating_sub(self.baseline.executions),
            validations: current.validations.saturating_sub(self.baseline.validations),
            revisions: current.revisions.saturating_sub(self.baseline.revisions),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcg_is_reproducible() {
        let mut first = Lcg::new(7);
        let mut second = Lcg::new(7);
        for _ in 0..16 {
            assert_eq!(first.next_u64(), second.next_u64());
        }
    }

    #[test]
    fn bounded_draws_stay_in_range() {
        let mut rng = Lcg::new(1);
        for _ in 0..100 {
            assert!(rng.below(10) < 10);
            let value = rng.range(5, 9);
            assert!((5..9).contains(&value));
        }
        assert_eq!(rng.below(0), 0);
    }
}
