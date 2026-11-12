//! Declared fixed points.
//!
//! A value cycle such as `x = !x` cannot be repaired by scheduling, and
//! `x = x` has many solutions. The runtime therefore requires an explicit
//! fixed-point family with four declared pieces:
//!
//! * a domain: partially ordered values with a bottom and a join;
//! * a monotone transfer function;
//! * a termination argument (a finite-height domain, or a checked budget);
//! * a meaning: the least fixed point under the declared order.
//!
//! The solver is a worklist over affected equations. It is invoked from inside
//! an ordinary computation, so the inputs it reads are tracked and a change
//! recomputes the region from scratch. That reset policy is the conservative
//! deletion answer the plan requires for a general lattice: a removed fact can
//! never survive on stale support.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use irrigation_core::error::{ComputeError, Result};

/// A partially ordered domain with a least element and a least upper bound.
pub trait Lattice: Clone + PartialEq + Send + Sync + 'static {
    /// The least element of the domain.
    fn bottom() -> Self;

    /// Joins `other` into `self`, reporting whether the value grew.
    fn join(&mut self, other: &Self) -> bool;
}

impl Lattice for bool {
    fn bottom() -> Self {
        false
    }

    fn join(&mut self, other: &Self) -> bool {
        let changed = !*self && *other;
        *self |= *other;
        changed
    }
}

impl<T> Lattice for BTreeSet<T>
where
    T: Ord + Clone + Send + Sync + 'static,
{
    fn bottom() -> Self {
        BTreeSet::new()
    }

    fn join(&mut self, other: &Self) -> bool {
        let before = self.len();
        self.extend(other.iter().cloned());
        self.len() != before
    }
}

/// A monotone system of equations over a [`Lattice`].
///
/// Implementations are ordinary data types built from tracked reads; the
/// transfer function is a trait method, not a closure, so the lowering macros
/// never see it and it cannot accidentally capture untracked state.
pub trait FixedPointProblem {
    /// Equation identity.
    type Key: Ord + Clone + Send + Sync + 'static;
    /// Equation value.
    type Value: Lattice;

    /// Facts that hold before any iteration.
    fn seeds(&self) -> Vec<(Self::Key, Self::Value)>;

    /// Contributions implied by one fact.
    fn transfer(&self, key: &Self::Key, value: &Self::Value) -> Vec<(Self::Key, Self::Value)>;
}

/// The result of a fixed-point computation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedPoint<K: Ord, V> {
    values: BTreeMap<K, V>,
    iterations: usize,
    contributions: usize,
    sccs: Vec<Vec<K>>,
    self_loops: BTreeSet<K>,
    dependents: BTreeMap<K, BTreeSet<K>>,
}

impl<K: Ord, V> FixedPoint<K, V> {
    /// The least fixed point.
    #[must_use]
    pub fn values(&self) -> &BTreeMap<K, V> {
        &self.values
    }

    /// Number of worklist rounds used, for diagnostics and benchmarks.
    #[must_use]
    pub fn iterations(&self) -> usize {
        self.iterations
    }

    /// Number of transfer contributions processed.
    ///
    /// A solve always restarts from the declared seeds (the conservative reset
    /// policy); this count is the reset's cost.
    #[must_use]
    pub fn contributions(&self) -> usize {
        self.contributions
    }

    /// Strongly connected components of the dependency graph, sorted.
    #[must_use]
    pub fn sccs(&self) -> &[Vec<K>] {
        &self.sccs
    }

    /// Keys one fact contributes to, from the last solve.
    #[must_use]
    pub fn dependents(&self, key: &K) -> Option<&BTreeSet<K>> {
        self.dependents.get(key)
    }

    /// Components that contain a cycle (more than one key or a self-loop).
    ///
    /// A reset of one of these components is the worst case the plan asks to
    /// report: everything in the component may have to be recomputed.
    #[must_use]
    pub fn recursive_components(&self) -> Vec<&[K]> {
        self.sccs
            .iter()
            .filter(|component| {
                component.len() > 1
                    || (component.len() == 1 && self.self_loops.contains(&component[0]))
            })
            .map(Vec::as_slice)
            .collect()
    }

    /// Keys whose value is above bottom, in order.
    #[must_use]
    pub fn non_bottom(&self) -> Vec<K>
    where
        K: Clone,
        V: PartialEq + Lattice,
    {
        let bottom = V::bottom();
        self.values
            .iter()
            .filter(|(_, value)| **value != bottom)
            .map(|(key, _)| key.clone())
            .collect()
    }
}

/// Default worklist budget. A domain of finite height terminates well before
/// this; the budget turns a non-terminating user order into an error instead of
/// a hang.
pub const DEFAULT_BUDGET: usize = 1_000_000;

/// Runs the worklist to exhaustion, recording the edges it walked.
///
/// This is the solve loop: pop a key, transfer it, join each contribution, enqueue
/// whatever moved, and count iterations and contributions against the budget. A
/// from-scratch solve and an add-only extension both need exactly this, and both had
/// their own copy -- so a change to the convergence rule could reach one and not the other.
fn drain<P: FixedPointProblem>(
    problem: &P,
    values: &mut BTreeMap<P::Key, P::Value>,
    queue: &mut std::collections::VecDeque<P::Key>,
    budget: usize,
    edges: &mut Vec<(P::Key, P::Key)>,
) -> Result<(usize, usize)> {
    let mut iterations = 0;
    let mut work = 0usize;
    while let Some(key) = queue.pop_front() {
        iterations += 1;
        let value = values.get(&key).cloned().unwrap_or_else(P::Value::bottom);
        for (target, contribution) in problem.transfer(&key, &value) {
            work += 1;
            edges.push((key.clone(), target.clone()));
            if work > budget {
                return Err(ComputeError::unsupported(format!(
                    "fixed point did not converge within {budget} contributions"
                )));
            }
            let entry = values
                .entry(target.clone())
                .or_insert_with(P::Value::bottom);
            if entry.join(&contribution) {
                queue.push_back(target);
            }
        }
    }
    Ok((iterations, work))
}

/// Computes the least fixed point of `problem` within the default budget.
///
/// # Errors
///
/// Returns [`ComputeError::Unsupported`] when the worklist exceeds the budget,
/// which means the declared order did not converge as specified.
pub fn solve<P: FixedPointProblem>(problem: &P) -> Result<FixedPoint<P::Key, P::Value>> {
    solve_with_budget(problem, DEFAULT_BUDGET)
}

/// Computes the least fixed point with an explicit worklist budget.
///
/// # Errors
///
/// Returns [`ComputeError::Unsupported`] when the budget is exhausted.
pub fn solve_with_budget<P: FixedPointProblem>(
    problem: &P,
    budget: usize,
) -> Result<FixedPoint<P::Key, P::Value>> {
    let mut values: BTreeMap<P::Key, P::Value> = BTreeMap::new();
    let mut queue: std::collections::VecDeque<P::Key> = std::collections::VecDeque::new();

    for (key, value) in problem.seeds() {
        let entry = values.entry(key.clone()).or_insert_with(P::Value::bottom);
        if entry.join(&value) {
            queue.push_back(key);
        }
    }

    let mut edges: Vec<(P::Key, P::Key)> = Vec::new();
    let (iterations, work) = drain(problem, &mut values, &mut queue, budget, &mut edges)?;

    let (sccs, self_loops) = strongly_connected(&values, &edges);
    let mut dependents: BTreeMap<P::Key, BTreeSet<P::Key>> = BTreeMap::new();
    for (from, to) in edges {
        dependents.entry(from).or_default().insert(to);
    }
    Ok(FixedPoint {
        values,
        iterations,
        contributions: work,
        sccs,
        self_loops,
        dependents,
    })
}

/// Extends a previous solve with monotone input changes.
///
/// `changed` names the facts whose transfer may now produce more. The previous
/// values are retained; only those keys and their newly reached dependents are
/// re-run. Removals are not represented: the caller must use `solve` for a
/// reset/deletion. This is the seminaive add-only path over the dependency index.
///
/// # Errors
///
/// Returns [`ComputeError::Unsupported`] when the budget is exhausted.
pub fn solve_from<P: FixedPointProblem>(
    previous: &FixedPoint<P::Key, P::Value>,
    changed: impl IntoIterator<Item = P::Key>,
    problem: &P,
) -> Result<FixedPoint<P::Key, P::Value>> {
    solve_from_with_budget(previous, changed, problem, DEFAULT_BUDGET)
}

/// Explicit-budget form of [`solve_from`].
///
/// # Errors
///
/// Returns [`ComputeError::Unsupported`] when the budget is exhausted.
pub fn solve_from_with_budget<P: FixedPointProblem>(
    previous: &FixedPoint<P::Key, P::Value>,
    changed: impl IntoIterator<Item = P::Key>,
    problem: &P,
    budget: usize,
) -> Result<FixedPoint<P::Key, P::Value>> {
    let mut values = previous.values.clone();
    let changed: Vec<P::Key> = changed.into_iter().collect();
    let mut queue: std::collections::VecDeque<P::Key> = changed.iter().cloned().collect();
    for key in &changed {
        if let Some(dependents) = previous.dependents.get(key) {
            queue.extend(dependents.iter().cloned());
        }
    }
    let mut edges: Vec<(P::Key, P::Key)> = previous
        .dependents
        .iter()
        .flat_map(|(from, tos)| tos.iter().map(move |to| (from.clone(), to.clone())))
        .collect();
    let (iterations, work) = drain(problem, &mut values, &mut queue, budget, &mut edges)?;
    let (sccs, self_loops) = strongly_connected(&values, &edges);
    let mut dependents: BTreeMap<P::Key, BTreeSet<P::Key>> = BTreeMap::new();
    for (from, to) in edges {
        dependents.entry(from).or_default().insert(to);
    }
    Ok(FixedPoint {
        values,
        iterations,
        contributions: work,
        sccs,
        self_loops,
        dependents,
    })
}

/// Kosaraju's algorithm over the discovered contribution graph.
fn strongly_connected<K: Ord + Clone, V>(
    values: &BTreeMap<K, V>,
    edges: &[(K, K)],
) -> (Vec<Vec<K>>, BTreeSet<K>) {
    let mut adjacency: BTreeMap<K, BTreeSet<K>> = BTreeMap::new();
    let mut reverse: BTreeMap<K, BTreeSet<K>> = BTreeMap::new();
    for key in values.keys() {
        adjacency.entry(key.clone()).or_default();
        reverse.entry(key.clone()).or_default();
    }
    let mut self_loops: BTreeSet<K> = BTreeSet::new();
    for (from, to) in edges {
        adjacency
            .entry(from.clone())
            .or_default()
            .insert(to.clone());
        reverse.entry(to.clone()).or_default().insert(from.clone());
        if from == to {
            self_loops.insert(from.clone());
        }
    }
    let mut visited: BTreeSet<K> = BTreeSet::new();
    let mut order: Vec<K> = Vec::new();
    for start in adjacency.keys() {
        if visited.contains(start) {
            continue;
        }
        let mut stack: Vec<(K, bool)> = vec![(start.clone(), false)];
        while let Some((node, expanded)) = stack.pop() {
            if expanded {
                order.push(node);
                continue;
            }
            // Marked when the node is *expanded*, not when a parent queues it.
            // Marking at queue time lets a sibling be finished before a node it
            // can reach: the first pass is then not a depth-first postorder, and
            // Kosaraju groups nodes that are not mutually reachable.
            if !visited.insert(node.clone()) {
                continue;
            }
            stack.push((node.clone(), true));
            if let Some(neighbors) = adjacency.get(&node) {
                for next in neighbors {
                    if !visited.contains(next) {
                        stack.push((next.clone(), false));
                    }
                }
            }
        }
    }
    let mut assigned: BTreeSet<K> = BTreeSet::new();
    let mut components: Vec<Vec<K>> = Vec::new();
    for start in order.into_iter().rev() {
        if !assigned.insert(start.clone()) {
            continue;
        }
        let mut component = Vec::new();
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            component.push(node.clone());
            if let Some(neighbors) = reverse.get(&node) {
                for next in neighbors {
                    if assigned.insert(next.clone()) {
                        stack.push(next.clone());
                    }
                }
            }
        }
        component.sort();
        components.push(component);
    }
    components.sort_by(|left, right| left.first().cmp(&right.first()));
    (components, self_loops)
}

#[cfg(test)]
#[path = "../tests/unit/fixed_point/tests.rs"]
mod tests;
