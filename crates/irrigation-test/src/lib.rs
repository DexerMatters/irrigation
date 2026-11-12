//! Verification support: deterministic generators, oracle comparison, and
//! execution accounting.
//!
//! The plan requires randomized edit sequences checked against a separate
//! from-scratch evaluator. This crate supplies the pieces that are shared by
//! such tests without pulling test-only dependencies into the runtime.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

use irrigation_runtime::{Runtime, RuntimeStats};

/// A deterministic seeded generator backed by `rand::rngs::StdRng`.
///
/// Reproducibility matters more than statistical quality here: a failing
/// transaction sequence must be replayable from its seed.
#[derive(Clone, Debug)]
pub struct TestRng {
    rng: rand::rngs::StdRng,
}

impl TestRng {
    /// Creates a generator from a seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        use rand::SeedableRng;
        Self {
            rng: rand::rngs::StdRng::seed_from_u64(seed),
        }
    }

    /// Returns the next 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        use rand::Rng;
        self.rng.random()
    }

    /// Returns a value below `bound` (`0` when `bound` is zero).
    pub fn below(&mut self, bound: usize) -> usize {
        use rand::Rng;
        if bound == 0 {
            0
        } else {
            self.rng.random_range(0..bound)
        }
    }

    /// Returns a value in `low..high` (`low` when the range is empty).
    pub fn range(&mut self, low: u64, high: u64) -> u64 {
        use rand::Rng;
        if high <= low {
            low
        } else {
            self.rng.random_range(low..high)
        }
    }

    /// Returns `true` with probability `1 / n`.
    pub fn one_in(&mut self, n: u64) -> bool {
        use rand::Rng;
        n > 0 && self.rng.random_range(0..n) == 0
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
            validations: current
                .validations
                .saturating_sub(self.baseline.validations),
            revisions: current.revisions.saturating_sub(self.baseline.revisions),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/lib/tests.rs"]
mod tests;
