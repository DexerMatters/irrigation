//! Unit tests for `src/fixed_point.rs`.

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
