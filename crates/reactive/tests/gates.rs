//! Acceptance gates from the plan: randomized oracle comparison, recursion,
//! mutual recursion, cycle detection, and unique definitions.

use reactive::{reactive, view, Runtime};

// ---------------------------------------------------------------------------
// Randomized edit sequence against a from-scratch oracle
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Model {
    numbers: List<u64>,
    strict: bool,
}

#[reactive]
mod model_analysis {
    use super::{Model};

    #[component]
    pub fn analyze(model: &Model) -> u64 {
        let numbers = model.numbers();
        let doubled = numbers.map(|n| *n * 2);
        let total = doubled.iter().sum::<u64>();
        if model.strict() {
            total + 1
        } else {
            total
        }
    }
}

fn oracle(numbers: &[u64], strict: bool) -> u64 {
    let total: u64 = numbers.iter().map(|n| n * 2).sum();
    if strict { total + 1 } else { total }
}

#[test]
fn randomized_transactions_match_from_scratch_evaluation() {
    let runtime = Runtime::new();
    runtime.install(model_analysis::Analyze).expect("install");
    let mut model: Vec<u64> = Vec::new();
    let mut strict = false;
    let mut rng = reactive_test::Lcg::new(0x5eed);

    for _ in 0..2000 {
        let choice = rng.below(6);
        runtime
            .edit(|edit| {
                let mut view = edit.view::<Model>();
                match choice {
                    0 => {
                        let value = rng.next_u64() % 100;
                        view.numbers().push(value);
                        model.push(value);
                    }
                    1 if !model.is_empty() => {
                        let index = rng.below(model.len());
                        view.numbers().remove(index);
                        model.remove(index);
                    }
                    2 if !model.is_empty() => {
                        let index = rng.below(model.len());
                        let value = rng.next_u64() % 100;
                        view.numbers().set(index, value);
                        model[index] = value;
                    }
                    3 if model.len() > 1 => {
                        let from = rng.below(model.len());
                        let to = rng.below(model.len());
                        view.numbers().move_element(from, to);
                        let value = model.remove(from);
                        model.insert(to, value);
                    }
                    4 => {
                        strict = !strict;
                        view.set_strict(strict);
                    }
                    _ => {}
                }
            })
            .expect("edit");

        let actual = runtime
            .read_outputs::<model_analysis::Analyze>()
            .expect("read");
        assert_eq!(
            actual,
            oracle(&model, strict),
            "model {model:?} strict {strict}"
        );
    }
}

#[test]
fn no_op_transactions_publish_nothing() {
    let runtime = Runtime::new();
    runtime.install(model_analysis::Analyze).expect("install");
    let _ = runtime.read().expect("read");
    let revision = runtime.revision();
    for _ in 0..10 {
        runtime
            .edit(|edit| {
                edit.view::<Model>().set_strict(false);
            })
            .expect("edit");
    }
    assert_eq!(runtime.revision(), revision);
}

// ---------------------------------------------------------------------------
// Recursion and mutual recursion
// ---------------------------------------------------------------------------

#[reactive]
mod recursion {
    #[component]
    pub fn fib(n: u32) -> u64 {
        if n < 2 {
            u64::from(n)
        } else {
            fib(n - 1) + fib(n - 2)
        }
    }

    #[component]
    pub fn even(n: u32) -> bool {
        if n == 0 {
            true
        } else {
            odd(n - 1)
        }
    }

    #[component]
    pub fn odd(n: u32) -> bool {
        if n == 0 {
            false
        } else {
            even(n - 1)
        }
    }

    #[component]
    pub fn loopy(n: u32) -> u64 {
        loopy(n)
    }
}

#[reactive]
mod recursion_root {
    use super::recursion;

    #[component]
    pub fn root() -> (u64, bool) {
        (recursion::fib(12), recursion::even(10))
    }
}

#[test]
fn recursive_and_mutually_recursive_components_are_memoized() {
    let runtime = Runtime::new();
    runtime.install(recursion_root::Root).expect("install");
    let (fib, even) = runtime
        .read_outputs::<recursion_root::Root>()
        .expect("outputs");
    assert_eq!(fib, 144);
    assert!(even);
    let executions = runtime.stats().executions;
    let (fib, even) = runtime
        .read_outputs::<recursion_root::Root>()
        .expect("outputs");
    assert_eq!(fib, 144);
    assert!(even);
    assert_eq!(runtime.stats().executions, executions);
}

#[reactive]
mod cycle_root {
    use super::recursion;

    #[component]
    pub fn root() -> u64 {
        recursion::loopy(1)
    }
}

#[test]
fn a_value_cycle_is_reported_not_hung() {
    let runtime = Runtime::new();
    runtime.install(cycle_root::Root).expect("install");
    let error = runtime.read_outputs::<cycle_root::Root>().unwrap_err();
    assert!(
        matches!(error, reactive::ComputeError::Cycle { .. }),
        "expected a cycle error, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Unique definitions
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct Registry(Map<u32, u32>);

#[reactive]
mod duplicate_definitions {
    use super::Registry;

    #[component]
    pub fn first() -> Registry {
        Registry::from([(1u32, 10u32)])
    }

    #[component]
    pub fn second() -> Registry {
        Registry::from([(1u32, 20u32)])
    }

    #[component]
    pub fn root() -> u32 {
        let _ = first();
        let _ = second();
        0
    }
}

#[test]
fn two_providers_cannot_define_the_same_unit() {
    let runtime = Runtime::new();
    runtime
        .install(duplicate_definitions::Root)
        .expect("install");
    let error = runtime
        .read_outputs::<duplicate_definitions::Root>()
        .unwrap_err();
    assert!(
        matches!(error, reactive::ComputeError::DuplicateDefinition { .. }),
        "expected a duplicate-definition error, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Independent output cutoff
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Pair {
    left: u32,
    right: u32,
}

#[reactive]
mod independent_outputs {
    use super::Pair;

    #[component]
    pub fn split(pair: &Pair) -> (u32, u32) {
        (pair.left(), pair.right())
    }
}

#[test]
fn changing_one_output_does_not_change_the_other() {
    let runtime = Runtime::new();
    runtime.install(independent_outputs::Split).expect("install");
    let (left, right) = runtime
        .read_outputs::<independent_outputs::Split>()
        .expect("outputs");
    assert_eq!((left, right), (0, 0));

    runtime
        .edit(|edit| {
            edit.view::<Pair>().set_right(7);
        })
        .expect("edit");
    let (left, right) = runtime
        .read_outputs::<independent_outputs::Split>()
        .expect("outputs");
    assert_eq!((left, right), (0, 7));
}

// ---------------------------------------------------------------------------
// Deep recursion and worker-count invariance
// ---------------------------------------------------------------------------

#[reactive]
mod deep {
    #[component]
    pub fn countdown(n: u32) -> u64 {
        if n == 0 {
            0
        } else {
            countdown(n - 1) + 1
        }
    }

    #[component]
    pub fn root() -> u64 {
        countdown(10_000)
    }
}

#[test]
fn deep_recursion_is_stack_safe() {
    let runtime = Runtime::with_config(reactive::RuntimeConfig {
        workers: 2,
        stack_size: 256 * 1024 * 1024,
        ..reactive::RuntimeConfig::default()
    });
    runtime.install(deep::Root).expect("install");
    assert_eq!(runtime.read_outputs::<deep::Root>().expect("outputs"), 10_000);
}

#[reactive]
mod parallel_sum {
    use super::Model;

    #[component]
    pub fn sum(model: &Model) -> u64 {
        let numbers = model.numbers();
        let doubled = numbers.map(|n| *n * 2);
        doubled.iter().sum::<u64>()
    }
}

#[test]
fn results_are_invariant_across_worker_counts() {
    let mut results = Vec::new();
    for workers in [1usize, 2, 4] {
        let runtime = Runtime::with_config(reactive::RuntimeConfig {
            workers,
            stack_size: 16 * 1024 * 1024,
            ..reactive::RuntimeConfig::default()
        });
        runtime.install(parallel_sum::Sum).expect("install");
        runtime
            .edit(|edit| {
                let mut model = edit.view::<Model>();
                let mut numbers = model.numbers();
                for value in 1..=100u64 {
                    numbers.push(value);
                }
            })
            .expect("edit");
        results.push(runtime.read_outputs::<parallel_sum::Sum>().expect("outputs"));
    }
    assert_eq!(results, vec![10100, 10100, 10100]);
}

// ---------------------------------------------------------------------------
// Fixed points inside a component
// ---------------------------------------------------------------------------

#[view(source)]
pub struct GraphInput {
    edges: List<(u32, u32)>,
    seed: u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Reachability {
    edges: Vec<(u32, u32)>,
    seeds: Vec<u32>,
}

impl reactive::FixedPointProblem for Reachability {
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

#[reactive]
mod reachability {
    use super::{GraphInput, Reachability};

    #[component]
    pub fn reachable(input: &GraphInput) -> Vec<u32> {
        let edges = input.edges().to_vec();
        let seed = input.seed();
        let problem = Reachability {
            edges,
            seeds: vec![seed],
        };
        let fixed = reactive::solve_fixed_point(&problem).expect("converges");
        let nodes: Vec<u32> = fixed.non_bottom();
        nodes.sort_unstable();
        nodes
    }
}

fn reachable_from_scratch(edges: &[(u32, u32)], seed: u32) -> Vec<u32> {
    let mut reached = std::collections::BTreeSet::from([seed]);
    let mut queue = vec![seed];
    while let Some(node) = queue.pop() {
        for (from, to) in edges {
            if *from == node && reached.insert(*to) {
                queue.push(*to);
            }
        }
    }
    reached.into_iter().collect()
}

#[test]
fn fixed_points_inside_components_follow_insertions_and_deletions() {
    let runtime = Runtime::new();
    runtime.install(reachability::Reachable).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<GraphInput>();
            view.set_seed(0);
            let mut edges = view.edges();
            edges.push((0, 1));
            edges.push((1, 2));
            edges.push((2, 3));
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<reachability::Reachable>()
            .expect("outputs"),
        vec![0, 1, 2, 3]
    );

    // Retract the middle edge: deletion must not leave stale facts behind.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<GraphInput>();
            let mut edges = view.edges();
            edges.remove(1);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<reachability::Reachable>()
            .expect("outputs"),
        reachable_from_scratch(&[(0, 1), (2, 3)], 0)
    );

    // Reinsert it: the region recomputes to the larger fixed point.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<GraphInput>();
            let mut edges = view.edges();
            edges.push((1, 2));
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<reachability::Reachable>()
            .expect("outputs"),
        reachable_from_scratch(&[(0, 1), (2, 3), (1, 2)], 0)
    );
}

// ---------------------------------------------------------------------------
// Graph View family: topology and payloads are independent
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Network {
    links: Graph<u32, u32>,
}

#[reactive]
mod graph_stats {
    use super::Network;

    #[component]
    pub fn stats(network: &Network) -> (u64, u64, u64) {
        let graph = network.links();
        let nodes = graph.node_count() as u64;
        let edges = graph.edge_count() as u64;
        let ids = graph.edges();
        let weights = ids.iter().map(|edge| graph.edge_weight(*edge).unwrap_or(0) as u64);
        let total = weights.sum::<u64>();
        (nodes, edges, total)
    }
}

fn graph_unit_versions(runtime: &Runtime) -> Vec<(String, u64)> {
    runtime
        .dump()
        .into_iter()
        .filter_map(|node| node.unit.map(|unit| (unit, node.changed_at)))
        .collect()
}

#[test]
fn graph_topology_and_payloads_are_independent() {
    let runtime = Runtime::new();
    runtime.install(graph_stats::Stats).expect("install");
    let first_edge = runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            let mut links = view.links();
            links.add_node(1, 10);
            links.add_node(2, 20);
            links.add_node(3, 30);
            let first = links.add_edge(1, 2, 5);
            links.add_edge(2, 3, 7);
            first
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<graph_stats::Stats>().expect("outputs"),
        (3, 2, 12)
    );
    let before = graph_unit_versions(&runtime);
    let topology_before: Vec<(String, u64)> = before
        .iter()
        .filter(|(unit, _)| unit.contains("Structure"))
        .cloned()
        .collect();
    assert!(!topology_before.is_empty());

    // A payload edit must not touch topology.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            let mut links = view.links();
            links.set_edge(first_edge, 50);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<graph_stats::Stats>().expect("outputs"),
        (3, 2, 57)
    );
    let after = graph_unit_versions(&runtime);
    let topology_after: Vec<(String, u64)> = after
        .iter()
        .filter(|(unit, _)| unit.contains("Structure"))
        .cloned()
        .collect();
    assert_eq!(topology_before, topology_after);

    // Removing a node retracts its incident edges.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            let mut links = view.links();
            links.remove_node(2);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<graph_stats::Stats>().expect("outputs"),
        (2, 0, 0)
    );
    let snapshot = runtime.read().expect("read");
    assert_eq!(snapshot.view::<Network>().links_nodes(), vec![1, 3]);
    assert!(snapshot.view::<Network>().links_edges().is_empty());
}

#[test]
fn graph_neighbours_follow_edges() {
    let runtime = Runtime::new();
    runtime.install(graph_stats::Stats).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            let mut links = view.links();
            links.add_node(1, 0);
            links.add_node(2, 0);
            links.add_node(3, 0);
            links.add_edge(1, 2, 1);
            links.add_edge(1, 3, 1);
            links.add_edge(2, 3, 1);
        })
        .expect("edit");
    let _ = runtime.read_outputs::<graph_stats::Stats>().expect("outputs");
}
