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
mod tests {
    use super::*;

    struct Reachability {
        edges: Vec<(u32, u32)>,
        seeds: Vec<u32>,
    }

    impl FixedPointProblem for Reachability {
        type Key = u32;
        type Value = bool;

        fn seeds(&self) -> Vec<(u32, bool)> {
            self.seeds.iter().map(|node| (*node, true)).collect()
        }

        fn transfer(&self, key: &u32, value: &bool) -> Vec<(u32, bool)> {
            if !*value {
                return Vec::new();
            }
            self.edges
                .iter()
                .filter(|(from, _)| from == key)
                .map(|(_, to)| (*to, true))
                .collect()
        }
    }

    fn reachable_from_scratch(edges: &[(u32, u32)], seeds: &[u32]) -> BTreeSet<u32> {
        let mut reached: BTreeSet<u32> = seeds.iter().copied().collect();
        let mut queue: Vec<u32> = seeds.to_vec();
        while let Some(node) = queue.pop() {
            for (from, to) in edges {
                if *from == node && reached.insert(*to) {
                    queue.push(*to);
                }
            }
        }
        reached
    }

    #[test]
    fn reachability_matches_a_from_scratch_traversal() {
        let problem = Reachability {
            edges: vec![(0, 1), (1, 2), (2, 3), (3, 1), (4, 5)],
            seeds: vec![0],
        };
        let fixed = solve(&problem).expect("converges");
        let reached: BTreeSet<u32> = fixed.non_bottom().into_iter().collect();
        assert_eq!(
            reached,
            reachable_from_scratch(&problem.edges, &problem.seeds)
        );
        assert_eq!(reached, [0, 1, 2, 3].into_iter().collect());
        assert!(fixed.contributions() >= fixed.iterations());
    }

    #[test]
    fn sccs_localize_recursive_regions() {
        let problem = Reachability {
            edges: vec![(0, 1), (1, 0), (2, 3), (3, 2), (4, 5)],
            seeds: vec![0, 2, 4],
        };
        let fixed = solve(&problem).expect("converges");
        let recursive = fixed.recursive_components();
        assert!(recursive.contains(&[0, 1].as_slice()), "{recursive:?}");
        assert!(recursive.contains(&[2, 3].as_slice()), "{recursive:?}");
        assert_eq!(recursive.len(), 2);
        assert!(
            fixed
                .sccs()
                .iter()
                .any(|component| component == &vec![4] || component == &vec![5])
        );
    }

    /// The reported counterexample: 0 reaches both 1 and 2, and 2 reaches 1, so
    /// 1 and 2 are not mutually reachable and must not share a component.
    #[test]
    fn a_diamond_that_is_not_a_cycle_stays_apart() {
        let problem = Reachability {
            edges: vec![(0, 1), (0, 2), (2, 1)],
            seeds: vec![0],
        };
        let fixed = solve(&problem).expect("converges");
        assert_eq!(
            fixed.sccs(),
            &[vec![0], vec![1], vec![2]],
            "a reachability diamond is three components, not two"
        );
        assert!(
            fixed.recursive_components().is_empty(),
            "nothing here is a cycle: {:?}",
            fixed.recursive_components()
        );
    }

    /// Every component must be strongly connected, on every graph that fits.
    /// A traversal that is not a depth-first postorder groups nodes that are
    /// only reachable one way, which this walk of all 512 three-node graphs
    /// finds without needing a counterexample.
    #[test]
    fn every_component_is_strongly_connected() {
        let nodes = 3u32;
        let possible: Vec<(u32, u32)> = (0..nodes)
            .flat_map(|from| (0..nodes).map(move |to| (from, to)))
            .collect();
        for mask in 0u32..(1 << possible.len()) {
            let edges: Vec<(u32, u32)> = possible
                .iter()
                .enumerate()
                .filter(|(bit, _)| mask & (1 << bit) != 0)
                .map(|(_, edge)| *edge)
                .collect();
            let problem = Reachability {
                edges: edges.clone(),
                seeds: (0..nodes).collect(),
            };
            let fixed = solve(&problem).expect("converges");
            for component in fixed.sccs() {
                for from in component {
                    for to in component {
                        assert!(
                            from == to || reachable_within(&edges, *from, *to, component),
                            "edges {edges:?} grouped {component:?}, but {from} cannot reach {to}"
                        );
                    }
                }
            }
        }
    }

    /// Breadth-first reachability that never leaves one component.
    fn reachable_within(edges: &[(u32, u32)], from: u32, to: u32, component: &[u32]) -> bool {
        let mut seen = std::collections::BTreeSet::new();
        let mut queue = std::collections::VecDeque::from([from]);
        seen.insert(from);
        while let Some(node) = queue.pop_front() {
            if node == to {
                return true;
            }
            for (edge_from, edge_to) in edges {
                if *edge_from == node && component.contains(edge_to) && seen.insert(*edge_to) {
                    queue.push_back(*edge_to);
                }
            }
        }
        false
    }

    #[test]
    fn a_self_loop_is_recursive() {
        let problem = Reachability {
            edges: vec![(0, 0)],
            seeds: vec![0],
        };
        let fixed = solve(&problem).expect("converges");
        let recursive = fixed.recursive_components();
        assert_eq!(recursive, vec![[0].as_slice()]);
    }

    #[test]
    fn randomized_reachability_matches_bfs() {
        let mut rng = irrigation_test::TestRng::new(0x0f_19);
        for case in 0..200 {
            let edge_count = rng.below(24);
            let edges: Vec<(u32, u32)> = (0..edge_count)
                .map(|_| (rng.below(8) as u32, rng.below(8) as u32))
                .collect();
            let seed_count = rng.below(3);
            let seeds: Vec<u32> = (0..seed_count).map(|_| rng.below(8) as u32).collect();
            let problem = Reachability {
                edges: edges.clone(),
                seeds: seeds.clone(),
            };
            let fixed = solve(&problem).expect("converges");
            let reached: BTreeSet<u32> = fixed.non_bottom().into_iter().collect();
            assert_eq!(
                reached,
                reachable_from_scratch(&edges, &seeds),
                "case {case}"
            );
        }
    }

    #[test]
    fn deletions_are_handled_by_recomputation() {
        let with_edge = Reachability {
            edges: vec![(0, 1), (1, 2)],
            seeds: vec![0],
        };
        let without_edge = Reachability {
            edges: vec![(0, 1)],
            seeds: vec![0],
        };
        let first = solve(&with_edge).expect("converges");
        let second = solve(&without_edge).expect("converges");
        let first_reached: BTreeSet<u32> = first.non_bottom().into_iter().collect();
        let second_reached: BTreeSet<u32> = second.non_bottom().into_iter().collect();
        assert_eq!(first_reached, [0, 1, 2].into_iter().collect());
        assert_eq!(second_reached, [0, 1].into_iter().collect());
    }

    impl Lattice for u32 {
        fn bottom() -> Self {
            0
        }

        fn join(&mut self, other: &Self) -> bool {
            let changed = *other > *self;
            *self = (*self).max(*other);
            changed
        }
    }

    #[test]
    fn non_converging_orders_hit_the_budget() {
        struct NeverConverges;
        impl FixedPointProblem for NeverConverges {
            type Key = u32;
            type Value = u32;

            fn seeds(&self) -> Vec<(u32, u32)> {
                vec![(0, 1)]
            }

            fn transfer(&self, _key: &u32, value: &u32) -> Vec<(u32, u32)> {
                vec![(0, value.saturating_add(1))]
            }
        }
        // `u32` with `max` is finite but enormous; the budget stops it.
        let error = solve_with_budget(&NeverConverges, 1000).unwrap_err();
        assert!(matches!(error, ComputeError::Unsupported { .. }));
    }

    #[test]
    fn solved_facts_index_their_dependents() {
        let problem = Reachability {
            edges: vec![(1, 2), (2, 3)],
            seeds: vec![1],
        };
        let result = solve(&problem).expect("fixed point");
        assert_eq!(result.dependents(&1), Some(&BTreeSet::from([2])));
        assert_eq!(result.dependents(&2), Some(&BTreeSet::from([3])));
    }

    /// `solve_from` claims to extend a settled solve with monotone additions.
    /// The claim is testable against `solve` itself: for every three-node graph,
    /// add one edge and the extended answer must equal a from-scratch solve of
    /// the grown graph.
    #[test]
    fn an_add_only_extension_matches_a_from_scratch_solve() {
        let nodes = 3u32;
        let possible: Vec<(u32, u32)> = (0..nodes)
            .flat_map(|from| (0..nodes).map(move |to| (from, to)))
            .collect();
        for mask in 0u32..(1 << possible.len()) {
            let edges: Vec<(u32, u32)> = possible
                .iter()
                .enumerate()
                .filter(|(bit, _)| mask & (1 << bit) != 0)
                .map(|(_, edge)| *edge)
                .collect();
            let base = Reachability {
                edges: edges.clone(),
                seeds: (0..nodes).collect(),
            };
            let settled = solve(&base).expect("base solve");
            for extra in &possible {
                if edges.contains(extra) {
                    continue;
                }
                let mut grown = edges.clone();
                grown.push(*extra);
                let next = Reachability {
                    edges: grown.clone(),
                    seeds: (0..nodes).collect(),
                };
                let extended = solve_from(&settled, [extra.0], &next).expect("extension");
                let fresh = solve(&next).expect("from scratch");
                assert_eq!(
                    extended.values(),
                    fresh.values(),
                    "adding {extra:?} to {edges:?} must not change the answer"
                );
            }
        }
    }

    #[test]
    fn an_add_only_update_reuses_the_dependency_index() {
        let first = Reachability {
            edges: vec![(1, 2)],
            seeds: vec![1],
        };
        let settled = solve(&first).expect("first");
        let next = Reachability {
            edges: vec![(1, 2), (2, 3)],
            seeds: vec![1],
        };
        let extended = solve_from(&settled, [1u32], &next).expect("extend");
        assert_eq!(extended.values().get(&3), Some(&true));
        assert!(
            extended.contributions() < extended.iterations().max(1) + 3,
            "the extension must not restart the whole solve"
        );
    }
}
