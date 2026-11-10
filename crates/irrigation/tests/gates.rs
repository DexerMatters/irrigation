//! Acceptance gates from the plan: randomized oracle comparison, recursion,
//! mutual recursion, cycle detection, and unique definitions.

use irrigation::{Runtime, incremental_data, irrigation, tree_view, view};

// ---------------------------------------------------------------------------
// Randomized edit sequence against a from-scratch oracle
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Model {
    numbers: List<u64>,
    strict: bool,
}

#[irrigation]
mod model_analysis {
    use super::Model;

    #[component]
    pub fn analyze(model: &Model) -> u64 {
        let numbers = model.numbers();
        let doubled = numbers.map(|n| *n * 2);
        let total = doubled.iter().sum::<u64>();
        if model.strict() { total + 1 } else { total }
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
    let mut rng = irrigation_test::TestRng::new(0x5eed);

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

#[irrigation]
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
        if n == 0 { true } else { odd(n - 1) }
    }

    #[component]
    pub fn odd(n: u32) -> bool {
        if n == 0 { false } else { even(n - 1) }
    }

    #[component]
    pub fn loopy(n: u32) -> u64 {
        loopy(n)
    }
}

#[irrigation]
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

#[irrigation]
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
        matches!(error, irrigation::ComputeError::Cycle { .. }),
        "expected a cycle error, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Unique definitions
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct Registry(Map<u32, u32>);

#[irrigation]
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
        matches!(error, irrigation::ComputeError::DuplicateDefinition { .. }),
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

#[irrigation]
mod independent_outputs {
    use super::Pair;

    #[component]
    pub fn split(pair: &Pair) -> (u32, u32) {
        (pair.left(), pair.right())
    }
}

fn output_projection_versions(runtime: &Runtime) -> Vec<(irrigation::NodeId, u64)> {
    runtime
        .dump()
        .into_iter()
        .filter(|node| node.kind == "InvocationOutput")
        .map(|node| (node.id, node.changed_at))
        .collect()
}

#[test]
fn changing_one_output_does_not_change_the_other() {
    let runtime = Runtime::new();
    runtime
        .install(independent_outputs::Split)
        .expect("install");
    let (left, right) = runtime
        .read_outputs::<independent_outputs::Split>()
        .expect("outputs");
    assert_eq!((left, right), (0, 0));
    let before = output_projection_versions(&runtime);
    assert_eq!(before.len(), 2, "the split exposes two output projections");

    runtime
        .edit(|edit| {
            edit.view::<Pair>().set_right(7);
        })
        .expect("edit");
    let (left, right) = runtime
        .read_outputs::<independent_outputs::Split>()
        .expect("outputs");
    assert_eq!((left, right), (0, 7));

    let after = output_projection_versions(&runtime);
    assert_eq!(
        after.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        before.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        "each projection keeps its own node identity"
    );
    assert_eq!(
        after[0].1, before[0].1,
        "the unchanged left projection must keep its change version"
    );
    assert!(
        after[1].1 > before[1].1,
        "the changed right projection must move to a new version"
    );
    assert_eq!(
        after[1].1,
        runtime.revision(),
        "the right projection is stamped at the revision its value changed in"
    );
}

// ---------------------------------------------------------------------------
// Deep recursion and worker-count invariance
// ---------------------------------------------------------------------------

#[irrigation]
mod deep {
    #[component]
    pub fn countdown(n: u32) -> u64 {
        if n == 0 { 0 } else { countdown(n - 1) + 1 }
    }

    #[component]
    pub fn root() -> u64 {
        countdown(10_000)
    }
}

#[test]
fn deep_recursion_is_stack_safe() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        stack_size: 256 * 1024 * 1024,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.install(deep::Root).expect("install");
    assert_eq!(
        runtime.read_outputs::<deep::Root>().expect("outputs"),
        10_000
    );
}

#[irrigation]
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
        let runtime = Runtime::with_config(irrigation::RuntimeConfig {
            workers,
            stack_size: 16 * 1024 * 1024,
            ..irrigation::RuntimeConfig::default()
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
        results.push(
            runtime
                .read_outputs::<parallel_sum::Sum>()
                .expect("outputs"),
        );
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

impl irrigation::FixedPointProblem for Reachability {
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

#[irrigation]
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
        let fixed = irrigation::solve_fixed_point(&problem).expect("converges");
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

#[irrigation]
mod graph_stats {
    use super::Network;

    #[component]
    pub fn stats(network: &Network) -> (u64, u64, u64) {
        let graph = network.links();
        let nodes = graph.node_count() as u64;
        let edges = graph.edge_count() as u64;
        let ids = graph.edges();
        let weights = ids
            .iter()
            .map(|edge| graph.edge_weight(*edge).unwrap_or(0) as u64);
        let total = weights.sum::<u64>();
        (nodes, edges, total)
    }
}

#[irrigation]
mod graph_adjacency {
    use super::Network;

    /// Flattens the three adjacency projections of node 1 and node 3: the
    /// neighbours of 1, then 1's outgoing `(edge, target)` pairs, then 3's
    /// incoming `(edge, source)` pairs.
    #[component]
    pub fn adjacency(network: &Network) -> Vec<u64> {
        let graph = network.links();
        graph
            .neighbors(1)
            .into_iter()
            .chain(
                graph
                    .outgoing(1)
                    .into_iter()
                    .flat_map(|(edge, target)| [edge, target]),
            )
            .chain(
                graph
                    .incoming(3)
                    .into_iter()
                    .flat_map(|(edge, source)| [edge, source]),
            )
            .collect()
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
        runtime
            .read_outputs::<graph_stats::Stats>()
            .expect("outputs"),
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
        runtime
            .read_outputs::<graph_stats::Stats>()
            .expect("outputs"),
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
        runtime
            .read_outputs::<graph_stats::Stats>()
            .expect("outputs"),
        (2, 0, 0)
    );
    let snapshot = runtime.read().expect("read");
    assert_eq!(snapshot.view::<Network>().links_nodes(), vec![1, 3]);
    assert!(snapshot.view::<Network>().links_edges().is_empty());
}

#[test]
fn graph_neighbours_follow_edges() {
    let runtime = Runtime::new();
    runtime
        .install(graph_adjacency::Adjacency)
        .expect("install");
    let (first, second, third) = runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            let mut links = view.links();
            links.add_node(1, 0);
            links.add_node(2, 0);
            links.add_node(3, 0);
            let first = links.add_edge(1, 2, 1);
            let second = links.add_edge(1, 3, 1);
            let third = links.add_edge(2, 3, 1);
            (first, second, third)
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<graph_adjacency::Adjacency>()
            .expect("outputs"),
        vec![
            2, 3, // neighbours of 1
            first, 2, second, 3, // 1's outgoing (edge, target) pairs
            second, 1, third, 2, // 3's incoming (edge, source) pairs
        ]
    );

    // Removing an edge drops it from every adjacency projection; the survivors
    // stay in edge-identity order.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            let mut links = view.links();
            links.remove_edge(first);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<graph_adjacency::Adjacency>()
            .expect("outputs"),
        vec![
            3, // the removed edge leaves the neighbours of 1
            second, 3, // the surviving outgoing edge
            second, 1, third, 2, // the incoming edges of 3
        ]
    );
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[test]
fn a_cancelled_demand_publishes_nothing() {
    let runtime = Runtime::new();
    runtime.install(model_analysis::Analyze).expect("install");
    let token = irrigation::CancelToken::new(1);
    token.cancel();
    let error = runtime.read_with_cancel(token).unwrap_err();
    assert!(error.is_cancelled(), "expected cancellation, got {error:?}");
    assert_eq!(runtime.revision(), 0);

    // The cancelled attempt left no cached value behind.
    assert_eq!(
        runtime
            .read_outputs::<model_analysis::Analyze>()
            .expect("outputs"),
        0
    );
}

#[test]
fn cancelling_an_idle_runtime_is_harmless() {
    let runtime = Runtime::new();
    runtime.install(model_analysis::Analyze).expect("install");
    runtime.cancel();
    assert_eq!(
        runtime
            .read_outputs::<model_analysis::Analyze>()
            .expect("outputs"),
        0
    );
}

// ---------------------------------------------------------------------------
// Reclamation and memory accounting
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Toggle {
    flag: bool,
    left: u64,
    right: u64,
}

#[irrigation]
mod branching {
    use super::Toggle;

    #[component]
    pub fn pick(toggle: &Toggle) -> u64 {
        if toggle.flag() {
            let left = toggle.left();
            left + 1
        } else {
            let right = toggle.right();
            right + 2
        }
    }
}

#[test]
fn abandoned_branches_are_reclaimed_without_changing_results() {
    let runtime = Runtime::new();
    runtime.install(branching::Pick).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Toggle>();
            view.set_flag(true);
            view.set_left(10);
            view.set_right(20);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<branching::Pick>().expect("outputs"),
        11
    );
    let before = runtime.memory_report();
    assert!(before.nodes > 0);
    assert!(before.trace_edges > 0);

    // Switch branches: the old branch's nodes become unreachable.
    runtime
        .edit(|edit| {
            edit.view::<Toggle>().set_flag(false);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<branching::Pick>().expect("outputs"),
        22
    );
    let reclaimed = runtime.collect();
    assert!(
        reclaimed.nodes_reclaimed > 0,
        "expected abandoned nodes to be reclaimed, got {reclaimed:?}"
    );

    // Correctness after reclamation: the live branch still repairs.
    runtime
        .edit(|edit| {
            edit.view::<Toggle>().set_right(30);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<branching::Pick>().expect("outputs"),
        32
    );

    // Switching back reconstructs the abandoned branch.
    runtime
        .edit(|edit| {
            edit.view::<Toggle>().set_flag(true);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<branching::Pick>().expect("outputs"),
        11
    );
}

#[test]
fn a_component_can_be_demanded_directly() {
    let runtime = Runtime::new();
    // Direct demand installs a computation frame of its own.
    let value = runtime
        .call::<recursion::__FibSpec>(recursion::__FibArgs { n: 10 })
        .expect("call");
    assert_eq!(value, 55);
    // A second demand reuses the interned invocation.
    let again = runtime
        .call::<recursion::__FibSpec>(recursion::__FibArgs { n: 10 })
        .expect("call");
    assert_eq!(again, 55);
}

// ---------------------------------------------------------------------------
// Map keys: negative dependencies and unrelated-key isolation
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Table {
    entries: Map<u32, u64>,
}

#[irrigation]
mod table {
    use super::Table;

    #[component]
    pub fn lookup(table: &Table) -> Option<u64> {
        let entries = table.entries();
        entries.get(&1)
    }
}

#[test]
fn missing_key_reads_are_dependencies_and_other_keys_are_isolated() {
    let runtime = Runtime::new();
    runtime.install(table::Lookup).expect("install");
    assert_eq!(
        runtime.read_outputs::<table::Lookup>().expect("outputs"),
        None
    );

    // An unrelated key must not wake the consumer.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Table>();
            let mut entries = view.entries();
            entries.insert(2, 20);
        })
        .expect("edit");
    let probe = irrigation_test::StatsProbe::start(&runtime);
    assert_eq!(
        runtime.read_outputs::<table::Lookup>().expect("outputs"),
        None
    );
    let delta = probe.delta(&runtime);
    assert_eq!(
        delta.executions, 0,
        "an unrelated key edit executed consumer work"
    );

    // Defining the observed key must wake it.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Table>();
            let mut entries = view.entries();
            entries.insert(1, 10);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<table::Lookup>().expect("outputs"),
        Some(10)
    );

    // Retracting the key must be observed too.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Table>();
            let mut entries = view.entries();
            entries.remove(&1);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<table::Lookup>().expect("outputs"),
        None
    );
}

// ---------------------------------------------------------------------------
// GeneralComponent: custom incremental algorithms
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Counter {
    value: u64,
}

fn counter_unit() -> irrigation::UnitKey {
    irrigation::UnitKey::field(
        <Counter as irrigation::ViewSchema>::DESCRIPTOR.id,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
    )
}

/// Tracks the counter with an incremental update: the change batch names the
/// unit, so the update re-reads exactly that unit.
struct Tracker;

impl irrigation::GeneralComponent for Tracker {
    type State = (u64, u64, u64);
    type Output = (u64, u64, u64);

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00AA_0001);
    const NAME: &'static str = "tests::Tracker";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<Counter as irrigation::ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> irrigation::DependencyUpdates {
        irrigation::DependencyUpdates::Incremental
    }

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(counter_unit())?;
        let state = (value, 1, 0);
        Ok((state, (value * 2, 1, 0)))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        state: &Self::State,
        changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let mut value = state.0;
        for change in changes.changes() {
            value = cx.read_typed(change.unit.clone())?;
        }
        let updates = state.2 + 1;
        let state = (value, state.1, updates);
        Ok((state, (value * 2, state.1, updates)))
    }
}

/// Uses the default `Reset` policy: every invalidation reinitializes.
struct Resetter;

impl irrigation::GeneralComponent for Resetter {
    type State = u64;
    type Output = (u64, u64);

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00AA_0002);
    const NAME: &'static str = "tests::Resetter";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<Counter as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(counter_unit())?;
        Ok((value + 1, (value, 1)))
    }

    fn update(
        &self,
        _cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        panic!("the default policy must not call update");
    }
}

#[test]
fn general_components_initialize_update_and_commit_state() {
    let runtime = Runtime::new();
    runtime.install_general_root(Tracker).expect("install");
    assert_eq!(
        runtime.read_outputs_general::<Tracker>().expect("outputs"),
        (0, 1, 0)
    );

    runtime
        .edit(|edit| {
            edit.view::<Counter>().set_value(5);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs_general::<Tracker>().expect("outputs"),
        (10, 1, 1),
        "update should run once with the change batch"
    );

    runtime
        .edit(|edit| {
            edit.view::<Counter>().set_value(7);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs_general::<Tracker>().expect("outputs"),
        (14, 1, 2)
    );

    // A no-op edit publishes nothing and does not update.
    let revision = runtime.revision();
    runtime
        .edit(|edit| {
            edit.view::<Counter>().set_value(7);
        })
        .expect("edit");
    assert_eq!(runtime.revision(), revision);
    assert_eq!(
        runtime.read_outputs_general::<Tracker>().expect("outputs"),
        (14, 1, 2)
    );
}

#[test]
fn general_components_default_to_reset_on_invalidation() {
    let runtime = Runtime::new();
    runtime.install_general_root(Resetter).expect("install");
    assert_eq!(
        runtime.read_outputs_general::<Resetter>().expect("outputs"),
        (0, 1)
    );
    runtime
        .edit(|edit| {
            edit.view::<Counter>().set_value(3);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs_general::<Resetter>().expect("outputs"),
        (3, 1),
        "reset reinitializes instead of updating"
    );
}

#[test]
fn general_components_publish_definitions_atomically() {
    let runtime = Runtime::new();
    runtime.install_general_root(Tracker).expect("install");
    let snapshot = runtime.read().expect("snapshot");
    assert_eq!(snapshot.revision(), 0);
    assert!(snapshot.contains::<Counter>());
}

// ---------------------------------------------------------------------------
// Incremental equi-join
// ---------------------------------------------------------------------------

#[view(source)]
pub struct LeftTable {
    entries: Map<u32, u64>,
}

#[view(source)]
pub struct RightTable {
    entries: Map<u32, u64>,
}

static JOIN_COMBINES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct PairJoin;

impl irrigation::JoinSpec for PairJoin {
    type Key = u32;
    type Left = u64;
    type Right = u64;
    type Joined = u64;

    fn left() -> (irrigation::ViewId, irrigation::FieldId) {
        (
            <LeftTable as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        )
    }

    fn right() -> (irrigation::ViewId, irrigation::FieldId) {
        (
            <RightTable as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        )
    }

    fn combine(left: &Self::Left, right: &Self::Right) -> Self::Joined {
        JOIN_COMBINES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        left * right
    }
}

/// The same left/right tables without the counting side effect: the
/// transaction gate neither reads nor perturbs the shared counter.
struct TransactionJoin;

impl irrigation::JoinSpec for TransactionJoin {
    type Key = u32;
    type Left = u64;
    type Right = u64;
    type Joined = u64;

    fn left() -> (irrigation::ViewId, irrigation::FieldId) {
        (
            <LeftTable as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        )
    }

    fn right() -> (irrigation::ViewId, irrigation::FieldId) {
        (
            <RightTable as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        )
    }

    fn combine(left: &Self::Left, right: &Self::Right) -> Self::Joined {
        left * right
    }
}

fn from_scratch_join(
    left: &[(u32, u64)],
    right: &[(u32, u64)],
) -> std::collections::BTreeMap<u32, u64> {
    let mut joined = std::collections::BTreeMap::new();
    for (key, left) in left {
        if let Some((_, right)) = right.iter().find(|(other, _)| other == key) {
            joined.insert(*key, left * right);
        }
    }
    joined
}

#[test]
fn incremental_join_matches_from_scratch_and_touches_only_changed_keys() {
    JOIN_COMBINES.store(0, std::sync::atomic::Ordering::SeqCst);
    let runtime = Runtime::new();
    runtime.register_view(<LeftTable as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<RightTable as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_root(irrigation::EquiJoin::<PairJoin>::new())
        .expect("install");

    let mut left: Vec<(u32, u64)> = vec![(1, 10), (2, 20), (3, 30)];
    let mut right: Vec<(u32, u64)> = vec![(1, 2), (3, 4), (4, 5)];
    runtime
        .edit(|edit| {
            {
                let mut l = edit.view::<LeftTable>();
                let mut entries = l.entries();
                for (key, value) in &left {
                    entries.insert(*key, *value);
                }
            }
            let mut r = edit.view::<RightTable>();
            let mut entries = r.entries();
            for (key, value) in &right {
                entries.insert(*key, *value);
            }
        })
        .expect("edit");
    let expected = from_scratch_join(&left, &right);
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::EquiJoin<PairJoin>>()
            .expect("join"),
        expected
    );
    assert_eq!(expected.get(&1), Some(&20));
    assert_eq!(expected.get(&3), Some(&120));

    // One right-side removal: no key is recomputed and the entry disappears.
    right.retain(|(key, _)| *key != 3);
    let before = JOIN_COMBINES.load(std::sync::atomic::Ordering::SeqCst);
    runtime
        .edit(|edit| {
            let mut r = edit.view::<RightTable>();
            let mut entries = r.entries();
            entries.remove(&3);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::EquiJoin<PairJoin>>()
            .expect("join"),
        from_scratch_join(&left, &right)
    );
    assert_eq!(
        JOIN_COMBINES.load(std::sync::atomic::Ordering::SeqCst),
        before,
        "a removal must not recombine surviving keys"
    );

    // One left-side insertion joins exactly one new key.
    left.push((4, 7));
    let before = JOIN_COMBINES.load(std::sync::atomic::Ordering::SeqCst);
    runtime
        .edit(|edit| {
            let mut l = edit.view::<LeftTable>();
            let mut entries = l.entries();
            entries.insert(4, 7);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::EquiJoin<PairJoin>>()
            .expect("join"),
        from_scratch_join(&left, &right)
    );
    assert_eq!(
        JOIN_COMBINES.load(std::sync::atomic::Ordering::SeqCst),
        before + 1,
        "an insertion must combine exactly one key"
    );

    // One left-side value update recomputes only that key.
    left[0].1 = 11;
    let before = JOIN_COMBINES.load(std::sync::atomic::Ordering::SeqCst);
    runtime
        .edit(|edit| {
            let mut l = edit.view::<LeftTable>();
            let mut entries = l.entries();
            entries.insert(1, 11);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::EquiJoin<PairJoin>>()
            .expect("join"),
        from_scratch_join(&left, &right)
    );
    assert_eq!(
        JOIN_COMBINES.load(std::sync::atomic::Ordering::SeqCst),
        before + 1,
        "a value update must combine exactly one key"
    );
}

// ---------------------------------------------------------------------------
// Schedule perturbation and concurrent demand
// ---------------------------------------------------------------------------

#[test]
fn repeated_schedules_produce_identical_results() {
    let mut expected: Option<u64> = None;
    for round in 0..12 {
        let workers = 1 + (round % 4);
        let runtime = Runtime::with_config(irrigation::RuntimeConfig {
            workers,
            stack_size: 32 * 1024 * 1024,
            ..irrigation::RuntimeConfig::default()
        });
        runtime.install(parallel_sum::Sum).expect("install");
        runtime
            .edit(|edit| {
                let mut model = edit.view::<Model>();
                let mut numbers = model.numbers();
                for value in 1..=64u64 {
                    numbers.push(value);
                }
            })
            .expect("edit");
        let value = runtime
            .read_outputs::<parallel_sum::Sum>()
            .expect("outputs");
        match expected {
            None => expected = Some(value),
            Some(expected) => assert_eq!(value, expected, "round {round} diverged"),
        }
    }
    assert_eq!(expected, Some(2 * (1..=64u64).sum::<u64>()));
}

#[test]
fn concurrent_host_reads_share_one_execution() {
    let runtime = Runtime::new();
    runtime.install(model_analysis::Analyze).expect("install");
    runtime
        .edit(|edit| {
            let mut model = edit.view::<Model>();
            let mut numbers = model.numbers();
            numbers.push(1);
            numbers.push(2);
            numbers.push(3);
        })
        .expect("edit");
    // Warm the graph so the body and member nodes are already certified.
    let _ = runtime
        .read_outputs::<model_analysis::Analyze>()
        .expect("warm");
    let before = runtime.stats().executions;

    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..8 {
            let runtime = &runtime;
            handles.push(scope.spawn(move || {
                runtime
                    .read_outputs::<model_analysis::Analyze>()
                    .expect("concurrent read")
            }));
        }
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("join"))
            .collect();
        for result in &results {
            assert_eq!(*result, results[0]);
        }
    });

    // Eight concurrent reads of a certified revision execute no node bodies.
    assert_eq!(runtime.stats().executions, before);
}

// ---------------------------------------------------------------------------
// Reduced sequence View field
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Sum;

impl irrigation::Reduction for Sum {
    type Item = u64;
    type Aggregate = u64;

    fn leaf(item: &Self::Item) -> Self::Aggregate {
        *item
    }

    fn combine(left: &Self::Aggregate, right: &Self::Aggregate) -> Self::Aggregate {
        left + right
    }

    fn identity() -> Self::Aggregate {
        0
    }
}

#[view(source)]
pub struct Totals {
    values: Reduced<Sum>,
}

#[irrigation]
mod totals_reader {
    use super::Totals;

    #[component]
    pub fn total(totals: &Totals) -> u64 {
        let values = totals.values();
        values.aggregate()
    }
}

#[test]
fn reduced_sequence_fields_track_their_aggregate() {
    let runtime = Runtime::new();
    runtime.install(totals_reader::Total).expect("install");
    runtime
        .edit(|edit| {
            let mut totals = edit.view::<Totals>();
            let mut values = totals.values();
            values.push(1);
            values.push(2);
            values.push(3);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<totals_reader::Total>()
            .expect("aggregate"),
        6
    );
    let snapshot = runtime.read().expect("snapshot");
    let view = snapshot.view::<Totals>();
    assert_eq!(view.values(), vec![1, 2, 3]);
    assert_eq!(view.values_aggregate(), 6);

    runtime
        .edit(|edit| {
            let mut totals = edit.view::<Totals>();
            let mut values = totals.values();
            values.set(1, 20);
            values.remove(0);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<totals_reader::Total>()
            .expect("aggregate"),
        23
    );
    let snapshot = runtime.read().expect("snapshot");
    assert_eq!(snapshot.view::<Totals>().values(), vec![20, 3]);
    assert_eq!(snapshot.view::<Totals>().values_aggregate(), 23);
}

/// A reduced sequence whose only definition names a position it does not have.
#[view(derived)]
pub struct OutOfRangeTotals(Reduced<Sum>);

/// Publishes a reduced sequence and one definition named by a position the
/// sequence cannot have.
///
/// `ViewField::define` is total over keys in every sibling family: a positional
/// write past the end is ignored, not an abort. A panic here would unwind the
/// install phase after the View's region had been detached.
struct ReducedOutOfRange;

impl irrigation::GeneralComponent for ReducedOutOfRange {
    type State = ();
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00C0_0030);
    const NAME: &'static str = "tests::ReducedOutOfRange";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<OutOfRangeTotals as irrigation::ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> irrigation::DependencyUpdates {
        irrigation::DependencyUpdates::Reset
    }

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let view = <OutOfRangeTotals as irrigation::ViewSchema>::DESCRIPTOR.id;
        let mut sequence = irrigation::AggSeq::new(
            <Sum as irrigation::Reduction>::leaf,
            <Sum as irrigation::Reduction>::combine,
            0u64,
        );
        for value in [1u64, 2, 3] {
            sequence = sequence.push(value);
        }
        cx.define(
            irrigation::UnitKey::field(view, irrigation::FieldId(0), irrigation::UnitKind::Value),
            sequence,
            "sequence",
        );
        cx.define(
            irrigation::UnitKey::entity(
                view,
                irrigation::FieldId(0),
                irrigation::UnitKind::Value,
                irrigation::any_key(9u64),
            ),
            42u64,
            "out_of_range",
        );
        Ok(((), ()))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

/// Reads the position the producer could not place, then the sequence behind it.
struct OutOfRangeReader;

impl irrigation::GeneralComponent for OutOfRangeReader {
    type State = ();
    type Output = (u64, usize);

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00C0_0031);
    const NAME: &'static str = "tests::OutOfRangeReader";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<OutOfRangeTotals as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[];

    fn dependency_updates(&self) -> irrigation::DependencyUpdates {
        irrigation::DependencyUpdates::Reset
    }

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        // Reading a unit of the View demands its producer, so this is what runs
        // the install phase that carries the out-of-range definition.
        let sequence: irrigation::AggSeq<u64, u64> = cx.read_typed(irrigation::UnitKey::field(
            <OutOfRangeTotals as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        Ok(((), (sequence.aggregate(), sequence.len())))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn an_out_of_range_reduced_definition_is_ignored() {
    let runtime = Runtime::new();
    runtime.register_view(<OutOfRangeTotals as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_producer(ReducedOutOfRange)
        .expect("install");
    runtime
        .install_general_root(OutOfRangeReader)
        .expect("reader");
    assert_eq!(
        runtime
            .read_outputs_general::<OutOfRangeReader>()
            .expect("an out-of-range definition must not abort the install"),
        (6, 3)
    );
    // The View's region is still installed: a later demand re-reads it instead
    // of failing on a region the install phase dropped.
    assert_eq!(
        runtime
            .read_outputs_general::<OutOfRangeReader>()
            .expect("reread"),
        (6, 3)
    );
}

#[test]
fn reduced_edits_report_preconditions() {
    use irrigation_core::ops::catch_engine;

    let runtime = Runtime::new();
    runtime.install(totals_reader::Total).expect("install");
    runtime
        .edit(|edit| {
            let mut totals = edit.view::<Totals>();
            let mut values = totals.values();
            values.push(1);
        })
        .expect("seed");

    let error = catch_engine(|| {
        runtime
            .edit(|edit| {
                let mut totals = edit.view::<Totals>();
                let mut values = totals.values();
                values.insert(9, 2);
            })
            .expect("edit");
    })
    .expect_err("an insert past the end must fail before it touches storage");
    assert!(
        matches!(
            error,
            irrigation::ComputeError::PreconditionUnmet { ref condition, .. }
                if condition.contains("reduced index 9 in range")
        ),
        "expected a precondition failure, got {error:?}"
    );

    let error = catch_engine(|| {
        runtime
            .edit(|edit| {
                let mut totals = edit.view::<Totals>();
                let mut values = totals.values();
                values.set(4, 2);
            })
            .expect("edit");
    })
    .expect_err("a set past the end must fail before it touches storage");
    assert!(
        matches!(
            error,
            irrigation::ComputeError::PreconditionUnmet { ref condition, .. }
                if condition.contains("reduced index 4 present")
        ),
        "expected a precondition failure, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Cross-thread single-flight under contention
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Seeds {
    values: List<u32>,
}

#[irrigation]
mod contention {
    use super::Seeds;

    #[component]
    pub fn square(n: u32) -> u64 {
        u64::from(n) * u64::from(n)
    }

    #[component]
    pub fn total(seeds: &Seeds) -> u64 {
        let values = seeds.values();
        let mapped = values.map(|n| square(*n));
        mapped.iter().sum::<u64>()
    }
}

#[test]
fn parallel_members_share_one_invocation_without_deadlock() {
    for round in 0..8 {
        let workers = 2 + (round % 3);
        let runtime = Runtime::with_config(irrigation::RuntimeConfig {
            workers,
            stack_size: 32 * 1024 * 1024,
            ..irrigation::RuntimeConfig::default()
        });
        runtime.install(contention::Total).expect("install");
        runtime
            .edit(|edit| {
                let mut seeds = edit.view::<Seeds>();
                let mut values = seeds.values();
                for _ in 0..32 {
                    values.push(5);
                }
            })
            .expect("edit");
        let total = runtime
            .read_outputs::<contention::Total>()
            .expect("outputs");
        assert_eq!(total, 32 * 25, "round {round} with {workers} workers");
    }
}

// ---------------------------------------------------------------------------
// A parallel list map inherits the demand it runs under
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};

static MAP_BLOCKED: OnceLock<Mutex<mpsc::Sender<()>>> = OnceLock::new();
static MAP_RELEASE: OnceLock<Mutex<mpsc::Receiver<()>>> = OnceLock::new();
static MAP_RAN_AFTER_BLOCK: AtomicUsize = AtomicUsize::new(0);

fn map_reached_the_block() {
    if let Some(blocked) = MAP_BLOCKED.get() {
        let _ = blocked.lock().expect("blocked").send(());
    }
}

fn map_waits_for_release() {
    if let Some(release) = MAP_RELEASE.get() {
        let _ = release.lock().expect("release").recv();
    }
}

#[irrigation]
mod parallel_cancel_map {
    use std::sync::atomic::Ordering;

    use super::Model;

    #[component]
    pub fn root(model: &Model) -> u64 {
        let numbers = model.numbers();
        let mapped = numbers.map(|n| {
            if *n == 0 {
                // The first member of the first chunk blocks until the demand
                // is cancelled. Its siblings in that chunk run only if the
                // cancellation never reaches them.
                super::map_reached_the_block();
                super::map_waits_for_release();
            } else if *n < 4 {
                super::MAP_RAN_AFTER_BLOCK.fetch_add(1, Ordering::Relaxed);
            }
            *n
        });
        mapped.iter().sum::<u64>()
    }
}

#[test]
fn cancelling_a_parallel_map_stops_its_remaining_members() {
    let (blocked_tx, blocked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let _ = MAP_BLOCKED.set(Mutex::new(blocked_tx));
    let _ = MAP_RELEASE.set(Mutex::new(release_rx));
    MAP_RAN_AFTER_BLOCK.store(0, Ordering::Relaxed);

    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        stack_size: 8 * 1024 * 1024,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.install(parallel_cancel_map::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut model = edit.view::<Model>();
            let mut numbers = model.numbers();
            for value in 0..8u64 {
                numbers.push(value);
            }
        })
        .expect("edit");

    let token = irrigation::CancelToken::new(1);
    let reader_token = token.clone();
    let reader = std::thread::spawn(move || runtime.read_with_cancel(reader_token));
    blocked_rx.recv().expect("the first member blocked");
    token.cancel();
    release_tx.send(()).expect("release");

    let result = reader.join().expect("reader thread");
    let error = result.expect_err("a cancelled demand cannot publish");
    assert!(error.is_cancelled(), "expected cancellation, got {error:?}");
    assert_eq!(
        MAP_RAN_AFTER_BLOCK.load(Ordering::Relaxed),
        0,
        "a cancelled demand must stop the members after the one that observed it"
    );
}
#[test]
fn snapshots_can_be_taken_without_a_root() {
    let runtime = Runtime::new();
    runtime.register_view(<Table as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| {
            let mut table = edit.view::<Table>();
            let mut entries = table.entries();
            entries.insert(7, 70);
        })
        .expect("edit");
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.view::<Table>().entries(), vec![(7, 70)]);
    assert_eq!(snapshot.revision(), runtime.revision());
}

#[test]
fn explanations_name_the_dependency_that_moved() {
    let runtime = Runtime::new();
    runtime.install(branching::Pick).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Toggle>();
            view.set_flag(true);
            view.set_left(10);
        })
        .expect("edit");
    let _ = runtime.read_outputs::<branching::Pick>().expect("warm");
    runtime
        .edit(|edit| {
            edit.view::<Toggle>().set_left(11);
        })
        .expect("edit");
    // Explain the root body before repairing it: the observation that moved in
    // the last transaction is visible as changed or suspect.
    let root = runtime
        .dump()
        .into_iter()
        .find(|node| node.kind == "InvocationBody" && node.label == "root body")
        .expect("root body");
    let mut explanation = runtime.explain_node(root.id);
    assert!(!explanation.dependencies.is_empty());
    assert!(explanation.suspect_at > explanation.verified_at);
    // A control-flow statement is its own node, so the root can legitimately
    // have a single (moved) dependency. Descend to it to observe the fine
    // dependencies of the switched branch.
    if explanation
        .dependencies
        .iter()
        .all(|dependency| dependency.changed || dependency.suspect)
    {
        let inner = explanation
            .dependencies
            .iter()
            .find(|dependency| dependency.suspect || dependency.changed)
            .expect("a moved dependency")
            .node;
        explanation = runtime.explain_node(inner);
    }
    let moved: Vec<_> = explanation
        .dependencies
        .iter()
        .filter(|dependency| dependency.changed || dependency.suspect)
        .collect();
    assert!(
        !moved.is_empty(),
        "the affected observations must be visible: {explanation:?}"
    );
    assert!(
        moved.len() < explanation.dependencies.len(),
        "unaffected observations must stay certified: {explanation:?}"
    );

    let _ = runtime.read_outputs::<branching::Pick>().expect("read");
}

// ---------------------------------------------------------------------------
// Async GeneralComponent driver
// ---------------------------------------------------------------------------

struct AsyncTracker;

impl irrigation::AsyncGeneralComponent for AsyncTracker {
    type State = u64;
    type Output = u64;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00AA_0003);
    const NAME: &'static str = "tests::AsyncTracker";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<Counter as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize<'a>(
        &'a self,
        cx: &'a mut irrigation::GeneralCx,
    ) -> irrigation::ComponentFuture<'a, (Self::State, Self::Output)> {
        Box::pin(async move {
            let value: u64 = cx.read_typed(counter_unit())?;
            Ok((value, value * 2))
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a mut irrigation::GeneralCx,
        state: &'a Self::State,
        changes: &'a irrigation::ChangeBatch,
    ) -> irrigation::ComponentFuture<'a, (Self::State, Self::Output)> {
        Box::pin(async move {
            let mut value = *state;
            for change in changes.changes() {
                value = cx.read_typed(change.unit.clone())?;
            }
            Ok((value, value * 2))
        })
    }
}

#[test]
fn async_components_are_driven_to_completion() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(irrigation::AsyncAdapter::new(AsyncTracker))
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::AsyncAdapter<AsyncTracker>>()
            .expect("outputs"),
        0
    );
    runtime
        .edit(|edit| {
            edit.view::<Counter>().set_value(6);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::AsyncAdapter<AsyncTracker>>()
            .expect("outputs"),
        12
    );
}

// ---------------------------------------------------------------------------
// Retired dependency scopes
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Switch {
    flag: bool,
    left: u64,
    right: u64,
}

fn switch_unit(field: u16) -> irrigation::UnitKey {
    irrigation::UnitKey::field(
        <Switch as irrigation::ViewSchema>::DESCRIPTOR.id,
        irrigation::FieldId(field),
        irrigation::UnitKind::Value,
    )
}

/// Reads either `left` or `right` depending on `flag`, and switches its
/// observed unit when the flag changes.
struct Selector;

impl irrigation::GeneralComponent for Selector {
    type State = (bool, u64);
    type Output = u64;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00AA_0004);
    const NAME: &'static str = "tests::Selector";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<Switch as irrigation::ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> irrigation::DependencyUpdates {
        irrigation::DependencyUpdates::Incremental
    }

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let flag: bool = cx.read_typed(switch_unit(0))?;
        let value: u64 = if flag {
            cx.read_typed(switch_unit(1))?
        } else {
            cx.read_typed(switch_unit(2))?
        };
        Ok(((flag, value), value))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        state: &Self::State,
        changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let mut flag = state.0;
        let mut value = state.1;
        for change in changes.changes() {
            if change.unit == switch_unit(0) {
                flag = cx.read_typed(switch_unit(0))?;
                value = if flag {
                    cx.read_typed(switch_unit(1))?
                } else {
                    cx.read_typed(switch_unit(2))?
                };
            } else if change.unit == switch_unit(1) && flag {
                value = cx.read_typed(switch_unit(1))?;
            } else if change.unit == switch_unit(2) && !flag {
                value = cx.read_typed(switch_unit(2))?;
            }
        }
        Ok(((flag, value), value))
    }
}

#[test]
fn a_switched_scope_retires_its_old_dependency() {
    let runtime = Runtime::new();
    runtime.install_general_root(Selector).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Switch>();
            view.set_flag(false);
            view.set_left(10);
            view.set_right(20);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs_general::<Selector>().expect("outputs"),
        20
    );

    // Editing the retired side must not execute the component.
    let before = runtime.stats().executions;
    runtime
        .edit(|edit| {
            edit.view::<Switch>().set_left(11);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs_general::<Selector>().expect("outputs"),
        20
    );
    assert_eq!(runtime.stats().executions, before);

    // Switching the flag re-reads the newly selected side.
    runtime
        .edit(|edit| {
            edit.view::<Switch>().set_flag(true);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs_general::<Selector>().expect("outputs"),
        11
    );

    // Now the other side is retired.
    let before = runtime.stats().executions;
    runtime
        .edit(|edit| {
            edit.view::<Switch>().set_right(21);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs_general::<Selector>().expect("outputs"),
        11
    );
    assert_eq!(runtime.stats().executions, before);
}

// ---------------------------------------------------------------------------
// Cutoff must not retain the old dependency graph
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Choice {
    flag: bool,
    left: u64,
    right: u64,
}

#[irrigation]
mod constant_branch {
    use super::Choice;

    #[component]
    pub fn choose(choice: &Choice) -> u64 {
        if choice.flag() {
            let _ = choice.left();
            1
        } else {
            let _ = choice.right();
            1
        }
    }
}

#[test]
fn an_equal_output_still_replaces_the_dependency_trace() {
    let runtime = Runtime::new();
    runtime.install(constant_branch::Choose).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Choice>();
            view.set_flag(false);
            view.set_left(1);
            view.set_right(2);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<constant_branch::Choose>()
            .expect("outputs"),
        1
    );

    // Switch branches: the output stays 1, so cutoff suppresses a value event,
    // but the trace must now point at the left branch.
    runtime
        .edit(|edit| {
            edit.view::<Choice>().set_flag(true);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<constant_branch::Choose>()
            .expect("outputs"),
        1
    );

    // Editing the newly active branch must execute the body again.
    let before = runtime.stats().executions;
    runtime
        .edit(|edit| {
            edit.view::<Choice>().set_left(9);
        })
        .expect("edit");
    let _ = runtime
        .read_outputs::<constant_branch::Choose>()
        .expect("outputs");
    assert!(
        runtime.stats().executions > before,
        "the active branch was not tracked after cutoff"
    );
}

#[test]
fn observers_receive_the_changed_view_version() {
    use std::sync::{Arc, Mutex};
    let runtime = Runtime::new();
    runtime.install(model_analysis::Analyze).expect("install");
    runtime
        .edit(|edit| {
            let mut model = edit.view::<Model>();
            let mut numbers = model.numbers();
            numbers.push(1);
        })
        .expect("edit");
    let seen: Arc<Mutex<Vec<irrigation::ChangeSummary>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let subscription = runtime
        .observe_with_changes::<Model>(move |_snapshot, changes| {
            sink.lock().expect("lock").push(*changes);
        })
        .expect("observe");

    runtime
        .edit(|edit| {
            let mut model = edit.view::<Model>();
            let mut numbers = model.numbers();
            numbers.push(2);
        })
        .expect("edit");

    let summaries = seen.lock().expect("lock");
    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries[0].view,
        <Model as irrigation::ViewSchema>::view_id()
    );
    assert!(summaries[0].current > summaries[0].previous);
    drop(summaries);
    drop(subscription);
}

// ---------------------------------------------------------------------------
// Dynamic definition collisions
// ---------------------------------------------------------------------------

#[view(source)]
pub struct KeyInput {
    key: u32,
}

#[view(derived)]
pub struct CollisionRegistry(Map<u32, u32>);

#[irrigation]
mod dynamic_collision {
    use super::{CollisionRegistry, KeyInput};

    #[component]
    pub fn provider_a(input: &KeyInput) -> CollisionRegistry {
        let key = input.key();
        CollisionRegistry::from([(key, 1u32)])
    }

    #[component]
    pub fn provider_b() -> CollisionRegistry {
        CollisionRegistry::from([(7u32, 2u32)])
    }

    #[component]
    pub fn root(input: &KeyInput) -> u32 {
        let _ = provider_a(input);
        let _ = provider_b();
        0
    }
}

#[test]
fn a_dynamic_expansion_that_collides_is_diagnosed() {
    let runtime = Runtime::new();
    runtime.install(dynamic_collision::Root).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<KeyInput>().set_key(1);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<dynamic_collision::Root>()
            .expect("disjoint keys"),
        0
    );

    // The dispatcher now routes both providers at key 7.
    runtime
        .edit(|edit| {
            edit.view::<KeyInput>().set_key(7);
        })
        .expect("edit");
    let error = runtime
        .read_outputs::<dynamic_collision::Root>()
        .unwrap_err();
    assert!(
        matches!(error, irrigation::ComputeError::DuplicateDefinition { .. }),
        "expected a duplicate-definition error, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Panics abort one execution scope without poisoning the runtime
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Bomb {
    armed: bool,
}

fn explode() -> u64 {
    panic!("boom");
}

#[irrigation]
mod bomb {
    use super::{Bomb, explode};

    #[component]
    pub fn risky(bomb: &Bomb) -> u64 {
        if bomb.armed() { explode() } else { 7 }
    }
}

#[test]
fn a_panic_fails_one_scope_and_the_next_revision_recovers() {
    let runtime = Runtime::new();
    runtime.install(bomb::Risky).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<Bomb>().set_armed(true);
        })
        .expect("edit");
    let error = runtime.read_outputs::<bomb::Risky>().unwrap_err();
    assert!(
        matches!(error, irrigation::ComputeError::Panicked { .. }),
        "expected a panic error, got {error:?}"
    );

    // The single-flight slot is released and the failure is not permanent.
    runtime
        .edit(|edit| {
            edit.view::<Bomb>().set_armed(false);
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<bomb::Risky>().expect("recovers"), 7);
}

#[test]
fn runtimes_are_isolated_namespaces() {
    let first = Runtime::new();
    first.install(model_analysis::Analyze).expect("install a");
    let second = Runtime::new();
    second.install(model_analysis::Analyze).expect("install b");

    first
        .edit(|edit| {
            let mut model = edit.view::<Model>();
            let mut numbers = model.numbers();
            numbers.push(1);
        })
        .expect("edit a");
    second
        .edit(|edit| {
            let mut model = edit.view::<Model>();
            let mut numbers = model.numbers();
            numbers.push(2);
            numbers.push(3);
        })
        .expect("edit b");

    assert_eq!(
        first.read_outputs::<model_analysis::Analyze>().expect("a"),
        2
    );
    assert_eq!(
        second.read_outputs::<model_analysis::Analyze>().expect("b"),
        10
    );
    // Revisions are per-runtime, not process-global.
    assert_eq!(first.revision(), 1);
    assert_eq!(second.revision(), 1);
}

// ---------------------------------------------------------------------------
// Result values and `?` are ordinary tracked data
// ---------------------------------------------------------------------------

#[view(source)]
pub struct TextInput {
    text: String,
}

fn parse_number(text: &str) -> Result<u64, String> {
    text.parse::<u64>().map_err(|error| error.to_string())
}

#[irrigation]
mod parser {
    use super::{TextInput, parse_number};

    #[component]
    pub fn parse_value(input: &TextInput) -> Result<u64, String> {
        let text = input.text();
        let value = parse_number(&text)?;
        Ok(value * 2)
    }
}

#[test]
fn result_control_flow_is_tracked_like_any_other_value() {
    let runtime = Runtime::new();
    runtime.install(parser::ParseValue).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<TextInput>().set_text(String::from("21"));
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<parser::ParseValue>()
            .expect("outputs"),
        Ok(42)
    );

    runtime
        .edit(|edit| {
            edit.view::<TextInput>().set_text(String::from("nope"));
        })
        .expect("edit");
    let result = runtime
        .read_outputs::<parser::ParseValue>()
        .expect("outputs");
    assert!(result.is_err());

    runtime
        .edit(|edit| {
            edit.view::<TextInput>().set_text(String::from("5"));
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<parser::ParseValue>()
            .expect("outputs"),
        Ok(10)
    );
}

// ---------------------------------------------------------------------------
// Supported composition: the branch chooses values, the definition always runs
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ModeFlag {
    strict: bool,
}

#[view(derived)]
pub struct ModeReport(Map<u32, u32>);

#[irrigation]
mod mode_report {
    use super::{ModeFlag, ModeReport};

    #[component]
    pub fn report(flag: &ModeFlag) -> ModeReport {
        // The routing decision chooses a value; the definition itself is staged
        // on every execution, so a branch switch can never leave a stale unit.
        let value = if flag.strict() { 100 } else { 200 };
        ModeReport::from([(1u32, value)])
    }
}

#[test]
fn branch_chosen_values_republish_every_execution() {
    let runtime = Runtime::new();
    runtime.install(mode_report::Report).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<ModeFlag>().set_strict(false);
        })
        .expect("edit");
    let _ = runtime
        .read_outputs::<mode_report::Report>()
        .expect("outputs");
    let snapshot = runtime.read().expect("snapshot");
    assert_eq!(snapshot.view::<ModeReport>().field0(), vec![(1, 200)]);

    for (strict, expected) in [(true, 100), (false, 200), (true, 100)] {
        runtime
            .edit(|edit| {
                edit.view::<ModeFlag>().set_strict(strict);
            })
            .expect("edit");
        let _ = runtime
            .read_outputs::<mode_report::Report>()
            .expect("outputs");
        let snapshot = runtime.read().expect("snapshot");
        assert_eq!(
            snapshot.view::<ModeReport>().field0(),
            vec![(1, expected)],
            "strict={strict}"
        );
    }
}

// ---------------------------------------------------------------------------
// Branch-scoped definitions are retired with their branch
// ---------------------------------------------------------------------------

#[view(source)]
pub struct BranchFlag {
    on: bool,
}

#[view(derived)]
pub struct BranchSlot(Map<u32, u32>);

#[irrigation]
mod branch_scope {
    use super::{BranchFlag, BranchSlot};

    #[component]
    pub fn root(flag: &BranchFlag) -> u32 {
        if flag.on() {
            let _ = BranchSlot::from([(1u32, 10u32)]);
        } else {
            let _ = BranchSlot::from([]);
        }
        0
    }
}

#[test]
fn a_definition_inside_an_untaken_branch_is_retracted() {
    let runtime = Runtime::new();
    runtime.install(branch_scope::Root).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<BranchFlag>().set_on(true);
        })
        .expect("edit");
    let _ = runtime
        .read_outputs::<branch_scope::Root>()
        .expect("outputs");
    assert_eq!(
        runtime
            .read()
            .expect("snapshot")
            .view::<BranchSlot>()
            .field0(),
        vec![(1, 10)]
    );

    runtime
        .edit(|edit| {
            edit.view::<BranchFlag>().set_on(false);
        })
        .expect("edit");
    let _ = runtime
        .read_outputs::<branch_scope::Root>()
        .expect("outputs");
    assert_eq!(
        runtime
            .read()
            .expect("snapshot")
            .view::<BranchSlot>()
            .field0(),
        Vec::<(u32, u32)>::new(),
        "the retired branch's definition must be retracted"
    );

    runtime
        .edit(|edit| {
            edit.view::<BranchFlag>().set_on(true);
        })
        .expect("edit");
    let _ = runtime
        .read_outputs::<branch_scope::Root>()
        .expect("outputs");
    assert_eq!(
        runtime
            .read()
            .expect("snapshot")
            .view::<BranchSlot>()
            .field0(),
        vec![(1, 10)],
        "the branch restages its definition when reachable again"
    );
}

// ---------------------------------------------------------------------------
// Provider handover resolved at the end of the revision
// ---------------------------------------------------------------------------

#[view(source)]
pub struct HandoverFlag {
    on: bool,
}

#[view(derived)]
pub struct HandoverSlot(Map<u32, u32>);

#[irrigation]
mod handover {
    use super::{HandoverFlag, HandoverSlot};

    #[component]
    pub fn provider_a(flag: &HandoverFlag) -> HandoverSlot {
        if flag.on() {
            HandoverSlot::from([])
        } else {
            HandoverSlot::from([(1u32, 10u32)])
        }
    }

    #[component]
    pub fn provider_b(flag: &HandoverFlag) -> HandoverSlot {
        if flag.on() {
            HandoverSlot::from([(1u32, 20u32)])
        } else {
            HandoverSlot::from([])
        }
    }

    #[component]
    pub fn root(flag: &HandoverFlag) -> u32 {
        // The incoming provider is demanded first.
        let _ = provider_b(flag);
        let _ = provider_a(flag);
        0
    }
}

#[test]
fn provider_handover_survives_demand_order() {
    let runtime = Runtime::new();
    runtime.install(handover::Root).expect("install");
    for (on, expected) in [(false, 10u32), (true, 20), (false, 10), (true, 20)] {
        runtime
            .edit(|edit| {
                edit.view::<HandoverFlag>().set_on(on);
            })
            .expect("edit");
        let _ = runtime.read_outputs::<handover::Root>().expect("outputs");
        let snapshot = runtime.read().expect("snapshot");
        assert_eq!(
            snapshot.view::<HandoverSlot>().field0(),
            vec![(1, expected)],
            "on={on}"
        );
    }
}

#[test]
fn a_small_pool_completes_contended_host_demand() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        stack_size: 16 * 1024 * 1024,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.install(model_analysis::Analyze).expect("install");
    runtime
        .edit(|edit| {
            let mut model = edit.view::<Model>();
            let mut numbers = model.numbers();
            for value in 1..=32u64 {
                numbers.push(value);
            }
        })
        .expect("edit");

    for round in 0..6 {
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..6 {
                let runtime = &runtime;
                handles.push(scope.spawn(move || {
                    runtime
                        .read_outputs::<model_analysis::Analyze>()
                        .expect("contended read")
                }));
            }
            let results: Vec<u64> = handles
                .into_iter()
                .map(|handle| handle.join().expect("join"))
                .collect();
            for result in &results {
                assert_eq!(*result, results[0], "round {round}");
            }
        });
    }
    assert_eq!(
        runtime
            .read_outputs::<model_analysis::Analyze>()
            .expect("final"),
        2 * (1..=32u64).sum::<u64>()
    );
}

// ---------------------------------------------------------------------------
// Glitch freedom: a diamond never mixes revisions
// ---------------------------------------------------------------------------

#[view(source)]
pub struct DiamondInput {
    value: u64,
}

#[irrigation]
mod diamond {
    use super::DiamondInput;

    #[component]
    pub fn doubled(input: &DiamondInput) -> u64 {
        input.value() * 2
    }

    #[component]
    pub fn parity(input: &DiamondInput) -> u64 {
        input.value() % 2
    }

    #[component]
    pub fn combine(input: &DiamondInput) -> (u64, u64) {
        let doubled = doubled(input);
        let parity = parity(input);
        (doubled, parity)
    }
}

#[test]
fn a_diamond_tuple_never_mixes_revisions() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        stack_size: 16 * 1024 * 1024,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.install(diamond::Combine).expect("install");
    // (value, (doubled, parity)): `doubled` changes every step; `parity` holds
    // at 0 for 2->4 and changes at 5. A consumer that combined a new `doubled`
    // with a stale `parity` would fail here.
    for (value, expected) in [
        (2, (4, 0)),
        (4, (8, 0)),
        (5, (10, 1)),
        (6, (12, 0)),
        (6, (12, 0)),
    ] {
        runtime
            .edit(|edit| {
                edit.view::<DiamondInput>().set_value(value);
            })
            .expect("edit");
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let runtime = &runtime;
                    scope
                        .spawn(move || runtime.read_outputs::<diamond::Combine>().expect("outputs"))
                })
                .collect();
            for handle in handles {
                assert_eq!(handle.join().expect("join"), expected, "value={value}");
            }
        });
    }
}

#[test]
fn a_provider_handover_updates_its_observer() {
    type Observed = Vec<Vec<(u32, u32)>>;
    use std::sync::{Arc, Mutex};
    let runtime = Runtime::new();
    runtime.install(handover::Root).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<HandoverFlag>().set_on(false);
        })
        .expect("edit");
    let _ = runtime
        .read_outputs::<handover::Root>()
        .expect("initial demand");

    let seen: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let subscription = runtime
        .observe::<HandoverSlot>(move |snapshot| {
            sink.lock()
                .expect("lock")
                .push(snapshot.view::<HandoverSlot>().field0());
        })
        .expect("observe");

    for (on, expected) in [(true, 20u32), (false, 10), (true, 20)] {
        runtime
            .edit(|edit| {
                edit.view::<HandoverFlag>().set_on(on);
            })
            .expect("edit");
        let snapshot = runtime.read().expect("snapshot");
        assert_eq!(
            snapshot.view::<HandoverSlot>().field0(),
            vec![(1, expected)],
            "on={on}"
        );
    }
    let observed = seen.lock().expect("lock");
    assert_eq!(
        observed.last(),
        Some(&vec![(1u32, 20u32)]),
        "the observer never saw the handed-over member: {observed:?}"
    );
    drop(observed);
    drop(subscription);
}

// ---------------------------------------------------------------------------
// Derived list live reads (N1)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct DerivedListFlag {
    on: bool,
}

#[view(derived)]
pub struct DerivedSlot(List<u64>);

#[irrigation]
mod derived_list_provider {
    use super::{DerivedListFlag, DerivedSlot};

    #[component]
    pub fn provider(flag: &DerivedListFlag) -> u64 {
        if flag.on() {
            let _ = DerivedSlot::from(vec![1u64, 2, 3]);
            1
        } else {
            let _ = DerivedSlot::from(vec![4u64]);
            2
        }
    }

    #[component]
    pub fn root(flag: &DerivedListFlag) -> (u64, Vec<u64>, usize, Option<u64>) {
        let marker = provider(flag);
        let slot = DerivedSlot;
        (marker, slot.to_vec(), slot.len(), slot.get(2))
    }
}

#[test]
fn derived_list_live_reads_match_the_snapshot() {
    let runtime = Runtime::new();
    runtime
        .install(derived_list_provider::Root)
        .expect("install");
    runtime
        .edit(|edit| edit.view::<DerivedListFlag>().set_on(true))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<derived_list_provider::Root>()
            .expect("live read"),
        (1, vec![1, 2, 3], 3, Some(3))
    );
    assert_eq!(
        runtime.snapshot().view::<DerivedSlot>().field0(),
        vec![1, 2, 3]
    );

    runtime
        .edit(|edit| edit.view::<DerivedListFlag>().set_on(false))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<derived_list_provider::Root>()
            .expect("live read"),
        (2, vec![4], 1, None)
    );
    assert_eq!(runtime.snapshot().view::<DerivedSlot>().field0(), vec![4]);
}

// ---------------------------------------------------------------------------
// Edit outcome vs observer refresh (N4, F3)
// ---------------------------------------------------------------------------

/// A general root whose demand always fails.
struct BrokenRoot;

impl irrigation::GeneralComponent for BrokenRoot {
    type State = u64;
    type Output = u64;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00DD_0001);
    const NAME: &'static str = "gates::BrokenRoot";

    fn initialize(
        &self,
        _cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        Err(irrigation::ComputeError::internal("deliberate failure"))
    }

    fn update(
        &self,
        _cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        unreachable!("the failing root never updates")
    }
}

#[test]
fn a_source_only_edit_notifies_its_observer() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runtime = Runtime::new();
    runtime
        .edit(|edit| edit.view::<DerivedListFlag>().set_on(false))
        .expect("seed");
    let seen = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&seen);
    let subscription = runtime
        .observe::<DerivedListFlag>(move |_snapshot| {
            sink.fetch_add(1, Ordering::SeqCst);
        })
        .expect("observe");
    runtime
        .edit(|edit| edit.view::<DerivedListFlag>().set_on(true))
        .expect("edit");
    assert_eq!(seen.load(Ordering::SeqCst), 1, "the observer never fired");
    assert!(runtime.notification_error().is_none());
    drop(subscription);
}

#[test]
fn an_edit_is_not_failed_by_a_failing_root() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(BrokenRoot)
        .expect("install general");
    runtime
        .edit(|edit| edit.view::<DerivedListFlag>().set_on(false))
        .expect("seed");
    let subscription = runtime
        .observe::<DerivedListFlag>(|_snapshot| {})
        .expect("observe");
    let result = runtime.edit(|edit| edit.view::<DerivedListFlag>().set_on(true));
    assert!(result.is_ok(), "a committed edit must not fail: {result:?}");
    assert!(
        runtime.notification_error().is_some(),
        "the failed refresh must be reported"
    );
    assert!(runtime.snapshot().view::<DerivedListFlag>().on());
    drop(subscription);
}

// ---------------------------------------------------------------------------
// Whole-field definition retraction (N2)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct RetractFlag {
    on: bool,
}

#[view(derived)]
pub struct Conditional(u64);

#[irrigation]
mod conditional_definition {
    use super::{Conditional, RetractFlag};

    #[component]
    pub fn provider(flag: &RetractFlag) -> u64 {
        if flag.on() {
            let total = Conditional;
            total.define_field0(42);
            1
        } else {
            2
        }
    }

    #[component]
    pub fn root(flag: &RetractFlag) -> (u64, u64) {
        let marker = provider(flag);
        let total = Conditional;
        (marker, total.field0())
    }
}

#[test]
fn a_retracted_whole_field_definition_reverts_to_empty() {
    let runtime = Runtime::new();
    runtime
        .install(conditional_definition::Root)
        .expect("install");
    for (on, expected) in [(true, (1, 42)), (false, (2, 0)), (true, (1, 42))] {
        runtime
            .edit(|edit| edit.view::<RetractFlag>().set_on(on))
            .expect("edit");
        assert_eq!(
            runtime
                .read_outputs::<conditional_definition::Root>()
                .expect("read"),
            expected,
            "on={on}"
        );
        assert_eq!(
            runtime.snapshot().view::<Conditional>().field0(),
            expected.1,
            "snapshot must agree with the live read (on={on})"
        );
    }
}

// ---------------------------------------------------------------------------
// Parallel operator error preservation (N7)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ParallelNumbers {
    values: List<u64>,
}

#[irrigation]
mod parallel_errors {
    use super::ParallelNumbers;

    #[component]
    pub fn recursive(tag: u64) -> u64 {
        recursive(tag)
    }

    #[component]
    pub fn root(numbers: &ParallelNumbers) -> u64 {
        let values = numbers.values();
        values.map(|value| recursive(*value)).iter().sum()
    }
}

#[test]
fn a_cycle_inside_a_parallel_member_stays_a_cycle() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        stack_size: 16 * 1024 * 1024,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.install(parallel_errors::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut numbers = edit.view::<ParallelNumbers>();
            let mut values = numbers.values();
            for value in 0..16u64 {
                values.push(value);
            }
        })
        .expect("seed");
    match runtime.read_outputs::<parallel_errors::Root>() {
        Err(irrigation::ComputeError::Cycle { path }) => {
            assert!(
                path.iter()
                    .any(|step| step.contains("parallel_errors::recursive")),
                "the cycle path must name the recursive invocation: {path:?}"
            );
        }
        other => panic!("expected a cycle, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A failed definition install is atomic (F5)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct AuditFlag {
    on: bool,
}

#[view(derived)]
pub struct AuditOuter(u64);

#[view(derived)]
pub struct AuditMissing(u64);

fn audit_flag_unit() -> irrigation::UnitKey {
    irrigation::UnitKey::field(
        <AuditFlag as irrigation::ViewSchema>::DESCRIPTOR.id,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
    )
}

fn audit_outer_unit() -> irrigation::UnitKey {
    irrigation::UnitKey::field(
        <AuditOuter as irrigation::ViewSchema>::DESCRIPTOR.id,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
    )
}

fn audit_missing_unit() -> irrigation::UnitKey {
    irrigation::UnitKey::field(
        <AuditMissing as irrigation::ViewSchema>::DESCRIPTOR.id,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
    )
}

struct InstallAudit;

impl irrigation::GeneralComponent for InstallAudit {
    type State = u64;
    type Output = u64;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00DD_0002);
    const NAME: &'static str = "gates::InstallAudit";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[
        <AuditFlag as irrigation::ViewSchema>::DESCRIPTOR,
        <AuditOuter as irrigation::ViewSchema>::DESCRIPTOR,
    ];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let on: bool = cx.read_typed(audit_flag_unit())?;
        if on {
            cx.define(audit_outer_unit(), 7u64, "outer");
        } else {
            // This view is not registered: the whole install must be rejected
            // without retracting the `on` definition above.
            cx.define(audit_missing_unit(), 9u64, "missing");
        }
        Ok((0, 0))
    }

    fn update(
        &self,
        _cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        unreachable!("the audit component resets")
    }
}

#[test]
fn a_failed_definition_install_is_atomic() {
    let runtime = Runtime::new();
    runtime.install_general_root(InstallAudit).expect("install");
    runtime
        .edit(|edit| edit.view::<AuditFlag>().set_on(true))
        .expect("edit");
    assert!(runtime.read_outputs_general::<InstallAudit>().is_ok());
    assert_eq!(runtime.snapshot().view::<AuditOuter>().field0(), 7);

    runtime
        .edit(|edit| edit.view::<AuditFlag>().set_on(false))
        .expect("edit");
    let failed = runtime.read_outputs_general::<InstallAudit>();
    assert!(
        matches!(
            failed,
            Err(irrigation::ComputeError::DefinitionDenied { .. })
        ),
        "expected a rejected definition, got {failed:?}"
    );
    assert_eq!(
        runtime.snapshot().view::<AuditOuter>().field0(),
        7,
        "a rejected install must not retract earlier definitions"
    );
}

// ---------------------------------------------------------------------------
// Unique definitions on the call path (N3)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct CallSlot(u64);

#[irrigation]
mod call_conflict {
    use super::CallSlot;

    #[component]
    pub fn first(tag: u64) -> u64 {
        let _ = tag;
        let slot = CallSlot;
        slot.define_field0(1);
        1
    }

    #[component]
    pub fn second(tag: u64) -> u64 {
        let _ = tag;
        let slot = CallSlot;
        slot.define_field0(2);
        2
    }
}

#[test]
fn two_called_components_cannot_define_one_unit() {
    let runtime = Runtime::new();
    let first = runtime.call::<call_conflict::__FirstSpec>(call_conflict::__FirstArgs { tag: 0 });
    assert_eq!(first.expect("first call"), 1);
    let second =
        runtime.call::<call_conflict::__SecondSpec>(call_conflict::__SecondArgs { tag: 0 });
    let error = second.expect_err("expected a duplicate definition");
    assert!(matches!(
        error,
        irrigation::ComputeError::DuplicateDefinition { .. }
    ));
    let message = format!("{error:?}");
    assert!(
        message.matches(".rs:").count() >= 2,
        "both definition sites must be reported: {message}"
    );
}

// ---------------------------------------------------------------------------
// Absence predicate cells are reclaimed with their consumers (N10)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct AbsenceFlag {
    on: bool,
}

#[view(source)]
pub struct AbsenceTable {
    entries: Map<u32, u64>,
}

#[view(source)]
pub struct AbsenceQueries {
    keys: List<u32>,
}

#[irrigation]
mod absence_probe {
    use super::{AbsenceFlag, AbsenceQueries, AbsenceTable};

    #[component]
    pub fn root(flag: &AbsenceFlag, table: &AbsenceTable, queries: &AbsenceQueries) -> usize {
        if flag.on() {
            let entries = table.entries();
            let keys = queries.keys().to_vec();
            keys.iter()
                .filter(|key| entries.get(*key).is_none())
                .count()
        } else {
            0
        }
    }
}

#[test]
fn absence_cells_are_reclaimed_with_their_consumers() {
    let runtime = Runtime::new();
    runtime.install(absence_probe::Root).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<AbsenceFlag>().set_on(true);
            let mut queries = edit.view::<AbsenceQueries>();
            let mut keys = queries.keys();
            for key in 1_000..1_100u32 {
                keys.push(key);
            }
        })
        .expect("seed");
    assert_eq!(
        runtime.read_outputs::<absence_probe::Root>().expect("read"),
        100
    );
    let before = runtime.memory_report().units;

    // The branch that queried 100 absent keys is gone; the cells go with it.
    runtime
        .edit(|edit| edit.view::<AbsenceFlag>().set_on(false))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<absence_probe::Root>().expect("read"),
        0
    );
    let stats = runtime.collect();
    assert!(
        stats.units_reclaimed >= 100,
        "expected the 100 absence cells to be reclaimed, got {stats:?}"
    );
    let after = runtime.memory_report().units;
    assert!(after + 100 <= before, "units before={before} after={after}");

    // The same query recreates its cells and still answers correctly.
    runtime
        .edit(|edit| edit.view::<AbsenceFlag>().set_on(true))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<absence_probe::Root>().expect("read"),
        100
    );
}

// ---------------------------------------------------------------------------
// Map-member scopes compare keys, not just hashes (N5)
// ---------------------------------------------------------------------------

/// Two different keys with one shared hash.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CollidingKey(pub u64);

impl std::hash::Hash for CollidingKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&0u64, state);
    }
}

#[view(source)]
pub struct CollisionTable {
    entries: Map<CollidingKey, u64>,
}

#[irrigation]
mod collision_map {
    use super::CollisionTable;

    #[component]
    pub fn root(table: &CollisionTable) -> u64 {
        let entries = table.entries();
        entries
            .map_values(|value| *value * 2)
            .iter()
            .map(|(_, value)| *value)
            .sum()
    }
}

#[test]
fn map_member_scopes_compare_keys_not_only_hashes() {
    let runtime = Runtime::new();
    runtime.install(collision_map::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut table = edit.view::<CollisionTable>();
            let mut entries = table.entries();
            entries.insert(CollidingKey(1), 10);
            entries.insert(CollidingKey(2), 20);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<collision_map::Root>().expect("read"),
        60,
        "two colliding-hash keys must keep separate member scopes"
    );
}

// ---------------------------------------------------------------------------
// Derived map structure follows definition changes (F1)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct StructureMode {
    mode: u64,
    value: u64,
}

#[view(derived)]
pub struct StructureTable(Map<u32, u64>);

#[irrigation]
mod structure_map {
    use super::{StructureMode, StructureTable};

    #[component]
    pub fn root(mode: &StructureMode) -> (Vec<u32>, usize, Vec<(u32, u64)>) {
        irrigation::ensure_view(<StructureTable as irrigation::ViewSchema>::DESCRIPTOR);
        let table = StructureTable;
        if mode.mode() == 1 {
            table.define(3, 30);
        } else if mode.mode() == 2 {
            table.define(1, mode.value());
            table.define(2, mode.value() + 1);
        }
        let keys = table.keys();
        let values = keys
            .iter()
            .map(|key| (*key, table.get(key).expect("defined")))
            .collect();
        (keys, table.len(), values)
    }
}

#[test]
fn derived_map_structure_follows_definition_changes() {
    let runtime = Runtime::new();
    runtime.install(structure_map::Root).expect("install");
    let read = |runtime: &Runtime| runtime.read_outputs::<structure_map::Root>().expect("read");
    assert_eq!(read(&runtime), (Vec::new(), 0, Vec::new()));

    runtime
        .edit(|edit| edit.view::<StructureMode>().set_mode(1))
        .expect("edit");
    assert_eq!(read(&runtime), (vec![3], 1, vec![(3, 30)]));

    runtime
        .edit(|edit| {
            let mut mode = edit.view::<StructureMode>();
            mode.set_mode(2);
            mode.set_value(10);
        })
        .expect("edit");
    assert_eq!(read(&runtime), (vec![1, 2], 2, vec![(1, 10), (2, 11)]));

    // A value-only change must not change the key set.
    runtime
        .edit(|edit| edit.view::<StructureMode>().set_value(99))
        .expect("edit");
    assert_eq!(read(&runtime), (vec![1, 2], 2, vec![(1, 99), (2, 100)]));

    // Removing every definition empties the structure.
    runtime
        .edit(|edit| edit.view::<StructureMode>().set_mode(0))
        .expect("edit");
    assert_eq!(read(&runtime), (Vec::new(), 0, Vec::new()));

    // Re-adding keys after an empty revision sees the new key set.
    runtime
        .edit(|edit| edit.view::<StructureMode>().set_mode(2))
        .expect("edit");
    assert_eq!(read(&runtime), (vec![1, 2], 2, vec![(1, 99), (2, 100)]));
}

#[test]
fn derived_map_value_edits_do_not_touch_structure() {
    let runtime = Runtime::new();
    runtime.install(structure_map::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut mode = edit.view::<StructureMode>();
            mode.set_mode(2);
            mode.set_value(10);
        })
        .expect("edit");
    let _ = runtime.read_outputs::<structure_map::Root>().expect("read");
    let structure_version = |runtime: &Runtime| {
        runtime
            .dump()
            .into_iter()
            .find(|node| {
                node.kind == "Unit"
                    && node
                        .unit
                        .as_deref()
                        .is_some_and(|unit| unit.contains("Structure"))
            })
            .expect("structure unit")
            .changed_at
    };
    let before = structure_version(&runtime);
    let revision = runtime.revision();

    // Values change, the key set does not: the structure unit must stay put.
    runtime
        .edit(|edit| edit.view::<StructureMode>().set_value(99))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<structure_map::Root>().expect("read"),
        (vec![1, 2], 2, vec![(1, 99), (2, 100)])
    );
    assert_eq!(structure_version(&runtime), before);

    // Removing the keys must move the structure unit.
    runtime
        .edit(|edit| edit.view::<StructureMode>().set_mode(0))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<structure_map::Root>().expect("read"),
        (Vec::new(), 0, Vec::new())
    );
    assert!(structure_version(&runtime) > before);
    assert!(runtime.revision() > revision);
}

// ---------------------------------------------------------------------------
// Per-projection demand (N12)
// ---------------------------------------------------------------------------

#[irrigation]
mod projection_gate {
    #[component]
    pub fn two_projection_root() -> (u32, u32) {
        (1, 2)
    }
}

#[test]
fn reading_one_projection_does_not_evaluate_the_other() {
    let runtime = Runtime::new();
    runtime
        .install(projection_gate::TwoProjectionRoot)
        .expect("install");
    let outputs = |runtime: &Runtime| {
        runtime
            .dump()
            .into_iter()
            .filter(|node| node.kind == "InvocationOutput")
            .collect::<Vec<_>>()
    };

    let first = runtime
        .read_projection::<projection_gate::TwoProjectionRoot>(0)
        .expect("projection 0");
    assert_eq!(*first.downcast_ref::<u32>().expect("u32"), 1);
    let nodes = outputs(&runtime);
    assert_eq!(nodes.len(), 2);
    assert!(nodes[0].value.is_some(), "the demanded projection ran");
    assert!(
        nodes[1].value.is_none(),
        "the sibling projection must not be evaluated"
    );

    let second = runtime
        .read_projection::<projection_gate::TwoProjectionRoot>(1)
        .expect("projection 1");
    assert_eq!(*second.downcast_ref::<u32>().expect("u32"), 2);
    let nodes = outputs(&runtime);
    assert!(nodes[1].value.is_some());
    assert_eq!(
        runtime
            .read_outputs::<projection_gate::TwoProjectionRoot>()
            .expect("outputs"),
        (1, 2)
    );

    let error = runtime
        .read_projection::<projection_gate::TwoProjectionRoot>(2)
        .expect_err("out of range");
    assert!(matches!(error, irrigation::ComputeError::Internal { .. }));
}

#[irrigation]
mod call_projection_gate {
    #[component]
    pub fn pair(tag: u64) -> (u32, u32) {
        let _ = tag;
        (3, 4)
    }
}

#[test]
fn a_call_projection_is_independently_demandable() {
    let runtime = Runtime::new();
    let first = runtime
        .call_projection::<call_projection_gate::__PairSpec>(
            call_projection_gate::__PairArgs { tag: 0 },
            0,
        )
        .expect("projection 0");
    assert_eq!(*first.downcast_ref::<u32>().expect("u32"), 3);
    let outputs: Vec<_> = runtime
        .dump()
        .into_iter()
        .filter(|node| node.kind == "InvocationOutput")
        .collect();
    assert_eq!(outputs.len(), 2);
    assert!(outputs[0].value.is_some());
    assert!(
        outputs[1].value.is_none(),
        "the sibling projection must not be evaluated"
    );

    let second = runtime
        .call_projection::<call_projection_gate::__PairSpec>(
            call_projection_gate::__PairArgs { tag: 0 },
            1,
        )
        .expect("projection 1");
    assert_eq!(*second.downcast_ref::<u32>().expect("u32"), 4);
    let outputs: Vec<_> = runtime
        .dump()
        .into_iter()
        .filter(|node| node.kind == "InvocationOutput")
        .collect();
    assert!(outputs[1].value.is_some());
    assert_eq!(
        runtime
            .call::<call_projection_gate::__PairSpec>(call_projection_gate::__PairArgs { tag: 0 })
            .expect("whole call"),
        (3, 4)
    );
}

#[test]
fn a_join_tracks_both_inputs_changing_in_one_transaction() {
    let runtime = Runtime::new();
    runtime.register_view(<LeftTable as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<RightTable as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_root(irrigation::EquiJoin::<TransactionJoin>::new())
        .expect("install");

    let mut left: Vec<(u32, u64)> = vec![(1, 10), (2, 20), (3, 30)];
    let mut right: Vec<(u32, u64)> = vec![(1, 2), (2, 3), (3, 4)];
    runtime
        .edit(|edit| {
            {
                let mut l = edit.view::<LeftTable>();
                let mut entries = l.entries();
                for (key, value) in &left {
                    entries.insert(*key, *value);
                }
            }
            let mut r = edit.view::<RightTable>();
            let mut entries = r.entries();
            for (key, value) in &right {
                entries.insert(*key, *value);
            }
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::EquiJoin<TransactionJoin>>()
            .expect("join"),
        from_scratch_join(&left, &right)
    );

    // Both sides change in one revision, with removals, an update and
    // insertions on each side.
    left = vec![(1, 10), (2, 21), (4, 40)];
    right = vec![(1, 2), (2, 3), (5, 50)];
    runtime
        .edit(|edit| {
            {
                let mut l = edit.view::<LeftTable>();
                let mut entries = l.entries();
                entries.remove(&3);
                entries.insert(2, 21);
                entries.insert(4, 40);
            }
            let mut r = edit.view::<RightTable>();
            let mut entries = r.entries();
            entries.remove(&3);
            entries.insert(5, 50);
        })
        .expect("edit");
    let expected = from_scratch_join(&left, &right);
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::EquiJoin<TransactionJoin>>()
            .expect("join"),
        expected
    );
    assert_eq!(expected.get(&1), Some(&20));
    assert_eq!(expected.get(&2), Some(&63));
}

#[test]
fn invalidation_cost_is_counted() {
    let runtime = Runtime::new();
    runtime.install(structure_map::Root).expect("install");
    // The reader has to exist before the edit, or there is no dependent to
    // invalidate: a seed nothing reads invalidates nobody, and the kernel no
    // longer records a marking step for it.
    let _ = runtime
        .read_outputs::<structure_map::Root>()
        .expect("read the root so the mode unit has a dependent");
    let before = runtime.stats().invalidations;
    runtime
        .edit(|edit| edit.view::<StructureMode>().set_mode(1))
        .expect("edit");
    let after = runtime.stats().invalidations;
    assert!(
        after > before,
        "the edit must invalidate its dependents: {before} -> {after}"
    );

    runtime
        .edit(|edit| edit.view::<StructureMode>().set_mode(1))
        .expect("no-op edit");
    assert_eq!(
        runtime.stats().invalidations,
        after,
        "a no-op transaction must not invalidate"
    );

    for mode in 0..8 {
        runtime
            .edit(|edit| edit.view::<StructureMode>().set_mode(mode % 2))
            .expect("edit");
    }
    assert!(
        runtime.stats().invalidation_ns > 0,
        "invalidation time must be recorded"
    );
}

// ---------------------------------------------------------------------------
// Phase 1: one structure contract (F1 completion)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ConsumerTable {
    entries: Map<u32, u64>,
}

#[irrigation]
mod structure_consumers {
    use super::ConsumerTable;

    #[component]
    pub fn keys_only(table: &ConsumerTable) -> Vec<u32> {
        table.entries().keys()
    }

    #[component]
    pub fn one_value(table: &ConsumerTable) -> Option<u64> {
        table.entries().get(&1)
    }
}

#[test]
fn value_only_edits_do_not_run_structure_consumers() {
    let runtime = Runtime::new();
    runtime
        .install(structure_consumers::KeysOnly)
        .expect("install");
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(1, 10);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<structure_consumers::KeysOnly>()
            .expect("read"),
        vec![1]
    );
    let executions = runtime.stats().executions;

    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(1, 11);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<structure_consumers::KeysOnly>()
            .expect("read"),
        vec![1]
    );
    assert_eq!(
        runtime.stats().executions,
        executions,
        "a value-only edit must not execute a structure consumer"
    );
}

#[test]
fn structure_changes_do_not_run_value_consumers() {
    let runtime = Runtime::new();
    runtime
        .install(structure_consumers::OneValue)
        .expect("install");
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(1, 10);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<structure_consumers::OneValue>()
            .expect("read"),
        Some(10)
    );
    let executions = runtime.stats().executions;

    // Adding an unrelated key changes structure, membership for key 2, and the
    // content, but none of the units the value consumer observed.
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(2, 20);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<structure_consumers::OneValue>()
            .expect("read"),
        Some(10)
    );
    assert_eq!(
        runtime.stats().executions,
        executions,
        "a structure change must not execute a value consumer"
    );

    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(1, 12);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<structure_consumers::OneValue>()
            .expect("read"),
        Some(12)
    );
    assert!(runtime.stats().executions > executions);
}

#[view(source)]
pub struct FamiliesFlag {
    count: u64,
}

#[view(derived)]
pub struct WholeList(List<u64>);

#[view(derived)]
pub struct WholeSet(Set<u64>);

#[irrigation]
mod derived_families {
    use super::{FamiliesFlag, WholeList, WholeSet};

    #[component]
    pub fn root(flag: &FamiliesFlag) -> (Vec<u64>, Vec<u64>) {
        if flag.count() == 2 {
            let _ = WholeList::from([10u64, 20]);
            let _ = WholeSet::from([1u64, 2]);
        } else {
            let _ = WholeList::from([10u64, 20, 30]);
            let _ = WholeSet::from([1u64, 2, 3]);
        }
        (
            WholeList.ids().to_vec(),
            WholeSet.members().iter().cloned().collect(),
        )
    }
}

#[test]
fn structure_is_maintained_for_every_family() {
    let runtime = Runtime::new();
    runtime.install(derived_families::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<FamiliesFlag>().set_count(3))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<derived_families::Root>()
            .expect("read"),
        (vec![0, 1, 2], vec![1, 2, 3])
    );
    let structure_units = |runtime: &Runtime| {
        runtime
            .dump()
            .into_iter()
            .filter(|node| {
                node.kind == "Unit"
                    && node
                        .unit
                        .as_deref()
                        .is_some_and(|unit| unit.contains("Structure"))
            })
            .count()
    };
    assert!(
        structure_units(&runtime) >= 2,
        "list and set structure units must exist"
    );

    runtime
        .edit(|edit| edit.view::<FamiliesFlag>().set_count(2))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<derived_families::Root>()
            .expect("read"),
        (vec![0, 1], vec![1, 2])
    );
}

// ---------------------------------------------------------------------------
// Keyed derived lists (Phase 2)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct KeyedMode {
    mode: u64,
}

#[view(derived)]
pub struct KeyedList(List<u64>);

#[irrigation]
mod keyed_lists {
    use super::{KeyedList, KeyedMode};

    #[component]
    pub fn root(mode: &KeyedMode) -> (Vec<u64>, Vec<u64>, Option<u64>, Option<u64>) {
        if mode.mode() == 1 {
            KeyedList.define_keyed([(2u64, 20u64), (1, 10)]);
        } else if mode.mode() == 2 {
            KeyedList.define_keyed([(1u64, 10u64)]);
        } else if mode.mode() == 3 {
            KeyedList.define_keyed([(1u64, 11u64), (2, 20)]);
        } else {
            KeyedList.define_keyed([(1u64, 10u64), (2, 20)]);
        }
        let list = KeyedList;
        (
            list.ids().to_vec(),
            list.to_vec(),
            list.get_key(1),
            list.get_key(2),
        )
    }
}

#[test]
fn keyed_list_identity_survives_reorder_and_removal() {
    let runtime = Runtime::new();
    runtime.install(keyed_lists::Root).expect("install");
    let read = |runtime: &Runtime| runtime.read_outputs::<keyed_lists::Root>().expect("read");
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(0))
        .expect("edit");
    assert_eq!(
        read(&runtime),
        (vec![1, 2], vec![10, 20], Some(10), Some(20))
    );

    // Reordering the structure keeps every member identity and value.
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        read(&runtime),
        (vec![2, 1], vec![20, 10], Some(10), Some(20))
    );

    // Removing one key retracts exactly that member.
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(2))
        .expect("edit");
    let revision = runtime.revision();
    assert_eq!(read(&runtime), (vec![1], vec![10], Some(10), None));
    let units = runtime.dump();
    let membership = |id: &str| {
        units
            .iter()
            .find(|node| {
                node.kind == "Unit"
                    && node
                        .unit
                        .as_deref()
                        .is_some_and(|unit| unit.contains("Membership") && unit.contains(id))
            })
            .expect("membership unit")
    };
    assert_eq!(
        membership("Some(\"2\")").changed_at,
        revision,
        "the removed member's membership must change"
    );
    assert!(
        membership("Some(\"1\")").changed_at < revision,
        "the surviving member's membership must not change"
    );

    // A value edit on one member updates only that member's value.
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(3))
        .expect("edit");
    assert_eq!(
        read(&runtime),
        (vec![1, 2], vec![11, 20], Some(11), Some(20))
    );
}

static KEYED_MAP_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[irrigation]
mod keyed_map {
    use super::{KeyedList, KeyedMode};
    use std::sync::atomic::Ordering;

    #[component]
    pub fn root(mode: &KeyedMode) -> Vec<u64> {
        if mode.mode() == 1 {
            KeyedList.define_keyed([(2u64, 20u64), (1, 10)]);
        } else if mode.mode() == 2 {
            KeyedList.define_keyed([(1u64, 11u64), (2, 20)]);
        } else {
            KeyedList.define_keyed([(1u64, 10u64), (2, 20)]);
        }
        KeyedList.map(|value| {
            super::KEYED_MAP_CALLS.fetch_add(1, Ordering::SeqCst);
            *value * 2
        })
    }
}

#[test]
fn derived_list_map_reuses_members_after_reorder_and_value_edit() {
    KEYED_MAP_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
    let runtime = Runtime::new();
    runtime.install(keyed_map::Root).expect("install");
    assert_eq!(
        runtime.read_outputs::<keyed_map::Root>().expect("read"),
        vec![20, 40]
    );
    assert_eq!(KEYED_MAP_CALLS.load(std::sync::atomic::Ordering::SeqCst), 2);

    // A reorder re-runs the map call but reuses both member results.
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<keyed_map::Root>().expect("read"),
        vec![40, 20]
    );
    assert_eq!(
        KEYED_MAP_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "a reorder must not recompute any member"
    );

    // A value edit recomputes exactly the edited member.
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(2))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<keyed_map::Root>().expect("read"),
        vec![22, 40]
    );
    assert_eq!(
        KEYED_MAP_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "only the edited member is recomputed"
    );
}

#[view(derived)]
pub struct KeyedSet(Set<u64>);

#[irrigation]
mod keyed_sets {
    use super::{KeyedMode, KeyedSet};

    #[component]
    pub fn root(mode: &KeyedMode) -> (bool, bool, bool, Vec<u64>) {
        if mode.mode() == 1 {
            KeyedSet.define_keyed([1u64, 2, 3]);
        } else if mode.mode() == 2 {
            KeyedSet.define_keyed([2u64]);
        } else {
            KeyedSet.define_keyed([1u64, 2]);
        }
        let set = KeyedSet;
        (
            set.contains(&1u64),
            set.contains(&2u64),
            set.contains(&3u64),
            set.to_vec(),
        )
    }
}

#[test]
fn keyed_set_membership_is_per_member() {
    let runtime = Runtime::new();
    runtime.install(keyed_sets::Root).expect("install");
    let read = |runtime: &Runtime| runtime.read_outputs::<keyed_sets::Root>().expect("read");
    assert_eq!(read(&runtime), (true, true, false, vec![1, 2]));

    // Adding a third member changes only that member's membership unit.
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(1))
        .expect("edit");
    let revision = runtime.revision();
    assert_eq!(read(&runtime), (true, true, true, vec![1, 2, 3]));
    let units = runtime.dump();
    let membership = |id: &str| {
        units
            .iter()
            .find(|node| {
                node.kind == "Unit"
                    && node
                        .unit
                        .as_deref()
                        .is_some_and(|unit| unit.contains("Membership") && unit.contains(id))
            })
            .expect("membership unit")
    };
    assert!(membership("Some(\"1\")").changed_at < revision);
    assert_eq!(membership("Some(\"3\")").changed_at, revision);

    // Removing a member flips its unit and leaves the survivors untouched.
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(2))
        .expect("edit");
    let removal = runtime.revision();
    assert_eq!(read(&runtime), (false, true, false, vec![2]));
    let units = runtime.dump();
    let membership = |id: &str| {
        units
            .iter()
            .find(|node| {
                node.kind == "Unit"
                    && node
                        .unit
                        .as_deref()
                        .is_some_and(|unit| unit.contains("Membership") && unit.contains(id))
            })
            .expect("membership unit")
    };
    assert_eq!(membership("Some(\"1\")").changed_at, removal);
    assert!(membership("Some(\"2\")").changed_at < removal);
}

static KEYED_FILTER_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static KEYED_FLAT_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[irrigation]
mod keyed_filters {
    use super::{KeyedList, KeyedMode};
    use std::sync::atomic::Ordering;

    #[component]
    pub fn root(mode: &KeyedMode) -> (Vec<u64>, Vec<u64>) {
        if mode.mode() == 1 {
            KeyedList.define_keyed([(2u64, 20u64), (1, 10)]);
        } else if mode.mode() == 2 {
            KeyedList.define_keyed([(1u64, 11u64), (2, 20)]);
        } else {
            KeyedList.define_keyed([(1u64, 10u64), (2, 20)]);
        }
        let filtered = KeyedList.filter(|value| {
            super::KEYED_FILTER_CALLS.fetch_add(1, Ordering::SeqCst);
            *value % 20 == 0
        });
        let flat = KeyedList.flat_map(|value| {
            super::KEYED_FLAT_CALLS.fetch_add(1, Ordering::SeqCst);
            vec![*value]
        });
        (filtered, flat)
    }
}

#[test]
fn derived_list_filter_and_flat_map_memoize_per_identity() {
    KEYED_FILTER_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
    KEYED_FLAT_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
    let runtime = Runtime::new();
    runtime.install(keyed_filters::Root).expect("install");
    assert_eq!(
        runtime.read_outputs::<keyed_filters::Root>().expect("read"),
        (vec![20], vec![10, 20])
    );
    assert_eq!(
        KEYED_FILTER_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    assert_eq!(
        KEYED_FLAT_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        2
    );

    // A reorder reuses every predicate and flat-map result.
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<keyed_filters::Root>().expect("read"),
        (vec![20], vec![20, 10])
    );
    assert_eq!(
        KEYED_FILTER_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "a reorder must not re-run the predicate"
    );
    assert_eq!(
        KEYED_FLAT_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "a reorder must not re-run flat_map"
    );

    // A value edit re-runs only the edited member's predicate and flat-map.
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(2))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<keyed_filters::Root>().expect("read"),
        (vec![20], vec![11, 20])
    );
    assert_eq!(
        KEYED_FILTER_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        3
    );
    assert_eq!(
        KEYED_FLAT_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        3
    );
}

// ---------------------------------------------------------------------------
// Derived Reduced fields (Phase 3)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct DerivedTotals(Reduced<Sum>);

#[view(source)]
pub struct TotalsMode {
    n: u64,
}

#[irrigation]
mod derived_reduced {
    use super::{DerivedTotals, TotalsMode};

    #[component]
    pub fn root(mode: &TotalsMode) -> (Vec<u64>, u64) {
        if mode.n() == 0 {
            DerivedTotals.define_all([1u64, 2, 3]);
        } else {
            DerivedTotals.define_all([4u64]);
        }
        (DerivedTotals.to_vec(), DerivedTotals.aggregate())
    }
}

#[test]
fn a_derived_reduction_defines_and_redefines_its_sequence() {
    let runtime = Runtime::new();
    runtime.install(derived_reduced::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<derived_reduced::Root>()
            .expect("read"),
        (vec![1, 2, 3], 6)
    );
    runtime
        .edit(|edit| edit.view::<TotalsMode>().set_n(1))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<derived_reduced::Root>()
            .expect("read"),
        (vec![4], 4)
    );
}

#[view(derived)]
pub struct DerivedSummary {
    total: Reduced<Sum>,
}

#[irrigation]
mod derived_reduced_named {
    use super::DerivedSummary;

    #[component]
    pub fn root() -> (Vec<u64>, u64) {
        DerivedSummary.define_total([5u64, 6]);
        (
            DerivedSummary.total().to_vec(),
            DerivedSummary.total().aggregate(),
        )
    }
}

#[test]
fn a_named_derived_reduction_uses_generated_accessors() {
    let runtime = Runtime::new();
    runtime
        .install(derived_reduced_named::Root)
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs::<derived_reduced_named::Root>()
            .expect("read"),
        (vec![5, 6], 11)
    );
}

// ---------------------------------------------------------------------------
// Cross-family operators (Phase 3)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct CrossTable {
    entries: Map<u32, u64>,
}

#[view(derived)]
pub struct CrossList(List<u64>);

#[view(derived)]
pub struct CrossMap(Map<u32, u64>);

#[irrigation]
mod map_to_list_gate {
    use super::{CrossList, CrossTable};

    #[component]
    pub fn root(table: &CrossTable) -> (Vec<u64>, Option<u64>, Option<u64>) {
        irrigation::ensure_view(<CrossList as irrigation::ViewSchema>::DESCRIPTOR);
        let target = CrossList.field0();
        table
            .entries()
            .to_list(&target, |key, value| Some((u64::from(*key), *value * 2)));
        (target.ids().to_vec(), target.get_key(1), target.get_key(2))
    }
}

#[test]
fn map_to_list_preserves_key_identity() {
    let runtime = Runtime::new();
    runtime.install(map_to_list_gate::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut table = edit.view::<CrossTable>();
            let mut entries = table.entries();
            entries.insert(1, 10);
            entries.insert(2, 20);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<map_to_list_gate::Root>()
            .expect("read"),
        (vec![1, 2], Some(20), Some(40))
    );

    runtime
        .edit(|edit| {
            let mut table = edit.view::<CrossTable>();
            let mut entries = table.entries();
            entries.insert(1, 11);
        })
        .expect("edit");
    let revision = runtime.revision();
    assert_eq!(
        runtime
            .read_outputs::<map_to_list_gate::Root>()
            .expect("read"),
        (vec![1, 2], Some(22), Some(40))
    );
    let view = format!("{:?}", <CrossList as irrigation::ViewSchema>::DESCRIPTOR.id);
    let unit = runtime
        .dump()
        .into_iter()
        .find(|node| {
            node.kind == "Unit"
                && node.unit.as_deref().is_some_and(|unit| {
                    unit.contains(&view)
                        && unit.contains("Membership")
                        && unit.contains("Some(\"2\")")
                })
        })
        .expect("list membership unit");
    assert!(
        unit.changed_at < revision,
        "an untouched key must keep its list member"
    );
}

#[irrigation]
mod list_to_map_gate {
    use super::{CrossMap, KeyedList, KeyedMode};

    #[component]
    pub fn root(mode: &KeyedMode) -> (Vec<(u32, u64)>, Option<u64>, Option<u64>) {
        irrigation::ensure_view(<CrossMap as irrigation::ViewSchema>::DESCRIPTOR);
        if mode.mode() == 1 {
            KeyedList.define_keyed([(1u64, 11u64)]);
        } else {
            KeyedList.define_keyed([(1u64, 10u64), (2, 20)]);
        }
        let target = CrossMap.field0();
        KeyedList
            .field0()
            .to_map(&target, |id, value| Some((id as u32, *value)));
        (target.to_vec(), target.get(&1), target.get(&2))
    }
}

#[test]
fn list_to_map_retracts_missing_keys() {
    let runtime = Runtime::new();
    runtime.install(list_to_map_gate::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<list_to_map_gate::Root>()
            .expect("read"),
        (vec![(1, 10), (2, 20)], Some(10), Some(20))
    );
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<list_to_map_gate::Root>()
            .expect("read"),
        (vec![(1, 11)], Some(11), None)
    );
}

// ---------------------------------------------------------------------------
// Derived graphs (Phase 3)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct GraphMode {
    mode: u64,
}

#[view(derived)]
pub struct DerivedGraph(Graph<u64, u64>);

#[allow(clippy::type_complexity)]
#[irrigation]
mod derived_graph {
    use super::{DerivedGraph, GraphMode};

    #[component]
    pub fn root(
        mode: &GraphMode,
    ) -> (
        Vec<u64>,
        Vec<u64>,
        Option<u64>,
        Option<irrigation::EdgePayload<u64>>,
        Vec<(u64, u64)>,
    ) {
        if mode.mode() == 1 {
            DerivedGraph.define_graph(&[(1u64, 11u64), (2, 20)], &[(10u64, 1, 2, 100u64)]);
        } else if mode.mode() == 3 {
            // An edge naming a node this definition does not contain.
            DerivedGraph.define_graph(&[(1u64, 10u64)], &[(10u64, 1, 2, 100u64)]);
        } else if mode.mode() == 2 {
            DerivedGraph.define_graph(&[(1u64, 10u64)], &[]);
        } else {
            DerivedGraph.define_graph(&[(1u64, 10u64), (2, 20)], &[(10u64, 1, 2, 100u64)]);
        }
        (
            DerivedGraph.nodes(),
            DerivedGraph.edges(),
            DerivedGraph.node(1),
            DerivedGraph.edge(10),
            DerivedGraph.outgoing(1),
        )
    }
}

#[test]
fn derived_graph_topology_and_payloads_are_separate() {
    let runtime = Runtime::new();
    runtime.install(derived_graph::Root).expect("install");
    assert_eq!(
        runtime.read_outputs::<derived_graph::Root>().expect("read"),
        (
            vec![1, 2],
            vec![10],
            Some(10),
            Some(irrigation::EdgePayload {
                from: 1,
                to: 2,
                value: 100,
            }),
            vec![(10, 2)],
        )
    );
    let topology_version = |runtime: &Runtime| {
        let view = format!(
            "{:?}",
            <DerivedGraph as irrigation::ViewSchema>::DESCRIPTOR.id
        );
        runtime
            .dump()
            .into_iter()
            .find(|node| {
                node.kind == "Unit"
                    && node
                        .unit
                        .as_deref()
                        .is_some_and(|unit| unit.contains(&view) && unit.contains("Structure"))
            })
            .expect("topology unit")
            .changed_at
    };
    let before = topology_version(&runtime);

    // A payload edit leaves the topology unit untouched.
    runtime
        .edit(|edit| edit.view::<GraphMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<derived_graph::Root>().expect("read"),
        (
            vec![1, 2],
            vec![10],
            Some(11),
            Some(irrigation::EdgePayload {
                from: 1,
                to: 2,
                value: 100,
            }),
            vec![(10, 2)],
        )
    );
    assert_eq!(topology_version(&runtime), before);

    // Removing a node retracts it and its incident edge.
    runtime
        .edit(|edit| edit.view::<GraphMode>().set_mode(2))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<derived_graph::Root>().expect("read"),
        (vec![1], Vec::new(), Some(10), None, Vec::new())
    );
    assert!(topology_version(&runtime) > before);
}

// ---------------------------------------------------------------------------
// Per-key join View and Graph::node_map (Phase 3)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct JoinTarget(Map<u32, u64>);

struct TargetJoin;

impl irrigation::JoinSpec for TargetJoin {
    type Key = u32;
    type Left = u64;
    type Right = u64;
    type Joined = u64;

    fn left() -> (irrigation::ViewId, irrigation::FieldId) {
        (
            <LeftTable as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        )
    }

    fn right() -> (irrigation::ViewId, irrigation::FieldId) {
        (
            <RightTable as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        )
    }

    fn combine(left: &u64, right: &u64) -> u64 {
        left * right
    }

    fn target() -> Option<(irrigation::ViewId, irrigation::FieldId)> {
        Some((
            <JoinTarget as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        ))
    }
}

#[test]
fn join_view_updates_only_changed_keys() {
    let runtime = Runtime::new();
    runtime.register_view(<LeftTable as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<RightTable as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<JoinTarget as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_root(irrigation::EquiJoin::<TargetJoin>::new())
        .expect("install");
    runtime
        .edit(|edit| {
            {
                let mut l = edit.view::<LeftTable>();
                let mut entries = l.entries();
                entries.insert(1, 10);
                entries.insert(2, 20);
            }
            let mut r = edit.view::<RightTable>();
            let mut entries = r.entries();
            entries.insert(1, 2);
            entries.insert(2, 3);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::EquiJoin<TargetJoin>>()
            .expect("join"),
        [(1, 20), (2, 60)].into_iter().collect()
    );
    assert_eq!(
        runtime.snapshot().view::<JoinTarget>().field0(),
        vec![(1, 20), (2, 60)]
    );

    runtime
        .edit(|edit| {
            let mut l = edit.view::<LeftTable>();
            let mut entries = l.entries();
            entries.insert(1, 11);
        })
        .expect("edit");
    let revision = runtime.revision();
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::EquiJoin<TargetJoin>>()
            .expect("join"),
        [(1, 22), (2, 60)].into_iter().collect()
    );
    assert_eq!(
        runtime.snapshot().view::<JoinTarget>().field0(),
        vec![(1, 22), (2, 60)]
    );
    let view = format!(
        "{:?}",
        <JoinTarget as irrigation::ViewSchema>::DESCRIPTOR.id
    );
    let unit = |id: &str| {
        runtime
            .dump()
            .into_iter()
            .find(|node| {
                node.kind == "Unit"
                    && node.unit.as_deref().is_some_and(|unit| {
                        unit.contains(&view) && unit.contains("Value") && unit.contains(id)
                    })
            })
            .expect("target value unit")
    };
    assert_eq!(unit("Some(\"1\")").changed_at, revision);
    assert!(unit("Some(\"2\")").changed_at < revision);
}

#[view(derived)]
pub struct GraphNodeMap(Map<u64, u64>);

#[irrigation]
mod graph_node_map_gate {
    use super::{DerivedGraph, GraphMode, GraphNodeMap};

    #[component]
    pub fn root(mode: &GraphMode) -> (Vec<(u64, u64)>, Option<u64>, Option<u64>) {
        irrigation::ensure_view(<GraphNodeMap as irrigation::ViewSchema>::DESCRIPTOR);
        if mode.mode() == 1 {
            DerivedGraph.define_graph(&[(1u64, 11u64), (2, 20)], &[(10u64, 1, 2, 100u64)]);
        } else if mode.mode() == 2 {
            DerivedGraph.define_graph(&[(1u64, 10u64)], &[]);
        } else {
            DerivedGraph.define_graph(&[(1u64, 10u64), (2, 20)], &[(10u64, 1, 2, 100u64)]);
        }
        let target = GraphNodeMap.field0();
        DerivedGraph.field0().node_map(&target);
        (target.to_vec(), target.get(&1), target.get(&2))
    }
}

#[test]
fn graph_node_map_updates_one_node() {
    let runtime = Runtime::new();
    runtime.install(graph_node_map_gate::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<graph_node_map_gate::Root>()
            .expect("read"),
        (vec![(1, 10), (2, 20)], Some(10), Some(20))
    );
    runtime
        .edit(|edit| edit.view::<GraphMode>().set_mode(1))
        .expect("edit");
    let revision = runtime.revision();
    assert_eq!(
        runtime
            .read_outputs::<graph_node_map_gate::Root>()
            .expect("read"),
        (vec![(1, 11), (2, 20)], Some(11), Some(20))
    );
    let view = format!(
        "{:?}",
        <GraphNodeMap as irrigation::ViewSchema>::DESCRIPTOR.id
    );
    let unit = |id: &str| {
        runtime
            .dump()
            .into_iter()
            .find(|node| {
                node.kind == "Unit"
                    && node.unit.as_deref().is_some_and(|unit| {
                        unit.contains(&view) && unit.contains("Value") && unit.contains(id)
                    })
            })
            .expect("node-map value unit")
    };
    assert_eq!(unit("Some(\"1\")").changed_at, revision);
    assert!(unit("Some(\"2\")").changed_at < revision);

    runtime
        .edit(|edit| edit.view::<GraphMode>().set_mode(2))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<graph_node_map_gate::Root>()
            .expect("read"),
        (vec![(1, 10)], Some(10), None)
    );
}

// ---------------------------------------------------------------------------
// Per-entry observers (F9)
// ---------------------------------------------------------------------------

#[test]
fn a_per_key_observer_ignores_other_keys() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runtime = Runtime::new();
    runtime.register_view(<ConsumerTable as irrigation::ViewSchema>::DESCRIPTOR);
    let seen = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&seen);
    let subscription = runtime
        .observe_entry::<ConsumerTable, u32>(irrigation::FieldId(0), 1u32, move |_snapshot| {
            sink.fetch_add(1, Ordering::SeqCst);
        })
        .expect("observe entry");

    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(1, 10);
            entries.insert(2, 20);
        })
        .expect("edit");
    assert_eq!(seen.load(Ordering::SeqCst), 1);

    // Key 2 changes; the key-1 observer stays quiet.
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(2, 21);
        })
        .expect("edit");
    assert_eq!(seen.load(Ordering::SeqCst), 1);

    // Key 1 changes; the observer fires.
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(1, 11);
        })
        .expect("edit");
    assert_eq!(seen.load(Ordering::SeqCst), 2);
    drop(subscription);
}

// ---------------------------------------------------------------------------
// Retention (F10, scope half)
// ---------------------------------------------------------------------------

#[test]
fn collect_reclaims_unreachable_scopes() {
    let runtime = Runtime::new();
    for tag in 0..32u64 {
        let _ = runtime
            .call::<call_projection_gate::__PairSpec>(call_projection_gate::__PairArgs { tag })
            .expect("call");
    }
    let before = runtime.memory_report().scopes;
    assert!(
        before >= 32,
        "each distinct invocation gets a scope: {before}"
    );
    let stats = runtime.collect();
    assert!(stats.scopes_reclaimed > 0, "{stats:?}");
    assert!(runtime.memory_report().scopes < before);
}

// ---------------------------------------------------------------------------
// Explainability: validation vs execution (N15)
// ---------------------------------------------------------------------------

#[test]
fn explanations_distinguish_validation_from_execution() {
    let runtime = Runtime::new();
    runtime
        .install(structure_consumers::KeysOnly)
        .expect("install");
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(1, 10);
        })
        .expect("seed");
    let _ = runtime
        .read_outputs::<structure_consumers::KeysOnly>()
        .expect("read");
    let root = runtime
        .dump()
        .into_iter()
        .find(|node| node.kind == "InvocationBody" && node.label == "root body")
        .expect("root body")
        .id;
    let first = runtime.explain_node(root);
    assert_eq!(first.executed_at, 1);
    assert_eq!(first.validated_at, 1);
    assert!(first.state.contains("Ready"));
    assert_eq!(first.waiters, 0);

    // A value-only edit replays the cached root without executing it.
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(1, 11);
        })
        .expect("edit");
    let _ = runtime
        .read_outputs::<structure_consumers::KeysOnly>()
        .expect("read");
    let replayed = runtime.explain_node(root);
    assert_eq!(replayed.executed_at, 1, "the body must not re-execute");
    assert_eq!(replayed.validated_at, 2);

    // A structure change invalidates the body, so it executes.
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(2, 20);
        })
        .expect("edit");
    let _ = runtime
        .read_outputs::<structure_consumers::KeysOnly>()
        .expect("read");
    let executed = runtime.explain_node(root);
    assert_eq!(executed.executed_at, 3);
    assert_eq!(executed.validated_at, 3);
    assert_eq!(runtime.explain().waiting_tasks, 0);
}

// ---------------------------------------------------------------------------
// Routing generations (C2 slice)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct RouteFlag {
    on: bool,
}

#[view(derived)]
pub struct RouteSlot(u64);

#[irrigation]
mod routing_generation_gate {
    use super::{RouteFlag, RouteSlot};

    #[component]
    pub fn root(flag: &RouteFlag) -> u64 {
        let slot = RouteSlot;
        if flag.on() {
            slot.define_field0(42);
        } else {
            slot.define_field0(42);
        }
        slot.field0()
    }
}

#[test]
fn a_provider_handover_advances_the_routing_generation() {
    let runtime = Runtime::new();
    runtime
        .install(routing_generation_gate::Root)
        .expect("install");
    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(true))
        .expect("seed");
    let _ = runtime
        .read_outputs::<routing_generation_gate::Root>()
        .expect("read");
    let view = <RouteSlot as irrigation::ViewSchema>::DESCRIPTOR.id;
    let unit =
        irrigation::UnitKey::field(view, irrigation::FieldId(0), irrigation::UnitKind::Value);
    let before_routing = runtime.routing_generation(&unit);
    let before_provider = runtime.provider_of(&unit);
    assert!(before_provider.is_some());
    let before_value = runtime
        .dump()
        .into_iter()
        .find(|node| {
            node.kind == "Unit"
                && node.unit.as_deref().is_some_and(|unit| {
                    unit.contains(&format!("{view:?}")) && unit.contains("Value")
                })
        })
        .expect("value unit")
        .changed_at;

    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(false))
        .expect("edit");
    let _ = runtime
        .read_outputs::<routing_generation_gate::Root>()
        .expect("read");
    assert_eq!(
        runtime
            .read_outputs::<routing_generation_gate::Root>()
            .expect("read"),
        42
    );
    let after_value = runtime
        .dump()
        .into_iter()
        .find(|node| {
            node.kind == "Unit"
                && node.unit.as_deref().is_some_and(|unit| {
                    unit.contains(&format!("{view:?}")) && unit.contains("Value")
                })
        })
        .expect("value unit")
        .changed_at;
    assert_eq!(after_value, before_value, "the value stayed equal");
    assert!(
        runtime.routing_generation(&unit) > before_routing,
        "the handover must advance the routing generation"
    );
    assert_ne!(runtime.provider_of(&unit), before_provider);
}

// ---------------------------------------------------------------------------
// GeneralComponent: dependency scopes and output transaction (C3 slice)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ScopedTable {
    entries: Map<u32, u64>,
}

#[view(derived)]
pub struct ScopedOut(Map<u32, u64>);

fn scoped_value(key: u32) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        <ScopedTable as irrigation::ViewSchema>::DESCRIPTOR.id,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(key),
    )
}

struct ScopedReader;

impl irrigation::GeneralComponent for ScopedReader {
    type State = ([u64; 2], usize);
    type Output = ([u64; 2], usize);

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00DD_0003);
    const NAME: &'static str = "gates::ScopedReader";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[
        <ScopedTable as irrigation::ViewSchema>::DESCRIPTOR,
        <ScopedOut as irrigation::ViewSchema>::DESCRIPTOR,
    ];

    fn dependency_updates(&self) -> irrigation::DependencyUpdates {
        irrigation::DependencyUpdates::Incremental
    }

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let mut values = [0u64; 2];
        cx.begin_scope(1);
        values[0] = cx.read_typed(scoped_value(1))?;
        cx.end_scope();
        cx.begin_scope(2);
        values[1] = cx.read_typed(scoped_value(2))?;
        cx.end_scope();
        cx.retain(1);
        cx.output().entry(
            <ScopedOut as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
            1u32,
            values[0],
        );
        Ok(((values, 0), (values, 0)))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        state: &Self::State,
        changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let batch = changes.changes().len();
        cx.begin_scope(1);
        let first: u64 = cx.read_typed(scoped_value(1))?;
        cx.end_scope();
        cx.retain(1);
        let values = [first, state.0[1]];
        cx.output().entry(
            <ScopedOut as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
            1u32,
            first,
        );
        Ok(((values, batch), (values, batch)))
    }
}

#[test]
fn retired_dependency_scopes_filter_notifications() {
    let runtime = Runtime::new();
    runtime.install_general_root(ScopedReader).expect("install");
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ScopedTable>();
            let mut entries = table.entries();
            entries.insert(1, 10);
            entries.insert(2, 20);
        })
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<ScopedReader>()
            .expect("initialize"),
        ([10, 20], 0)
    );
    assert_eq!(
        runtime.snapshot().view::<ScopedOut>().field0(),
        vec![(1, 10)]
    );

    // Key 2 was read under a retired scope: its edit is filtered from the batch.
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ScopedTable>();
            let mut entries = table.entries();
            entries.insert(2, 21);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<ScopedReader>()
            .expect("retired scope"),
        ([10, 20], 0)
    );

    // Key 1 is in the retained scope: its edit is delivered.
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ScopedTable>();
            let mut entries = table.entries();
            entries.insert(1, 11);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<ScopedReader>()
            .expect("retained scope"),
        ([11, 20], 1)
    );
    assert_eq!(
        runtime.snapshot().view::<ScopedOut>().field0(),
        vec![(1, 11)]
    );
}

// ---------------------------------------------------------------------------
// Argument churn stays within the retention policy (Phase 4)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ChurnKey {
    value: u64,
}

#[irrigation]
mod churn {
    use super::ChurnKey;

    #[component]
    pub fn scoped(value: u64) -> u64 {
        value
    }

    #[component]
    pub fn root(key: &ChurnKey) -> u64 {
        let value = key.value();
        scoped(value)
    }
}

#[test]
fn argument_churn_stays_within_the_retention_policy() {
    let runtime = Runtime::new();
    runtime.install(churn::Root).expect("install");
    for value in 0..64u64 {
        runtime
            .edit(|edit| edit.view::<ChurnKey>().set_value(value))
            .expect("edit");
        assert_eq!(runtime.read_outputs::<churn::Root>().expect("read"), value);
    }
    let stats = runtime.collect();
    assert!(stats.invocations_reclaimed > 0, "{stats:?}");
    let report = runtime.memory_report();
    assert!(
        report.invocations <= 1,
        "only the live invocation should remain: {}",
        report.invocations
    );
    assert!(runtime.wait_edges().is_empty());
}

// ---------------------------------------------------------------------------
// A retained definition keeps the observations it was validated against
// ---------------------------------------------------------------------------

#[view(source)]
pub struct RetainMode {
    mode: u64,
}

#[view(source)]
pub struct RetainTable {
    entries: Map<u32, u64>,
}

#[view(derived)]
pub struct RetainedOut(u64);

/// Reads an absent membership cell and installs a derived unit: the unit retains
/// the component, and the cell is a negative observation its trace names.
struct RetainedRoot;

impl irrigation::GeneralComponent for RetainedRoot {
    type State = u64;
    type Output = u64;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CC_0005);
    const NAME: &'static str = "gates::RetainedRoot";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[
        <RetainMode as irrigation::ViewSchema>::DESCRIPTOR,
        <RetainTable as irrigation::ViewSchema>::DESCRIPTOR,
    ];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<RetainedOut as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let mode: u64 = cx.read_typed(irrigation::UnitKey::field(
            <RetainMode as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        // Absent from the content, so this cell is an absence predicate that only
        // a live trace keeps alive.
        let present: bool = cx.read_typed(irrigation::UnitKey::entity(
            <RetainTable as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Membership,
            irrigation::any_key(1u32),
        ))?;
        let output = u64::from(present) + mode;
        let mut txn = cx.output();
        txn.field(
            <RetainedOut as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            output,
        );
        Ok((mode, output))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn collect_keeps_the_trace_of_a_node_retained_for_its_definitions() {
    let runtime = Runtime::new();
    runtime.install_general_root(RetainedRoot).expect("install");
    assert_eq!(
        runtime
            .read_outputs_general::<RetainedRoot>()
            .expect("first demand"),
        0
    );

    // The installed unit retains the producer, but the absence cell its trace
    // names is rooted by nothing else.
    let table = format!("{:?}", <RetainTable as irrigation::ViewSchema>::view_id());
    let cell = runtime
        .dump()
        .into_iter()
        .find(|node| {
            node.unit
                .as_deref()
                .is_some_and(|unit| unit.contains(&table) && unit.contains("Membership"))
        })
        .expect("the producer read the key's absence")
        .id;
    let _ = runtime.collect(); // reclamation only; the report is not asserted here
    assert!(
        runtime.is_live(cell),
        "the pass reclaimed the observation a retained node's trace names"
    );

    runtime
        .edit(|edit| edit.view::<RetainMode>().set_mode(7))
        .expect("edit");
    // Re-validation walks the trace the pass had to keep: a reclaimed cell would
    // panic here on a freed identity.
    assert_eq!(
        runtime
            .read_outputs_general::<RetainedRoot>()
            .expect("the retained node must re-validate against the observations it kept"),
        7,
        "the retained producer must see the new mode, not a reclaimed dependency"
    );
    assert_eq!(runtime.snapshot().view::<RetainedOut>().field0(), 7);
}

// ---------------------------------------------------------------------------
// Strict components and declared primitives (N6 partial)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct StrictKey {
    value: u64,
}

fn strict_double(value: u64) -> u64 {
    value * 2
}

#[irrigation]
mod strict_gate {
    use super::{StrictKey, strict_double};

    #[component(strict, allows = [strict_double])]
    pub fn root(key: &StrictKey) -> u64 {
        let value = key.value();
        strict_double(value)
    }
}

#[test]
fn a_strict_component_runs_a_declared_primitive() {
    let runtime = Runtime::new();
    runtime.install(strict_gate::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<StrictKey>().set_value(21))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<strict_gate::Root>().expect("read"),
        42
    );
}

// ---------------------------------------------------------------------------
// Arena slot reuse and stale identity detection (F10)
// ---------------------------------------------------------------------------

#[test]
fn collect_reuses_arena_slots_and_detects_stale_ids() {
    let runtime = Runtime::new();
    for tag in 0..32u64 {
        let _ = runtime
            .call::<call_projection_gate::__PairSpec>(call_projection_gate::__PairArgs { tag })
            .expect("call");
    }
    let before = runtime.memory_report().nodes;
    let stale = runtime
        .dump()
        .into_iter()
        .find(|node| node.kind == "InvocationBody")
        .expect("a body")
        .id;
    let stats = runtime.collect();
    assert!(stats.nodes_reclaimed > 0);
    assert!(!runtime.is_live(stale), "a reclaimed id is stale");

    for tag in 100..132u64 {
        let _ = runtime
            .call::<call_projection_gate::__PairSpec>(call_projection_gate::__PairArgs { tag })
            .expect("call");
    }
    let after = runtime.memory_report().nodes;
    assert!(
        after <= before + 8,
        "reclaimed slots must be reused: {before} -> {after}"
    );
    assert!(
        !runtime.is_live(stale),
        "the stale id must not alias a reused slot"
    );
    let live = runtime
        .dump()
        .into_iter()
        .filter(|node| node.kind == "InvocationBody")
        .map(|node| node.id)
        .find(|id| runtime.is_live(*id))
        .expect("a live body");
    assert!(runtime.is_live(live));
}

// ---------------------------------------------------------------------------
// Signed-weight bag join (C5)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct LeftWeights {
    entries: Map<u32, i64>,
}

#[view(source)]
pub struct RightWeights {
    entries: Map<u32, i64>,
}

#[view(derived)]
pub struct JoinedWeights(Map<u32, i64>);

struct WeightJoin;

impl irrigation::JoinSpec for WeightJoin {
    type Key = u32;
    type Left = i64;
    type Right = i64;
    type Joined = i64;

    fn left() -> (irrigation::ViewId, irrigation::FieldId) {
        (
            <LeftWeights as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        )
    }

    fn right() -> (irrigation::ViewId, irrigation::FieldId) {
        (
            <RightWeights as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        )
    }

    fn combine(left: &i64, right: &i64) -> i64 {
        left * right
    }

    fn target() -> Option<(irrigation::ViewId, irrigation::FieldId)> {
        Some((
            <JoinedWeights as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        ))
    }
}

#[test]
fn signed_weight_join_applies_the_bilinear_rule() {
    let runtime = Runtime::new();
    runtime.register_view(<LeftWeights as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<RightWeights as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<JoinedWeights as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_root(irrigation::EquiJoin::<WeightJoin>::new())
        .expect("install");
    runtime
        .edit(|edit| {
            {
                let mut left = edit.view::<LeftWeights>();
                let mut entries = left.entries();
                entries.insert(1, 2);
                entries.insert(2, -3);
            }
            let mut right = edit.view::<RightWeights>();
            let mut entries = right.entries();
            entries.insert(1, 3);
            entries.insert(2, 4);
        })
        .expect("seed");
    let joined = runtime
        .read_outputs_general::<irrigation::EquiJoin<WeightJoin>>()
        .expect("read");
    assert_eq!(joined.get(&1), Some(&6));
    assert_eq!(joined.get(&2), Some(&(-12)));
    assert_eq!(
        runtime.snapshot().view::<JoinedWeights>().field0(),
        vec![(1, 6), (2, -12)]
    );

    // Both heterogeneous inputs move in one transaction: the cross term is
    // counted once, giving 5 * 7 rather than a double-counted delta.
    runtime
        .edit(|edit| {
            {
                let mut left = edit.view::<LeftWeights>();
                let mut entries = left.entries();
                entries.insert(1, 5);
            }
            let mut right = edit.view::<RightWeights>();
            let mut entries = right.entries();
            entries.insert(1, 7);
        })
        .expect("edit");
    let joined = runtime
        .read_outputs_general::<irrigation::EquiJoin<WeightJoin>>()
        .expect("read");
    assert_eq!(joined.get(&1), Some(&35));
    assert_eq!(
        runtime.snapshot().view::<JoinedWeights>().field0(),
        vec![(1, 35), (2, -12)]
    );

    // Retracting a weight removes its joined entry.
    runtime
        .edit(|edit| {
            let mut left = edit.view::<LeftWeights>();
            let mut entries = left.entries();
            entries.remove(&2);
        })
        .expect("retract");
    let joined = runtime
        .read_outputs_general::<irrigation::EquiJoin<WeightJoin>>()
        .expect("read");
    assert!(!joined.contains_key(&2));
    eprintln!(
        "target: {:?}",
        runtime.snapshot().view::<JoinedWeights>().field0()
    );
    assert!(!joined.contains_key(&2));
    assert_eq!(
        runtime.snapshot().view::<JoinedWeights>().field0(),
        vec![(1, 35)]
    );
}

// ---------------------------------------------------------------------------
// Typed general component: inputs, params, returns (C3)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct TypedSource {
    left: u64,
    right: u64,
}

#[derive(Clone)]
struct TypedInputs {
    left: u64,
    right: u64,
}

impl irrigation::GeneralInputs for TypedInputs {
    fn read(cx: &mut irrigation::GeneralCx) -> irrigation::Result<Self> {
        let view = <TypedSource as irrigation::ViewSchema>::DESCRIPTOR.id;
        Ok(Self {
            left: cx.read_typed(irrigation::UnitKey::field(
                view,
                irrigation::FieldId(0),
                irrigation::UnitKind::Value,
            ))?,
            right: cx.read_typed(irrigation::UnitKey::field(
                view,
                irrigation::FieldId(1),
                irrigation::UnitKind::Value,
            ))?,
        })
    }
}

struct TypedProduct;

impl irrigation::TypedGeneralComponent for TypedProduct {
    type Inputs = TypedInputs;
    type Params = u64;
    type State = (u64, u64);
    type Returns = (u64, u64, u64);

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00DD_0004);
    const NAME: &'static str = "gates::TypedProduct";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<TypedSource as irrigation::ViewSchema>::DESCRIPTOR];

    fn params(&self) -> u64 {
        10
    }

    fn dependency_updates(&self) -> irrigation::DependencyUpdates {
        irrigation::DependencyUpdates::Incremental
    }

    fn initialize(
        &self,
        _cx: &mut irrigation::GeneralCx,
        inputs: &TypedInputs,
        params: &u64,
    ) -> irrigation::Result<((u64, u64), (u64, u64, u64))> {
        let state = (inputs.left, inputs.right);
        Ok((state, (state.0, state.1, state.0 * state.1 * params)))
    }

    fn update(
        &self,
        _cx: &mut irrigation::GeneralCx,
        _state: &(u64, u64),
        inputs: &TypedInputs,
        _changes: &irrigation::ChangeBatch,
        params: &u64,
    ) -> irrigation::Result<((u64, u64), (u64, u64, u64))> {
        let state = (inputs.left, inputs.right);
        Ok((state, (state.0, state.1, state.0 * state.1 * params)))
    }
}

#[test]
fn typed_general_components_publish_typed_returns() {
    let runtime = Runtime::new();
    runtime
        .install_typed_general(TypedProduct)
        .expect("install");
    runtime
        .edit(|edit| {
            let mut source = edit.view::<TypedSource>();
            source.set_left(2);
            source.set_right(3);
        })
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::TypedAdapter<TypedProduct>>()
            .expect("read"),
        (2, 3, 60)
    );
    runtime
        .edit(|edit| edit.view::<TypedSource>().set_right(4))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::TypedAdapter<TypedProduct>>()
            .expect("read"),
        (2, 4, 80)
    );
}

// ---------------------------------------------------------------------------
// Automatic collection policy (Phase 4)
// ---------------------------------------------------------------------------

#[test]
fn automatic_collection_reclaims_on_demand_entry() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        auto_collect_every: Some(1),
        ..irrigation::RuntimeConfig::default()
    });
    runtime.install(churn::Root).expect("install");
    for value in 0..6u64 {
        runtime
            .edit(|edit| edit.view::<ChurnKey>().set_value(value))
            .expect("edit");
        assert_eq!(runtime.read_outputs::<churn::Root>().expect("read"), value);
    }
    assert!(
        runtime.stats().auto_collects > 0,
        "the interval policy must reclaim without a manual collect"
    );
    // The interval reclaims before each demand, so at most the current and the
    // previous invocation remain until the next pass.
    assert!(runtime.memory_report().invocations <= 2);
    assert!(runtime.memory_report().scopes <= 2);
}

// ---------------------------------------------------------------------------
// Async general component: pending futures (C3)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct AsyncSource {
    left: u64,
    right: u64,
}

struct PendingOnce(bool);

impl std::future::Future for PendingOnce {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            std::task::Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

struct AsyncSum;

impl irrigation::AsyncGeneralComponent for AsyncSum {
    type State = (u64, u64);
    type Output = (u64, u64, u64);

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00DD_0005);
    const NAME: &'static str = "gates::AsyncSum";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<AsyncSource as irrigation::ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> irrigation::DependencyUpdates {
        irrigation::DependencyUpdates::Incremental
    }

    fn initialize<'a>(
        &'a self,
        cx: &'a mut irrigation::GeneralCx,
    ) -> irrigation::ComponentFuture<'a, ((u64, u64), (u64, u64, u64))> {
        Box::pin(async move {
            let view = <AsyncSource as irrigation::ViewSchema>::DESCRIPTOR.id;
            let left: u64 = cx.read_typed(irrigation::UnitKey::field(
                view,
                irrigation::FieldId(0),
                irrigation::UnitKind::Value,
            ))?;
            let right: u64 = cx.read_typed(irrigation::UnitKey::field(
                view,
                irrigation::FieldId(1),
                irrigation::UnitKind::Value,
            ))?;
            PendingOnce(false).await;
            Ok(((left, right), (left, right, left + right)))
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a mut irrigation::GeneralCx,
        _state: &'a (u64, u64),
        _changes: &'a irrigation::ChangeBatch,
    ) -> irrigation::ComponentFuture<'a, ((u64, u64), (u64, u64, u64))> {
        Box::pin(async move {
            let view = <AsyncSource as irrigation::ViewSchema>::DESCRIPTOR.id;
            let left: u64 = cx.read_typed(irrigation::UnitKey::field(
                view,
                irrigation::FieldId(0),
                irrigation::UnitKind::Value,
            ))?;
            let right: u64 = cx.read_typed(irrigation::UnitKey::field(
                view,
                irrigation::FieldId(1),
                irrigation::UnitKind::Value,
            ))?;
            PendingOnce(false).await;
            Ok(((left, right), (left, right, left + right)))
        })
    }
}

#[test]
fn pending_component_futures_are_driven_to_completion() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(irrigation::AsyncAdapter::new(AsyncSum))
        .expect("install");
    runtime
        .edit(|edit| {
            let mut source = edit.view::<AsyncSource>();
            source.set_left(2);
            source.set_right(3);
        })
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::AsyncAdapter<AsyncSum>>()
            .expect("initialize"),
        (2, 3, 5)
    );
    runtime
        .edit(|edit| edit.view::<AsyncSource>().set_right(4))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::AsyncAdapter<AsyncSum>>()
            .expect("update"),
        (2, 4, 6)
    );
}

// ---------------------------------------------------------------------------
// Tree flatten map (Phase 3 cross-family)
// ---------------------------------------------------------------------------

#[tree_view(root = FlattenDoc, view = FlattenTree)]
pub mod flatten_tree {
    /// Root of the flattening test tree.
    #[derive(Clone, Debug, PartialEq)]
    pub enum FlattenDoc {
        /// A forest of items.
        Forest { items: Vec<FlattenItem> },
    }

    /// One tree item.
    #[derive(Clone, Debug, PartialEq)]
    pub enum FlattenItem {
        /// A leaf with a value.
        Leaf { value: u64 },
        /// A group of items.
        Group { items: Vec<FlattenItem> },
    }
}

#[view(derived)]
pub struct FlatTags(Map<u64, u32>);

#[expect(
    clippy::unnecessary_wraps,
    reason = "`flatten_map` takes `fn(&Node) -> Option<u32>`; this grammar maps every node"
)]
fn flatten_tag(node: &flatten_tree::FlattenTreeNode) -> Option<u32> {
    match node {
        flatten_tree::FlattenTreeNode::FlattenDoc(_) => Some(0),
        flatten_tree::FlattenTreeNode::FlattenItem(_) => Some(1),
    }
}

#[irrigation]
mod flatten_gate {
    use super::{FlatTags, flatten_tag, flatten_tree};

    #[component]
    pub fn root(tree: &flatten_tree::FlattenTree) -> u32 {
        irrigation::ensure_view(<FlatTags as irrigation::ViewSchema>::DESCRIPTOR);
        let target = FlatTags.field0();
        match tree.root() {
            Some(document) => {
                document.flatten_map(&target, flatten_tag);
                0
            }
            None => 0,
        }
    }
}

#[test]
fn tree_flatten_map_publishes_one_entry_per_node() {
    let runtime = Runtime::new();
    runtime.install(flatten_gate::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<flatten_tree::FlattenTree>();
            tree.set_root(&flatten_tree::FlattenDoc::Forest {
                items: vec![
                    flatten_tree::FlattenItem::Leaf { value: 1 },
                    flatten_tree::FlattenItem::Group {
                        items: vec![flatten_tree::FlattenItem::Leaf { value: 2 }],
                    },
                ],
            });
        })
        .expect("seed");
    assert_eq!(
        runtime.read_outputs::<flatten_gate::Root>().expect("read"),
        0
    );
    let tags = runtime.snapshot().view::<FlatTags>().field0();
    assert_eq!(tags.len(), 4);
    assert_eq!(tags.iter().filter(|(_, tag)| *tag == 0).count(), 1);
    assert_eq!(tags.iter().filter(|(_, tag)| *tag == 1).count(), 3);

    // Rebuilding the tree retracts the old node ids and publishes the new ones.
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<flatten_tree::FlattenTree>();
            tree.set_root(&flatten_tree::FlattenDoc::Forest {
                items: vec![flatten_tree::FlattenItem::Leaf { value: 3 }],
            });
        })
        .expect("edit");
    let _ = runtime.read_outputs::<flatten_gate::Root>().expect("read");
    let tags = runtime.snapshot().view::<FlatTags>().field0();
    assert_eq!(tags.len(), 2);
    assert_eq!(tags.iter().filter(|(_, tag)| *tag == 0).count(), 1);
    assert_eq!(tags.iter().filter(|(_, tag)| *tag == 1).count(), 1);
}

// ---------------------------------------------------------------------------
// request().await receipts (C3)
// ---------------------------------------------------------------------------

struct AsyncRequestSum;

impl irrigation::AsyncGeneralComponent for AsyncRequestSum {
    type State = (u64, u64);
    type Output = (u64, u64, u64);

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00DD_0006);
    const NAME: &'static str = "gates::AsyncRequestSum";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<AsyncSource as irrigation::ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> irrigation::DependencyUpdates {
        irrigation::DependencyUpdates::Incremental
    }

    fn initialize<'a>(
        &'a self,
        cx: &'a mut irrigation::GeneralCx,
    ) -> irrigation::ComponentFuture<'a, ((u64, u64), (u64, u64, u64))> {
        Box::pin(async move {
            let view = <AsyncSource as irrigation::ViewSchema>::DESCRIPTOR.id;
            let left: u64 = cx
                .request_typed(irrigation::UnitKey::field(
                    view,
                    irrigation::FieldId(0),
                    irrigation::UnitKind::Value,
                ))
                .await?;
            let right: u64 = cx
                .request_typed(irrigation::UnitKey::field(
                    view,
                    irrigation::FieldId(1),
                    irrigation::UnitKind::Value,
                ))
                .await?;
            Ok(((left, right), (left, right, left + right)))
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a mut irrigation::GeneralCx,
        _state: &'a (u64, u64),
        _changes: &'a irrigation::ChangeBatch,
    ) -> irrigation::ComponentFuture<'a, ((u64, u64), (u64, u64, u64))> {
        Box::pin(async move {
            let view = <AsyncSource as irrigation::ViewSchema>::DESCRIPTOR.id;
            let left: u64 = cx
                .request_typed(irrigation::UnitKey::field(
                    view,
                    irrigation::FieldId(0),
                    irrigation::UnitKind::Value,
                ))
                .await?;
            let right: u64 = cx
                .request_typed(irrigation::UnitKey::field(
                    view,
                    irrigation::FieldId(1),
                    irrigation::UnitKind::Value,
                ))
                .await?;
            Ok(((left, right), (left, right, left + right)))
        })
    }
}

#[test]
fn request_receipts_are_awaitable() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(irrigation::AsyncAdapter::new(AsyncRequestSum))
        .expect("install");
    runtime
        .edit(|edit| {
            let mut source = edit.view::<AsyncSource>();
            source.set_left(2);
            source.set_right(3);
        })
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::AsyncAdapter<AsyncRequestSum>>()
            .expect("initialize"),
        (2, 3, 5)
    );
    runtime
        .edit(|edit| edit.view::<AsyncSource>().set_right(4))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::AsyncAdapter<AsyncRequestSum>>()
            .expect("update"),
        (2, 4, 6)
    );
}

// ---------------------------------------------------------------------------
// Identity reuse and cycle diagnostics (C4)
// ---------------------------------------------------------------------------

#[test]
fn site_identity_reuse_is_counted() {
    let runtime = Runtime::new();
    runtime.install(churn::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<ChurnKey>().set_value(1))
        .expect("seed");
    let _ = runtime.read_outputs::<churn::Root>().expect("read");
    let _ = runtime.read_outputs::<churn::Root>().expect("read");
    let stats = runtime.stats();
    assert!(stats.site_creations > 0, "{stats:?}");
    assert!(stats.site_reuses > 0, "{stats:?}");
    let explain = runtime.explain();
    assert_eq!(explain.site_creations, stats.site_creations);
    assert_eq!(explain.site_reuses, stats.site_reuses);
    assert!(runtime.wait_cycles().is_empty());
}

// ---------------------------------------------------------------------------
// Routing observation subscriptions (C2)
// ---------------------------------------------------------------------------

#[test]
fn routing_subscriptions_fire_only_on_provider_changes() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runtime = Runtime::new();
    runtime
        .install(routing_generation_gate::Root)
        .expect("install");
    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(true))
        .expect("seed");
    let _ = runtime
        .read_outputs::<routing_generation_gate::Root>()
        .expect("read");
    let unit = irrigation::UnitKey::field(
        <RouteSlot as irrigation::ViewSchema>::DESCRIPTOR.id,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
    );
    let fired = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&fired);
    let _routing_subscription = runtime
        .observe_routing::<RouteSlot>(unit, move |_snapshot| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("observe routing");

    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(false))
        .expect("handover");
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(true))
        .expect("handover back");
    assert_eq!(fired.load(Ordering::SeqCst), 2);

    // A source unit has no provider routing; value edits do not fire.
    let source_unit = irrigation::UnitKey::field(
        <RouteFlag as irrigation::ViewSchema>::DESCRIPTOR.id,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
    );
    let source_fired = std::sync::Arc::new(AtomicUsize::new(0));
    let source_counter = std::sync::Arc::clone(&source_fired);
    let _source_subscription = runtime
        .observe_routing::<RouteFlag>(source_unit, move |_snapshot| {
            source_counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("observe source routing");
    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(false))
        .expect("value edit");
    assert_eq!(source_fired.load(Ordering::SeqCst), 0);
    assert_eq!(fired.load(Ordering::SeqCst), 3);
}

// ---------------------------------------------------------------------------
// Bounded invocation cache (Phase 4)
// ---------------------------------------------------------------------------

#[test]
fn invocation_cap_bounds_the_cache() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        max_invocations: Some(2),
        ..irrigation::RuntimeConfig::default()
    });
    runtime.install(churn::Root).expect("install");
    for value in 0..16u64 {
        runtime
            .edit(|edit| edit.view::<ChurnKey>().set_value(value))
            .expect("edit");
        assert_eq!(runtime.read_outputs::<churn::Root>().expect("read"), value);
        assert!(
            runtime.memory_report().invocations <= 3,
            "the cache must stay bounded at step {value}: {}",
            runtime.memory_report().invocations
        );
    }
    assert!(
        runtime.stats().invocations_evicted > 0,
        "the cap must evict unreferenced invocations"
    );
}

#[irrigation]
mod source_entry_root {
    use super::ConsumerTable;

    #[component]
    pub fn root(table: &ConsumerTable) -> u64 {
        let entries = table.entries();
        let keys = entries.keys();
        keys.into_iter()
            .map(|key| entries.get(&key).unwrap_or(0))
            .sum::<u64>()
    }
}

#[test]
fn a_per_key_observer_completes_before_the_whole_view_is_rematerialized() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runtime = Runtime::new();
    runtime.install(source_entry_root::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(1, 10);
        })
        .expect("seed");
    let _ = runtime
        .read_outputs::<source_entry_root::Root>()
        .expect("warm");
    let executions = runtime.stats().executions;

    let seen = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&seen);
    let _subscription = runtime
        .observe_entry::<ConsumerTable, u32>(irrigation::FieldId(0), 1u32, move |_snapshot| {
            sink.fetch_add(1, Ordering::SeqCst);
        })
        .expect("observe entry");
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            entries.insert(1, 11);
        })
        .expect("edit");
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    assert_eq!(
        runtime.stats().executions,
        executions,
        "a source-only entry observer must not rematerialize the root"
    );
}

// ---------------------------------------------------------------------------
// Incoming adjacency (N18)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct IncomingGraph {
    edges: Graph<u64, u64>,
}

#[irrigation]
mod incoming_adjacency_gate {
    use super::IncomingGraph;

    #[component]
    pub fn root(graph: &IncomingGraph) -> Vec<(u64, u64)> {
        let edges = graph.edges();
        edges.incoming(2)
    }
}

#[test]
fn graph_incoming_adjacency_is_indexed_per_node() {
    let runtime = Runtime::new();
    runtime
        .install(incoming_adjacency_gate::Root)
        .expect("install");
    let first = runtime
        .edit(|edit| {
            let mut graph = edit.view::<IncomingGraph>();
            let mut edges = graph.edges();
            let first = edges.add_edge(1, 2, 7);
            let _ = edges.add_edge(3, 2, 8);
            let _ = edges.add_edge(2, 4, 9);
            first
        })
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<incoming_adjacency_gate::Root>()
            .expect("read"),
        vec![(first, 1), (first + 1, 3)]
    );

    runtime
        .edit(|edit| {
            let mut graph = edit.view::<IncomingGraph>();
            let mut edges = graph.edges();
            edges.remove_edge(first);
        })
        .expect("remove");
    assert_eq!(
        runtime
            .read_outputs::<incoming_adjacency_gate::Root>()
            .expect("read"),
        vec![(first + 1, 3)]
    );
}

// ---------------------------------------------------------------------------
// Region routing certificates (C2)
// ---------------------------------------------------------------------------

#[test]
fn region_routing_stamps_are_invalidated_by_handovers() {
    let runtime = Runtime::new();
    runtime
        .install(routing_generation_gate::Root)
        .expect("install");
    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(true))
        .expect("seed");
    let _ = runtime
        .read_outputs::<routing_generation_gate::Root>()
        .expect("read");
    let view = <RouteSlot as irrigation::ViewSchema>::DESCRIPTOR.id;
    let stamp = runtime.routing_stamp(view).expect("stamp");
    assert!(runtime.validate_routing(&stamp));

    // A no-op edit leaves the certificate valid.
    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(true))
        .expect("no-op");
    assert!(runtime.validate_routing(&stamp));

    // A provider handover invalidates the old certificate.
    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(false))
        .expect("handover");
    let _ = runtime
        .read_outputs::<routing_generation_gate::Root>()
        .expect("read");
    assert!(!runtime.validate_routing(&stamp));
    let fresh = runtime.routing_stamp(view).expect("stamp");
    assert!(runtime.validate_routing(&fresh));
}

// ---------------------------------------------------------------------------
// Scope identity in explanations (C4)
// ---------------------------------------------------------------------------

#[test]
fn explanations_expose_scope_identity() {
    let runtime = Runtime::new();
    runtime.install(churn::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<ChurnKey>().set_value(1))
        .expect("seed");
    let _ = runtime.read_outputs::<churn::Root>().expect("read");
    let dump = runtime.dump();
    let root = dump
        .iter()
        .find(|node| node.kind == "InvocationBody" && node.label == "root body")
        .expect("root body");
    let invocation = dump
        .iter()
        .find(|node| node.kind == "InvocationBody" && node.label != "root body")
        .expect("invocation");
    let root_explanation = runtime.explain_node(root.id);
    assert!(root_explanation.scope_path.is_empty());
    let invocation_explanation = runtime.explain_node(invocation.id);
    assert!(!invocation_explanation.scope_path.is_empty());
    assert!(
        invocation_explanation.scope_path[0].contains("discriminant"),
        "the ancestry names the internship discriminant: {:?}",
        invocation_explanation.scope_path
    );
    assert_ne!(invocation_explanation.scope, root_explanation.scope);
}

// ---------------------------------------------------------------------------
// Schedule perturbation (C5)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct PerturbSeeds {
    values: List<u32>,
}

static PERTURB: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn perturb(value: u32) -> u64 {
    let spins = PERTURB.load(std::sync::atomic::Ordering::Relaxed) % 16;
    for _ in 0..spins {
        std::hint::spin_loop();
    }
    if spins % 3 == 0 {
        std::thread::yield_now();
    }
    u64::from(value) * 3
}

#[irrigation]
mod perturb_map {
    use super::{PerturbSeeds, perturb};

    #[component]
    pub fn total(seeds: &PerturbSeeds) -> u64 {
        let values = seeds.values();
        values.map(|n| perturb(*n)).iter().sum::<u64>()
    }
}

#[test]
fn schedule_perturbation_preserves_parallel_results() {
    for workers in [1usize, 2, 4] {
        let runtime = Runtime::with_config(irrigation::RuntimeConfig {
            workers,
            stack_size: 32 * 1024 * 1024,
            ..irrigation::RuntimeConfig::default()
        });
        runtime.install(perturb_map::Total).expect("install");
        let mut model: Vec<u32> = (0..64).collect();
        runtime
            .edit(|edit| {
                let mut seeds = edit.view::<PerturbSeeds>();
                let mut values = seeds.values();
                for value in 0..64u32 {
                    values.push(value);
                }
            })
            .expect("seed");
        for round in 0..8 {
            PERTURB.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let index = (round * 7) % model.len();
            let value = 100 + round as u32;
            model[index] = value;
            runtime
                .edit(|edit| {
                    let mut seeds = edit.view::<PerturbSeeds>();
                    let mut values = seeds.values();
                    values.set(index, value);
                })
                .expect("edit");
            let expected: u64 = model.iter().map(|value| u64::from(*value) * 3).sum();
            assert_eq!(
                runtime.read_outputs::<perturb_map::Total>().expect("read"),
                expected,
                "workers {workers}, round {round}"
            );
        }
    }
}

#[test]
fn joining_two_empty_source_maps_is_empty() {
    let runtime = Runtime::new();
    runtime.register_view(<LeftWeights as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<RightWeights as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<JoinedWeights as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_root(irrigation::EquiJoin::<WeightJoin>::new())
        .expect("install");
    let joined = runtime
        .read_outputs_general::<irrigation::EquiJoin<WeightJoin>>()
        .expect("read");
    assert!(joined.is_empty());
    assert!(
        runtime
            .snapshot()
            .view::<JoinedWeights>()
            .field0()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Definition capabilities: declared views (C2)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct CapFlag {
    on: bool,
}

#[view(derived)]
pub struct CapSlot(u64);

#[irrigation]
mod cap_denied {
    use super::{CapFlag, CapSlot};

    #[component]
    pub fn root(flag: &CapFlag) -> u64 {
        let _ = flag.on();
        let unit = irrigation::UnitKey::field(
            <CapSlot as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        );
        irrigation::define_unit(unit, 7u64, "manual");
        0
    }
}

#[irrigation]
mod cap_allowed {
    use super::{CapFlag, CapSlot};

    #[component]
    pub fn root(flag: &CapFlag) -> u64 {
        let _ = flag.on();
        irrigation::ensure_view(<CapSlot as irrigation::ViewSchema>::DESCRIPTOR);
        let unit = irrigation::UnitKey::field(
            <CapSlot as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        );
        irrigation::define_unit(unit, 7u64, "manual");
        0
    }
}

#[test]
fn definitions_require_a_declared_view() {
    let runtime = Runtime::new();
    runtime.register_view(<CapSlot as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(cap_denied::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<CapFlag>().set_on(true))
        .expect("seed");
    match runtime.read_outputs::<cap_denied::Root>() {
        Err(irrigation::ComputeError::DefinitionDenied { reason, .. }) => {
            assert!(
                reason.contains("ensure_view"),
                "the denial must name the missing declaration: {reason}"
            );
        }
        other => panic!("expected a definition denial, got {other:?}"),
    }

    let runtime = Runtime::new();
    runtime.register_view(<CapSlot as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(cap_allowed::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<CapFlag>().set_on(true))
        .expect("seed");
    assert_eq!(
        runtime.read_outputs::<cap_allowed::Root>().expect("read"),
        0
    );
    assert_eq!(
        runtime.snapshot().view::<CapSlot>().field0(),
        7,
        "the declared definition commits"
    );
}

// ---------------------------------------------------------------------------
// Local executor: non-Send state (C3)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct LocalMode {
    mode: u64,
}

struct LocalSum;

impl irrigation::LocalGeneralComponent for LocalSum {
    type State = std::rc::Rc<std::cell::RefCell<Vec<u64>>>;
    type Output = u64;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00DD_0007);
    const NAME: &'static str = "gates::LocalSum";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<LocalMode as irrigation::ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> irrigation::DependencyUpdates {
        irrigation::DependencyUpdates::Incremental
    }

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let mode: u64 = cx.read_typed(irrigation::UnitKey::field(
            <LocalMode as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        let state = std::rc::Rc::new(std::cell::RefCell::new(vec![mode]));
        Ok((state, mode))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let mode: u64 = cx.read_typed(irrigation::UnitKey::field(
            <LocalMode as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        state.borrow_mut().push(mode);
        let output = *state.borrow().last().expect("state has a mode");
        Ok((std::rc::Rc::clone(state), output))
    }
}

#[test]
fn local_components_keep_non_send_state() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 0,
        ..irrigation::RuntimeConfig::default()
    });
    runtime
        .install_local_general(LocalSum)
        .expect("install local");
    runtime
        .edit(|edit| edit.view::<LocalMode>().set_mode(5))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::LocalAdapter<LocalSum>>()
            .expect("initialize"),
        5
    );
    runtime
        .edit(|edit| edit.view::<LocalMode>().set_mode(9))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::LocalAdapter<LocalSum>>()
            .expect("update"),
        9
    );

    // A pooled runtime rejects local components up front.
    let pooled = Runtime::new();
    assert!(pooled.install_local_general(LocalSum).is_err());
}

// ---------------------------------------------------------------------------
// Snapshot routing certificates (C2)
// ---------------------------------------------------------------------------

#[test]
fn snapshots_carry_routing_certificates() {
    let runtime = Runtime::new();
    runtime
        .install(routing_generation_gate::Root)
        .expect("install");
    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(true))
        .expect("seed");
    let _ = runtime
        .read_outputs::<routing_generation_gate::Root>()
        .expect("read");
    let view = <RouteSlot as irrigation::ViewSchema>::DESCRIPTOR.id;
    let snapshot = runtime.snapshot();
    let stamp = snapshot.routing_stamp(view).expect("routing stamp");
    assert!(runtime.validate_routing(&stamp));

    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(false))
        .expect("handover");
    let _ = runtime
        .read_outputs::<routing_generation_gate::Root>()
        .expect("read");
    assert!(
        !runtime.validate_routing(&stamp),
        "the old snapshot is stale"
    );
    let fresh = runtime
        .snapshot()
        .routing_stamp(view)
        .expect("fresh routing stamp");
    assert!(runtime.validate_routing(&fresh));
}

// ---------------------------------------------------------------------------
// Explicit route selection for live conflicts (C2)
// ---------------------------------------------------------------------------

/// Sticky selector: the first conflict picks `prefer_second`, and the choice
/// is then authoritative for later claims of the same route.
struct StickySelector {
    winner: std::sync::Mutex<Option<irrigation::NodeId>>,
    prefer_second: bool,
}

impl irrigation::RouteSelector for StickySelector {
    fn select(
        &self,
        _unit: &irrigation::UnitKey,
        first: irrigation::NodeId,
        second: irrigation::NodeId,
    ) -> irrigation::NodeId {
        let mut winner = self
            .winner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *winner.get_or_insert(if self.prefer_second { second } else { first })
    }
}

#[test]
fn a_route_selector_resolves_live_conflicts() {
    let runtime = Runtime::new();
    runtime.set_route_selector(std::sync::Arc::new(StickySelector {
        winner: std::sync::Mutex::new(None),
        prefer_second: true,
    }));
    assert_eq!(
        runtime
            .call::<call_conflict::__FirstSpec>(call_conflict::__FirstArgs { tag: 0 })
            .expect("first"),
        1
    );
    assert_eq!(runtime.snapshot().view::<CallSlot>().field0(), 1);
    assert_eq!(
        runtime
            .call::<call_conflict::__SecondSpec>(call_conflict::__SecondArgs { tag: 0 })
            .expect("second"),
        2
    );
    assert_eq!(
        runtime.snapshot().view::<CallSlot>().field0(),
        2,
        "the selector keeps the later provider"
    );
    assert_eq!(
        runtime
            .call::<call_conflict::__FirstSpec>(call_conflict::__FirstArgs { tag: 0 })
            .expect("first again"),
        1
    );
    assert_eq!(
        runtime.snapshot().view::<CallSlot>().field0(),
        2,
        "the selected route stays authoritative"
    );

    // Preferring the earlier provider retracts the later definition; the
    // earlier provider restores the value on its next demand.
    let runtime = Runtime::new();
    runtime.set_route_selector(std::sync::Arc::new(StickySelector {
        winner: std::sync::Mutex::new(None),
        prefer_second: false,
    }));
    let _ = runtime
        .call::<call_conflict::__FirstSpec>(call_conflict::__FirstArgs { tag: 0 })
        .expect("first");
    assert_eq!(
        runtime
            .call::<call_conflict::__SecondSpec>(call_conflict::__SecondArgs { tag: 0 })
            .expect("second"),
        2
    );
    assert_eq!(
        runtime.snapshot().view::<CallSlot>().field0(),
        1,
        "the earlier route is restored"
    );
    let _ = runtime
        .call::<call_conflict::__FirstSpec>(call_conflict::__FirstArgs { tag: 0 })
        .expect("first");
    assert_eq!(runtime.snapshot().view::<CallSlot>().field0(), 1);
}

// ---------------------------------------------------------------------------
// Routing recipes as selector policies (C2)
// ---------------------------------------------------------------------------

#[test]
fn routing_recipes_select_their_provider() {
    let runtime = Runtime::new();
    assert_eq!(
        runtime
            .call::<call_conflict::__FirstSpec>(call_conflict::__FirstArgs { tag: 0 })
            .expect("first"),
        1
    );
    let view = <CallSlot as irrigation::ViewSchema>::DESCRIPTOR.id;
    let recipe = runtime.routing_recipe(view).expect("recipe");
    assert!(runtime.validate_routing(&recipe.stamp));
    let unit =
        irrigation::UnitKey::field(view, irrigation::FieldId(0), irrigation::UnitKind::Value);
    assert!(
        recipe.providers.iter().any(|(key, _)| *key == unit),
        "the recipe names the installed unit"
    );
    runtime.set_route_selector(std::sync::Arc::new(irrigation::RecipeSelector::new(
        &recipe,
    )));
    assert_eq!(
        runtime
            .call::<call_conflict::__SecondSpec>(call_conflict::__SecondArgs { tag: 0 })
            .expect("second"),
        2
    );
    assert_eq!(
        runtime.snapshot().view::<CallSlot>().field0(),
        1,
        "the recipe keeps its provider"
    );
}

// ---------------------------------------------------------------------------
// Aggregate identity diagnostics (C4)
// ---------------------------------------------------------------------------

#[test]
fn identity_report_summarizes_reuse() {
    let runtime = Runtime::new();
    runtime.install(churn::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<ChurnKey>().set_value(1))
        .expect("seed");
    let _ = runtime.read_outputs::<churn::Root>().expect("read");
    let _ = runtime.read_outputs::<churn::Root>().expect("read");
    let report = runtime.identity_report();
    assert!(report.site_creations > 0);
    assert!(report.site_reuses > 0);
    assert!(report.scopes >= 1);
    assert!(report.regions >= 1);
    assert!(report.unit_nodes >= 1);
    assert_eq!(report.site_reuses, runtime.stats().site_reuses);
    assert_eq!(report.invocations, runtime.memory_report().invocations);
}

// ---------------------------------------------------------------------------
// Planned provider routes (C2)
// ---------------------------------------------------------------------------

#[test]
fn planned_routes_authorize_one_component() {
    let runtime = Runtime::new();
    let view = <CallSlot as irrigation::ViewSchema>::DESCRIPTOR.id;
    let unit =
        irrigation::UnitKey::field(view, irrigation::FieldId(0), irrigation::UnitKind::Value);
    runtime.plan_route([(
        unit,
        <call_conflict::__FirstSpec as irrigation::ComponentSpec>::ID,
    )]);
    assert_eq!(
        runtime
            .call::<call_conflict::__FirstSpec>(call_conflict::__FirstArgs { tag: 0 })
            .expect("planned provider"),
        1
    );
    match runtime.call::<call_conflict::__SecondSpec>(call_conflict::__SecondArgs { tag: 0 }) {
        Err(irrigation::ComputeError::DefinitionDenied { reason, .. }) => {
            assert!(
                reason.contains("planned"),
                "the denial must name the planned route: {reason}"
            );
        }
        other => panic!("expected a planned-route denial, got {other:?}"),
    }
    assert_eq!(
        runtime.snapshot().view::<CallSlot>().field0(),
        1,
        "the planned provider's value stays authoritative"
    );
}

// ---------------------------------------------------------------------------
// Planned routes join the routing generation (C2)
// ---------------------------------------------------------------------------

#[test]
fn planning_a_route_advances_the_routing_generation() {
    let runtime = Runtime::new();
    runtime.register_view(<CallSlot as irrigation::ViewSchema>::DESCRIPTOR);
    let view = <CallSlot as irrigation::ViewSchema>::DESCRIPTOR.id;
    let unit =
        irrigation::UnitKey::field(view, irrigation::FieldId(0), irrigation::UnitKind::Value);
    let before = runtime.routing_stamp(view).expect("region stamp");
    runtime.plan_route([(
        unit.clone(),
        <call_conflict::__FirstSpec as irrigation::ComponentSpec>::ID,
    )]);
    assert!(
        !runtime.validate_routing(&before),
        "installing a plan is a routing change"
    );
    let after = runtime.routing_stamp(view).expect("region stamp");
    assert!(runtime.validate_routing(&after));
    let recipe = runtime.routing_recipe(view).expect("recipe");
    assert!(
        recipe.planned.iter().any(|(planned, component)| {
            *planned == unit
                && *component == <call_conflict::__FirstSpec as irrigation::ComponentSpec>::ID
        }),
        "the recipe names the planned component"
    );
}

// ---------------------------------------------------------------------------
// Tree import reports (N22 partial)
// ---------------------------------------------------------------------------

#[test]
fn tree_imports_report_cost_and_reuse() {
    let runtime = Runtime::new();
    runtime.install(flatten_gate::Root).expect("install");
    let mut import = None;
    let mut reconcile = None;
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<flatten_tree::FlattenTree>();
            let first = flatten_tree::FlattenDoc::Forest {
                items: vec![
                    flatten_tree::FlattenItem::Leaf { value: 1 },
                    flatten_tree::FlattenItem::Group {
                        items: vec![flatten_tree::FlattenItem::Leaf { value: 2 }],
                    },
                ],
            };
            let (root, report) = tree.import_root(&first);
            import = Some(report);
            let second = flatten_tree::FlattenDoc::Forest {
                items: vec![
                    flatten_tree::FlattenItem::Leaf { value: 9 },
                    flatten_tree::FlattenItem::Group {
                        items: vec![flatten_tree::FlattenItem::Leaf { value: 2 }],
                    },
                ],
            };
            let (_, report) = tree.reconcile_root(root, &second);
            reconcile = Some(report);
        })
        .expect("import");
    let import = import.expect("import report");
    assert!(import.created >= 4, "{import:?}");
    assert_eq!(import.reused, 0);
    assert_eq!(import.ambiguous, 0);
    let reconcile = reconcile.expect("reconcile report");
    assert!(reconcile.reused >= 1, "{reconcile:?}");
    assert_eq!(reconcile.ambiguous, 0);
    let _ = runtime.read_outputs::<flatten_gate::Root>().expect("read");
}

// ---------------------------------------------------------------------------
// Tracked context arguments (N23 partial)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct CtxFlag {
    on: bool,
}

#[irrigation]
mod ctx_reader {
    use super::CtxFlag;

    #[component]
    pub fn read_ctx(ctx: irrigation::Context<u64, bool>) -> bool {
        ctx.read()
    }

    #[component]
    pub fn root(flag: &CtxFlag) -> bool {
        let _ = flag.on();
        let unit = irrigation::UnitKey::field(
            <CtxFlag as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        );
        read_ctx(irrigation::Context::new(7u64, unit))
    }
}

#[test]
fn tracked_context_arguments_do_not_rekey_on_value_changes() {
    let runtime = Runtime::new();
    runtime.install(ctx_reader::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<CtxFlag>().set_on(false))
        .expect("seed");
    assert!(!runtime.read_outputs::<ctx_reader::Root>().expect("read"));
    let invocations = runtime.memory_report().invocations;
    runtime
        .edit(|edit| edit.view::<CtxFlag>().set_on(true))
        .expect("edit");
    assert!(runtime.read_outputs::<ctx_reader::Root>().expect("read"));
    assert_eq!(
        runtime.memory_report().invocations,
        invocations,
        "a value change with the same context key must reuse the invocation"
    );
}

// ---------------------------------------------------------------------------
// Typed tree references as cross-View keys (N22)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct RefMap(Map<flatten_tree::FlattenDocRef, u64>);

#[irrigation]
mod ref_map {
    use super::{RefMap, flatten_tree};

    #[component]
    pub fn root(tree: &flatten_tree::FlattenTree) -> Option<u64> {
        irrigation::ensure_view(<RefMap as irrigation::ViewSchema>::DESCRIPTOR);
        match tree.root() {
            Some(document) => {
                RefMap.define_field0(document.clone(), 7u64);
                RefMap.field0().get(&document)
            }
            None => None,
        }
    }
}

#[test]
fn typed_tree_references_are_cross_view_keys() {
    let runtime = Runtime::new();
    runtime.install(ref_map::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<flatten_tree::FlattenTree>();
            tree.set_root(&flatten_tree::FlattenDoc::Forest {
                items: vec![flatten_tree::FlattenItem::Leaf { value: 1 }],
            });
        })
        .expect("seed");
    assert_eq!(
        runtime.read_outputs::<ref_map::Root>().expect("read"),
        Some(7)
    );
    let entries = runtime.snapshot().view::<RefMap>().field0();
    assert_eq!(entries.len(), 1);
}

// ---------------------------------------------------------------------------
// Whole-value derived list identity reads (collections-seq-01, plan F2)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct WholeSlotMode {
    mode: u64,
}

#[view(derived)]
pub struct WholeSlot(List<u64>);

#[view(derived)]
pub struct WholeSlotMap(Map<u32, u64>);

#[irrigation]
mod whole_slot_reads {
    use super::{WholeSlot, WholeSlotMap, WholeSlotMode};

    #[component]
    pub fn provider(mode: &WholeSlotMode) -> u64 {
        if mode.mode() == 1 {
            let _ = WholeSlot::from([10u64, 20, 30]);
        } else if mode.mode() == 2 {
            let _ = WholeSlot::from([10u64]);
        } else {
            let _ = WholeSlot::from([10u64, 99]);
        }
        0
    }

    #[component]
    pub fn key_reader() -> (Option<u64>, Option<u64>) {
        (WholeSlot.get_key(1), WholeSlot.get_key(2))
    }

    #[component]
    pub fn root(mode: &WholeSlotMode) -> (Option<u64>, Option<u64>) {
        let _provider = provider(mode);
        let keys = key_reader();
        irrigation::ensure_view(<WholeSlotMap as irrigation::ViewSchema>::DESCRIPTOR);
        let target = WholeSlotMap.field0();
        WholeSlot
            .field0()
            .to_map(&target, |id, value| Some((id as u32, *value)));
        keys
    }
}

#[test]
fn whole_value_list_identity_reads_follow_the_whole_definition() {
    let runtime = Runtime::new();
    runtime.install(whole_slot_reads::Root).expect("install");
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<whole_slot_reads::Root>()
            .expect("read")
    };
    let mapped = |runtime: &Runtime| runtime.snapshot().view::<WholeSlotMap>().field0();

    runtime
        .edit(|edit| edit.view::<WholeSlotMode>().set_mode(1))
        .expect("edit");
    assert_eq!(read(&runtime), (Some(20), Some(30)));
    assert_eq!(mapped(&runtime), vec![(0, 10), (1, 20), (2, 30)]);
    // Shrinking the whole value retracts the identities it no longer has.
    runtime
        .edit(|edit| edit.view::<WholeSlotMode>().set_mode(2))
        .expect("edit");
    assert_eq!(read(&runtime), (None, None), "the list is shorter now");
    assert_eq!(mapped(&runtime), vec![(0, 10)]);

    // Growing it back publishes new positional identities; the old payloads
    // must not resurface under them.
    runtime
        .edit(|edit| edit.view::<WholeSlotMode>().set_mode(3))
        .expect("edit");
    assert_eq!(read(&runtime), (Some(99), None));
    assert_eq!(mapped(&runtime), vec![(0, 10), (1, 99)]);

    // A whole-value definition owns no per-id cells: reading one by identity
    // must not materialise any. The field's protocol certificate is a
    // membership unit without an entity, so only entity cells count here.
    let slot = format!("{:?}", <WholeSlot as irrigation::ViewSchema>::DESCRIPTOR.id);
    let cells = runtime
        .dump()
        .into_iter()
        .filter(|node| {
            node.kind == "Unit"
                && node.unit.as_deref().is_some_and(|unit| {
                    unit.contains(&slot)
                        && unit.contains("Membership")
                        && unit.contains("entity: Some")
                })
        })
        .count();
    assert_eq!(
        cells, 0,
        "a whole-value get_key must not create per-id membership cells"
    );
}

// ---------------------------------------------------------------------------
// Membership defines are total (collections-seq-02)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ClearMode {
    mode: u64,
}

#[view(derived)]
pub struct ClearableSet(Set<u64>);

struct SetPublisher;

impl irrigation::GeneralComponent for SetPublisher {
    type State = u64;
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CC_0001);
    const NAME: &'static str = "gates::SetPublisher";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<ClearMode as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<ClearableSet as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let mode: u64 = cx.read_typed(irrigation::UnitKey::field(
            <ClearMode as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        let view = <ClearableSet as irrigation::ViewSchema>::view_id();
        let mut output = cx.output();
        output.membership(view, irrigation::FieldId(0), 1u64, true);
        output.membership(view, irrigation::FieldId(0), 2u64, mode == 0);
        output.membership(view, irrigation::FieldId(0), 3u64, mode == 0);
        Ok((mode, ()))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation]
mod clearable_reader {
    use super::ClearableSet;

    #[component]
    pub fn root() -> (bool, bool, Vec<u64>, usize) {
        let set = ClearableSet;
        (
            set.contains(&2u64),
            set.contains(&3u64),
            set.to_vec(),
            set.len(),
        )
    }
}

#[test]
fn a_false_membership_define_clears_the_member() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(SetPublisher)
        .expect("producer");
    runtime.install(clearable_reader::Root).expect("install");
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<clearable_reader::Root>()
            .expect("read")
    };
    runtime
        .edit(|edit| edit.view::<ClearMode>().set_mode(1))
        .expect("edit");
    assert_eq!(read(&runtime), (false, false, vec![1], 1));

    runtime
        .edit(|edit| edit.view::<ClearMode>().set_mode(0))
        .expect("edit");
    assert_eq!(read(&runtime), (true, true, vec![1, 2, 3], 3));

    // A member that was installed and is now defined false leaves the content,
    // not just its membership cell.
    runtime
        .edit(|edit| edit.view::<ClearMode>().set_mode(1))
        .expect("edit");
    let cleared = runtime.revision();
    assert_eq!(
        read(&runtime),
        (false, false, vec![1], 1),
        "a false membership define must take the member out of the content"
    );

    let set = format!(
        "{:?}",
        <ClearableSet as irrigation::ViewSchema>::DESCRIPTOR.id
    );
    let changed_at = |runtime: &Runtime, kind: &str, entity: &str| {
        runtime
            .dump()
            .into_iter()
            .find(|node| {
                node.kind == "Unit"
                    && node.unit.as_deref().is_some_and(|unit| {
                        unit.contains(&set) && unit.contains(kind) && unit.contains(entity)
                    })
            })
            .map(|node| node.changed_at)
            .expect("unit node")
    };
    assert_eq!(changed_at(&runtime, "Membership", "Some(\"2\")"), cleared);
    assert_eq!(changed_at(&runtime, "Membership", "Some(\"3\")"), cleared);
    assert_eq!(
        changed_at(&runtime, "Structure", "entity: None"),
        cleared,
        "the structure unit must be recomputed from the cleared content"
    );
}

// ---------------------------------------------------------------------------
// The keyed/whole protocol is an observation (collections-seq-12)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ProtocolMode {
    mode: u64,
}

#[view(derived)]
pub struct FlippingSet(Set<u64>);

#[irrigation]
mod protocol_flip {
    use super::{FlippingSet, ProtocolMode};

    /// Publishes one field in whichever protocol the mode names.
    ///
    /// A plain function on purpose: both publications stage on the same caller,
    /// so the gate watches one owner change the protocol rather than two branch
    /// nodes handing the field over.
    fn publish(mode: u64) {
        if mode == 1 {
            FlippingSet.define_keyed([1u64]);
        } else {
            FlippingSet.define_all([1u64, 2]);
        }
    }

    #[component]
    pub fn provider(mode: &ProtocolMode) -> u64 {
        publish(mode.mode());
        0
    }

    #[component]
    pub fn reader() -> bool {
        // Only the membership verb: `to_vec` or `members` would read the
        // structure unit and witness the flip through it, hiding the dispatch
        // this gate is about.
        FlippingSet.contains(&2u64)
    }

    #[component]
    pub fn root(mode: &ProtocolMode) -> (u64, bool) {
        (provider(mode), reader())
    }
}

#[test]
fn a_protocol_flip_reaches_readers_that_materialised_the_other_branch() {
    let runtime = Runtime::new();
    runtime.install(protocol_flip::Root).expect("install");

    // Keyed publication first: the reader materialises the absent member's cell
    // from the content instead of reading the whole set.
    runtime
        .edit(|edit| edit.view::<ProtocolMode>().set_mode(1))
        .expect("edit");
    assert!(
        !runtime
            .read_outputs::<protocol_flip::Root>()
            .expect("read")
            .1
    );

    // The flip installs a whole value. The cell the reader is holding still says
    // absent, and only the protocol certificate can wake it.
    runtime
        .edit(|edit| edit.view::<ProtocolMode>().set_mode(0))
        .expect("edit");
    let flipped = runtime
        .read_outputs::<protocol_flip::Root>()
        .expect("read")
        .1;
    assert!(
        flipped,
        "a reader that materialised the keyed branch must see the whole value"
    );

    // And back: the whole branch must give way to the keyed cells again.
    runtime
        .edit(|edit| edit.view::<ProtocolMode>().set_mode(1))
        .expect("edit");
    assert!(
        !runtime
            .read_outputs::<protocol_flip::Root>()
            .expect("read")
            .1
    );
}

#[view(source)]
pub struct CrossSiteMode {
    mode: u64,
}

#[view(derived)]
pub struct CrossSiteSet(Set<u64>);

#[irrigation]
mod cross_site_protocol_flip {
    use super::{CrossSiteMode, CrossSiteSet};

    /// One branch site publishes the keyed protocol.
    #[component]
    pub fn keyed() -> u64 {
        CrossSiteSet.define_keyed([1u64]);
        0
    }

    /// The other branch site publishes the whole one.
    #[component]
    pub fn whole() -> u64 {
        CrossSiteSet.define_all([1u64, 2]);
        0
    }

    #[component]
    pub fn provider(mode: &CrossSiteMode) -> u64 {
        if mode.mode() == 1 { keyed() } else { whole() }
    }

    #[component]
    pub fn members() -> Vec<u64> {
        CrossSiteSet.to_vec()
    }

    #[component]
    pub fn root(mode: &CrossSiteMode) -> (u64, Vec<u64>) {
        (provider(mode), members())
    }
}

#[test]
fn a_cross_site_protocol_flip_keeps_the_new_branch_value() {
    let runtime = Runtime::new();
    runtime
        .install(cross_site_protocol_flip::Root)
        .expect("install");
    runtime
        .edit(|edit| edit.view::<CrossSiteMode>().set_mode(1))
        .expect("edit");
    let keyed = runtime
        .read_outputs::<cross_site_protocol_flip::Root>()
        .expect("read")
        .1;
    assert_eq!(keyed, vec![1], "the keyed branch publishes one member");

    // The whole branch installs its value while the keyed branch node is still
    // reachable; that node retires afterwards, and its field-level retraction
    // must not clear the value the new site published.
    runtime
        .edit(|edit| edit.view::<CrossSiteMode>().set_mode(0))
        .expect("edit");
    let flipped = runtime
        .read_outputs::<cross_site_protocol_flip::Root>()
        .expect("read")
        .1;
    assert_eq!(
        flipped,
        vec![1, 2],
        "a cross-site flip must keep the whole value from-scratch produces"
    );

    // And back: the retired whole site must not empty the keyed cells either.
    runtime
        .edit(|edit| edit.view::<CrossSiteMode>().set_mode(1))
        .expect("edit");
    let back = runtime
        .read_outputs::<cross_site_protocol_flip::Root>()
        .expect("read")
        .1;
    assert_eq!(
        back,
        vec![1],
        "a cross-site flip back must keep the keyed members from-scratch produces"
    );
}

#[view(source)]
pub struct CrossSiteListMode {
    mode: u64,
}

#[view(derived)]
pub struct CrossSiteList(List<u64>);

#[irrigation]
mod cross_site_list_flip {
    use super::{CrossSiteList, CrossSiteListMode};

    #[component]
    pub fn keyed() -> u64 {
        CrossSiteList.define_keyed([(1u64, 10u64), (2, 20)]);
        0
    }

    #[component]
    pub fn whole() -> u64 {
        CrossSiteList.define_all([10u64, 20, 30]);
        0
    }

    #[component]
    pub fn provider(mode: &CrossSiteListMode) -> u64 {
        if mode.mode() == 1 { keyed() } else { whole() }
    }

    #[component]
    pub fn members() -> Vec<u64> {
        CrossSiteList.to_vec()
    }

    #[component]
    pub fn root(mode: &CrossSiteListMode) -> (u64, Vec<u64>) {
        (provider(mode), members())
    }
}

#[test]
fn a_cross_site_list_flip_keeps_the_new_branch_value() {
    let runtime = Runtime::new();
    runtime
        .install(cross_site_list_flip::Root)
        .expect("install");
    runtime
        .edit(|edit| edit.view::<CrossSiteListMode>().set_mode(1))
        .expect("edit");
    let keyed = runtime
        .read_outputs::<cross_site_list_flip::Root>()
        .expect("read")
        .1;
    assert_eq!(
        keyed,
        vec![10, 20],
        "the keyed branch publishes two members"
    );

    runtime
        .edit(|edit| edit.view::<CrossSiteListMode>().set_mode(0))
        .expect("edit");
    let flipped = runtime
        .read_outputs::<cross_site_list_flip::Root>()
        .expect("read")
        .1;
    assert_eq!(
        flipped,
        vec![10, 20, 30],
        "a cross-site list flip must keep the whole value from-scratch produces"
    );

    runtime
        .edit(|edit| edit.view::<CrossSiteListMode>().set_mode(1))
        .expect("edit");
    let back = runtime
        .read_outputs::<cross_site_list_flip::Root>()
        .expect("read")
        .1;
    assert_eq!(
        back,
        vec![10, 20],
        "a cross-site list flip back must keep the keyed members from-scratch produces"
    );
}

// ---------------------------------------------------------------------------
// Keyed list publication retracts dropped ids (collections-seq-03)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct PublishMode {
    mode: u64,
}

#[view(derived)]
pub struct PublishedList(List<u64>);

struct ListPublisher;

impl irrigation::GeneralComponent for ListPublisher {
    type State = u64;
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CC_0002);
    const NAME: &'static str = "gates::ListPublisher";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<PublishMode as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<PublishedList as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        use irrigation::ListOutputExt;
        let mode: u64 = cx.read_typed(irrigation::UnitKey::field(
            <PublishMode as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        let view = <PublishedList as irrigation::ViewSchema>::view_id();
        if mode == 0 {
            cx.list_define_keyed(view, irrigation::FieldId(0), [(1u64, 10u64), (2, 20)]);
        } else {
            cx.list_define_keyed(view, irrigation::FieldId(0), [(1u64, 10u64)]);
        }
        Ok((mode, ()))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation]
mod published_reader {
    use super::PublishedList;

    #[component]
    pub fn root() -> (Option<u64>, Option<u64>, Vec<u64>, Vec<u64>) {
        let list = PublishedList;
        (
            list.get_key(1),
            list.get_key(2),
            list.to_vec(),
            list.ids().to_vec(),
        )
    }
}

#[test]
fn keyed_list_publication_retracts_dropped_ids() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(ListPublisher)
        .expect("producer");
    runtime.install(published_reader::Root).expect("install");
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<published_reader::Root>()
            .expect("read")
    };

    // The default mode publishes one entry; publish both, then drop one.
    runtime
        .edit(|edit| edit.view::<PublishMode>().set_mode(1))
        .expect("edit");
    assert_eq!(read(&runtime), (Some(10), None, vec![10], vec![1]));

    runtime
        .edit(|edit| edit.view::<PublishMode>().set_mode(0))
        .expect("edit");
    assert_eq!(
        read(&runtime),
        (Some(10), Some(20), vec![10, 20], vec![1, 2])
    );

    runtime
        .edit(|edit| edit.view::<PublishMode>().set_mode(1))
        .expect("edit");
    let dropped = runtime.revision();
    assert_eq!(
        read(&runtime),
        (Some(10), None, vec![10], vec![1]),
        "a dropped identity must stop answering by key"
    );

    let list = format!(
        "{:?}",
        <PublishedList as irrigation::ViewSchema>::DESCRIPTOR.id
    );
    let membership = runtime
        .dump()
        .into_iter()
        .find(|node| {
            node.kind == "Unit"
                && node.unit.as_deref().is_some_and(|unit| {
                    unit.contains(&list)
                        && unit.contains("Membership")
                        && unit.contains("Some(\"2\")")
                })
        })
        .expect("membership unit");
    assert_eq!(
        membership.changed_at, dropped,
        "retraction must mirror the installation of the dropped id"
    );
}

// ---------------------------------------------------------------------------
// Tree moves validate before mutating (collections-structured-01)
// ---------------------------------------------------------------------------

#[tree_view(root = MoveDoc, view = MoveTree)]
pub mod move_tree {
    /// Root of the movement test tree.
    #[derive(Clone, Debug, PartialEq)]
    pub enum MoveDoc {
        /// A forest of items.
        Forest { items: Vec<MoveItem> },
    }

    /// One tree item.
    #[derive(Clone, Debug, PartialEq)]
    pub enum MoveItem {
        /// A leaf with a value.
        Leaf { value: u64 },
        /// A group of items.
        Group { items: Vec<MoveItem> },
        /// A single wrapped child.
        Wrapper { inner: Box<MoveItem> },
    }
}

#[irrigation]
mod move_tree_stats {
    use super::move_tree;

    #[component]
    pub fn root(tree: &move_tree::MoveTree) -> (usize, Vec<u64>) {
        match tree.root() {
            Some(document) => {
                let items = document.items();
                (items.len(), items.iter().map(|item| item.raw()).collect())
            }
            None => (0, Vec::new()),
        }
    }
}

#[test]
fn a_same_parent_tree_reorder_does_not_duplicate_the_child() {
    let runtime = Runtime::new();
    runtime.install(move_tree_stats::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<move_tree::MoveTree>();
            tree.set_root(&move_tree::MoveDoc::Forest {
                items: vec![
                    move_tree::MoveItem::Leaf { value: 1 },
                    move_tree::MoveItem::Leaf { value: 2 },
                    move_tree::MoveItem::Group {
                        items: vec![move_tree::MoveItem::Leaf { value: 3 }],
                    },
                ],
            });
        })
        .expect("seed");
    let before = runtime
        .read_outputs::<move_tree_stats::Root>()
        .expect("read");
    assert_eq!(before.0, 3);
    let nodes_before = runtime
        .snapshot()
        .view::<move_tree::MoveTree>()
        .node_count();
    let (root_id, first_id) = {
        let snapshot = runtime.read().expect("read");
        let root = snapshot.view::<move_tree::MoveTree>().root().expect("root");
        let items = root.items();
        (root.raw(), items[0].raw())
    };

    runtime
        .edit(|edit| {
            let mut tree = edit.view::<move_tree::MoveTree>();
            assert!(tree.move_subtree(first_id, root_id, 0, 2));
        })
        .expect("edit");

    let after = runtime
        .read_outputs::<move_tree_stats::Root>()
        .expect("read");
    assert_eq!(
        after.0, 3,
        "a same-parent reorder must not change the child count"
    );
    let expected = vec![before.1[1], before.1[2], before.1[0]];
    assert_eq!(after.1, expected, "the reorder must move the child once");
    assert_eq!(
        runtime
            .snapshot()
            .view::<move_tree::MoveTree>()
            .node_count(),
        nodes_before,
        "a reorder must not create or strand nodes"
    );
}

#[test]
fn a_rejected_tree_move_publishes_nothing() {
    let runtime = Runtime::new();
    runtime.install(move_tree_stats::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<move_tree::MoveTree>();
            tree.set_root(&move_tree::MoveDoc::Forest {
                items: vec![
                    move_tree::MoveItem::Leaf { value: 1 },
                    move_tree::MoveItem::Wrapper {
                        inner: Box::new(move_tree::MoveItem::Leaf { value: 2 }),
                    },
                    move_tree::MoveItem::Leaf { value: 3 },
                ],
            });
        })
        .expect("seed");
    let layout = |runtime: &Runtime| {
        let snapshot = runtime.read().expect("read");
        let root = snapshot.view::<move_tree::MoveTree>().root().expect("root");
        let items = root.items();
        let inner = items[1].inner().expect("wrapped child");
        (
            root.raw(),
            items
                .iter()
                .map(move_tree::MoveItemRef::raw)
                .collect::<Vec<_>>(),
            inner.raw(),
            inner.value(),
        )
    };
    let before = layout(&runtime);
    let revision = runtime.revision();

    let accepted = runtime
        .edit(|edit| {
            let mut tree = edit.view::<move_tree::MoveTree>();
            tree.move_subtree(before.1[0], before.1[1], 0, 0)
        })
        .expect("edit");
    assert!(
        !accepted,
        "a move into an occupied single-child slot must be rejected"
    );
    assert_eq!(
        runtime.revision(),
        revision,
        "a rejected move must publish nothing"
    );
    assert_eq!(layout(&runtime), before, "a rejected move must not move");

    let accepted = runtime
        .edit(|edit| {
            let mut tree = edit.view::<move_tree::MoveTree>();
            tree.move_subtree(before.1[0], before.0, 9, 0)
        })
        .expect("edit");
    assert!(!accepted, "a move into a missing slot must be rejected");
    assert_eq!(runtime.revision(), revision);
    assert_eq!(layout(&runtime), before);
}

// ---------------------------------------------------------------------------
// Source graph edge membership (collections-structured-02, plan F1)
// ---------------------------------------------------------------------------

#[irrigation]
mod graph_edge_probe {
    use super::Network;

    #[component]
    pub fn probe(network: &Network) -> (Option<u32>, usize) {
        let graph = network.links();
        (graph.edge(0).map(|edge| edge.value), graph.edge_count())
    }
}

#[test]
fn a_negative_edge_read_wakes_when_the_edge_arrives() {
    let runtime = Runtime::new();
    runtime.install(graph_edge_probe::Probe).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            let mut links = view.links();
            links.add_node(1, 0u32);
            links.add_node(2, 0u32);
        })
        .expect("seed");
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<graph_edge_probe::Probe>()
            .expect("read")
    };
    assert_eq!(read(&runtime), (None, 0));

    runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            let mut links = view.links();
            links.add_edge(1, 2, 5u32);
        })
        .expect("edit");
    let added = runtime.revision();
    assert_eq!(
        read(&runtime),
        (Some(5), 1),
        "an edge read that observed absence must see the edge"
    );

    let view = format!("{:?}", <Network as irrigation::ViewSchema>::DESCRIPTOR.id);
    let membership_changed_at = |runtime: &Runtime| {
        runtime
            .dump()
            .into_iter()
            .find(|node| {
                node.kind == "Unit"
                    && node.unit.as_deref().is_some_and(|unit| {
                        unit.contains(&view)
                            && unit.contains("Membership")
                            && unit.contains("EdgeKey(0)")
                    })
            })
            .map(|node| node.changed_at)
            .expect("edge membership unit")
    };
    assert_eq!(
        membership_changed_at(&runtime),
        added,
        "edge membership must be installed with the edge"
    );

    runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            view.links().remove_edge(0);
        })
        .expect("edit");
    let removed = runtime.revision();
    assert_eq!(read(&runtime), (None, 0));
    assert_eq!(
        membership_changed_at(&runtime),
        removed,
        "edge membership must be retracted with the edge"
    );
}

// ---------------------------------------------------------------------------
// Checked writes require absence at commit instead of reading it (S09)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct InsertTable(Map<u32, u64>);

#[irrigation]
mod checked_insert {
    use super::InsertTable;

    #[component]
    pub fn fresh() -> u64 {
        InsertTable.field0().insert(1, 10, "map_insert");
        1
    }
}

#[test]
fn a_checked_map_insert_publishes_a_fresh_key_in_one_pass() {
    let runtime = Runtime::new();
    runtime.register_view(<InsertTable as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(checked_insert::Fresh).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<checked_insert::Fresh>()
            .expect("a fresh insert must not fail at its own precondition"),
        1
    );
    assert_eq!(
        runtime
            .read()
            .expect("snapshot")
            .view::<InsertTable>()
            .field0(),
        vec![(1u32, 10u64)]
    );
}

#[test]
fn a_checked_map_insert_does_not_observe_the_membership_it_installs() {
    let runtime = Runtime::new();
    runtime.register_view(<InsertTable as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(checked_insert::Fresh).expect("install");
    // The demand may fail at the insert's own precondition; the dependency
    // structure it leaves behind is what this gate reads.
    let _ = runtime.read_outputs::<checked_insert::Fresh>();
    let dump = runtime.dump();
    let view = format!("{:?}", <InsertTable as irrigation::ViewSchema>::view_id());
    let membership = dump
        .iter()
        .find(|node| {
            node.unit
                .as_deref()
                .is_some_and(|unit| unit.contains(&view) && unit.contains("Membership"))
        })
        .expect("the insert installed the key's membership")
        .id;
    let observers: Vec<_> = dump
        .iter()
        .filter(|node| node.deps.iter().any(|dep| dep.node == membership))
        .map(|node| node.id)
        .collect();
    assert!(
        observers.is_empty(),
        "the write group observed the membership unit it installs: {observers:?}"
    );
}

// ---------------------------------------------------------------------------
// Checked edits project the committed content, never the unit they replace
// ---------------------------------------------------------------------------

#[view(source)]
pub struct EditMode {
    mode: u32,
}

#[view(derived)]
pub struct EditedMap(Map<u32, u64>);

#[view(derived)]
pub struct EditedList(List<u32>);

#[irrigation]
mod checked_edits {
    use super::{EditMode, EditedList, EditedMap};

    #[component]
    pub fn root(mode: &EditMode) -> u32 {
        match mode.mode() {
            0 => {
                EditedMap.field0().insert(1, 10, "map_insert");
                EditedList.field0().define_all([1u32, 2], "list_build");
            }
            1 => {
                // The key is read off the map this body then writes, so the
                // attempt observes the unit its own commit replaces.
                let base = EditedMap.field0().to_vec();
                let key = base.last().map_or(1, |(key, _)| *key);
                EditedMap
                    .field0()
                    .update_if_present(key, |value| value + 1, "map_update");
                let _ = EditedMap.field0().to_vec();
            }
            2 => {
                // The index is the length of the list this body then writes.
                let base = EditedList.field0().to_vec();
                EditedList.field0().insert_at(base.len(), 3, "list_insert");
                let _ = EditedList.field0().to_vec();
            }
            3 => {
                let base = EditedList.field0().to_vec();
                EditedList.field0().remove_at(base.len() - 1, "list_remove");
                let _ = EditedList.field0().to_vec();
            }
            4 => {
                // The key is read off the map this body then inserts into.
                let base = EditedMap.field0().to_vec();
                let key = base.last().map_or(0, |(key, _)| *key + 1);
                EditedMap.field0().insert(key, 20, "map_insert");
                let _ = EditedMap.field0().to_vec();
            }
            5 => {
                EditedMap
                    .field0()
                    .update_if_present(1, |value| value + 1, "map_update");
            }
            6 => {
                let _ = EditedMap.field0().to_vec();
                EditedMap
                    .field0()
                    .update_if_present(1, |value| value + 1, "map_update");
            }
            7 => {
                EditedMap
                    .field0()
                    .update_if_present(1, |value| value + 1, "map_update");
                let _ = EditedMap.field0().to_vec();
            }
            8 => {
                let base = EditedMap.field0().to_vec();
                let key = base.last().map_or(1, |(key, _)| *key);
                EditedMap
                    .field0()
                    .update_if_present(key, |value| value + 1, "map_update");
            }
            9 => {
                // The supported shape: the index is a local, and the verb
                // projects the committed sequence itself.
                EditedList.field0().insert_at(1, 3, "list_insert");
            }
            10 => {
                EditedList.field0().remove_at(1, "list_remove");
            }
            _ => {}
        }
        0
    }
}

fn checked_edits_runtime() -> Runtime {
    let runtime = Runtime::new();
    runtime.register_view(<EditedMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<EditedList as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(checked_edits::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<checked_edits::Root>()
            .expect("build"),
        0
    );
    runtime
}

/// Asserts that a demand failed with the plan's self-dependent precondition.
fn assert_self_dependent(error: &irrigation::ComputeError) {
    assert!(
        matches!(
            error,
            irrigation::ComputeError::PreconditionUnmet {
                reason: irrigation_core::error::PreconditionReason::SelfDependent,
                ..
            }
        ),
        "a read-your-own-write is a precondition failure, got {error:?}"
    );
    assert!(
        error.is_permanent(),
        "the failure must be permanent, not a retryable attempt, got {error:?}"
    );
}

/// Asserts that deciding a failure spent no retry.
fn assert_no_retry(runtime: &Runtime, retries: u64) {
    assert_eq!(
        runtime.stats().demand_retries,
        retries,
        "the failure must be decided at commit instead of consuming the retry budget"
    );
}

#[test]
fn a_checked_map_update_that_reads_its_port_is_self_dependent() {
    let runtime = checked_edits_runtime();
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<EditMode>().set_mode(1))
        .expect("edit");
    let error = runtime
        .read_outputs::<checked_edits::Root>()
        .expect_err("a body that reads the port its own write installs must fail");
    assert_self_dependent(&error);
    assert_no_retry(&runtime, retries);
    let repeated = runtime
        .read_outputs::<checked_edits::Root>()
        .expect_err("a repeated demand must replay the failure, not retry it");
    assert_self_dependent(&repeated);
    assert_no_retry(&runtime, retries);
    assert_eq!(
        runtime.inspect_snapshot().view::<EditedMap>().field0(),
        vec![(1u32, 10u64)],
        "the rejected group must not apply the update closure"
    );
}

#[test]
fn a_checked_map_insert_that_reads_its_port_is_self_dependent() {
    let runtime = checked_edits_runtime();
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<EditMode>().set_mode(4))
        .expect("edit");
    let error = runtime
        .read_outputs::<checked_edits::Root>()
        .expect_err("a body that reads the port its own write installs must fail");
    assert_self_dependent(&error);
    assert_no_retry(&runtime, retries);
    assert_eq!(
        runtime.inspect_snapshot().view::<EditedMap>().field0(),
        vec![(1u32, 10u64)],
        "the rejected group must not add a key"
    );
}

#[test]
fn a_checked_list_insert_that_reads_its_port_is_self_dependent() {
    let runtime = checked_edits_runtime();
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<EditMode>().set_mode(2))
        .expect("edit");
    let error = runtime
        .read_outputs::<checked_edits::Root>()
        .expect_err("a body that reads the port its own write installs must fail");
    assert_self_dependent(&error);
    assert_no_retry(&runtime, retries);
    assert_eq!(
        runtime.inspect_snapshot().view::<EditedList>().field0(),
        vec![1u32, 2],
        "the rejected group must not insert an element"
    );
}

#[test]
fn a_checked_list_remove_that_reads_its_port_is_self_dependent() {
    let runtime = checked_edits_runtime();
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<EditMode>().set_mode(3))
        .expect("edit");
    let error = runtime
        .read_outputs::<checked_edits::Root>()
        .expect_err("a body that reads the port its own write installs must fail");
    assert_self_dependent(&error);
    assert_no_retry(&runtime, retries);
    assert_eq!(
        runtime.inspect_snapshot().view::<EditedList>().field0(),
        vec![1u32, 2],
        "the rejected group must not remove an element"
    );
}

#[test]
fn a_checked_update_that_projects_the_committed_value_publishes_in_one_pass() {
    let runtime = checked_edits_runtime();
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<EditMode>().set_mode(5))
        .expect("edit");
    runtime
        .read_outputs::<checked_edits::Root>()
        .expect("an update that never reads the port it installs must publish");
    assert_eq!(
        runtime.stats().demand_retries,
        retries,
        "the verb projects the committed value instead of tracing the port it installs"
    );
    assert_eq!(
        runtime
            .read()
            .expect("snapshot")
            .view::<EditedMap>()
            .field0(),
        vec![(1u32, 11u64)],
        "the closure applies to the committed value exactly once"
    );
}

#[test]
fn a_checked_list_edit_that_projects_the_committed_sequence_publishes_in_one_pass() {
    let runtime = checked_edits_runtime();
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<EditMode>().set_mode(9))
        .expect("edit");
    runtime
        .read_outputs::<checked_edits::Root>()
        .expect("an insert of a local index must publish");
    assert_eq!(
        runtime.stats().demand_retries,
        retries,
        "the insert must not trace the sequence it replaces"
    );
    assert_eq!(
        runtime
            .read()
            .expect("snapshot")
            .view::<EditedList>()
            .field0(),
        vec![1u32, 3, 2]
    );
    runtime
        .edit(|edit| edit.view::<EditMode>().set_mode(10))
        .expect("edit");
    runtime
        .read_outputs::<checked_edits::Root>()
        .expect("a removal of a local index must publish");
    assert_eq!(
        runtime.stats().demand_retries,
        retries,
        "the removal must not trace the sequence it replaces"
    );
    assert_eq!(
        runtime
            .read()
            .expect("snapshot")
            .view::<EditedList>()
            .field0(),
        vec![1u32, 2]
    );
}

#[test]
fn a_detached_read_of_the_written_field_does_not_fail_the_update() {
    let runtime = checked_edits_runtime();
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<EditMode>().set_mode(6))
        .expect("edit");
    runtime
        .read_outputs::<checked_edits::Root>()
        .expect("a read the update does not consume must not make the update self-dependent");
    assert!(
        runtime.stats().demand_retries <= retries + 1,
        "the demand may revalidate its root once, but a self-dependent loop would not terminate"
    );
    assert_eq!(
        runtime
            .read()
            .expect("snapshot")
            .view::<EditedMap>()
            .field0(),
        vec![(1u32, 11u64)],
        "the update applies its closure exactly once"
    );
}

#[view(derived)]
pub struct NeighbourMap(Map<u32, u64>);

#[irrigation]
mod reads_one_entity_writes_another {
    use super::NeighbourMap;

    #[component]
    pub fn root() -> u64 {
        NeighbourMap.field0().define(1, 10, "map_seed");
        let known = NeighbourMap.field0().get(&1).unwrap_or(0);
        NeighbourMap.field0().define(2, known + 1, "map_neighbour");
        known
    }
}

#[test]
fn a_write_of_another_entity_is_not_self_dependent() {
    let runtime = Runtime::new();
    runtime.register_view(<NeighbourMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(reads_one_entity_writes_another::Root)
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs::<reads_one_entity_writes_another::Root>()
            .expect("reading one entry must not fail a write of another"),
        10
    );
    assert_eq!(
        runtime.stats().demand_retries,
        0,
        "one entity's port is not the port the group installs"
    );
    assert_eq!(
        runtime
            .read()
            .expect("snapshot")
            .view::<NeighbourMap>()
            .field0(),
        vec![(1u32, 10u64), (2u32, 11u64)]
    );
    assert_eq!(
        runtime
            .read_outputs::<reads_one_entity_writes_another::Root>()
            .expect("reuse"),
        10
    );
    assert_eq!(
        runtime.stats().demand_retries,
        0,
        "a second demand must not retry either"
    );
}

// ---------------------------------------------------------------------------
// A retried attempt must not re-apply the edit it staged (S10)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct RetryTick(u32);

#[view(derived)]
pub struct RetryList(List<u32>);

#[irrigation]
mod retried_edit {
    use super::{EditMode, RetryList, RetryTick};

    #[component]
    pub fn bump(tag: u32) -> u32 {
        RetryTick.define_field0(tag);
        tag
    }

    #[component]
    pub fn root(mode: &EditMode) -> u32 {
        let tick = RetryTick.field0();
        match mode.mode() {
            0 => RetryList.field0().define_all([1u32, 2, 3], "list_build"),
            _ => {
                RetryList.field0().remove_at(1, "list_remove");
                // Moving a unit this body already observed before the commit
                // installs anything fails the commit's trace check, so the
                // attempt is rejected with its staged edit discarded; the retry
                // re-applies the removal from the same base.
                let _ = bump(1);
            }
        }
        tick
    }
}

#[test]
fn a_retried_list_edit_is_applied_once() {
    let runtime = Runtime::new();
    runtime.register_view(<RetryList as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<RetryTick as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(retried_edit::Root).expect("install");
    assert_eq!(
        runtime.read_outputs::<retried_edit::Root>().expect("build"),
        0
    );
    runtime
        .edit(|edit| edit.view::<EditMode>().set_mode(1))
        .expect("edit");
    let before = runtime.stats().validation_failures;
    let retries = runtime.stats().demand_retries;
    assert_eq!(
        runtime
            .read_outputs::<retried_edit::Root>()
            .expect("a retried edit must publish"),
        1,
        "the attempt must have retried after observing the moved tick"
    );
    assert!(
        runtime.stats().validation_failures > before,
        "the gate did not force a retry"
    );
    assert!(
        runtime.stats().demand_retries > retries,
        "the gate did not force a demand retry"
    );
    assert_eq!(
        runtime
            .read()
            .expect("snapshot")
            .view::<RetryList>()
            .field0(),
        vec![1u32, 3],
        "the retried attempt must apply the removal exactly once"
    );
}

// ---------------------------------------------------------------------------
// A membership define is total in every family (collections-seq-11)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct TotalMode {
    mode: u64,
}

#[view(derived)]
pub struct TotalMap(Map<u64, u64>);

#[view(derived)]
pub struct TotalList(List<u64>);

#[view(derived)]
pub struct TotalSet(Set<u64>);

#[view(derived)]
pub struct TotalGraph(Graph<u64, u64>);

struct MembershipPublisher;

impl irrigation::GeneralComponent for MembershipPublisher {
    type State = u64;
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CC_0003);
    const NAME: &'static str = "gates::MembershipPublisher";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<TotalMode as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[
        <TotalMap as irrigation::ViewSchema>::DESCRIPTOR,
        <TotalList as irrigation::ViewSchema>::DESCRIPTOR,
        <TotalSet as irrigation::ViewSchema>::DESCRIPTOR,
        <TotalGraph as irrigation::ViewSchema>::DESCRIPTOR,
    ];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let mode: u64 = cx.read_typed(irrigation::UnitKey::field(
            <TotalMode as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        let present = mode == 0;
        let mut output = cx.output();
        let map = <TotalMap as irrigation::ViewSchema>::view_id();
        output.entry(map, irrigation::FieldId(0), 1u64, 10u64);
        output.entry(map, irrigation::FieldId(0), 2u64, 20u64);
        output.membership(map, irrigation::FieldId(0), 2u64, present);
        let list = <TotalList as irrigation::ViewSchema>::view_id();
        output.entry(list, irrigation::FieldId(0), 7u64, 70u64);
        output.entry(list, irrigation::FieldId(0), 8u64, 80u64);
        // The structure unit is what declares the keyed protocol, so `get_key`
        // takes the per-identity branch instead of materialising a whole-value
        // read for a field the publisher never defined as one.
        output.define(
            irrigation::UnitKey::field(
                list,
                irrigation::FieldId(0),
                irrigation::UnitKind::Structure,
            ),
            [7u64, 8u64].into_iter().collect::<irrigation::PSeq<u64>>(),
        );
        output.membership(list, irrigation::FieldId(0), 8u64, present);
        let set = <TotalSet as irrigation::ViewSchema>::view_id();
        output.membership(set, irrigation::FieldId(0), 1u64, true);
        output.membership(set, irrigation::FieldId(0), 2u64, present);
        let graph = <TotalGraph as irrigation::ViewSchema>::view_id();
        output.entry(graph, irrigation::FieldId(0), irrigation::NodeKey(1), 10u64);
        output.entry(graph, irrigation::FieldId(0), irrigation::NodeKey(2), 20u64);
        output.membership(
            graph,
            irrigation::FieldId(0),
            irrigation::NodeKey(2),
            present,
        );
        Ok((mode, ()))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation]
mod total_map_reader {
    use super::TotalMap;

    #[component]
    pub fn root() -> (Vec<u64>, bool, Option<u64>) {
        let map = TotalMap;
        (map.keys(), map.contains_key(&2u64), map.get(&2u64))
    }
}

#[test]
fn a_false_membership_define_clears_a_map_entry() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(MembershipPublisher)
        .expect("producer");
    runtime.install(total_map_reader::Root).expect("install");
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<total_map_reader::Root>()
            .expect("read")
    };

    // A revision must separate the install from the first demand: at revision
    // zero a node is certified by construction, so a read would serve the empty
    // content without asking the producer.
    runtime
        .edit(|edit| edit.view::<TotalMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        read(&runtime),
        (vec![1], false, None),
        "a false membership define must take the key out of the content, so \
         keys() and contains_key() agree"
    );
    assert_eq!(
        runtime.snapshot().view::<TotalMap>().field0(),
        vec![(1u64, 10u64)]
    );

    runtime
        .edit(|edit| edit.view::<TotalMode>().set_mode(0))
        .expect("edit");
    assert_eq!(
        read(&runtime),
        (vec![1, 2], true, Some(20)),
        "a true membership define must restore what false removed"
    );
}

#[irrigation]
mod total_list_reader {
    use super::TotalList;

    #[component]
    pub fn root() -> (Vec<u64>, Option<u64>) {
        let list = TotalList;
        (list.ids().to_vec(), list.get_key(8u64))
    }
}

#[test]
fn a_false_membership_define_clears_a_list_member() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(MembershipPublisher)
        .expect("producer");
    runtime.install(total_list_reader::Root).expect("install");
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<total_list_reader::Root>()
            .expect("read")
    };

    runtime
        .edit(|edit| edit.view::<TotalMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        read(&runtime),
        (vec![7], None),
        "a false membership define must take the identity out of the list's \
         structure, not just out of its cell"
    );
    assert_eq!(runtime.snapshot().view::<TotalList>().field0(), vec![70u64]);

    runtime
        .edit(|edit| edit.view::<TotalMode>().set_mode(0))
        .expect("edit");
    assert_eq!(
        read(&runtime),
        (vec![7, 8], Some(80)),
        "a true membership define must restore what false removed"
    );
}

#[irrigation]
mod total_set_reader {
    use super::TotalSet;

    #[component]
    pub fn root() -> (bool, Vec<u64>) {
        let set = TotalSet;
        (set.contains(&2u64), set.to_vec())
    }
}

#[test]
fn a_false_membership_define_clears_a_set_member() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(MembershipPublisher)
        .expect("producer");
    runtime.install(total_set_reader::Root).expect("install");
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<total_set_reader::Root>()
            .expect("read")
    };

    runtime
        .edit(|edit| edit.view::<TotalMode>().set_mode(1))
        .expect("edit");
    assert_eq!(read(&runtime), (false, vec![1]));
    assert_eq!(runtime.snapshot().view::<TotalSet>().field0(), vec![1u64]);

    runtime
        .edit(|edit| edit.view::<TotalMode>().set_mode(0))
        .expect("edit");
    assert_eq!(read(&runtime), (true, vec![1, 2]));
}

#[irrigation]
mod total_graph_reader {
    use super::TotalGraph;

    #[component]
    pub fn root() -> (Vec<u64>, bool, Option<u64>) {
        let graph = TotalGraph;
        (graph.nodes(), graph.contains_node(2), graph.node(2))
    }
}

#[test]
fn a_false_membership_define_clears_a_graph_node() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(MembershipPublisher)
        .expect("producer");
    runtime.install(total_graph_reader::Root).expect("install");
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<total_graph_reader::Root>()
            .expect("read")
    };

    runtime
        .edit(|edit| edit.view::<TotalMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        read(&runtime),
        (vec![1], false, None),
        "a false membership define must take the node out of the topology"
    );
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.view::<TotalGraph>().field0_nodes(), vec![1u64]);
    assert_eq!(snapshot.view::<TotalGraph>().field0_node(2), None);

    runtime
        .edit(|edit| edit.view::<TotalMode>().set_mode(0))
        .expect("edit");
    assert_eq!(
        read(&runtime),
        (vec![1, 2], true, Some(20)),
        "a true membership define must restore what false removed"
    );
}

// ---------------------------------------------------------------------------
// A collection write verb projects its base untracked and stages its
// precondition instead of reading the unit the commit installs
// ---------------------------------------------------------------------------

#[view(source)]
pub struct WriteMode {
    mode: u64,
}

#[view(derived)]
pub struct WriteGraph(Graph<u64, u64>);

#[view(derived)]
pub struct WriteSet(Set<u64>);

#[view(derived)]
pub struct WriteMap(Map<u64, u64>);

#[irrigation]
mod graph_inserts {
    use super::WriteGraph;

    #[component]
    pub fn nodes() -> u64 {
        WriteGraph.field0().define_node(10, 1);
        WriteGraph.field0().define_node(20, 2);
        0
    }

    #[component]
    pub fn checked_inserts() -> u64 {
        WriteGraph.field0().insert_node(30, 3, "checked_node");
        WriteGraph
            .field0()
            .insert_edge(7, 10, 20, 5, "checked_edge");
        0
    }

    #[component]
    pub fn root() -> u64 {
        let _ = nodes();
        let _ = checked_inserts();
        0
    }
}

#[test]
fn a_checked_graph_insert_publishes_in_one_pass() {
    let runtime = Runtime::new();
    runtime.register_view(<WriteGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(graph_inserts::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<graph_inserts::Root>()
            .expect("a fresh checked insert must publish"),
        0
    );
    assert_eq!(
        runtime.stats().demand_retries,
        0,
        "a checked insert must not observe the units its own commit installs"
    );
    let snapshot = runtime.read().expect("snapshot");
    assert_eq!(
        snapshot.view::<WriteGraph>().field0_nodes(),
        vec![10, 20, 30]
    );
    assert_eq!(snapshot.view::<WriteGraph>().field0_edges(), vec![7]);
}

#[irrigation]
mod graph_duplicate {
    use super::{WriteGraph, WriteMode};

    #[component]
    pub fn root(mode: &WriteMode) -> u64 {
        if mode.mode() == 0 {
            WriteGraph.field0().define_node(10, 1);
        } else {
            WriteGraph.field0().insert_node(10, 2, "duplicate_node");
        }
        0
    }
}

#[test]
fn a_checked_graph_insert_replaces_the_groups_own_previous_publication() {
    let runtime = Runtime::new();
    runtime.register_view(<WriteGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(graph_duplicate::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<graph_duplicate::Root>()
            .expect("build"),
        0
    );
    runtime
        .edit(|edit| edit.view::<WriteMode>().set_mode(1))
        .expect("edit");
    // The from-scratch answer for this revision is the mode-1 body alone: it
    // never defines node 10, so the key is absent and the checked insert
    // publishes. The previous payload belongs to this same group, which the
    // install replaces; treating it as another writer's node is what the old
    // expectation got wrong.
    assert_eq!(
        runtime
            .read_outputs::<graph_duplicate::Root>()
            .expect("the mode-1 body inserts node 10 into an otherwise empty graph"),
        0
    );
    let snapshot = runtime.inspect_snapshot();
    assert_eq!(
        snapshot.view::<WriteGraph>().field0_nodes(),
        vec![10],
        "the checked insert publishes the node the mode-1 body defines"
    );
    assert_eq!(
        snapshot.view::<WriteGraph>().field0_node(10),
        Some(2),
        "the insert's payload replaces the group's own previous publication"
    );
}

#[irrigation]
mod graph_removal {
    use super::{WriteGraph, WriteMode};

    #[component]
    pub fn root(mode: &WriteMode) -> u64 {
        if mode.mode() == 0 {
            WriteGraph.field0().define_graph(
                &[(1u64, 10u64), (2, 20)],
                &[(7u64, 1u64, 2u64, 5u64)],
                "graph_build",
            );
        } else {
            WriteGraph.field0().remove_node(1, "graph_remove");
        }
        0
    }
}

#[test]
fn removing_a_graph_node_publishes_in_one_pass() {
    let runtime = Runtime::new();
    runtime.register_view(<WriteGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(graph_removal::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<graph_removal::Root>()
            .expect("build"),
        0
    );
    let before = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<WriteMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<graph_removal::Root>()
            .expect("a removal must publish"),
        0
    );
    assert_eq!(
        runtime.stats().demand_retries,
        before,
        "a removal must not read the graph units its own commit replaces"
    );
    let snapshot = runtime.read().expect("snapshot");
    assert_eq!(snapshot.view::<WriteGraph>().field0_nodes(), vec![2]);
    assert_eq!(
        snapshot.view::<WriteGraph>().field0_edges(),
        Vec::<u64>::new()
    );
}

#[irrigation]
mod graph_redefinition {
    use super::{WriteGraph, WriteMode};

    #[component]
    pub fn root(mode: &WriteMode) -> u64 {
        if mode.mode() == 0 {
            WriteGraph.field0().define_graph(
                &[(1u64, 10u64), (2, 20)],
                &[(7u64, 1u64, 2u64, 5u64)],
                "graph_build",
            );
        } else {
            WriteGraph.field0().define_graph(
                &[(1u64, 11u64), (2, 20), (3, 30)],
                &[(7u64, 1u64, 2u64, 5u64), (8u64, 2u64, 3u64, 6u64)],
                "graph_rebuild",
            );
        }
        0
    }
}

#[test]
fn redefining_a_derived_graph_publishes_in_one_pass() {
    let runtime = Runtime::new();
    runtime.register_view(<WriteGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(graph_redefinition::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<graph_redefinition::Root>()
            .expect("build"),
        0
    );
    let before = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<WriteMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<graph_redefinition::Root>()
            .expect("a rebuild must publish"),
        0
    );
    assert_eq!(
        runtime.stats().demand_retries,
        before,
        "a whole-graph definition must not read the topology it replaces"
    );
    let snapshot = runtime.read().expect("snapshot");
    assert_eq!(snapshot.view::<WriteGraph>().field0_nodes(), vec![1, 2, 3]);
    assert_eq!(snapshot.view::<WriteGraph>().field0_edges(), vec![7, 8]);
    assert_eq!(snapshot.view::<WriteGraph>().field0_node(1), Some(11));
}

#[irrigation]
mod set_keyed {
    use super::{WriteMode, WriteSet};

    #[component]
    pub fn root(mode: &WriteMode) -> u64 {
        if mode.mode() == 0 {
            WriteSet.define_keyed([1u64, 2]);
        } else {
            WriteSet.define_keyed([2u64, 3]);
        }
        0
    }
}

#[test]
fn a_changed_keyed_set_publication_does_not_retry() {
    let runtime = Runtime::new();
    runtime.register_view(<WriteSet as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(set_keyed::Root).expect("install");
    assert_eq!(runtime.read_outputs::<set_keyed::Root>().expect("build"), 0);
    let before = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<WriteMode>().set_mode(1))
        .expect("edit");
    assert_eq!(runtime.read_outputs::<set_keyed::Root>().expect("read"), 0);
    assert_eq!(
        runtime.stats().demand_retries,
        before,
        "a keyed publication must not read the structure it defines"
    );
    let snapshot = runtime.read().expect("snapshot");
    assert_eq!(snapshot.view::<WriteSet>().field0(), vec![2, 3]);
}

#[irrigation]
mod map_removal {
    use super::{WriteMap, WriteMode};

    #[component]
    pub fn root(mode: &WriteMode) -> u64 {
        if mode.mode() == 0 {
            WriteMap.field0().define(1, 10, "map_define");
        } else {
            WriteMap.field0().remove_if_present(1, "map_remove");
        }
        0
    }
}

#[test]
fn removing_a_map_entry_does_not_retry() {
    let runtime = Runtime::new();
    runtime.register_view(<WriteMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(map_removal::Root).expect("install");
    assert_eq!(
        runtime.read_outputs::<map_removal::Root>().expect("build"),
        0
    );
    let before = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<WriteMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<map_removal::Root>().expect("read"),
        0
    );
    assert_eq!(
        runtime.stats().demand_retries,
        before,
        "a removal must project presence from the committed content, not the cell it retracts"
    );
    let snapshot = runtime.read().expect("snapshot");
    assert!(snapshot.view::<WriteMap>().field0().is_empty());
}
#[view(source)]
pub struct EntityGraphMode {
    mode: u64,
}

#[view(derived)]
pub struct EntityGraph(Graph<u64, u64>);

#[irrigation]
mod entity_graph_writers {
    use super::{EntityGraph, EntityGraphMode};

    #[component]
    pub fn build(mode: &EntityGraphMode) -> u64 {
        let graph = EntityGraph.field0();
        graph.define_node(1, 10);
        graph.define_node(2, 20);
        graph.define_edge_trusted(7, 1, 2, 70, "graph_edge");
        if mode.mode() == 1 {
            graph.define_edge_trusted(8, 1, 2, 80, "graph_edge");
        }
        0
    }

    #[component]
    pub fn adjacency() -> Vec<(u64, u64)> {
        EntityGraph.field0().outgoing(1)
    }

    #[component]
    pub fn root(mode: &EntityGraphMode) -> Vec<(u64, u64)> {
        let _ = build(mode);
        adjacency()
    }
}

fn whole_value_adjacency_units(runtime: &Runtime) -> Vec<irrigation::NodeId> {
    let view = format!("{:?}", <EntityGraph as irrigation::ViewSchema>::view_id());
    runtime
        .dump()
        .into_iter()
        .filter(|node| {
            node.kind == "Unit"
                && node
                    .unit
                    .as_deref()
                    .is_some_and(|unit| unit.contains(&view) && unit.contains("Adjacency"))
        })
        .map(|node| node.id)
        .collect()
}

#[test]
fn a_per_entity_graph_publication_is_read_through_its_topology() {
    let runtime = Runtime::new();
    runtime.register_view(<EntityGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(entity_graph_writers::Root)
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs::<entity_graph_writers::Root>()
            .expect("build"),
        vec![(7, 2)],
        "a per-entity build must publish adjacency"
    );
    assert!(
        whole_value_adjacency_units(&runtime).is_empty(),
        "the reader materialized a whole-value adjacency cell instead of the topology"
    );

    runtime
        .edit(|edit| edit.view::<EntityGraphMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<entity_graph_writers::Root>()
            .expect("an edge added after a per-entity build must publish"),
        vec![(7, 2), (8, 2)],
        "the certificate must agree with how the field was published"
    );
    assert!(
        whole_value_adjacency_units(&runtime).is_empty(),
        "the reader must keep dispatching on the certificate"
    );
}

// ---------------------------------------------------------------------------
// Retracting one graph entity retires that entity only (collections-r11-01/02)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct EntityRetractMode {
    mode: u64,
}

#[view(derived)]
pub struct RetractableGraph(Graph<u64, u64>);

/// The payload unit an entity write installs for one graph node.
fn graph_node_value_unit(node: u64) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        <RetractableGraph as irrigation::ViewSchema>::view_id(),
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(irrigation::NodeKey(node)),
    )
}

/// What the graph's read routes answer for one revision.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphAnswers {
    nodes: Vec<u64>,
    edges: Vec<u64>,
    node_one: Option<u64>,
    node_two: Option<u64>,
    edge: Option<irrigation::EdgePayload<u64>>,
    weight: Option<u64>,
}

#[irrigation]
mod graph_entity_retraction {
    use super::{EntityRetractMode, GraphAnswers, RetractableGraph, graph_node_value_unit};

    /// The edge writer never owns the retracted node, and it reads nothing, so
    /// a later revision leaves its unit installed without restaging it.
    #[component]
    pub fn edge(_tag: u64) -> u64 {
        RetractableGraph
            .field0()
            .define_edge_trusted(7, 1, 2, 70, "graph_edge");
        0
    }

    #[component]
    pub fn nodes(mode: &EntityRetractMode) -> u64 {
        let graph = RetractableGraph.field0();
        graph.define_node(2, if mode.mode() == 2 { 21 } else { 20 });
        if mode.mode() == 0 {
            graph.define_node(1, 10);
        } else {
            irrigation::retract_unit(graph_node_value_unit(1), "graph_node_retract");
        }
        0
    }

    #[component]
    pub fn read(_nodes: u64, _edge: u64) -> GraphAnswers {
        let graph = RetractableGraph.field0();
        GraphAnswers {
            nodes: graph.nodes(),
            edges: graph.edges(),
            node_one: graph.node(1),
            node_two: graph.node(2),
            edge: graph.edge(7),
            weight: graph.edge_weight(7),
        }
    }

    #[component]
    pub fn root(mode: &EntityRetractMode) -> Box<GraphAnswers> {
        let edge = edge(0);
        Box::new(read(nodes(mode), edge))
    }
}

#[test]
fn a_node_retraction_serves_the_edges_the_content_still_holds() {
    let runtime = Runtime::new();
    runtime.register_view(<RetractableGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(graph_entity_retraction::Root)
        .expect("install");
    assert_eq!(
        *runtime
            .read_outputs::<graph_entity_retraction::Root>()
            .expect("build"),
        GraphAnswers {
            nodes: vec![1, 2],
            edges: vec![7],
            node_one: Some(10),
            node_two: Some(20),
            edge: Some(irrigation::EdgePayload {
                from: 1,
                to: 2,
                value: 70,
            }),
            weight: Some(70),
        }
    );

    for (mode, node_two) in [(1u64, 20u64), (2, 21)] {
        runtime
            .edit(|edit| edit.view::<EntityRetractMode>().set_mode(mode))
            .expect("edit");
        let reads = *runtime
            .read_outputs::<graph_entity_retraction::Root>()
            .expect("read");
        let snapshot = runtime.inspect_snapshot();
        let committed = snapshot.view::<RetractableGraph>();
        // The committed content is the from-scratch evaluation of this
        // revision: a fresh graph built from its rows answers what it says. The
        // accessors serve cells and must give that same answer in both
        // directions — an edge the content holds is served, and one it dropped
        // is not.
        assert_eq!(
            reads.nodes,
            committed.field0_nodes(),
            "mode {mode}: nodes()"
        );
        assert_eq!(
            reads.edges,
            committed.field0_edges(),
            "mode {mode}: edges()"
        );
        assert_eq!(
            reads.node_one,
            committed.field0_node(1),
            "mode {mode}: node(1)"
        );
        assert_eq!(
            reads.node_two,
            committed.field0_node(2),
            "mode {mode}: node(2)"
        );
        assert_eq!(reads.edge, committed.field0_edge(7), "mode {mode}: edge(7)");
        assert_eq!(
            reads.weight,
            committed.field0_edge(7).map(|edge| edge.value),
            "mode {mode}: edge_weight(7)"
        );

        // The node's own retraction is the only entity the writer named. The
        // incident edge keeps its unit, so the content keeps it as the dangling
        // edge `define_edge_trusted` documents, and `edges()` says so.
        assert_eq!(committed.field0_nodes(), vec![2], "mode {mode}: content");
        assert_eq!(committed.field0_node(1), None, "mode {mode}: removed node");
        assert_eq!(
            committed.field0_node(2),
            Some(node_two),
            "mode {mode}: untouched node"
        );
        assert_eq!(reads.edges, vec![7], "mode {mode}: the edge survives");
        assert_eq!(
            reads.edge,
            Some(irrigation::EdgePayload {
                from: 1,
                to: 2,
                value: 70,
            }),
            "mode {mode}: the edge payload survives"
        );
    }
}

#[irrigation]
mod negative_node_read {
    use super::{EntityRetractMode, RetractableGraph};

    #[component]
    pub fn write(mode: &EntityRetractMode) -> u64 {
        if mode.mode() == 1 {
            RetractableGraph.field0().define_node(1, 10);
        }
        0
    }

    #[component]
    pub fn probe(tag: u64) -> (bool, Option<u64>) {
        let _ = tag;
        let graph = RetractableGraph.field0();
        (graph.contains_node(1), graph.node(1))
    }
}

#[test]
fn an_entity_draft_wakes_a_negative_node_read() {
    let runtime = Runtime::new();
    runtime.register_view(<RetractableGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(negative_node_read::Write).expect("install");
    let probe = || {
        runtime
            .call::<negative_node_read::__ProbeSpec>(negative_node_read::__ProbeArgs { tag: 0 })
            .expect("probe")
    };
    assert_eq!(probe(), (false, None), "the node is absent");
    assert_eq!(
        runtime
            .read_outputs::<negative_node_read::Write>()
            .expect("a writer that defines nothing"),
        0
    );
    assert_eq!(probe(), (false, None), "the node is still absent");

    runtime
        .edit(|edit| edit.view::<EntityRetractMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<negative_node_read::Write>()
            .expect("define the node"),
        0
    );
    assert_eq!(
        probe(),
        (true, Some(10)),
        "the payload draft installs the membership cell the probe read as absent"
    );
}

#[view(derived)]
pub struct BooleanGraph(Graph<bool, u64>);

#[irrigation]
mod boolean_graph_payload {
    use super::{BooleanGraph, EntityRetractMode};

    #[component]
    pub fn root(mode: &EntityRetractMode) -> (Option<bool>, Option<bool>) {
        let graph = BooleanGraph.field0();
        if mode.mode() == 0 {
            graph.define_node(1, false);
            graph.define_node(2, true);
            graph.define_node(3, true);
        } else {
            // A whole-field verb re-derives the cells from the content, so a
            // payload the content lost shows up in the accessor too.
            graph.remove_node(3, "graph_remove_node");
        }
        (graph.node(1), graph.node(2))
    }
}

#[test]
fn a_boolean_node_payload_is_not_overwritten_by_its_membership() {
    let runtime = Runtime::new();
    runtime.register_view(<BooleanGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(boolean_graph_payload::Root)
        .expect("install");
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<boolean_graph_payload::Root>()
            .expect("read")
    };
    assert_eq!(read(&runtime), (Some(false), Some(true)));
    let snapshot = runtime.inspect_snapshot();
    assert_eq!(
        snapshot.view::<BooleanGraph>().field0_node(1),
        Some(false),
        "the presence bit is not the node's payload"
    );
    assert_eq!(snapshot.view::<BooleanGraph>().field0_node(2), Some(true));

    runtime
        .edit(|edit| edit.view::<EntityRetractMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        read(&runtime),
        (Some(false), Some(true)),
        "a whole-field verb must project the payload the writer declared"
    );
    let snapshot = runtime.inspect_snapshot();
    assert_eq!(snapshot.view::<BooleanGraph>().field0_node(1), Some(false));
    assert_eq!(snapshot.view::<BooleanGraph>().field0_node(3), None);
}

// ---------------------------------------------------------------------------
// A caller's read of a port its own edit installs is self-dependent (plan 51)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct CallerMode {
    mode: u64,
}

#[view(derived)]
pub struct CallerList(List<u32>);

#[view(derived)]
pub struct CallerMap(Map<u32, u64>);

#[irrigation]
mod caller_read_before_list_edit {
    use super::{CallerList, CallerMode};

    /// Reads the sequence as the value the insert stages: the read and the
    /// write belong to the same computation, and the read comes first.
    #[component]
    pub fn root(mode: &CallerMode) -> Vec<u32> {
        match mode.mode() {
            0 => CallerList.field0().define_all([1u32, 2], "list_build"),
            _ => CallerList.field0().insert_at(
                0,
                CallerList.field0().to_vec().len() as u32,
                "list_insert",
            ),
        }
        CallerList.to_vec()
    }
}

#[test]
fn a_caller_read_before_an_insert_is_self_dependent() {
    let runtime = Runtime::new();
    runtime.register_view(<CallerList as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(caller_read_before_list_edit::Root)
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs::<caller_read_before_list_edit::Root>()
            .expect("build"),
        vec![1, 2]
    );
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<CallerMode>().set_mode(1))
        .expect("edit");
    let error = runtime
        .read_outputs::<caller_read_before_list_edit::Root>()
        .expect_err("an insert staged from a read of the list it writes must fail");
    assert_self_dependent(&error);
    assert_no_retry(&runtime, retries);
    assert_eq!(
        runtime.inspect_snapshot().view::<CallerList>().field0(),
        vec![1u32, 2],
        "the rejected group must not insert"
    );
}

// ---------------------------------------------------------------------------
// Every collection write verb claims, projects, and requires instead of
// reading the ports its own commit installs (collections-r6-01/02/04)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct VerbMode {
    mode: u64,
}

type MapVerbOutput = (Option<u64>, Option<u64>, Option<u64>);
type GraphVerbOutput = (Vec<u64>, Vec<u64>, Vec<Option<u64>>);
type WholeGraphVerbOutput = (Vec<u64>, Vec<u64>, Vec<Option<u64>>);
type AdapterOutput = (
    Vec<(u64, u64)>,
    Vec<u64>,
    Vec<(u64, u64)>,
    Vec<u64>,
    Vec<(u64, u64)>,
);
type ProjectedOutput = (Vec<(u64, u64)>, Vec<(u64, u64)>);

/// Every map write verb, each driven from a body that then reads the map.
///
/// One verb per arm: the preceding arm's definition site is retired by this
/// arm's commit, so a verb that restages the unit under test takes it over
/// without a second live provider.
#[view(derived)]
pub struct VerbMap(Map<u32, u64>);

#[irrigation]
mod map_verbs {
    use super::{MapVerbOutput, VerbMap, VerbMode};

    #[component]
    pub fn root(mode: &VerbMode) -> Box<MapVerbOutput> {
        let map = VerbMap.field0();
        match mode.mode() {
            0 => map.define(1, 10, "map_define"),
            1 => map.insert(2, 20, "map_insert"),
            2 => map.update(2, 21, "map_update"),
            3 => map.update_if_present(2, |value| value + 1, "map_update_if_present"),
            4 => map.remove_if_present(2, "map_remove_if_present"),
            5 => map.update_if_present(99, |value| value + 1, "map_absent_noop"),
            6 => map.define_all([(1u32, 10u64), (2, 20)], "map_define_all"),
            7 => map.define_all([(1u32, 11u64)], "map_redefine_all"),
            _ => {}
        }
        Box::new((map.get(&1), map.get(&2), map.get(&99)))
    }
}

#[test]
fn every_map_write_verb_publishes_without_a_self_read() {
    let runtime = Runtime::new();
    runtime.register_view(<VerbMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(map_verbs::Root).expect("install");
    let mut retries = runtime.stats().demand_retries;
    let expected: [(u64, MapVerbOutput); 8] = [
        (0, (Some(10), None, None)),
        (1, (None, Some(20), None)),
        (2, (None, Some(21), None)),
        (3, (None, Some(22), None)),
        (4, (None, None, None)),
        (5, (None, None, None)),
        (6, (Some(10), Some(20), None)),
        (7, (Some(11), None, None)),
    ];
    for (mode, (key1, key2, key99)) in expected {
        runtime
            .edit(|edit| edit.view::<VerbMode>().set_mode(mode))
            .expect("mode edit");
        assert_eq!(
            *runtime.read_outputs::<map_verbs::Root>().expect("map verb"),
            (key1, key2, key99),
            "mode {mode}"
        );
        assert_eq!(
            runtime.stats().demand_retries,
            retries,
            "mode {mode}: a collection verb must not read a port its commit moves"
        );
        retries = runtime.stats().demand_retries;
    }
    assert_eq!(
        runtime.inspect_snapshot().view::<VerbMap>().field0(),
        vec![(1u32, 11u64)],
        "the whole-map definition retracts the key it no longer produces"
    );
}

/// Every derived-list write verb, each driven from a body that then reads it.
#[view(derived)]
pub struct VerbList(List<u32>);

#[irrigation]
mod list_verbs {
    use super::{VerbList, VerbMode};

    #[component]
    pub fn root(mode: &VerbMode) -> Vec<u32> {
        let list = VerbList.field0();
        match mode.mode() {
            0 => list.define_all([1u32, 2, 3], "list_build"),
            1 => list.insert_at(1, 4, "list_insert"),
            2 => list.set_at(0, 9, "list_set"),
            3 => list.remove_at(2, "list_remove"),
            4 => list.define_keyed([(1u64, 5u32), (2, 6)], "list_keyed"),
            5 => list.define_all([7u32], "list_whole"),
            _ => {}
        }
        list.to_vec()
    }
}

#[test]
fn every_list_write_verb_publishes_without_a_self_read() {
    let runtime = Runtime::new();
    runtime.register_view(<VerbList as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(list_verbs::Root).expect("install");
    let mut retries = runtime.stats().demand_retries;
    let expected: [(u64, Vec<u32>); 6] = [
        (0, vec![1, 2, 3]),
        (1, vec![1, 4, 2, 3]),
        (2, vec![9, 4, 2, 3]),
        (3, vec![9, 4, 3]),
        (4, vec![5, 6]),
        (5, vec![7]),
    ];
    for (mode, items) in expected {
        runtime
            .edit(|edit| edit.view::<VerbMode>().set_mode(mode))
            .expect("mode edit");
        assert_eq!(
            runtime
                .read_outputs::<list_verbs::Root>()
                .expect("list verb"),
            items,
            "mode {mode}"
        );
        assert_eq!(
            runtime.stats().demand_retries,
            retries,
            "mode {mode}: a collection verb must not read a port its commit moves"
        );
        retries = runtime.stats().demand_retries;
        assert_eq!(
            runtime.inspect_snapshot().view::<VerbList>().field0(),
            items,
            "mode {mode}: committed content"
        );
    }
}

/// Every derived-set write verb.
#[view(derived)]
pub struct VerbSet(Set<u32>);

#[irrigation]
mod set_verbs {
    use super::{VerbMode, VerbSet};

    #[component]
    pub fn root(mode: &VerbMode) -> Vec<u32> {
        let set = VerbSet.field0();
        match mode.mode() {
            0 => set.define_all([1u32, 2, 3], "set_build"),
            1 => set.define_keyed([2u32, 3, 4], "set_keyed"),
            2 => set.define_all([5u32], "set_whole"),
            _ => {}
        }
        set.to_vec()
    }
}

#[test]
fn every_set_write_verb_publishes_without_a_self_read() {
    let runtime = Runtime::new();
    runtime.register_view(<VerbSet as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(set_verbs::Root).expect("install");
    let mut retries = runtime.stats().demand_retries;
    let expected: [(u64, Vec<u32>); 3] = [(0, vec![1, 2, 3]), (1, vec![2, 3, 4]), (2, vec![5])];
    for (mode, members) in expected {
        runtime
            .edit(|edit| edit.view::<VerbMode>().set_mode(mode))
            .expect("mode edit");
        assert_eq!(
            runtime.read_outputs::<set_verbs::Root>().expect("set verb"),
            members,
            "mode {mode}"
        );
        assert_eq!(
            runtime.stats().demand_retries,
            retries,
            "mode {mode}: a collection verb must not read a port its commit moves"
        );
        retries = runtime.stats().demand_retries;
    }
}

/// Per-entity graph verbs, each driven from a body that then reads the graph.
///
/// Each arm restates only the entities the verb does not touch, so no arm has
/// two live writers for the unit under test. Whole-field verbs live in their
/// own gate below: a whole-field definition owns the field's protocol unit, and
/// only another whole-field publisher can hand that unit over.
#[view(derived)]
pub struct VerbGraph(Graph<u64, u64>);

#[irrigation]
mod graph_verbs {
    use super::{GraphVerbOutput, VerbGraph, VerbMode};

    #[component]
    pub fn root(mode: &VerbMode) -> Box<GraphVerbOutput> {
        let graph = VerbGraph.field0();
        match mode.mode() {
            0 => {
                graph.define_node(1, 10);
                graph.define_node(2, 20);
                graph.define_edge_trusted(7, 1, 2, 70, "graph_edge");
            }
            1 => {
                graph.define_node(2, 20);
                graph.define_edge_trusted(7, 1, 2, 70, "graph_edge");
                graph.update_node(1, 11, "graph_update_node");
            }
            2 => {
                graph.define_node(1, 11);
                graph.define_node(2, 20);
                graph.update_edge(7, 1, 2, 71, "graph_update_edge");
            }
            3 => {
                graph.define_node(1, 11);
                graph.define_node(2, 20);
                graph.define_edge_trusted(7, 1, 2, 71, "graph_edge");
                graph.insert_node(3, 30, "graph_insert_node");
            }
            4 => {
                graph.define_node(1, 11);
                graph.define_node(2, 20);
                graph.define_node(3, 30);
                graph.define_edge_trusted(7, 1, 2, 71, "graph_edge");
                graph.insert_edge(8, 1, 3, 80, "graph_insert_edge");
            }
            5 => {
                graph.define_node(1, 11);
                graph.define_node(2, 20);
                graph.define_node(3, 30);
                graph.define_edge_trusted(7, 1, 2, 71, "graph_edge");
                graph.define_edge(9, 1, 2, 90);
            }
            _ => {}
        }
        Box::new((
            graph.nodes(),
            graph.edges(),
            vec![
                graph.node(1),
                graph.edge(7).map(|edge| edge.value),
                graph.edge(8).map(|edge| edge.value),
            ],
        ))
    }
}

#[test]
fn every_graph_entity_verb_publishes_without_a_self_read() {
    let runtime = Runtime::new();
    runtime.register_view(<VerbGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(graph_verbs::Root).expect("install");
    let mut retries = runtime.stats().demand_retries;
    let expected: [(u64, GraphVerbOutput); 6] = [
        (0, (vec![1, 2], vec![7], vec![Some(10), Some(70), None])),
        (1, (vec![1, 2], vec![7], vec![Some(11), Some(70), None])),
        (2, (vec![1, 2], vec![7], vec![Some(11), Some(71), None])),
        (3, (vec![1, 2, 3], vec![7], vec![Some(11), Some(71), None])),
        (
            4,
            (
                vec![1, 2, 3],
                vec![7, 8],
                vec![Some(11), Some(71), Some(80)],
            ),
        ),
        (
            5,
            (vec![1, 2, 3], vec![7, 9], vec![Some(11), Some(71), None]),
        ),
    ];
    for (mode, (nodes, edges, weights)) in expected {
        runtime
            .edit(|edit| edit.view::<VerbMode>().set_mode(mode))
            .expect("mode edit");
        assert_eq!(
            *runtime
                .read_outputs::<graph_verbs::Root>()
                .expect("graph verb"),
            (nodes.clone(), edges.clone(), weights.clone()),
            "mode {mode}"
        );
        assert_eq!(
            runtime.stats().demand_retries,
            retries,
            "mode {mode}: a collection verb must not read a port its commit moves"
        );
        retries = runtime.stats().demand_retries;
        let snapshot = runtime.inspect_snapshot();
        let graph = snapshot.view::<VerbGraph>();
        assert_eq!(graph.field0_nodes(), nodes, "mode {mode}: nodes");
        assert_eq!(graph.field0_edges(), edges, "mode {mode}: edges");
    }
}

/// Whole-field graph verbs, each driven from a body that then reads the graph.
#[view(derived)]
pub struct VerbWholeGraph(Graph<u64, u64>);

#[irrigation]
mod whole_graph_verbs {
    use super::{VerbMode, VerbWholeGraph, WholeGraphVerbOutput};

    #[component]
    pub fn root(mode: &VerbMode) -> Box<WholeGraphVerbOutput> {
        let graph = VerbWholeGraph.field0();
        match mode.mode() {
            0 => graph.define_graph(
                &[(1u64, 10u64), (2, 20), (3, 30)],
                &[(7u64, 1, 2, 70u64)],
                "graph_define",
            ),
            1 => graph.remove_node(2, "graph_remove_node"),
            2 => graph.remove_node_if_empty(3, "graph_remove_empty"),
            3 => graph.define_graph(
                &[(1u64, 11u64), (5, 50)],
                &[(9u64, 1, 5, 90u64)],
                "graph_define",
            ),
            _ => {}
        }
        Box::new((
            graph.nodes(),
            graph.edges(),
            vec![graph.node(1), graph.edge(7).map(|edge| edge.value)],
        ))
    }
}

#[test]
fn every_whole_graph_verb_publishes_without_a_self_read() {
    let runtime = Runtime::new();
    runtime.register_view(<VerbWholeGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(whole_graph_verbs::Root).expect("install");
    let mut retries = runtime.stats().demand_retries;
    let expected: [(u64, WholeGraphVerbOutput); 4] = [
        (0, (vec![1, 2, 3], vec![7], vec![Some(10), Some(70)])),
        (1, (vec![1, 3], vec![], vec![Some(10), None])),
        (2, (vec![1], vec![], vec![Some(10), None])),
        (3, (vec![1, 5], vec![9], vec![Some(11), None])),
    ];
    for (mode, (nodes, edges, weights)) in expected {
        runtime
            .edit(|edit| edit.view::<VerbMode>().set_mode(mode))
            .expect("mode edit");
        assert_eq!(
            *runtime
                .read_outputs::<whole_graph_verbs::Root>()
                .expect("whole graph verb"),
            (nodes.clone(), edges.clone(), weights),
            "mode {mode}"
        );
        assert_eq!(
            runtime.stats().demand_retries,
            retries,
            "mode {mode}: a collection verb must not read a port its commit moves"
        );
        retries = runtime.stats().demand_retries;
        let snapshot = runtime.inspect_snapshot();
        let graph = snapshot.view::<VerbWholeGraph>();
        assert_eq!(graph.field0_nodes(), nodes, "mode {mode}: nodes");
        assert_eq!(graph.field0_edges(), edges, "mode {mode}: edges");
    }
}

/// Adapter verbs: `to_list`, `to_map`, and `node_map` read one collection and
/// publish another, so their target is published as a whole field.
#[view(source)]
pub struct AdapterSource {
    values: List<u64>,
}

#[view(derived)]
pub struct AdapterSourceMap(Map<u64, u64>);

#[view(derived)]
pub struct AdapterSourceList(List<u64>);

#[view(derived)]
pub struct AdapterDerived(List<u64>);

#[view(derived)]
pub struct AdapterDerivedMap(Map<u64, u64>);

#[view(derived)]
pub struct AdapterMapList(List<u64>);

#[view(derived)]
pub struct AdapterGraph(Graph<u64, u64>);

#[view(derived)]
pub struct AdapterNodeMap(Map<u64, u64>);

#[irrigation]
mod adapter_verbs {
    use super::{
        AdapterDerived, AdapterDerivedMap, AdapterGraph, AdapterMapList, AdapterNodeMap,
        AdapterOutput, AdapterSource, AdapterSourceList, AdapterSourceMap,
    };

    #[component]
    pub fn root() -> Box<AdapterOutput> {
        let source = AdapterSource.values();
        source.to_map(&AdapterSourceMap.field0(), |id, value| Some((id, *value)));
        source.to_list(&AdapterSourceList.field0(), |id, value| Some((id, *value)));

        let derived = AdapterDerived.field0();
        derived.define_keyed([(1u64, 5u64), (2, 6)], "adapter_build");
        derived.to_map(&AdapterDerivedMap.field0(), |id, value| Some((id, *value)));

        AdapterSourceMap
            .field0()
            .to_list(&AdapterMapList.field0(), |key, value| {
                Some((u64::from(*key), *value))
            });

        let graph = AdapterGraph.field0();
        graph.define_node(1, 10);
        graph.define_node(2, 20);
        graph.define_edge_trusted(7, 1, 2, 70, "adapter_edge");
        graph.node_map(&AdapterNodeMap.field0());

        let published: AdapterOutput = (
            AdapterSourceMap.field0().to_vec(),
            AdapterSourceList.field0().to_vec(),
            AdapterDerivedMap.field0().to_vec(),
            AdapterMapList.field0().to_vec(),
            AdapterNodeMap.field0().to_vec(),
        );
        Box::new(published)
    }
}

#[test]
fn adapter_verbs_publish_their_whole_target() {
    let runtime = Runtime::new();
    for descriptor in [
        <AdapterSource as irrigation::ViewSchema>::DESCRIPTOR,
        <AdapterSourceMap as irrigation::ViewSchema>::DESCRIPTOR,
        <AdapterSourceList as irrigation::ViewSchema>::DESCRIPTOR,
        <AdapterDerived as irrigation::ViewSchema>::DESCRIPTOR,
        <AdapterDerivedMap as irrigation::ViewSchema>::DESCRIPTOR,
        <AdapterMapList as irrigation::ViewSchema>::DESCRIPTOR,
        <AdapterGraph as irrigation::ViewSchema>::DESCRIPTOR,
        <AdapterNodeMap as irrigation::ViewSchema>::DESCRIPTOR,
    ] {
        runtime.register_view(descriptor);
    }
    runtime.install(adapter_verbs::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut source = edit.view::<AdapterSource>();
            source.values().push(10);
            source.values().push(20);
        })
        .expect("seed");
    assert_eq!(
        *runtime
            .read_outputs::<adapter_verbs::Root>()
            .expect("build"),
        (
            vec![(1, 10), (2, 20)],
            vec![10, 20],
            vec![(1, 5), (2, 6)],
            vec![10, 20],
            vec![(1, 10), (2, 20)],
        )
    );
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<AdapterSource>().values().push(30))
        .expect("edit");
    assert_eq!(
        *runtime.read_outputs::<adapter_verbs::Root>().expect("read"),
        (
            vec![(1, 10), (2, 20), (3, 30)],
            vec![10, 20, 30],
            vec![(1, 5), (2, 6)],
            vec![10, 20, 30],
            vec![(1, 10), (2, 20)],
        )
    );
    assert_eq!(
        runtime.stats().demand_retries,
        retries,
        "an adapter must not read a target port its own commit installs"
    );
}

// ---------------------------------------------------------------------------
// A total map publisher retracts outside a restating body (collections-r6-04)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ProjectSource {
    values: List<u64>,
}

#[view(source)]
pub struct GraphSeed {
    count: u64,
}

#[view(derived)]
pub struct SeedGraph(Graph<u64, u64>);

#[view(derived)]
pub struct ProjectedMap(Map<u64, u64>);

#[view(derived)]
pub struct ProjectedNodes(Map<u64, u64>);

/// Publishes both adapter targets from a general component: no `replace_all`,
/// so only the whole-field publisher can retract the entries it drops.
struct ProjectSources;

impl irrigation::GeneralComponent for ProjectSources {
    type State = ();
    type Output = u64;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00E0_0021);
    const NAME: &'static str = "gates::ProjectSources";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[
        <ProjectSource as irrigation::ViewSchema>::DESCRIPTOR,
        <SeedGraph as irrigation::ViewSchema>::DESCRIPTOR,
        <ProjectedMap as irrigation::ViewSchema>::DESCRIPTOR,
        <ProjectedNodes as irrigation::ViewSchema>::DESCRIPTOR,
    ];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[
        <ProjectedMap as irrigation::ViewSchema>::DESCRIPTOR,
        <ProjectedNodes as irrigation::ViewSchema>::DESCRIPTOR,
    ];

    fn initialize(
        &self,
        _cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let source = ProjectSource.values();
        source.to_map(&ProjectedMap.field0(), |id, value| Some((id, *value)));
        SeedGraph.field0().node_map(&ProjectedNodes.field0());
        Ok(((), source.len() as u64))
    }

    fn update(
        &self,
        _cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        unreachable!("the projector resets")
    }
}

#[irrigation]
mod projected_reader {
    use super::{GraphSeed, ProjectedMap, ProjectedNodes, ProjectedOutput, SeedGraph};

    #[component]
    pub fn root(count: &GraphSeed) -> Box<ProjectedOutput> {
        let nodes = match count.count() {
            0 => vec![],
            1 => vec![(1u64, 10u64)],
            _ => vec![(1u64, 10u64), (2u64, 20u64)],
        };
        let edges = if count.count() >= 2 {
            vec![(7u64, 1, 2, 70u64)]
        } else {
            vec![]
        };
        SeedGraph.define_graph(&nodes, &edges);
        let projected: ProjectedOutput = (
            ProjectedMap.field0().to_vec(),
            ProjectedNodes.field0().to_vec(),
        );
        Box::new(projected)
    }
}

#[test]
fn a_general_component_total_publisher_retracts_dropped_entries() {
    let runtime = Runtime::new();
    for descriptor in [
        <ProjectSource as irrigation::ViewSchema>::DESCRIPTOR,
        <GraphSeed as irrigation::ViewSchema>::DESCRIPTOR,
        <SeedGraph as irrigation::ViewSchema>::DESCRIPTOR,
        <ProjectedMap as irrigation::ViewSchema>::DESCRIPTOR,
        <ProjectedNodes as irrigation::ViewSchema>::DESCRIPTOR,
    ] {
        runtime.register_view(descriptor);
    }
    runtime
        .install_general_producer(ProjectSources)
        .expect("projector");
    runtime.install(projected_reader::Root).expect("root");
    runtime
        .edit(|edit| {
            edit.view::<GraphSeed>().set_count(2);
            let mut source = edit.view::<ProjectSource>();
            source.values().push(10);
            source.values().push(20);
            source.values().push(30);
        })
        .expect("edit");
    assert_eq!(
        *runtime
            .read_outputs::<projected_reader::Root>()
            .expect("build"),
        (vec![(1, 10), (2, 20), (3, 30)], vec![(1, 10), (2, 20)],)
    );
    let retries = runtime.stats().demand_retries;

    // Drop source element 2 and graph node 2. The general component does not
    // restate its output, so the entries have to be retracted by the publisher.
    runtime
        .edit(|edit| {
            edit.view::<GraphSeed>().set_count(1);
            let mut source = edit.view::<ProjectSource>();
            source.values().remove(1);
        })
        .expect("edit");
    assert_eq!(
        *runtime
            .read_outputs::<projected_reader::Root>()
            .expect("read"),
        (vec![(1, 10), (3, 30)], vec![(1, 10)]),
        "the removed element's and node's entries must be retracted"
    );
    assert_eq!(
        runtime.stats().demand_retries,
        retries,
        "a total publisher must project the committed key set, not read it"
    );
}

/// A checked update on an absent entity fails at commit and leaves the
/// committed content untouched; the presence requirement replaces the read.
#[view(derived)]
pub struct StrictMap(Map<u32, u64>);

#[irrigation]
mod strict_updates {
    use super::{StrictGraph, StrictMap, VerbMode};

    #[component]
    pub fn map(mode: &VerbMode) -> u64 {
        let map = StrictMap.field0();
        match mode.mode() {
            0 => map.define(1, 10, "seed"),
            1 => {
                map.define(1, 10, "seed");
                map.update(2, 20, "missing_key_update");
            }
            _ => {}
        }
        0
    }

    #[component]
    pub fn graph(mode: &VerbMode) -> u64 {
        let graph = StrictGraph.field0();
        match mode.mode() {
            0 => graph.define_node(1, 10),
            1 => {
                graph.define_node(1, 10);
                graph.update_node(2, 20, "missing_node_update");
            }
            2 => {
                graph.define_node(1, 10);
                graph.update_edge(7, 1, 1, 70, "missing_edge_update");
            }
            _ => {}
        }
        0
    }
}

#[view(derived)]
pub struct StrictGraph(Graph<u64, u64>);

fn assert_missing_entity(error: &irrigation::ComputeError) {
    assert!(
        matches!(
            error,
            irrigation::ComputeError::PreconditionUnmet {
                reason: irrigation_core::error::PreconditionReason::NoProducer,
                ..
            }
        ),
        "an absent entity is a commit-time precondition failure, got {error:?}"
    );
}

#[test]
fn a_checked_update_of_an_absent_entity_fails_without_publishing() {
    let runtime = Runtime::new();
    runtime.register_view(<StrictMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(strict_updates::Map).expect("install");
    runtime
        .edit(|edit| edit.view::<VerbMode>().set_mode(0))
        .expect("edit");
    runtime.read_outputs::<strict_updates::Map>().expect("seed");
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<VerbMode>().set_mode(1))
        .expect("edit");
    let error = runtime
        .read_outputs::<strict_updates::Map>()
        .expect_err("an update of an absent key must fail at commit");
    assert_missing_entity(&error);
    assert_eq!(
        runtime.stats().demand_retries,
        retries,
        "the presence requirement must be decided at commit, not by a retry"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<StrictMap>().field0(),
        vec![(1u32, 10u64)],
        "the rejected group must not add a key"
    );
    let repeated = runtime
        .read_outputs::<strict_updates::Map>()
        .expect_err("the failure must replay");
    assert_missing_entity(&repeated);
    assert_eq!(
        runtime.inspect_snapshot().view::<StrictMap>().field0(),
        vec![(1u32, 10u64)]
    );

    let runtime = Runtime::new();
    runtime.register_view(<StrictGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(strict_updates::Graph).expect("install");
    runtime
        .edit(|edit| edit.view::<VerbMode>().set_mode(0))
        .expect("edit");
    runtime
        .read_outputs::<strict_updates::Graph>()
        .expect("seed");
    for mode in [1u64, 2] {
        runtime
            .edit(|edit| edit.view::<VerbMode>().set_mode(mode))
            .expect("edit");
        let error = runtime
            .read_outputs::<strict_updates::Graph>()
            .expect_err("a checked update of an absent entity must fail at commit");
        assert_missing_entity(&error);
        assert_eq!(
            runtime
                .inspect_snapshot()
                .view::<StrictGraph>()
                .field0_nodes(),
            vec![1],
            "mode {mode}: the rejected group must not add a node"
        );
        assert_eq!(
            runtime
                .inspect_snapshot()
                .view::<StrictGraph>()
                .field0_edges(),
            Vec::<u64>::new(),
            "mode {mode}: the rejected group must not add an edge"
        );
    }
}
// ---------------------------------------------------------------------------
// A drain install never lands after the demand's last commit
// (runtime-kernel-r7-01)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct LateWrite(pub Map<u32, u32>);

fn late_unit(key: u32) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        <LateWrite as irrigation::ViewSchema>::DESCRIPTOR.id,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(key),
    )
}

#[irrigation]
mod late_drain {
    use super::{LateWrite, late_unit};
    use irrigation::ViewSchema;

    /// Reads the port the deferred group installs, recording its absence.
    #[component]
    pub fn reader() -> u32 {
        irrigation::try_read_unit_value(late_unit(1), |_| None::<u32>).unwrap_or(0)
    }

    /// Stages a group that tracks nothing, so only its requirement can defer it.
    fn stage_late_write() {
        irrigation::ensure_view(<LateWrite as ViewSchema>::DESCRIPTOR);
        irrigation::require_condition(false, &[late_unit(9)], "gate present");
        irrigation::define_unit(late_unit(1), 42u32, "late");
    }

    /// Installs the unit the staged group requires, before the drain runs.
    fn provide_gate() {
        irrigation::ensure_view(<LateWrite as ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(late_unit(9), 1u32, "gate");
    }

    #[component]
    pub fn root() -> u32 {
        let first = reader();
        stage_late_write();
        provide_gate();
        first
    }
}

#[test]
fn a_drain_install_republishes_the_value_the_demand_committed() {
    let runtime = Runtime::new();
    runtime.register_view(<LateWrite as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(late_drain::Root).expect("install");
    let retries = runtime.stats().demand_retries;
    assert_eq!(
        runtime.read_outputs::<late_drain::Root>().expect("read"),
        42,
        "the drain published the port after the reader committed: the demand must not return the pre-drain value"
    );
    assert_eq!(
        runtime.stats().demand_retries,
        retries + 1,
        "the install invalidates a node this revision already finalized, so the demand retries once"
    );
    assert_eq!(
        runtime
            .read_outputs::<late_drain::Root>()
            .expect("read at the same revision"),
        42,
        "one revision serves one result"
    );
}
// ---------------------------------------------------------------------------
// A provider's whole-field value is masked from its committed cell, not by
// retracting the field (runtime-api-r7-02)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct SettledSequence {
    values: Reduced<Sum>,
    flag: bool,
}

#[irrigation]
mod sequence_provider {
    use super::SettledSequence;
    use irrigation::ViewSchema;

    /// Publishes the whole Reduced field, so its cell has no entity to read
    /// through the content vtable.
    #[component]
    pub fn publish() -> u64 {
        irrigation::ensure_view(<SettledSequence as ViewSchema>::DESCRIPTOR);
        SettledSequence
            .values()
            .define_all([10u64, 20, 30], "provider sequence");
        0
    }
}

#[test]
fn a_provider_owned_whole_field_value_does_not_mask_a_content_edit() {
    let runtime = Runtime::new();
    runtime.register_view(<SettledSequence as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(sequence_provider::Publish)
        .expect("install");
    runtime
        .read_outputs::<sequence_provider::Publish>()
        .expect("provider run");
    assert_eq!(
        runtime
            .inspect_snapshot()
            .view::<SettledSequence>()
            .values(),
        vec![10, 20, 30]
    );

    // The typed editor stages the cloned committed content on drop, provider
    // sequence included: editing a host field must still commit.
    runtime
        .edit(|edit| edit.view::<SettledSequence>().set_flag(true))
        .expect("a typed edit of a host field must not be refused");
    let committed = runtime.inspect_snapshot();
    assert!(committed.view::<SettledSequence>().flag());
    assert_eq!(
        committed.view::<SettledSequence>().values(),
        vec![10, 20, 30],
        "the provider's whole-field value must survive the edit"
    );

    // A replacement that empties or changes the provider's sequence is the
    // provider's to publish, and the refusal must still name it.
    let revision = runtime.revision();
    let mut changed = SettledSequenceContent::empty();
    changed.values = changed.values.push(20u64);
    for content in [SettledSequenceContent::empty(), changed] {
        let refused = runtime.edit(|edit| {
            edit.set_content(
                <SettledSequence as irrigation::ViewSchema>::view_id(),
                std::sync::Arc::new(content.clone()),
                irrigation::values_equal::<SettledSequenceContent>,
            );
        });
        assert!(
            matches!(
                refused,
                Err(irrigation::ComputeError::DefinitionDenied { .. })
            ),
            "a replacement that replaces the provider's sequence must be refused: {refused:?}"
        );
        assert_eq!(
            runtime.revision(),
            revision,
            "a refused replacement publishes no revision"
        );
    }
    assert_eq!(
        runtime
            .inspect_snapshot()
            .view::<SettledSequence>()
            .values(),
        vec![10, 20, 30],
        "the provider's sequence keeps its committed cell"
    );
}
// ---------------------------------------------------------------------------
// A content-only replacement moves the structure its readers observed
// (runtime-api-r7-03)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ContentOnlyMap {
    entries: Map<u32, u32>,
}

#[irrigation]
mod content_only_reader {
    use super::ContentOnlyMap;

    #[component]
    pub fn keys(map: &ContentOnlyMap) -> Vec<u32> {
        map.entries().keys()
    }
}

#[test]
fn a_content_only_edit_republishes_the_field_structure() {
    let runtime = Runtime::new();
    runtime.register_view(<ContentOnlyMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.install(content_only_reader::Keys).expect("install");
    runtime
        .edit(|edit| {
            let mut map = edit.view::<ContentOnlyMap>();
            let mut entries = map.entries();
            entries.insert(1, 10);
            entries.insert(2, 20);
        })
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<content_only_reader::Keys>()
            .expect("read"),
        vec![1, 2]
    );
    // The reading component materialized the structure unit; its version is
    // the observable the structure consumers are keyed on.
    let structure_version = |runtime: &Runtime| {
        runtime
            .dump()
            .into_iter()
            .find(|node| {
                node.unit
                    .as_deref()
                    .is_some_and(|unit| unit.contains("Structure"))
            })
            .expect("the keys() reader materializes the structure unit")
            .changed_at
    };
    let before = structure_version(&runtime);

    let (seeded, _) = irrigation::PMap::new().insert(1u32, 10u32);
    let (next, _) = seeded.insert(3u32, 30u32);
    runtime
        .edit(|edit| {
            edit.set_content(
                <ContentOnlyMap as irrigation::ViewSchema>::view_id(),
                std::sync::Arc::new(ContentOnlyMapContent {
                    entries: next.into(),
                }),
                irrigation::values_equal::<ContentOnlyMapContent>,
            );
        })
        .expect("no provider owns a cell in this View");
    assert_eq!(
        runtime
            .read_outputs::<content_only_reader::Keys>()
            .expect("re-read"),
        vec![1, 3],
        "a structure reader must see the replaced key set"
    );
    assert!(
        structure_version(&runtime) > before,
        "the structure unit must move with the content it is derived from"
    );
}

#[view(source)]
pub struct PublisherCount {
    count: u64,
}

#[view(derived)]
pub struct PublishedMap {
    entries: Map<u32, u32>,
}

struct MapContentPublisher;

impl irrigation::GeneralComponent for MapContentPublisher {
    type State = u64;
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CC_0102);
    const NAME: &'static str = "gates::MapContentPublisher";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<ClearMode as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<PublishedMap as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let entries: u64 = cx.read_typed(irrigation::UnitKey::field(
            <ClearMode as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        let mut map: irrigation::PMap<u32, u32> = irrigation::PMap::new();
        for key in 1..=entries {
            let key = key as u32;
            map = map.insert(key, key * 10).0;
        }
        cx.set_content(
            <PublishedMap as irrigation::ViewSchema>::view_id(),
            std::sync::Arc::new(PublishedMapContent {
                entries: map.into(),
            }),
        );
        Ok((entries, ()))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation]
mod published_map_reader {
    use super::PublishedMap;

    #[component]
    pub fn keys(map: &PublishedMap) -> Vec<u32> {
        map.entries().keys()
    }
}

#[test]
fn a_general_content_replacement_republishes_the_field_structure() {
    let runtime = Runtime::new();
    runtime.register_view(<ClearMode as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<PublishedMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_producer(MapContentPublisher)
        .expect("install producer");
    runtime
        .install(published_map_reader::Keys)
        .expect("install");

    runtime
        .edit(|edit| edit.view::<ClearMode>().set_mode(2))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<published_map_reader::Keys>()
            .expect("read"),
        vec![1, 2],
        "the producer replaces the content during the reader's own attempt"
    );

    runtime
        .edit(|edit| edit.view::<ClearMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<published_map_reader::Keys>()
            .expect("re-read"),
        vec![1],
        "the second replacement must republish the projection as well"
    );
}
// ---------------------------------------------------------------------------
// A whole-region replacement owns the cells it did not state
// (runtime-kernel-r8-02)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ContentEntryMap {
    entries: Map<u32, u32>,
}

#[irrigation]
mod content_entry_reader {
    use super::ContentEntryMap;

    #[component]
    pub fn value(map: &ContentEntryMap) -> u32 {
        map.entries().get(&1).unwrap_or(0)
    }

    #[component]
    pub fn present(map: &ContentEntryMap) -> bool {
        map.entries().contains_key(&2)
    }
}

/// Replaces the whole content of a View with no cell write beside it.
fn replace_content_entries(runtime: &Runtime, entries: irrigation::PMap<u32, u32>) {
    runtime
        .edit(|edit| {
            edit.set_content(
                <ContentEntryMap as irrigation::ViewSchema>::view_id(),
                std::sync::Arc::new(ContentEntryMapContent {
                    entries: entries.into(),
                }),
                irrigation::values_equal::<ContentEntryMapContent>,
            );
        })
        .expect("no provider owns a cell in this View");
}

#[test]
fn a_content_replacement_refreshes_a_materialized_entry_value() {
    let runtime = Runtime::new();
    runtime.register_view(<ContentEntryMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(content_entry_reader::Value)
        .expect("install");
    runtime
        .edit(|edit| {
            let mut map = edit.view::<ContentEntryMap>();
            map.entries().insert(1, 10);
        })
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<content_entry_reader::Value>()
            .expect("read"),
        10,
        "the reader materializes the entry's value cell"
    );

    replace_content_entries(&runtime, irrigation::PMap::new().insert(1u32, 99u32).0);
    assert_eq!(
        runtime
            .read_outputs::<content_entry_reader::Value>()
            .expect("re-read"),
        99,
        "a materialized entry value must follow the replaced content"
    );
    assert_eq!(
        runtime
            .inspect_snapshot()
            .view::<ContentEntryMap>()
            .entries(),
        vec![(1, 99)],
        "the snapshot and the cell agree"
    );
}

#[test]
fn a_content_replacement_moves_a_materialized_membership() {
    let runtime = Runtime::new();
    runtime.register_view(<ContentEntryMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(content_entry_reader::Present)
        .expect("install");
    runtime
        .edit(|edit| {
            let mut map = edit.view::<ContentEntryMap>();
            map.entries().insert(1, 10);
        })
        .expect("seed");
    assert!(
        !runtime
            .read_outputs::<content_entry_reader::Present>()
            .expect("read"),
        "the reader materializes key 2's absence as a membership cell"
    );

    let (seeded, _) = irrigation::PMap::new().insert(1u32, 10u32);
    let (with_two, _) = seeded.insert(2u32, 20u32);
    replace_content_entries(&runtime, with_two);
    assert!(
        runtime
            .read_outputs::<content_entry_reader::Present>()
            .expect("re-read"),
        "a key the replacement introduced must become present"
    );

    replace_content_entries(&runtime, seeded);
    assert!(
        !runtime
            .read_outputs::<content_entry_reader::Present>()
            .expect("re-read"),
        "a key the replacement dropped must become absent"
    );
}

#[irrigation]
mod published_map_value_reader {
    use super::PublishedMap;

    #[component]
    pub fn value(map: &PublishedMap) -> u32 {
        map.entries().get(&1).unwrap_or(0)
    }
}

/// Replaces a whole View's content with one entry whose payload follows a count.
struct MapValuePublisher;

impl irrigation::GeneralComponent for MapValuePublisher {
    type State = u64;
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CC_0103);
    const NAME: &'static str = "gates::MapValuePublisher";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<PublisherCount as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<PublishedMap as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let scaled: u64 = cx.read_typed(irrigation::UnitKey::field(
            <PublisherCount as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        let (entries, _) = irrigation::PMap::new().insert(1u32, (scaled * 10) as u32);
        cx.set_content(
            <PublishedMap as irrigation::ViewSchema>::view_id(),
            std::sync::Arc::new(PublishedMapContent {
                entries: entries.into(),
            }),
        );
        Ok((scaled, ()))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_general_content_replacement_refreshes_a_materialized_entry_value() {
    let runtime = Runtime::new();
    runtime.register_view(<PublisherCount as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<PublishedMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_producer(MapValuePublisher)
        .expect("install producer");
    runtime
        .install(published_map_value_reader::Value)
        .expect("install");

    runtime
        .edit(|edit| edit.view::<PublisherCount>().set_count(2))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<published_map_value_reader::Value>()
            .expect("read"),
        20,
        "the producer replaces the content during the reader's own attempt"
    );

    runtime
        .edit(|edit| edit.view::<PublisherCount>().set_count(5))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<published_map_value_reader::Value>()
            .expect("re-read"),
        50,
        "the produced entry value must follow the replacement"
    );
}

#[test]
fn an_ordinary_edit_does_not_pay_for_the_views_materialized_cells() {
    const WIDTH: u32 = 512;
    let runtime = Runtime::new();
    runtime.register_view(<ConsumerTable as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(structure_consumers::OneValue)
        .expect("install");
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            let mut entries = table.entries();
            for key in 1..=WIDTH {
                entries.insert(key, 0);
            }
        })
        .expect("seed");
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<structure_consumers::OneValue>()
            .expect("read")
    };
    assert_eq!(read(&runtime), Some(0));

    // The edit stages the cells it touches, so the replacement beside it states
    // its delta: a View `WIDTH` entries wide must not turn one edit into a walk
    // over every entry.
    let before = runtime.stats().invalidations;
    runtime
        .edit(|edit| {
            let mut table = edit.view::<ConsumerTable>();
            table.entries().insert(2, 20);
        })
        .expect("edit");
    let invalidated = runtime.stats().invalidations - before;
    assert_eq!(read(&runtime), Some(0), "key 1 is untouched");
    assert!(
        invalidated <= 8,
        "an ordinary edit must not invalidate the View's other cells: {invalidated} nodes for {WIDTH} entries"
    );

    // A replacement that leaves the content the same walks no cell at all.
    let quiet = runtime.stats().invalidations;
    let revision = runtime.revision();
    runtime
        .edit(|edit| {
            let content = edit.content::<ConsumerTableContent>(
                <ConsumerTable as irrigation::ViewSchema>::view_id(),
            );
            edit.set_content(
                <ConsumerTable as irrigation::ViewSchema>::view_id(),
                std::sync::Arc::new((*content).clone()),
                irrigation::values_equal::<ConsumerTableContent>,
            );
        })
        .expect("an unchanged replacement commits nothing");
    assert_eq!(
        runtime.stats().invalidations,
        quiet,
        "an unchanged content must not walk the View's cells"
    );
    assert_eq!(
        runtime.revision(),
        revision,
        "an unchanged content publishes no revision"
    );
}
#[view(derived)]
pub struct ScaledSlot {
    value: u64,
}

#[irrigation]
mod scaled_slot_reader {
    use super::ScaledSlot;

    #[component]
    pub fn value(slot: &ScaledSlot) -> u64 {
        slot.value()
    }
}

/// Publishes one scalar View's whole content from a count.
struct ScaledSlotPublisher;

impl irrigation::GeneralComponent for ScaledSlotPublisher {
    type State = u64;
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CC_0104);
    const NAME: &'static str = "gates::ScaledSlotPublisher";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<PublisherCount as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<ScaledSlot as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let scaled: u64 = cx.read_typed(irrigation::UnitKey::field(
            <PublisherCount as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        cx.set_content(
            <ScaledSlot as irrigation::ViewSchema>::view_id(),
            std::sync::Arc::new(ScaledSlotContent { value: scaled * 10 }),
        );
        Ok((scaled, ()))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_general_content_replacement_refreshes_a_materialized_scalar_cell() {
    let runtime = Runtime::new();
    runtime.register_view(<PublisherCount as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<ScaledSlot as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_producer(ScaledSlotPublisher)
        .expect("install producer");
    runtime.install(scaled_slot_reader::Value).expect("install");

    runtime
        .edit(|edit| edit.view::<PublisherCount>().set_count(2))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<scaled_slot_reader::Value>()
            .expect("read"),
        20,
        "the producer replaces the content while the reader holds the cell"
    );

    runtime
        .edit(|edit| edit.view::<PublisherCount>().set_count(5))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<scaled_slot_reader::Value>()
            .expect("re-read"),
        50,
        "the materialized scalar cell must follow the replacement"
    );
}
// ---------------------------------------------------------------------------
// A collection re-checks every member it reads (collections-r8-02)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct RacedKeep {
    keep: u64,
}

#[view(derived)]
pub struct RacedMap(Map<u32, u64>);

#[view(derived)]
pub struct RacedList(List<u64>);

/// Parks the reading demand between the member reads of one collection, so a
/// peer demand can commit a retraction the reader already saw the structure for.
struct RaceBarrier {
    armed: AtomicBool,
    reached: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl RaceBarrier {
    fn new() -> (Self, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        (
            Self {
                armed: AtomicBool::new(true),
                reached: Mutex::new(reached_tx),
                release: Mutex::new(release_rx),
            },
            reached_rx,
            release_tx,
        )
    }

    /// Blocks the first member of the first attempt until the test releases it.
    ///
    /// The retry that follows the invalidated structure must run to completion,
    /// so later members never park.
    fn hold(&self) {
        if !self.armed.swap(false, Ordering::AcqRel) {
            return;
        }
        let _ = self.reached.lock().expect("reached").send(());
        let _ = self.release.lock().expect("release").recv();
    }
}

static MAP_RACE: OnceLock<RaceBarrier> = OnceLock::new();
static LIST_RACE: OnceLock<RaceBarrier> = OnceLock::new();

fn map_race_hold() {
    if let Some(barrier) = MAP_RACE.get() {
        barrier.hold();
    }
}

fn list_race_hold() {
    if let Some(barrier) = LIST_RACE.get() {
        barrier.hold();
    }
}

fn arm_race(slot: &'static OnceLock<RaceBarrier>) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (barrier, reached, release) = RaceBarrier::new();
    let _ = slot.set(barrier);
    (reached, release)
}

fn race_runtime() -> Runtime {
    Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        stack_size: 16 * 1024 * 1024,
        ..irrigation::RuntimeConfig::default()
    })
}

#[irrigation]
mod raced_map_writer {
    use super::{RacedKeep, RacedMap};

    /// Publishes the map as one whole field: the run that keeps one key
    /// retracts the other, which is the writer the reading demand races.
    #[component]
    pub fn write(keep: &RacedKeep) -> u64 {
        irrigation::ensure_view(<RacedMap as irrigation::ViewSchema>::DESCRIPTOR);
        let entries = RacedMap.field0();
        let items = if keep.keep() >= 2 {
            vec![(1u32, 10u64), (2u32, 20u64)]
        } else {
            vec![(1u32, 10u64)]
        };
        entries.define_all(items, "raced map writer");
        keep.keep()
    }
}

#[irrigation]
mod raced_map_reader {
    use super::RacedMap;

    #[component]
    pub fn read(tag: u64) -> u64 {
        let _ = tag;
        irrigation::ensure_view(<RacedMap as irrigation::ViewSchema>::DESCRIPTOR);
        let entries = RacedMap.field0();
        entries
            .map_values(|value| {
                super::map_race_hold();
                *value * 2
            })
            .iter()
            .map(|(_, value)| *value)
            .sum()
    }
}

#[test]
fn map_values_skips_a_member_retracted_after_the_structure_read() {
    let runtime = race_runtime();
    runtime
        .install(raced_map_writer::Write)
        .expect("install writer");
    runtime
        .edit(|edit| edit.view::<RacedKeep>().set_keep(2))
        .expect("seed keys");
    runtime
        .read_outputs::<raced_map_writer::Write>()
        .expect("seed map");
    runtime
        .edit(|edit| edit.view::<RacedKeep>().set_keep(1))
        .expect("invalidate the writer");

    let (reached, release) = arm_race(&MAP_RACE);
    let reader_runtime = runtime.clone();
    let reader = std::thread::spawn(move || {
        reader_runtime.call::<raced_map_reader::__ReadSpec>(raced_map_reader::__ReadArgs { tag: 0 })
    });
    reached
        .recv()
        .expect("the reading demand must observe the seeded key set");

    // The writer commits the retraction the reader has not observed yet; the
    // reader is parked after its structure read and before the second payload.
    runtime
        .read_outputs::<raced_map_writer::Write>()
        .expect("retract the second key");
    release.send(()).expect("release the reader");

    let settled = reader.join().expect("reader thread");
    assert_eq!(
        settled.expect("a retracted member must settle the demand, never abort it"),
        20,
        "the demand returns the fresh key set"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<RacedMap>().field0(),
        vec![(1, 10)],
        "the retraction is committed"
    );
}

#[irrigation]
mod raced_list_writer {
    use super::{RacedKeep, RacedList};

    /// Publishes the keyed list as one whole definition: the run that keeps one
    /// identity supersedes the other identity's cells.
    #[component]
    pub fn write(keep: &RacedKeep) -> u64 {
        let items = if keep.keep() >= 2 {
            vec![(1u64, 10u64), (2u64, 20u64)]
        } else {
            vec![(1u64, 10u64)]
        };
        RacedList.define_keyed(items);
        keep.keep()
    }
}

#[irrigation]
mod raced_list_reader {
    use super::RacedList;

    #[component]
    pub fn read(tag: u64) -> u64 {
        let _ = tag;
        irrigation::ensure_view(<RacedList as irrigation::ViewSchema>::DESCRIPTOR);
        RacedList
            .map(|value| {
                super::list_race_hold();
                *value * 2
            })
            .iter()
            .sum()
    }
}

#[test]
fn keyed_list_map_skips_a_member_retracted_after_the_structure_read() {
    let runtime = race_runtime();
    runtime
        .install(raced_list_writer::Write)
        .expect("install writer");
    runtime
        .edit(|edit| edit.view::<RacedKeep>().set_keep(2))
        .expect("seed identities");
    runtime
        .read_outputs::<raced_list_writer::Write>()
        .expect("seed list");
    runtime
        .edit(|edit| edit.view::<RacedKeep>().set_keep(1))
        .expect("invalidate the writer");

    let (reached, release) = arm_race(&LIST_RACE);
    let reader_runtime = runtime.clone();
    let reader = std::thread::spawn(move || {
        reader_runtime
            .call::<raced_list_reader::__ReadSpec>(raced_list_reader::__ReadArgs { tag: 0 })
    });
    reached
        .recv()
        .expect("the reading demand must observe the seeded identity set");

    runtime
        .read_outputs::<raced_list_writer::Write>()
        .expect("retract the second identity");
    release.send(()).expect("release the reader");

    let settled = reader.join().expect("reader thread");
    assert_eq!(
        settled.expect("a retracted identity must settle the demand, never abort it"),
        20,
        "the demand returns the fresh identity set"
    );
}

#[view(source)]
pub struct DraftedMap(Map<u32, u64>);

fn drafted_membership(view: irrigation::ViewId) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Membership,
        irrigation::any_key(2u32),
    )
}

#[irrigation]
mod drafted_member_reader {
    use super::DraftedMap;

    #[component]
    pub fn read(map: &DraftedMap) -> (bool, Option<u64>) {
        let entries = map.field0();
        (entries.contains_key(&2), entries.get(&2))
    }
}

#[test]
fn a_membership_only_draft_reads_as_absent_rather_than_aborting() {
    let runtime = Runtime::new();
    runtime.register_view(<DraftedMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| {
            edit.set(
                drafted_membership(<DraftedMap as irrigation::ViewSchema>::view_id()),
                true,
            );
        })
        .expect("seed the membership-only draft");
    runtime
        .install(drafted_member_reader::Read)
        .expect("install");

    assert_eq!(
        runtime
            .read_outputs::<drafted_member_reader::Read>()
            .expect("a drafted membership must not abort the demand"),
        (true, None),
        "presence is the membership cell's answer and the absent payload is the payload's"
    );
}
#[irrigation]
mod caller_read_then_edit {
    use super::{CallerList, CallerMode};

    /// The read and the write share a computation and the write's call site
    /// observes the tracked local the read produced.
    #[component]
    pub fn root(mode: &CallerMode) -> (Vec<u32>, Vec<u32>) {
        let base = CallerList.field0().to_vec();
        match mode.mode() {
            0 => CallerList.field0().define_all([1u32, 2], "list_build"),
            _ => CallerList
                .field0()
                .insert_at(0, base.len() as u32, "list_insert"),
        }
        (base, CallerList.field0().to_vec())
    }
}

#[test]
fn a_caller_read_then_edit_is_self_dependent() {
    let runtime = Runtime::new();
    runtime.register_view(<CallerList as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(caller_read_then_edit::Root)
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs::<caller_read_then_edit::Root>()
            .expect("build"),
        (vec![1, 2], vec![1, 2])
    );
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<CallerMode>().set_mode(1))
        .expect("edit");
    let error = runtime
        .read_outputs::<caller_read_then_edit::Root>()
        .expect_err("an edit computed from a read of its own target must fail");
    assert_self_dependent(&error);
    assert_no_retry(&runtime, retries);
    assert_eq!(
        runtime.inspect_snapshot().view::<CallerList>().field0(),
        vec![1u32, 2],
        "the edit must not be applied to a value the read did not observe"
    );
}

#[irrigation]
mod caller_read_before_map_insert {
    use super::{CallerMap, CallerMode};

    /// Reads the map as the value the insert stages, inside the same
    /// computation that installs the key.
    #[component]
    pub fn root(mode: &CallerMode) -> Vec<(u32, u64)> {
        CallerMap.field0().define(1, 10, "map_seed");
        if mode.mode() == 1 {
            CallerMap
                .field0()
                .insert(2, CallerMap.field0().to_vec().len() as u64, "map_insert");
        }
        CallerMap.field0().to_vec()
    }
}

#[test]
fn a_caller_read_before_a_map_insert_is_self_dependent() {
    let runtime = Runtime::new();
    runtime.register_view(<CallerMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(caller_read_before_map_insert::Root)
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs::<caller_read_before_map_insert::Root>()
            .expect("build"),
        vec![(1u32, 10u64)]
    );
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<CallerMode>().set_mode(1))
        .expect("edit");
    let error = runtime
        .read_outputs::<caller_read_before_map_insert::Root>()
        .expect_err("an insert staged from a read of the map it writes must fail");
    assert_self_dependent(&error);
    assert_no_retry(&runtime, retries);
    assert_eq!(
        runtime.inspect_snapshot().view::<CallerMap>().field0(),
        vec![(1u32, 10u64)],
        "the rejected group must not add a key"
    );
}

#[irrigation]
mod caller_read_before_map_update {
    use super::{CallerMap, CallerMode};

    /// The closure reads the map, so it is not idempotent: a retried attempt
    /// applies it to the value the first attempt published and diverges.
    #[component]
    pub fn root(mode: &CallerMode) -> Vec<(u32, u64)> {
        if mode.mode() == 0 {
            CallerMap.field0().define(1, 10, "map_seed");
        } else {
            CallerMap.field0().update_if_present(
                1,
                |value| value + CallerMap.field0().to_vec().len() as u64,
                "map_update",
            );
        }
        CallerMap.field0().to_vec()
    }
}

#[test]
fn a_caller_read_before_a_map_update_is_self_dependent() {
    let runtime = Runtime::new();
    runtime.register_view(<CallerMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(caller_read_before_map_update::Root)
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs::<caller_read_before_map_update::Root>()
            .expect("build"),
        vec![(1u32, 10u64)]
    );
    let retries = runtime.stats().demand_retries;
    runtime
        .edit(|edit| edit.view::<CallerMode>().set_mode(1))
        .expect("edit");
    let error = runtime
        .read_outputs::<caller_read_before_map_update::Root>()
        .expect_err("an update whose closure reads its own value must fail");
    assert_self_dependent(&error);
    assert_no_retry(&runtime, retries);
    assert_eq!(
        runtime.inspect_snapshot().view::<CallerMap>().field0(),
        vec![(1u32, 10u64)],
        "the rejected group must not apply the closure"
    );
}

// ---------------------------------------------------------------------------
// A commit this attempt did not make still invalidates its observation
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct SiblingMap(Map<u32, u64>);

#[irrigation]
mod stale_read_after_a_sibling_write {
    use super::SiblingMap;

    #[component]
    pub fn reader() -> Option<u64> {
        SiblingMap.field0().get(&1)
    }

    #[component]
    pub fn writer() -> u64 {
        SiblingMap.field0().define(1, 42, "map_define");
        0
    }

    #[component]
    pub fn root() -> Option<u64> {
        let first = reader();
        let _ = writer();
        first
    }
}

#[test]
fn an_observation_another_writer_moved_still_retries() {
    let runtime = Runtime::new();
    runtime.register_view(<SiblingMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(stale_read_after_a_sibling_write::Root)
        .expect("install");
    let before = runtime.stats().demand_retries;
    assert_eq!(
        runtime
            .read_outputs::<stale_read_after_a_sibling_write::Root>()
            .expect("the retried attempt sees the sibling's write"),
        Some(42),
        "the demand re-reads the value the sibling published"
    );
    assert!(
        runtime.stats().demand_retries > before,
        "another writer's commit must still force a retry"
    );
    assert_eq!(
        runtime
            .read()
            .expect("snapshot")
            .view::<SiblingMap>()
            .field0(),
        vec![(1u32, 42u64)]
    );
}

// ---------------------------------------------------------------------------
// Retraction mirrors installation: a removal must retract the payload it
// retracts, not just the membership cell
// (collections-r9-03)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct RetractedMap(Map<u32, u64>);

fn retracted_value(view: irrigation::ViewId) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(2u32),
    )
}

fn retracted_membership(view: irrigation::ViewId) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Membership,
        irrigation::any_key(2u32),
    )
}

#[irrigation]
mod retracted_entry {
    use super::{RetractedMap, retracted_value};
    use irrigation::ViewSchema;

    /// Installs the entry in one run and removes it in the next. Both runs are
    /// this component: a removal takes the unit over from the definition site
    /// the same component retired, while another live provider's unit is a
    /// conflict.
    #[component]
    pub fn write(mode: u64) -> u64 {
        irrigation::ensure_view(<RetractedMap as ViewSchema>::DESCRIPTOR);
        let entries = RetractedMap.field0();
        if mode == 0 {
            entries.define(2, 10, "retracted_map_define");
        } else if mode == 1 {
            entries.remove_if_present(2, "retracted_map_remove");
        }
        mode
    }

    /// Reads the key's membership, payload, value cell, and key set.
    #[component]
    pub fn read(tag: u64) -> (bool, Option<u64>, Option<u64>, Vec<u32>) {
        let _ = tag;
        let entries = RetractedMap.field0();
        (
            entries.contains_key(&2),
            entries.get(&2),
            irrigation::try_read_unit_value(
                retracted_value(<RetractedMap as ViewSchema>::view_id()),
                |_| None::<u64>,
            ),
            entries.keys(),
        )
    }
}

#[test]
fn a_removed_entry_does_not_resurrect_its_payload_through_a_membership_rearm() {
    let runtime = Runtime::new();
    runtime.register_view(<RetractedMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .call::<retracted_entry::__WriteSpec>(retracted_entry::__WriteArgs { mode: 0 })
        .expect("define the entry");
    assert_eq!(
        runtime.inspect_snapshot().view::<RetractedMap>().field0(),
        vec![(2u32, 10u64)]
    );

    runtime
        .call::<retracted_entry::__WriteSpec>(retracted_entry::__WriteArgs { mode: 1 })
        .expect("remove the entry");
    assert!(
        runtime
            .inspect_snapshot()
            .view::<RetractedMap>()
            .field0()
            .is_empty(),
        "the removal retracts the entry from the committed content"
    );
    assert_eq!(
        runtime
            .call::<retracted_entry::__ReadSpec>(retracted_entry::__ReadArgs { tag: 0 })
            .expect("read after the removal"),
        (false, None, None, Vec::new()),
        "a removed entry keeps neither its membership nor a certified payload"
    );

    // The module's documented read rule — a membership-only draft reads absent —
    // applied to the state a removal leaves behind: the draft gate seeds this
    // shape, but with no payload cell to resurrect.
    runtime
        .edit(|edit| {
            edit.set(
                retracted_membership(<RetractedMap as irrigation::ViewSchema>::view_id()),
                true,
            );
        })
        .expect("re-arm the membership cell");
    assert_eq!(
        runtime
            .call::<retracted_entry::__ReadSpec>(retracted_entry::__ReadArgs { tag: 1 })
            .expect("read after the re-arm"),
        (true, None, None, Vec::new()),
        "the re-armed membership must not resurrect the deleted payload"
    );
}

// ---------------------------------------------------------------------------
// Retraction mirrors installation through the editors too: a membership-only
// removal must not leave a certified payload behind (collections-r10-01)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct EditorMap(Map<u32, u64>);

#[view(source)]
pub struct EditorList(List<u32>);

#[view(source)]
pub struct EditorGraph(Graph<u64, u64>);

fn editor_list_membership(view: irrigation::ViewId, id: u64) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Membership,
        irrigation::any_key(id),
    )
}

fn editor_list_value(view: irrigation::ViewId, id: u64) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(id),
    )
}

fn editor_graph_value(view: irrigation::ViewId, node: u64) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(irrigation::NodeKey(node)),
    )
}

#[irrigation]
mod editor_removals {
    use super::{
        EditorGraph, EditorList, EditorMap, editor_graph_value, editor_list_membership,
        editor_list_value,
    };
    use irrigation::ViewSchema;

    #[component]
    pub fn map(key: u32) -> (bool, Option<u64>) {
        let entries = EditorMap.field0();
        (entries.contains_key(&key), entries.get(&key))
    }

    #[component]
    pub fn list(id: u64) -> (bool, Option<u32>) {
        let view = <EditorList as ViewSchema>::view_id();
        (
            irrigation::read_unit_value(editor_list_membership(view, id), |_| Some(false)),
            irrigation::try_read_unit_value(editor_list_value(view, id), |_| None::<u32>),
        )
    }

    #[component]
    pub fn graph(node: u64) -> (bool, Vec<u64>, Option<u64>) {
        let graph = EditorGraph.field0();
        (
            graph.contains_node(node),
            graph.nodes(),
            irrigation::try_read_unit_value(
                editor_graph_value(<EditorGraph as ViewSchema>::view_id(), node),
                |_| None::<u64>,
            ),
        )
    }
}

#[test]
fn an_editor_removal_retracts_the_payload_it_removes() {
    let runtime = Runtime::new();

    runtime.register_view(<EditorMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| edit.view::<EditorMap>().field0().insert(1, 10))
        .expect("map insert");
    runtime
        .edit(|edit| edit.view::<EditorMap>().field0().remove(&1))
        .expect("map remove");
    runtime
        .edit(|edit| {
            edit.set(
                irrigation::UnitKey::entity(
                    <EditorMap as irrigation::ViewSchema>::view_id(),
                    irrigation::FieldId(0),
                    irrigation::UnitKind::Membership,
                    irrigation::any_key(1u32),
                ),
                true,
            );
        })
        .expect("map membership re-arm");
    assert_eq!(
        runtime
            .call::<editor_removals::__MapSpec>(editor_removals::__MapArgs { key: 1 })
            .expect("map read"),
        (true, None),
        "a re-armed map membership must not resurrect the removed payload"
    );

    runtime.register_view(<EditorList as irrigation::ViewSchema>::DESCRIPTOR);
    let id = runtime
        .edit(|edit| edit.view::<EditorList>().field0().push(7))
        .expect("list insert");
    runtime
        .edit(|edit| edit.view::<EditorList>().field0().remove(0))
        .expect("list remove");
    runtime
        .edit(|edit| {
            edit.set(
                irrigation::UnitKey::entity(
                    <EditorList as irrigation::ViewSchema>::view_id(),
                    irrigation::FieldId(0),
                    irrigation::UnitKind::Membership,
                    irrigation::any_key(id),
                ),
                true,
            );
        })
        .expect("list membership re-arm");
    assert_eq!(
        runtime
            .call::<editor_removals::__ListSpec>(editor_removals::__ListArgs { id })
            .expect("list read"),
        (true, None),
        "a re-armed list membership must not resurrect the removed payload"
    );

    runtime.register_view(<EditorGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| edit.view::<EditorGraph>().field0().add_node(5, 50))
        .expect("graph insert");
    runtime
        .edit(|edit| edit.view::<EditorGraph>().field0().remove_node(5))
        .expect("graph remove");
    runtime
        .edit(|edit| {
            edit.set(
                irrigation::UnitKey::entity(
                    <EditorGraph as irrigation::ViewSchema>::view_id(),
                    irrigation::FieldId(0),
                    irrigation::UnitKind::Membership,
                    irrigation::any_key(irrigation::NodeKey(5)),
                ),
                true,
            );
        })
        .expect("graph membership re-arm");
    assert_eq!(
        runtime
            .call::<editor_removals::__GraphSpec>(editor_removals::__GraphArgs { node: 5 })
            .expect("graph read"),
        (true, Vec::new(), None),
        "a re-armed graph membership must not resurrect the removed payload"
    );
}

// ---------------------------------------------------------------------------
// A conditional verb re-decides when the key it skipped appears
// (collections-r9-01)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ConditionalMap(Map<u32, u64>);

#[irrigation]
mod conditional_update {
    use super::ConditionalMap;

    /// The conditional update is the only writer of the entry it touches, so
    /// the presence read is the site's whole decision.
    #[component]
    pub fn write() -> u64 {
        ConditionalMap
            .field0()
            .update_if_present(2, |value| value + 1, "map_update_if_present");
        0
    }

    /// Reads the map through a separate demand: the writer must not read the
    /// payload its own group installs.
    #[component]
    pub fn read(tag: u64) -> Vec<(u32, u64)> {
        let _ = tag;
        ConditionalMap.field0().to_vec()
    }
}

#[test]
fn a_conditional_map_update_re_decides_when_the_key_appears() {
    let runtime = Runtime::new();
    runtime.register_view(<ConditionalMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(conditional_update::Write)
        .expect("install writer");
    assert_eq!(
        runtime
            .read_outputs::<conditional_update::Write>()
            .expect("an absent key stages nothing"),
        0
    );
    assert!(
        runtime
            .inspect_snapshot()
            .view::<ConditionalMap>()
            .field0()
            .is_empty(),
        "the skipped update must not write"
    );
    assert_eq!(
        runtime
            .call::<conditional_update::__ReadSpec>(conditional_update::__ReadArgs { tag: 0 })
            .expect("read"),
        Vec::<(u32, u64)>::new()
    );

    // The key arrives from the host. The absence the verb read at the previous
    // revision is the dependency that re-evaluates its site.
    runtime
        .edit(|edit| {
            edit.view::<ConditionalMap>().field0().insert(2, 10);
        })
        .expect("seed the key");
    assert_eq!(
        runtime.inspect_snapshot().view::<ConditionalMap>().field0(),
        vec![(2u32, 10u64)]
    );

    assert_eq!(
        runtime
            .read_outputs::<conditional_update::Write>()
            .expect("the key is present now"),
        0
    );
    assert_eq!(
        runtime
            .call::<conditional_update::__ReadSpec>(conditional_update::__ReadArgs { tag: 1 })
            .expect("read"),
        vec![(2u32, 11u64)],
        "the closure runs exactly once, against the value the key arrived with"
    );
}

#[view(source)]
pub struct ConditionalRemoveMap(Map<u32, u64>);

/// What every read route of one map entry says: key set, entries, presence,
/// and the payload cell itself.
type EntryRead = (Vec<u32>, Vec<(u32, u64)>, bool, Option<u64>);

fn conditional_remove_value(view: irrigation::ViewId) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(2u32),
    )
}

#[irrigation]
mod conditional_remove {
    use super::{ConditionalRemoveMap, EntryRead, conditional_remove_value};
    use irrigation::ViewSchema;

    #[component]
    pub fn write() -> u64 {
        ConditionalRemoveMap
            .field0()
            .remove_if_present(2, "map_remove_if_present");
        0
    }

    #[component]
    pub fn read(tag: u64) -> Vec<(u32, u64)> {
        let _ = tag;
        ConditionalRemoveMap.field0().to_vec()
    }

    /// Reads every route to the entry: the key set, the entries, presence and
    /// the payload cell itself.
    #[component]
    pub fn read_state(tag: u64) -> EntryRead {
        let _ = tag;
        let entries = ConditionalRemoveMap.field0();
        (
            entries.keys(),
            entries.to_vec(),
            entries.contains_key(&2),
            irrigation::try_read_unit_value(
                conditional_remove_value(<ConditionalRemoveMap as ViewSchema>::view_id()),
                |_| None::<u64>,
            ),
        )
    }
}

#[test]
fn a_conditional_map_removal_re_decides_when_the_key_appears() {
    let runtime = Runtime::new();
    runtime.register_view(<ConditionalRemoveMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(conditional_remove::Write)
        .expect("install writer");
    assert_eq!(
        runtime
            .read_outputs::<conditional_remove::Write>()
            .expect("an absent key stages nothing"),
        0
    );

    runtime
        .edit(|edit| {
            edit.view::<ConditionalRemoveMap>().field0().insert(2, 10);
        })
        .expect("seed the key");
    assert_eq!(
        runtime
            .inspect_snapshot()
            .view::<ConditionalRemoveMap>()
            .field0(),
        vec![(2u32, 10u64)]
    );
    assert_eq!(
        runtime
            .call::<conditional_remove::__ReadStateSpec>(conditional_remove::__ReadStateArgs {
                tag: 0
            })
            .expect("read the seeded entry"),
        (vec![2], vec![(2, 10)], true, Some(10)),
        "the seeded entry is visible through every read route"
    );

    assert_eq!(
        runtime
            .read_outputs::<conditional_remove::Write>()
            .expect("the key is present now"),
        0
    );
    // The committed content is the contract's answer, and every reader of the
    // retracted entry — the key set, the entries, presence and the value cell —
    // must agree with it.
    assert!(
        runtime
            .inspect_snapshot()
            .view::<ConditionalRemoveMap>()
            .field0()
            .is_empty(),
        "the re-decided removal retracts the key that arrived"
    );
    assert_eq!(
        runtime
            .call::<conditional_remove::__ReadStateSpec>(conditional_remove::__ReadStateArgs {
                tag: 1
            })
            .expect("read the retracted entry"),
        (Vec::new(), Vec::new(), false, None),
        "the key set, the entries, presence and the value cell must agree the entry is gone"
    );
}

// ---------------------------------------------------------------------------
// A retracted graph payload reads as absent through the accessor
// (collections-r10-01)
// ---------------------------------------------------------------------------

#[irrigation]
mod graph_payload_reads {
    use super::EditorGraph;

    #[component]
    pub fn node(node: u64) -> (bool, Option<u64>) {
        let graph = EditorGraph.field0();
        (graph.contains_node(node), graph.node(node))
    }

    #[component]
    pub fn edge(edge: u64) -> (Option<(u64, u64, u64)>, Option<u64>) {
        let graph = EditorGraph.field0();
        (
            graph
                .edge(edge)
                .map(|payload| (payload.from, payload.to, payload.value)),
            graph.edge_weight(edge),
        )
    }
}

fn graph_node_membership(view: irrigation::ViewId, node: u64) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Membership,
        irrigation::any_key(irrigation::NodeKey(node)),
    )
}

fn graph_edge_membership(view: irrigation::ViewId, edge: u64) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Membership,
        irrigation::any_key(irrigation::EdgeKey(edge)),
    )
}

#[test]
fn a_retracted_graph_payload_reads_as_absent_through_the_accessor() {
    let runtime = Runtime::new();
    runtime.register_view(<EditorGraph as irrigation::ViewSchema>::DESCRIPTOR);
    let view = <EditorGraph as irrigation::ViewSchema>::view_id();

    // A node the membership still names after its payload cell was retracted.
    // The raw unit read in `an_editor_removal_retracts_the_payload_it_removes`
    // pins the kernel shape; this reads the verb a component actually calls.
    runtime
        .edit(|edit| edit.view::<EditorGraph>().field0().add_node(5, 50))
        .expect("node insert");
    runtime
        .edit(|edit| edit.set(graph_node_membership(view, 5), false))
        .expect("retract the node payload");
    runtime
        .edit(|edit| edit.set(graph_node_membership(view, 5), true))
        .expect("re-arm the node membership");
    assert_eq!(
        runtime
            .call::<graph_payload_reads::__NodeSpec>(graph_payload_reads::__NodeArgs { node: 5 })
            .expect("an absent payload is a member that is not there, not a failed demand"),
        (true, None),
        "the accessor reads the re-armed membership and the retracted payload as absent"
    );

    let edge = runtime
        .edit(|edit| {
            let mut view = edit.view::<EditorGraph>();
            let mut graph = view.field0();
            graph.add_node(1, 10);
            graph.add_node(2, 20);
            graph.add_edge(1, 2, 7)
        })
        .expect("edge insert");
    runtime
        .edit(|edit| edit.set(graph_edge_membership(view, edge), false))
        .expect("retract the edge payload");
    runtime
        .edit(|edit| edit.set(graph_edge_membership(view, edge), true))
        .expect("re-arm the edge membership");
    assert_eq!(
        runtime
            .call::<graph_payload_reads::__EdgeSpec>(graph_payload_reads::__EdgeArgs { edge })
            .expect("an absent edge payload is absent, not a failed demand"),
        (None, None),
        "edge() and edge_weight() answer None when the payload cell is retracted"
    );
}

// ---------------------------------------------------------------------------
// The chunked map records a dependency for an absent member
// (collections-r10-02)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ChunkedList(List<u64>);

#[irrigation]
mod chunked_list_map {
    use super::ChunkedList;

    #[component]
    pub fn read(tag: u64) -> Vec<u64> {
        let _ = tag;
        ChunkedList.field0().map(|value| *value * 2)
    }
}

fn chunked_list_value(view: irrigation::ViewId, id: u64) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(id),
    )
}

fn chunked_list_membership(view: irrigation::ViewId, id: u64) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Membership,
        irrigation::any_key(id),
    )
}

/// Eight identities in the structure, seven of them with a payload.
///
/// The structure cell is published directly rather than derived from the
/// content: a host edit that also republishes the structure would move the
/// site's structure observation and mask the missing per-member dependency.
fn chunked_list_with_an_absent_member(workers: usize) -> Runtime {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.register_view(<ChunkedList as irrigation::ViewSchema>::DESCRIPTOR);
    let view = <ChunkedList as irrigation::ViewSchema>::view_id();
    let ids: Vec<u64> = runtime
        .edit(|edit| {
            let mut view = edit.view::<ChunkedList>();
            let mut field = view.field0();
            (1..=7u64).map(|index| field.push(index * 10)).collect()
        })
        .expect("seed the defined elements");
    let absent = 1000;
    runtime
        .edit(|edit| {
            let mut structure = ids.clone();
            structure.push(absent);
            edit.set(
                irrigation::UnitKey::field(
                    view,
                    irrigation::FieldId(0),
                    irrigation::UnitKind::Structure,
                ),
                structure.into_iter().collect::<irrigation::PSeq<u64>>(),
            );
        })
        .expect("publish the identity list");
    runtime
        .edit(|edit| edit.set(chunked_list_membership(view, absent), true))
        .expect("declare the absent identity present");
    runtime
}

#[test]
fn a_chunked_list_map_records_a_dependency_for_an_absent_member() {
    let mut answers = Vec::new();
    for workers in [1usize, 2] {
        let runtime = chunked_list_with_an_absent_member(workers);
        let view = <ChunkedList as irrigation::ViewSchema>::view_id();
        let read = |runtime: &Runtime| {
            runtime
                .call::<chunked_list_map::__ReadSpec>(chunked_list_map::__ReadArgs { tag: 0 })
                .expect("a member whose payload is absent is skipped, never a failed demand")
        };
        assert_eq!(
            read(&runtime),
            vec![20, 40, 60, 80, 100, 120, 140],
            "the absent member is skipped, workers {workers}"
        );
        runtime
            .edit(|edit| edit.set(chunked_list_value(view, 1000), 80u64))
            .expect("install the absent payload");
        answers.push((workers, read(&runtime)));
    }
    assert_eq!(
        answers[0].1,
        vec![20, 40, 60, 80, 100, 120, 140, 160],
        "the sequential map re-executes when the absent payload lands"
    );
    assert_eq!(
        answers[1].1, answers[0].1,
        "the chunked map records the same per-member dependency as the sequential arm"
    );
}

// ---------------------------------------------------------------------------
// Re-installation is the inverse of retraction: the payload an editor stages
// after a removal is the entry's committed value (runtime-api-r10-01)
// ---------------------------------------------------------------------------

#[irrigation]
mod editor_reinstall {
    use super::EditorMap;

    #[component]
    pub fn entry(key: u32) -> (bool, Option<u64>, Vec<(u32, u64)>) {
        let entries = EditorMap.field0();
        (
            entries.contains_key(&key),
            entries.get(&key),
            entries.to_vec(),
        )
    }
}

#[test]
fn an_editor_reinstall_after_a_removal_serves_the_payload_it_stages() {
    let runtime = Runtime::new();
    runtime.register_view(<EditorMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| edit.view::<EditorMap>().field0().insert(1, 10))
        .expect("insert");
    assert_eq!(
        runtime
            .call::<editor_reinstall::__EntrySpec>(editor_reinstall::__EntryArgs { key: 1 })
            .expect("read"),
        (true, Some(10), vec![(1, 10)]),
        "the entry is installed before it is removed"
    );
    runtime
        .edit(|edit| edit.view::<EditorMap>().field0().remove(&1))
        .expect("remove");
    runtime
        .edit(|edit| edit.view::<EditorMap>().field0().insert(1, 10))
        .expect("re-install");

    let value = irrigation::UnitKey::entity(
        <EditorMap as irrigation::ViewSchema>::view_id(),
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(1u32),
    );
    let current = {
        let mut seen = None;
        runtime
            .edit(|edit| seen = edit.current::<u64>(&value))
            .expect("a transaction that stages nothing");
        seen
    };
    assert_eq!(
        current,
        Some(10),
        "a payload staged after a retraction is the cell's value again"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<EditorMap>().field0(),
        vec![(1, 10)],
        "the committed content says the entry is present"
    );
    assert_eq!(
        runtime
            .call::<editor_reinstall::__EntrySpec>(editor_reinstall::__EntryArgs { key: 1 })
            .expect("read after the re-install"),
        (true, Some(10), vec![(1, 10)]),
        "a re-installed payload is served, listed, and certified at one revision"
    );
}

// ---------------------------------------------------------------------------
// A content replacement's ports are the ones it moves, including the field
// structure the same commit recomputes (runtime-kernel-r10-01)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct StructureFeedback {
    entries: Map<u32, u32>,
}

fn structure_feedback_structure() -> irrigation::UnitKey {
    irrigation::UnitKey::field(
        <StructureFeedback as irrigation::ViewSchema>::view_id(),
        irrigation::FieldId(0),
        irrigation::UnitKind::Structure,
    )
}

/// Replaces the content of a View whose structure it read.
struct StructurePublisher;

impl irrigation::GeneralComponent for StructurePublisher {
    type State = ();
    type Output = u32;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CC_0104);
    const NAME: &'static str = "gates::StructurePublisher";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<StructureFeedback as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let keys: irrigation::PMap<u32, ()> = cx.read_typed(structure_feedback_structure())?;
        let (entries, _) = irrigation::PMap::new().insert(1u32, 10u32);
        let (entries, _) = entries.insert(2u32, 20u32);
        cx.set_content(
            <StructureFeedback as irrigation::ViewSchema>::view_id(),
            std::sync::Arc::new(StructureFeedbackContent {
                entries: entries.into(),
            }),
        );
        Ok(((), keys.len() as u32))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_general_root_that_replaces_the_content_whose_structure_it_read_is_self_dependent() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(StructurePublisher)
        .expect("install");
    let error = runtime
        .read_outputs_general::<StructurePublisher>()
        .expect_err("a body that replaces the content of the structure cell it read must fail");
    assert!(
        matches!(
            error,
            irrigation::ComputeError::PreconditionUnmet {
                reason: irrigation_core::error::PreconditionReason::SelfDependent,
                ..
            }
        ),
        "the field structure a replacement recomputes is one of its ports: {error:?}"
    );
    assert!(
        error.is_permanent(),
        "the rule is a program error, not a retryable attempt: {error:?}"
    );
    assert_eq!(
        runtime
            .inspect_snapshot()
            .view::<StructureFeedback>()
            .entries(),
        Vec::new(),
        "the rejected replacement must not publish"
    );
}

#[view(source)]
pub struct SettledContent {
    value: u32,
}

fn settled_content_unit() -> irrigation::UnitKey {
    irrigation::UnitKey::field(
        <SettledContent as irrigation::ViewSchema>::view_id(),
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
    )
}

/// Stages the content the View already holds, from the cell it read.
struct SettledPublisher;

impl irrigation::GeneralComponent for SettledPublisher {
    type State = ();
    type Output = u32;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CC_0105);
    const NAME: &'static str = "gates::SettledPublisher";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<SettledContent as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u32 = cx.read_typed(settled_content_unit())?;
        cx.set_content(
            <SettledContent as irrigation::ViewSchema>::view_id(),
            std::sync::Arc::new(SettledContentContent { value }),
        );
        Ok(((), value))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_general_root_that_stages_the_content_it_already_committed_publishes() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(SettledPublisher)
        .expect("install");
    runtime
        .edit(|edit| {
            edit.set_content(
                <SettledContent as irrigation::ViewSchema>::view_id(),
                std::sync::Arc::new(SettledContentContent { value: 4 }),
                irrigation::values_equal::<SettledContentContent>,
            );
        })
        .expect("seed the content the body restages");
    assert_eq!(
        runtime
            .read_outputs_general::<SettledPublisher>()
            .expect("an equal replacement installs nothing to be read-your-own-write"),
        4,
        "the body stages the content it read and publishes normally"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<SettledContent>().value(),
        4,
        "the unchanged content is still the committed one"
    );
}

// ---------------------------------------------------------------------------
// Every spelling of a retraction is the same removal (runtime-kernel-r10-04)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct RetractSpelling {
    entries: Map<u32, u32>,
}

#[view(source)]
pub struct RetractTrigger {
    step: u32,
}

fn retract_spelling_membership() -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        <RetractSpelling as irrigation::ViewSchema>::view_id(),
        irrigation::FieldId(0),
        irrigation::UnitKind::Membership,
        irrigation::any_key(1u32),
    )
}

fn retract_spelling_value() -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        <RetractSpelling as irrigation::ViewSchema>::view_id(),
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(1u32),
    )
}

/// Defines one entry, then retracts the entity through its presence cell.
struct PresenceRetractor;

impl irrigation::GeneralComponent for PresenceRetractor {
    type State = u32;
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CC_0106);
    const NAME: &'static str = "gates::PresenceRetractor";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[
        <RetractSpelling as irrigation::ViewSchema>::DESCRIPTOR,
        <RetractTrigger as irrigation::ViewSchema>::DESCRIPTOR,
    ];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let step: u32 = cx.read_typed(irrigation::UnitKey::field(
            <RetractTrigger as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        if step == 0 {
            cx.output().entry(
                <RetractSpelling as irrigation::ViewSchema>::view_id(),
                irrigation::FieldId(0),
                1u32,
                10u32,
            );
        } else {
            cx.output().retract(retract_spelling_membership());
        }
        Ok((step, ()))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation]
mod retracted_payload_reader {
    use super::{retract_spelling_membership, retract_spelling_value};

    #[component]
    pub fn read(tag: u64) -> (bool, Option<u32>) {
        let _ = tag;
        (
            irrigation::read_unit_value(retract_spelling_membership(), |_| Some(false)),
            irrigation::try_read_unit_value(retract_spelling_value(), |_| None::<u32>),
        )
    }
}

#[test]
fn a_presence_key_retraction_clears_the_payload_it_removes() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(PresenceRetractor)
        .expect("install the provider");
    runtime
        .read_outputs_general::<PresenceRetractor>()
        .expect("define the entry");
    assert_eq!(
        runtime
            .call::<retracted_payload_reader::__ReadSpec>(retracted_payload_reader::__ReadArgs {
                tag: 0
            })
            .expect("read"),
        (true, Some(10)),
        "the provider's entry is installed before the retraction"
    );

    runtime
        .edit(|edit| edit.view::<RetractTrigger>().set_step(1))
        .expect("trigger the retraction");
    runtime
        .read_outputs_general::<PresenceRetractor>()
        .expect("retract through the presence cell");
    assert!(
        runtime
            .inspect_snapshot()
            .view::<RetractSpelling>()
            .entries()
            .is_empty(),
        "the content no longer states the entity"
    );
    assert_eq!(
        runtime
            .call::<retracted_payload_reader::__ReadSpec>(retracted_payload_reader::__ReadArgs {
                tag: 0
            })
            .expect("read after the retraction"),
        (false, None),
        "one retraction clears both cells, whichever cell named the entity"
    );

    // A revision that restates neither cell must not resurrect the removed
    // payload from the definition record the retraction released.
    runtime
        .edit(|edit| edit.view::<RetractTrigger>().set_step(2))
        .expect("a revision that restates nothing");
    runtime
        .read_outputs_general::<PresenceRetractor>()
        .expect("retract an entity that is already gone");
    assert_eq!(
        runtime
            .call::<retracted_payload_reader::__ReadSpec>(retracted_payload_reader::__ReadArgs {
                tag: 0
            })
            .expect("read after a silent revision"),
        (false, None),
        "a value the provider did not restate must not come back"
    );
}

// ---------------------------------------------------------------------------
// A staged cell write cannot state a content delta it does not cover
// (runtime-api-r10-03)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct MixedDelta {
    first: u64,
    second: u64,
}

#[irrigation]
mod mixed_delta_reader {
    use super::MixedDelta;

    #[component]
    pub fn both() -> (u64, u64) {
        (MixedDelta.first(), MixedDelta.second())
    }
}

#[test]
fn a_stated_delta_that_does_not_cover_the_content_still_invalidates_the_cells_it_misses() {
    let runtime = Runtime::new();
    runtime
        .install(mixed_delta_reader::Both)
        .expect("install the reader");
    runtime
        .edit(|edit| edit.view::<MixedDelta>().set_first(1))
        .expect("seed the first cell");
    runtime
        .edit(|edit| edit.view::<MixedDelta>().set_second(2))
        .expect("seed the second cell");
    assert_eq!(
        runtime
            .read_outputs::<mixed_delta_reader::Both>()
            .expect("read"),
        (1, 2),
        "the reader materializes one cell per scalar field"
    );

    // The staged write covers the first field only; the replacement moves the
    // second. One stated cell cannot stand in for the other.
    runtime
        .edit(|edit| {
            edit.set(
                irrigation::UnitKey::field(
                    <MixedDelta as irrigation::ViewSchema>::view_id(),
                    irrigation::FieldId(0),
                    irrigation::UnitKind::Value,
                ),
                3u64,
            );
            edit.set_content(
                <MixedDelta as irrigation::ViewSchema>::view_id(),
                std::sync::Arc::new(MixedDeltaContent {
                    first: 3,
                    second: 9,
                }),
                irrigation::values_equal::<MixedDeltaContent>,
            );
        })
        .expect("a mixed delta");
    assert_eq!(
        runtime.inspect_snapshot().view::<MixedDelta>().second(),
        9,
        "the committed content is the replacement"
    );
    assert_eq!(
        runtime
            .read_outputs::<mixed_delta_reader::Both>()
            .expect("re-read"),
        (3, 9),
        "a cell the transaction did not write must follow the content it replaced"
    );
}

// ---------------------------------------------------------------------------
// A graph cell keeps one payload grammar on both sides of a replacement
// (collections-r12-01)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct EdgeCellGrammar(Graph<u32, u64>);

/// The value cell one graph edge's payload lives in.
fn graph_edge_value_unit(view: irrigation::ViewId, edge: u64) -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(irrigation::EdgeKey(edge)),
    )
}

#[irrigation]
mod edge_cell_grammar_reader {
    use super::EdgeCellGrammar;

    /// Reads through the public accessors, which seed their cells with the
    /// graph's cell grammar (`Option<EdgePayload<E>>`).
    #[component]
    pub fn read(edge: u64) -> (Option<irrigation::EdgePayload<u64>>, Option<u64>) {
        let graph = EdgeCellGrammar.field0();
        (graph.edge(edge), graph.edge_weight(edge))
    }
}

#[test]
fn a_whole_region_replacement_keeps_the_edge_cell_grammar() {
    let runtime = Runtime::new();
    runtime.register_view(<EdgeCellGrammar as irrigation::ViewSchema>::DESCRIPTOR);
    let view = <EdgeCellGrammar as irrigation::ViewSchema>::view_id();
    let edge = runtime
        .edit(|edit| {
            let mut editor = edit.view::<EdgeCellGrammar>();
            let mut graph = editor.field0();
            graph.add_node(1, 10);
            graph.add_node(2, 20);
            graph.add_edge(1, 2, 70)
        })
        .expect("seed");
    let read = |runtime: &Runtime| {
        runtime
            .call::<edge_cell_grammar_reader::__ReadSpec>(edge_cell_grammar_reader::__ReadArgs {
                edge,
            })
            .expect("read the edge")
    };
    assert_eq!(
        read(&runtime),
        (
            Some(irrigation::EdgePayload {
                from: 1,
                to: 2,
                value: 70,
            }),
            Some(70),
        ),
        "the accessor materializes the edge cell"
    );

    // A whole-region replacement with no unit write beside it. The committed
    // content is the new payload and the materialized cell is opened for the
    // runtime's own projection.
    runtime
        .edit(|edit| {
            let committed = edit.content::<EdgeCellGrammarContent>(view);
            let replaced = committed.field0.set_edge(edge, 99);
            edit.set_content(
                view,
                std::sync::Arc::new(EdgeCellGrammarContent { field0: replaced }),
                irrigation::values_equal::<EdgeCellGrammarContent>,
            );
        })
        .expect("replace the content");

    // `Edit::current` is the runtime's projection of a cell the replacement
    // left for the next read: it must answer the grammar the cell holds
    // (`Option<EdgePayload>`), not a bare record cut from the storage.
    assert_eq!(
        runtime
            .edit(|edit| edit.current::<Option<irrigation::EdgePayload<u64>>>(
                &graph_edge_value_unit(view, edge)
            ))
            .expect("project the cell"),
        Some(Some(irrigation::EdgePayload {
            from: 1,
            to: 2,
            value: 99,
        })),
        "the runtime's content projection speaks the cell grammar"
    );
    assert_eq!(
        read(&runtime),
        (
            Some(irrigation::EdgePayload {
                from: 1,
                to: 2,
                value: 99,
            }),
            Some(99),
        ),
        "edge() and edge_weight() serve the replaced payload"
    );
}

#[irrigation]
mod edge_cell_grammar_nodes {
    use super::EdgeCellGrammar;

    #[component]
    pub fn nodes(tag: u64) -> Vec<u64> {
        let _ = tag;
        EdgeCellGrammar.field0().nodes()
    }
}

#[test]
fn a_restated_edge_payload_publishes_no_revision() {
    let runtime = Runtime::new();
    runtime.register_view(<EdgeCellGrammar as irrigation::ViewSchema>::DESCRIPTOR);
    let view = <EdgeCellGrammar as irrigation::ViewSchema>::view_id();
    // Seed committed content alone, so the edge's value unit starts without a
    // cell and the normalization has to ask the content.
    let edge = runtime
        .edit(|edit| {
            let content = EdgeCellGrammarContent::empty();
            let (links, edge) = content
                .field0
                .add_node(1, 10)
                .add_node(2, 20)
                .add_edge(1, 2, 70);
            edit.set_content(
                view,
                std::sync::Arc::new(EdgeCellGrammarContent { field0: links }),
                irrigation::values_equal::<EdgeCellGrammarContent>,
            );
            edge
        })
        .expect("seed the content");
    let nodes = runtime
        .call::<edge_cell_grammar_nodes::__NodesSpec>(edge_cell_grammar_nodes::__NodesArgs {
            tag: 0,
        })
        .expect("read the topology");
    assert_eq!(
        nodes,
        vec![1, 2],
        "the reader materializes the structure cell the editor will also state"
    );

    // The editor restates the payload the content already holds: the staged
    // write equals the content's projection, so the transaction is a no-op.
    let revision = runtime.revision();
    runtime
        .edit(|edit| edit.view::<EdgeCellGrammar>().field0().set_edge(edge, 70))
        .expect("restate the payload");
    assert_eq!(
        runtime.revision(),
        revision,
        "a write that restates the committed edge payload publishes nothing"
    );
}

// ---------------------------------------------------------------------------
// A restated structure write is a no-op whether or not its cell exists
// (collections-r13-01)
// ---------------------------------------------------------------------------

#[test]
fn a_no_op_editor_drop_publishes_nothing_and_notifies_nothing() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runtime = Runtime::new();
    let view = <Model as irrigation::ViewSchema>::view_id();
    runtime.register_view(<Model as irrigation::ViewSchema>::DESCRIPTOR);
    // Seed committed content alone, so the list's structure unit starts
    // without a cell: the transaction's own writes are the only answer the
    // normalization has.
    let mut content = ModelContent::empty();
    content.numbers = content.numbers.push((1u64, 10u64));
    runtime
        .edit(|edit| {
            edit.set_content(
                view,
                Arc::new(content.clone()),
                irrigation::values_equal::<ModelContent>,
            );
        })
        .expect("seed the content");

    let delivered = Arc::new(AtomicUsize::new(0));
    let notified = Arc::clone(&delivered);
    let _subscription = runtime
        .observe_units::<Model>(
            vec![irrigation::structure_unit(view, irrigation::FieldId(0))],
            move |_| {
                notified.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect("subscribe to the structure");

    // Acquiring the list editor and dropping it stages the list's structure:
    // the projection the committed content already holds.
    let revision = runtime.revision();
    runtime
        .edit(|edit| {
            let mut editor = edit.view::<Model>();
            drop(editor.numbers());
        })
        .expect("a transaction that stages nothing");
    assert_eq!(
        delivered.load(Ordering::SeqCst),
        0,
        "a write that restates the committed structure notifies no subscriber"
    );
    assert_eq!(
        runtime.revision(),
        revision,
        "a write that restates the committed structure publishes no revision"
    );

    // A structure the editor really moves still publishes and still notifies.
    runtime
        .edit(|edit| edit.view::<Model>().numbers().push(20))
        .expect("push");
    assert_eq!(
        runtime.revision(),
        revision + 1,
        "a changed structure publishes its revision"
    );
    assert_eq!(
        delivered.load(Ordering::SeqCst),
        1,
        "a changed structure notifies the subscriber"
    );
}

#[irrigation]
mod edge_cell_grammar_provider {
    use super::{EdgeCellGrammar, graph_edge_value_unit};

    /// A provider owns one edge's payload cell in a source View.
    #[component]
    pub fn root(_graph: &EdgeCellGrammar) -> u64 {
        irrigation::ensure_view(<EdgeCellGrammar as irrigation::ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(
            graph_edge_value_unit(<EdgeCellGrammar as irrigation::ViewSchema>::view_id(), 0),
            Some(irrigation::EdgePayload {
                from: 1u64,
                to: 2u64,
                value: 70u64,
            }),
            "graph_edge_provider",
        );
        0
    }
}

#[test]
fn a_replacement_cannot_overwrite_a_provider_owned_edge_payload() {
    let runtime = Runtime::new();
    runtime
        .install(edge_cell_grammar_provider::Root)
        .expect("install the provider");
    assert_eq!(
        runtime
            .read_outputs::<edge_cell_grammar_provider::Root>()
            .expect("read the provider"),
        0
    );
    let view = <EdgeCellGrammar as irrigation::ViewSchema>::view_id();
    let refused = runtime
        .edit(|edit| {
            let committed = edit.content::<EdgeCellGrammarContent>(view);
            let replaced = committed.field0.set_edge(0, 99);
            edit.set_content(
                view,
                std::sync::Arc::new(EdgeCellGrammarContent { field0: replaced }),
                irrigation::values_equal::<EdgeCellGrammarContent>,
            );
        })
        .expect_err("a replacement that overwrites the provider's edge payload must be refused");
    match refused {
        irrigation::ComputeError::DefinitionDenied { reason, .. } => assert!(
            reason.contains("owns"),
            "the refusal must name the owned cell: {reason}"
        ),
        other => panic!("expected a definition denial, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A presence draft never carries a graph node's payload (collections-r12-02)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct PresenceMode {
    mode: u64,
}

#[view(derived)]
pub struct PresenceGraph(Graph<bool, u64>);

struct PresencePublisher;

impl irrigation::GeneralComponent for PresencePublisher {
    type State = u64;
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00BB_0001);
    const NAME: &'static str = "gates::PresencePublisher";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<PresenceMode as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<PresenceGraph as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let mode: u64 = cx.read_typed(irrigation::UnitKey::field(
            <PresenceMode as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        let view = <PresenceGraph as irrigation::ViewSchema>::view_id();
        let mut output = cx.output();
        // The payload draft installs the node; the presence draft beside it
        // states that the node is there and nothing about its value.
        output.entry(view, irrigation::FieldId(0), irrigation::NodeKey(1), false);
        output.membership(view, irrigation::FieldId(0), irrigation::NodeKey(1), true);
        Ok((mode, ()))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation]
mod presence_graph_reader {
    use super::PresenceGraph;

    #[component]
    pub fn root() -> (bool, Option<bool>, Vec<u64>) {
        let graph = PresenceGraph.field0();
        (graph.contains_node(1), graph.node(1), graph.nodes())
    }
}

#[test]
fn a_presence_draft_does_not_overwrite_a_boolean_node_payload() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(PresencePublisher)
        .expect("producer");
    runtime
        .install(presence_graph_reader::Root)
        .expect("install the reader");

    // A revision must separate the install from the first demand: at revision
    // zero a node is certified by construction.
    runtime
        .edit(|edit| edit.view::<PresenceMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<presence_graph_reader::Root>()
            .expect("read"),
        (true, Some(false), vec![1]),
        "a membership draft states presence, never the node's bool payload"
    );
    assert_eq!(
        runtime.snapshot().view::<PresenceGraph>().field0_node(1),
        Some(false),
        "the committed content keeps the payload its definition installed"
    );
}

// ---------------------------------------------------------------------------
// The forwarded scalar `define` registers its View (collections-r13-02)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ForwardMode {
    mode: u64,
}

#[view(derived)]
pub struct ForwardedCount(u64);

#[irrigation]
mod forwarded_scalar_definition {
    use super::{ForwardMode, ForwardedCount};

    #[component]
    pub fn producer(_mode: &ForwardMode) -> u64 {
        let count = ForwardedCount;
        count.define(7);
        1
    }

    #[component]
    pub fn root(mode: &ForwardMode) -> (u64, u64) {
        let marker = producer(mode);
        let count = ForwardedCount;
        (marker, count.field0())
    }
}

#[test]
fn the_forwarded_scalar_define_registers_its_view() {
    assert!(
        !<forwarded_scalar_definition::Root as irrigation::RootComponent>::VIEWS
            .iter()
            .any(
                |descriptor| descriptor.id == <ForwardedCount as irrigation::ViewSchema>::view_id()
            ),
        "the gate must not pre-register the View its producer defines"
    );
    let runtime = Runtime::new();
    runtime
        .install(forwarded_scalar_definition::Root)
        .expect("install");
    runtime
        .edit(|edit| edit.view::<ForwardMode>().set_mode(0))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<forwarded_scalar_definition::Root>()
            .expect("read"),
        (1, 7),
        "a producer that never reads its own View must be able to define it"
    );
}

#[test]
fn the_forwarded_scalar_define_declares_its_view() {
    let runtime = Runtime::new();
    runtime.register_view(<ForwardedCount as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install(forwarded_scalar_definition::Root)
        .expect("install");
    runtime
        .edit(|edit| edit.view::<ForwardMode>().set_mode(0))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<forwarded_scalar_definition::Root>()
            .expect("read"),
        (1, 7),
        "a registered View must still be declared by the scope that defines it"
    );
}

// ---------------------------------------------------------------------------
// Presence has one answer: the cell, or the committed content
// (collections-r14-01)
// ---------------------------------------------------------------------------

#[irrigation]
mod content_seeded_map_verbs {
    use super::Table;

    #[component]
    pub fn insert(table: &Table) -> u64 {
        table.entries().insert(1, 99, "content_seeded_map_insert");
        1
    }

    #[component]
    pub fn update(table: &Table) -> u64 {
        table.entries().update(1, 20, "content_seeded_map_update");
        1
    }
}

#[irrigation]
mod content_seeded_graph_verbs {
    use super::Network;

    #[component]
    pub fn insert_node(network: &Network) -> u64 {
        network
            .links()
            .insert_node(1, 99, "content_seeded_node_insert");
        1
    }

    #[component]
    pub fn update_node(network: &Network) -> u64 {
        network
            .links()
            .update_node(1, 20, "content_seeded_node_update");
        1
    }
}

/// Commits a source View's content without materializing any of its cells.
fn seed_table_content(runtime: &Runtime, entries: &[(u32, u64)]) {
    runtime.register_view(<Table as irrigation::ViewSchema>::DESCRIPTOR);
    let mut content = TableContent::empty();
    for (key, value) in entries {
        content.entries = content.entries.insert(*key, *value).0;
    }
    runtime
        .edit(|edit| {
            edit.set_content(
                <Table as irrigation::ViewSchema>::view_id(),
                std::sync::Arc::new(content),
                irrigation::values_equal::<TableContent>,
            );
        })
        .expect("seed the content");
}

/// Commits a source graph's content without materializing any of its cells.
fn seed_network_content(runtime: &Runtime) {
    runtime.register_view(<Network as irrigation::ViewSchema>::DESCRIPTOR);
    let mut content = NetworkContent::empty();
    content.links = content.links.add_node(1, 10);
    runtime
        .edit(|edit| {
            edit.set_content(
                <Network as irrigation::ViewSchema>::view_id(),
                std::sync::Arc::new(content),
                irrigation::values_equal::<NetworkContent>,
            );
        })
        .expect("seed the content");
}

#[test]
fn a_content_seeded_key_fails_a_checked_map_insert() {
    let runtime = Runtime::new();
    seed_table_content(&runtime, &[(1, 10)]);
    runtime
        .install(content_seeded_map_verbs::Insert)
        .expect("install the provider");
    let revision = runtime.revision();
    let error = runtime
        .read_outputs::<content_seeded_map_verbs::Insert>()
        .expect_err("the key exists in the committed content");
    assert!(
        matches!(
            error,
            irrigation::ComputeError::PreconditionUnmet {
                reason: irrigation_core::error::PreconditionReason::NoProducer,
                ..
            }
        ),
        "a checked insert of a present key must fail its absence requirement: {error:?}"
    );
    assert_eq!(
        runtime.revision(),
        revision,
        "the rejected insert publishes nothing"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Table>().entries(),
        vec![(1u32, 10u64)],
        "the rejected insert must not overwrite the committed entry"
    );
}

#[test]
fn a_content_seeded_absent_key_installs_a_checked_map_insert() {
    let runtime = Runtime::new();
    seed_table_content(&runtime, &[(2, 20)]);
    runtime
        .install(content_seeded_map_verbs::Insert)
        .expect("install the provider");
    assert_eq!(
        runtime
            .read_outputs::<content_seeded_map_verbs::Insert>()
            .expect("the key is absent from the committed content"),
        1
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Table>().entries(),
        vec![(1u32, 99u64), (2u32, 20u64)],
        "a checked insert of an absent content-only key publishes in one pass"
    );
}

#[test]
fn a_content_seeded_key_installs_a_checked_map_update() {
    let runtime = Runtime::new();
    seed_table_content(&runtime, &[(1, 10)]);
    runtime
        .install(content_seeded_map_verbs::Update)
        .expect("install the provider");
    assert_eq!(
        runtime
            .read_outputs::<content_seeded_map_verbs::Update>()
            .expect("the key is present in the committed content"),
        1
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Table>().entries(),
        vec![(1u32, 20u64)],
        "a checked update of a content-only key installs its payload"
    );
}

#[test]
fn a_content_seeded_node_fails_a_checked_graph_insert() {
    let runtime = Runtime::new();
    seed_network_content(&runtime);
    runtime
        .install(content_seeded_graph_verbs::InsertNode)
        .expect("install the provider");
    let revision = runtime.revision();
    let error = runtime
        .read_outputs::<content_seeded_graph_verbs::InsertNode>()
        .expect_err("the node exists in the committed content");
    assert!(
        matches!(
            error,
            irrigation::ComputeError::PreconditionUnmet {
                reason: irrigation_core::error::PreconditionReason::NoProducer,
                ..
            }
        ),
        "a checked node insert of a present node must fail its absence requirement: {error:?}"
    );
    assert_eq!(
        runtime.revision(),
        revision,
        "the rejected insert publishes nothing"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Network>().links_node(1),
        Some(10u32),
        "the rejected insert must not overwrite the committed payload"
    );
}

#[test]
fn a_content_seeded_node_installs_a_checked_graph_update() {
    let runtime = Runtime::new();
    seed_network_content(&runtime);
    runtime
        .install(content_seeded_graph_verbs::UpdateNode)
        .expect("install the provider");
    assert_eq!(
        runtime
            .read_outputs::<content_seeded_graph_verbs::UpdateNode>()
            .expect("the node is present in the committed content"),
        1
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Network>().links_node(1),
        Some(20u32),
        "a checked node update of a content-only node installs its payload"
    );
}

// ---------------------------------------------------------------------------
// One unit has one payload: a structure unit is the family's projection
// (macros-r14-01, runtime-kernel-r14-01)
// ---------------------------------------------------------------------------

#[test]
fn a_field_structure_unit_answers_its_projection_not_the_storage() {
    use std::any::Any;
    use std::sync::Arc;

    let descriptor = <Model as irrigation::ViewSchema>::DESCRIPTOR;
    let mut content = ModelContent::empty();
    content.numbers = content.numbers.push((7u64, 10u64));
    let content: Arc<dyn Any + Send + Sync> = Arc::new(content);
    let field = irrigation::FieldId(0);
    let view = <Model as irrigation::ViewSchema>::view_id();
    let unit = irrigation::structure_unit(view, field);

    let projected: Vec<u64> = (descriptor.content.structure)(&content, field)
        .expect("a list field projects its identity sequence")
        .value
        .downcast_ref::<irrigation::PSeq<u64>>()
        .expect("the projection is the identity sequence")
        .iter()
        .copied()
        .collect();
    assert_eq!(projected, vec![7u64]);

    let answered = (descriptor.content.get)(&content, &unit)
        .expect("the content answers the field-level structure unit");
    assert_eq!(
        answered
            .downcast_ref::<irrigation::PSeq<u64>>()
            .expect("the structure unit's payload is the family's projection")
            .iter()
            .copied()
            .collect::<Vec<u64>>(),
        projected,
        "the structure unit answers the projection, not the storage"
    );
    assert!(
        answered
            .downcast_ref::<irrigation::PSeq<(u64, u64)>>()
            .is_none(),
        "the unit getter must not answer the whole field storage"
    );

    // The storage is the payload a whole-field definition installs, and the
    // retraction that restores it reads this op.
    let storage = (descriptor.content.field_storage)(&content, field)
        .expect("the field has a whole storage")
        .downcast_ref::<irrigation::PSeq<(u64, u64)>>()
        .cloned()
        .expect("the list storage is its sequence");
    assert_eq!(storage.get(0), Some(&(7u64, 10u64)));
}

#[view(source)]
pub struct WholeValueFlag {
    on: bool,
}

#[view(derived)]
pub struct WholeValueList(List<u64>);

#[irrigation]
mod whole_value_retraction {
    use super::{WholeValueFlag, WholeValueList};

    #[component]
    pub fn write(flag: &WholeValueFlag) -> u64 {
        if flag.on() {
            let _ = WholeValueList::from([10u64, 20]);
        }
        0
    }

    #[component]
    pub fn read(tag: u64) -> Vec<u64> {
        let _ = tag;
        WholeValueList.to_vec()
    }
}

#[test]
fn a_retracted_whole_value_definition_keeps_the_field_storage_payload() {
    let runtime = Runtime::new();
    runtime
        .install(whole_value_retraction::Write)
        .expect("install the provider");
    // The provider defines the View on its first run, so a reader must not run
    // before that run; registering it here keeps the View content-typed for the
    // retraction the gate is about.
    runtime.register_view(<WholeValueList as irrigation::ViewSchema>::DESCRIPTOR);
    let read = |runtime: &Runtime| {
        runtime
            .call::<whole_value_retraction::__ReadSpec>(whole_value_retraction::__ReadArgs {
                tag: 0,
            })
            .expect("read the whole list")
    };
    runtime
        .edit(|edit| edit.view::<WholeValueFlag>().set_on(true))
        .expect("define the whole list");
    runtime
        .read_outputs::<whole_value_retraction::Write>()
        .expect("run the provider");
    assert_eq!(read(&runtime), vec![10u64, 20]);

    // The retraction restores the field's empty *storage*, not its identity
    // projection: the whole-field cell holds what the definition installed.
    runtime
        .edit(|edit| edit.view::<WholeValueFlag>().set_on(false))
        .expect("retract the whole list");
    runtime
        .read_outputs::<whole_value_retraction::Write>()
        .expect("run the retraction");
    assert_eq!(
        read(&runtime),
        Vec::<u64>::new(),
        "a retracted whole-field definition reverts to the empty storage"
    );

    runtime
        .edit(|edit| edit.view::<WholeValueFlag>().set_on(true))
        .expect("redefine the whole list");
    runtime
        .read_outputs::<whole_value_retraction::Write>()
        .expect("run the redefinition");
    assert_eq!(read(&runtime), vec![10u64, 20]);
}

// ---------------------------------------------------------------------------
// A restated structure write publishes nothing without a content write
// (runtime-kernel-r14-01)
// ---------------------------------------------------------------------------

#[test]
fn a_restated_structure_write_alone_publishes_nothing_and_notifies_nothing() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runtime = Runtime::new();
    let view = <Model as irrigation::ViewSchema>::view_id();
    let field = irrigation::FieldId(0);
    runtime.register_view(<Model as irrigation::ViewSchema>::DESCRIPTOR);
    let mut content = ModelContent::empty();
    content.numbers = content.numbers.push((1u64, 10u64));
    runtime
        .edit(|edit| {
            edit.set_content(
                view,
                Arc::new(content),
                irrigation::values_equal::<ModelContent>,
            );
        })
        .expect("seed the content");

    let delivered = Arc::new(AtomicUsize::new(0));
    let notified = Arc::clone(&delivered);
    let _subscription = runtime
        .observe_units::<Model>(vec![irrigation::structure_unit(view, field)], move |_| {
            notified.fetch_add(1, Ordering::SeqCst);
        })
        .expect("subscribe to the structure");

    // Only the structure unit is staged, so the transaction's whole decision is
    // that unit's: no content write arrives beside it to short-circuit it.
    let projection: irrigation::PSeq<u64> = [1u64].into_iter().collect();
    let revision = runtime.revision();
    runtime
        .edit(|edit| edit.set(irrigation::structure_unit(view, field), projection))
        .expect("restate the committed projection");
    assert_eq!(
        runtime.revision(),
        revision,
        "a write that restates the committed structure publishes no revision"
    );
    assert_eq!(
        delivered.load(Ordering::SeqCst),
        0,
        "a write that restates the committed structure notifies no subscriber"
    );

    // A projection the committed content does not hold is a real change.
    let moved: irrigation::PSeq<u64> = [1u64, 2].into_iter().collect();
    runtime
        .edit(|edit| edit.set(irrigation::structure_unit(view, field), moved))
        .expect("move the projection");
    assert_eq!(
        runtime.revision(),
        revision + 1,
        "a moved structure publishes its revision"
    );
    assert_eq!(
        delivered.load(Ordering::SeqCst),
        1,
        "a moved structure notifies the subscriber"
    );
}

// ---------------------------------------------------------------------------
// A removal owns the payload staged beside it (runtime-api-r14-02)
// ---------------------------------------------------------------------------

/// The payload one entity's value cell currently holds, as the runtime's own
/// dump reports it: `Some` when the cell carries a value, `None` when it does
/// not — including a cell that exists but was retired.
fn staged_removal_payload(
    runtime: &Runtime,
    view: irrigation::ViewId,
    entity: &str,
) -> Option<String> {
    let debug_view = format!("{view:?}");
    runtime
        .dump()
        .into_iter()
        .find(|node| {
            node.kind == "Unit"
                && node.unit.as_deref().is_some_and(|unit| {
                    unit.contains(&debug_view)
                        && unit.contains("kind: Value")
                        && unit.contains(entity)
                })
        })
        .and_then(|node| node.value)
}

#[test]
fn a_removal_owns_the_payload_staged_beside_it() {
    let view = <EditorMap as irrigation::ViewSchema>::view_id();
    let entity = "entity: Some(\"1\")";

    // An entity that never existed: the removal is the transaction's final
    // state, so the payload staged beside it is not published at all.
    let runtime = Runtime::new();
    runtime.register_view(<EditorMap as irrigation::ViewSchema>::DESCRIPTOR);
    let revision = runtime.revision();
    runtime
        .edit(|edit| {
            let mut view = edit.view::<EditorMap>();
            view.field0().insert(1, 10);
            view.field0().remove(&1);
        })
        .expect("stage the insert and the removal");
    assert_eq!(
        runtime.revision(),
        revision,
        "a removal of what it just inserted publishes nothing"
    );
    assert_eq!(
        staged_removal_payload(&runtime, view, entity),
        None,
        "the dropped payload is not a value the runtime serves"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<EditorMap>().field0(),
        Vec::new()
    );

    // An entity that existed: the removal publishes, and the payload staged
    // beside it belongs to the absence.
    runtime
        .edit(|edit| edit.view::<EditorMap>().field0().insert(1, 10))
        .expect("install the entity");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<EditorMap>();
            view.field0().insert(1, 20);
            view.field0().remove(&1);
        })
        .expect("stage the replacement and the removal");
    assert_eq!(
        staged_removal_payload(&runtime, view, entity),
        None,
        "a removed entity keeps no certified payload"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<EditorMap>().field0(),
        Vec::new()
    );

    // The payload and the removal in either staging order: presence owns the
    // entity's cells, and a membership re-arm must not resurrect the value.
    for payload_first in [true, false] {
        let runtime = Runtime::new();
        runtime.register_view(<EditorMap as irrigation::ViewSchema>::DESCRIPTOR);
        runtime
            .edit(|edit| edit.view::<EditorMap>().field0().insert(1, 10))
            .expect("install the entity");
        let value = irrigation::UnitKey::entity(
            view,
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
            irrigation::any_key(1u32),
        );
        let membership = irrigation::UnitKey::entity(
            view,
            irrigation::FieldId(0),
            irrigation::UnitKind::Membership,
            irrigation::any_key(1u32),
        );
        runtime
            .edit(|edit| {
                // One order stages a value that would publish, the other a value
                // equal to the committed one: neither survives the removal.
                let payload = if payload_first { 20u64 } else { 10u64 };
                if payload_first {
                    edit.set(value.clone(), payload);
                    edit.set(membership.clone(), false);
                } else {
                    edit.set(membership.clone(), false);
                    edit.set(value.clone(), payload);
                }
            })
            .expect("stage the payload and the removal");
        assert_eq!(
            staged_removal_payload(&runtime, view, entity),
            None,
            "the payload never survives the removal (payload_first={payload_first})"
        );
        runtime
            .edit(|edit| edit.set(membership, true))
            .expect("re-arm the membership");
        assert_eq!(
            staged_removal_payload(&runtime, view, entity),
            None,
            "a re-armed membership must not resurrect the dropped payload \
             (payload_first={payload_first})"
        );
    }
}

// ---------------------------------------------------------------------------
// A false `Membership` define is the entity's retraction, decided once per
// write group (runtime-kernel-r15-01, collections-r15-01)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct WithdrawMode {
    mode: u64,
}

#[view(derived)]
pub struct WithdrawMap(Map<u64, u64>);

fn withdraw_value() -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        <WithdrawMap as irrigation::ViewSchema>::view_id(),
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(1u64),
    )
}

/// Stages the spellings a provider has for one entity's presence.
///
/// The steps are the install, the presence-only retraction, the two staging
/// orders of a payload beside a false presence, and the presence-only re-arm
/// they are compared against.
struct WithdrawPublisher;

impl irrigation::GeneralComponent for WithdrawPublisher {
    type State = u64;
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CC_0150);
    const NAME: &'static str = "gates::WithdrawPublisher";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<WithdrawMode as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let step: u64 = cx.read_typed(irrigation::UnitKey::field(
            <WithdrawMode as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        let view = <WithdrawMap as irrigation::ViewSchema>::view_id();
        let field = irrigation::FieldId(0);
        let mut output = cx.output();
        match step {
            0 => output.entry(view, field, 1u64, 10u64),
            1 => output.membership(view, field, 1u64, false),
            2 => {
                output.entry(view, field, 1u64, 10u64);
                output.membership(view, field, 1u64, false);
            }
            3 => {
                output.membership(view, field, 1u64, false);
                output.entry(view, field, 1u64, 10u64);
            }
            4 => output.membership(view, field, 1u64, true),
            _ => {}
        }
        Ok((step, ()))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation]
mod withdrawn_entry {
    use super::WithdrawMap;

    #[component]
    pub fn read(tag: u64) -> bool {
        let _ = tag;
        WithdrawMap.field0().contains_key(&1)
    }
}

/// Whether the entity's payload cell is certified, read from the read-only dump.
fn withdrawn_payload(runtime: &Runtime) -> Option<String> {
    let key = format!("{:?}", withdraw_value());
    runtime
        .dump()
        .into_iter()
        .find(|node| node.unit.as_deref() == Some(key.as_str()))
        .and_then(|node| node.value)
}

fn read_withdrawn(runtime: &Runtime) -> bool {
    runtime
        .call::<withdrawn_entry::__ReadSpec>(withdrawn_entry::__ReadArgs { tag: 0 })
        .expect("read")
}

#[test]
fn a_provider_presence_define_retracts_the_payload_it_removes() {
    let runtime = Runtime::new();
    runtime.register_view(<WithdrawMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_root(WithdrawPublisher)
        .expect("install the provider");

    // A revision must separate the install from the first demand: at revision
    // zero a node is certified by construction.
    runtime
        .edit(|edit| edit.view::<WithdrawMode>().set_mode(0))
        .expect("mode");
    runtime
        .read_outputs_general::<WithdrawPublisher>()
        .expect("define the entry");
    assert!(
        read_withdrawn(&runtime),
        "the provider's entry is installed before it is retracted"
    );
    assert!(
        withdrawn_payload(&runtime).is_some(),
        "the installed entity has a certified payload cell"
    );

    runtime
        .edit(|edit| edit.view::<WithdrawMode>().set_mode(1))
        .expect("mode");
    runtime
        .read_outputs_general::<WithdrawPublisher>()
        .expect("retract through the presence draft");
    assert!(
        !read_withdrawn(&runtime),
        "a `Membership = false` definition is the entity's retraction"
    );
    assert_eq!(
        withdrawn_payload(&runtime),
        None,
        "absence implies the payload cell is absent"
    );

    // A delta re-arm states presence only. It must not resurrect the value the
    // retraction cleared.
    runtime
        .edit(|edit| edit.view::<WithdrawMode>().set_mode(4))
        .expect("mode");
    runtime
        .read_outputs_general::<WithdrawPublisher>()
        .expect("re-arm the presence");
    assert!(
        read_withdrawn(&runtime),
        "a re-armed presence must not serve the removed payload"
    );
    assert_eq!(withdrawn_payload(&runtime), None);

    // A revision that restates nothing at all keeps the same absence.
    runtime
        .edit(|edit| edit.view::<WithdrawMode>().set_mode(5))
        .expect("mode");
    runtime
        .read_outputs_general::<WithdrawPublisher>()
        .expect("restate nothing");
    assert!(
        read_withdrawn(&runtime),
        "a payload the provider did not restate must not come back"
    );
    assert_eq!(withdrawn_payload(&runtime), None);
}

#[test]
fn a_payload_staged_beside_a_false_presence_is_decided_by_the_presence() {
    for step in [2u64, 3] {
        let runtime = Runtime::new();
        runtime.register_view(<WithdrawMap as irrigation::ViewSchema>::DESCRIPTOR);
        runtime
            .install_general_root(WithdrawPublisher)
            .expect("install the provider");
        runtime
            .edit(|edit| edit.view::<WithdrawMode>().set_mode(step))
            .expect("mode");
        runtime
            .read_outputs_general::<WithdrawPublisher>()
            .expect("commit the group");
        assert!(
            !read_withdrawn(&runtime),
            "one write group, one absence (step {step})"
        );
        assert_eq!(
            withdrawn_payload(&runtime),
            None,
            "the payload never survives the group's absence (step {step})"
        );
        assert_eq!(
            runtime.inspect_snapshot().view::<WithdrawMap>().field0(),
            Vec::new(),
            "the group's own presence marks the entry absent (step {step})"
        );
        // The presence-only re-arm must not resurrect the dropped payload.
        runtime
            .edit(|edit| edit.view::<WithdrawMode>().set_mode(4))
            .expect("mode");
        runtime
            .read_outputs_general::<WithdrawPublisher>()
            .expect("re-arm the presence");
        assert!(
            read_withdrawn(&runtime),
            "a re-armed presence must not serve a payload the group dropped \
             (step {step})"
        );
        assert_eq!(withdrawn_payload(&runtime), None);
    }
}

// ---------------------------------------------------------------------------
// The delta a replacement carries names the units the commit writes, not the
// entity's owner pair (runtime-api-r15-01)
// ---------------------------------------------------------------------------

#[irrigation]
mod replaced_entry_reader {
    use super::EditorMap;

    #[component]
    pub fn read(tag: u64) -> (bool, Option<u64>) {
        let _ = tag;
        let entries = EditorMap.field0();
        (entries.contains_key(&1), entries.get(&1))
    }
}

#[test]
fn a_content_replacement_beside_a_presence_arm_does_not_exempt_the_payload() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runtime = Runtime::new();
    runtime.register_view(<EditorMap as irrigation::ViewSchema>::DESCRIPTOR);
    let view = <EditorMap as irrigation::ViewSchema>::view_id();
    let value = irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(1u32),
    );
    let membership = irrigation::UnitKey::entity(
        view,
        irrigation::FieldId(0),
        irrigation::UnitKind::Membership,
        irrigation::any_key(1u32),
    );
    // A cell-granular write with no content beside it: the payload is certified
    // while the committed content is empty.
    runtime
        .edit(|edit| edit.set(value.clone(), 5u64))
        .expect("install the raw payload");

    let fired = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<EditorMap>(vec![value], move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("subscribe to the payload");

    // One transaction re-arms the presence and replaces the content. The
    // presence write publishes, and it writes only its own cell: the payload
    // cell is the replacement's, and the replacement never wrote it.
    runtime
        .edit(|edit| {
            edit.set(membership.clone(), true);
            let mut content = EditorMapContent::empty();
            content.field0 = content.field0.insert(1u32, 9u64).0;
            edit.set_content(
                view,
                Arc::new(content),
                irrigation::values_equal::<EditorMapContent>,
            );
        })
        .expect("re-arm the presence and replace the content");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the replacement moved a payload the transaction never wrote, so its \
         subscriber hears about it"
    );
    assert_eq!(
        runtime
            .call::<replaced_entry_reader::__ReadSpec>(replaced_entry_reader::__ReadArgs { tag: 0 })
            .expect("read"),
        (true, Some(9)),
        "the untouched payload is re-derived from the content the same commit \
         installed"
    );
}

// ---------------------------------------------------------------------------
// A retirement that does not own the field still clears the payload it
// installed (runtime-kernel-r15-03)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct SplitMode {
    mode: u64,
}

fn split_whole_value() -> irrigation::UnitKey {
    irrigation::UnitKey::field(
        <EditorMap as irrigation::ViewSchema>::view_id(),
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
    )
}

#[irrigation]
mod split_protocol_sites {
    use super::{EditorMap, SplitMode, split_whole_value};
    use irrigation::ViewSchema;

    /// Publishes the whole-field payload and nothing else.
    #[component]
    pub fn whole() -> u64 {
        irrigation::ensure_view(<EditorMap as ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(split_whole_value(), 10u64, "split_whole_payload");
        0
    }

    /// Publishes the field's structure, which makes this site the field's
    /// protocol owner without installing the payload beside it.
    #[component]
    pub fn keyed() -> u64 {
        irrigation::ensure_view(<EditorMap as ViewSchema>::DESCRIPTOR);
        let structure: irrigation::PMap<u32, ()> = [(1u32, ())].into_iter().collect();
        irrigation::define_unit(
            irrigation::structure_unit(
                <EditorMap as ViewSchema>::view_id(),
                irrigation::FieldId(0),
            ),
            structure,
            "split_keyed_structure",
        );
        0
    }

    #[component]
    pub fn provider(mode: &SplitMode) -> (u64, u64) {
        match mode.mode() {
            // Only the keyed half publishes, so the field's protocol has an
            // owner before the whole payload exists.
            1 => (0, keyed()),
            // Both halves are live at once.
            2 => (whole(), keyed()),
            // The whole half's site retires; the keyed half stays.
            _ => (0, keyed()),
        }
    }

    #[component]
    pub fn root(mode: &SplitMode) -> (u64, u64) {
        provider(mode)
    }
}

#[irrigation]
mod split_payload_reader {
    use super::split_whole_value;

    #[component]
    pub fn read(tag: u64) -> Option<u64> {
        let _ = tag;
        irrigation::try_read_unit_value(split_whole_value(), |_| None::<u64>)
    }
}

#[test]
fn a_retirement_that_does_not_own_the_field_clears_its_payload() {
    let runtime = Runtime::new();
    runtime
        .install(split_protocol_sites::Root)
        .expect("install the provider");
    runtime
        .edit(|edit| edit.view::<SplitMode>().set_mode(1))
        .expect("publish the keyed protocol");
    runtime
        .read_outputs::<split_protocol_sites::Root>()
        .expect("run the keyed half");

    runtime
        .edit(|edit| edit.view::<SplitMode>().set_mode(2))
        .expect("publish both halves");
    runtime
        .read_outputs::<split_protocol_sites::Root>()
        .expect("run both halves");
    let read = |runtime: &Runtime| {
        runtime
            .call::<split_payload_reader::__ReadSpec>(split_payload_reader::__ReadArgs { tag: 0 })
            .expect("read")
    };
    assert_eq!(
        read(&runtime),
        Some(10),
        "the whole payload is installed while its site is live"
    );

    // The whole site's node retires while the keyed site keeps the field's
    // protocol. The payload it installed belongs to no definition and no owner,
    // so the from-scratch answer is absent.
    runtime
        .edit(|edit| edit.view::<SplitMode>().set_mode(3))
        .expect("retire the whole half");
    runtime
        .read_outputs::<split_protocol_sites::Root>()
        .expect("run the keyed half alone");
    assert_eq!(
        read(&runtime),
        None,
        "a payload whose definition and owner are both gone is not served, and \
         its readers are invalidated"
    );
}

// ---------------------------------------------------------------------------
// The content is the cutoff authority on the definition path too
// (collections-r15-02)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct RestateMode {
    mode: u64,
}

#[irrigation]
mod content_restating_entry {
    use super::Table;

    /// Restates the entry the committed content already holds.
    #[component]
    pub fn restate(table: &Table) -> u64 {
        table.entries().update(1, 10, "content_restating_entry");
        1
    }

    #[component]
    pub fn read(tag: u64) -> (bool, Option<u64>) {
        let _ = tag;
        let entries = Table.entries();
        (entries.contains_key(&1), entries.get(&1))
    }
}

fn table_membership() -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        <Table as irrigation::ViewSchema>::view_id(),
        irrigation::FieldId(0),
        irrigation::UnitKind::Membership,
        irrigation::any_key(1u32),
    )
}

fn table_value() -> irrigation::UnitKey {
    irrigation::UnitKey::entity(
        <Table as irrigation::ViewSchema>::view_id(),
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
        irrigation::any_key(1u32),
    )
}

/// One unit's change version, read from the read-only dump.
fn unit_changed_at(runtime: &Runtime, unit: &irrigation::UnitKey) -> u64 {
    let key = format!("{unit:?}");
    runtime
        .dump()
        .into_iter()
        .find(|node| node.unit.as_deref() == Some(key.as_str()))
        .map(|node| node.changed_at)
        .expect("the cell is materialized")
}

/// Publishes a revision that defines no cell of `Table`.
fn publish_restate_noise(runtime: &Runtime, mode: u64) {
    runtime
        .edit(|edit| edit.view::<RestateMode>().set_mode(mode))
        .expect("an unrelated publishing edit");
}

#[test]
fn a_definition_that_restates_the_committed_content_keeps_the_units_version() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runtime = Runtime::new();
    seed_table_content(&runtime, &[(1, 10)]);
    let content_revision = runtime.revision();

    let fired = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<Table>(vec![table_membership(), table_value()], move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("subscribe to the entry");

    // The definition lands at a revision later than the content it restates.
    publish_restate_noise(&runtime, 1);
    assert_ne!(runtime.revision(), content_revision);

    runtime
        .install(content_restating_entry::Restate)
        .expect("install the provider");
    assert_eq!(
        runtime
            .read_outputs::<content_restating_entry::Restate>()
            .expect("restate the entry"),
        1
    );
    publish_restate_noise(&runtime, 2);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "a definition that restates the content is not a change of the unit"
    );
    assert_eq!(
        unit_changed_at(&runtime, &table_membership()),
        content_revision,
        "the presence cell is born at the content's revision"
    );
    assert_eq!(
        unit_changed_at(&runtime, &table_value()),
        content_revision,
        "the payload cell is born at the content's revision, not at the \
         definition's"
    );
}

#[test]
fn a_restating_definition_does_not_invalidate_the_reader_it_restates() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runtime = Runtime::new();
    seed_table_content(&runtime, &[(1, 10)]);
    assert_eq!(
        runtime
            .call::<content_restating_entry::__ReadSpec>(content_restating_entry::__ReadArgs {
                tag: 0
            })
            .expect("read the content"),
        (true, Some(10))
    );

    let fired = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<Table>(vec![table_membership(), table_value()], move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("subscribe to the entry");

    publish_restate_noise(&runtime, 1);
    runtime
        .install(content_restating_entry::Restate)
        .expect("install the provider");
    assert_eq!(
        runtime
            .read_outputs::<content_restating_entry::Restate>()
            .expect("restate the entry"),
        1
    );
    let executions = runtime.stats().executions;
    publish_restate_noise(&runtime, 2);
    assert_eq!(
        runtime
            .call::<content_restating_entry::__ReadSpec>(content_restating_entry::__ReadArgs {
                tag: 0
            })
            .expect("re-read"),
        (true, Some(10)),
        "the restating definition leaves the answer alone"
    );
    assert_eq!(
        runtime.stats().executions,
        executions,
        "a restating definition invalidates no reader"
    );
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "a restating definition delivers no change"
    );
}

// ---------------------------------------------------------------------------
// A definition that restates an equal unit does not rebuild content
// ---------------------------------------------------------------------------

#[view(source)]
pub struct RestateFlag {
    on: bool,
}

#[view(derived)]
pub struct RestatedMap(Map<u32, u32>);

struct RestatingGeneral;

impl irrigation::GeneralComponent for RestatingGeneral {
    type State = ();
    type Output = u32;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CD_0101);
    const NAME: &'static str = "gates::RestatingGeneral";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<RestateFlag as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<RestatedMap as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let flag = irrigation::UnitKey::field(
            <RestateFlag as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        );
        let _: bool = cx.read_typed(flag)?;
        cx.output().entry(
            <RestatedMap as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            1u32,
            10u32,
        );
        Ok(((), 0))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn an_equal_definition_restatement_skips_content_application() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(RestatingGeneral)
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs_general::<RestatingGeneral>()
            .expect("seed"),
        0
    );
    assert_eq!(runtime.stats().content_applies, 1);
    let skips = runtime.stats().content_restatements;

    runtime
        .edit(|edit| edit.view::<RestateFlag>().set_on(true))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<RestatingGeneral>()
            .expect("re-read"),
        0
    );
    assert_eq!(
        runtime.stats().content_applies,
        1,
        "the equal restatement must not apply a content draft"
    );
    assert!(
        runtime.stats().content_restatements > skips,
        "the equal restatement must be counted; {:?}",
        runtime.stats()
    );
}

// ---------------------------------------------------------------------------
// A projection-only unit does not move field content
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct MembershipOnly(Map<u32, u32>);

struct ProjectionGeneral;

impl irrigation::GeneralComponent for ProjectionGeneral {
    type State = ();
    type Output = u32;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CD_0102);
    const NAME: &'static str = "gates::ProjectionGeneral";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<MembershipOnly as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        cx.output().membership(
            <MembershipOnly as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            1u32,
            true,
        );
        Ok(((), 0))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_projection_only_unit_skips_content_application() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(ProjectionGeneral)
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs_general::<ProjectionGeneral>()
            .expect("outputs"),
        0
    );
    let stats = runtime.stats();
    assert_eq!(
        stats.content_applies, 0,
        "a projection-only unit must not rebuild content"
    );
    assert!(
        stats.content_projections > 0,
        "the projection-only movement must be counted: {stats:?}"
    );
}

// ---------------------------------------------------------------------------
// Several changed units in one commit share one content batch
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct BatchedMap(Map<u32, u32>);

struct BatchedGeneral;

impl irrigation::GeneralComponent for BatchedGeneral {
    type State = ();
    type Output = u32;

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00CD_0103);
    const NAME: &'static str = "gates::BatchedGeneral";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<BatchedMap as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let view = <BatchedMap as irrigation::ViewSchema>::view_id();
        cx.output().entry(view, irrigation::FieldId(0), 1u32, 10u32);
        cx.output().entry(view, irrigation::FieldId(0), 2u32, 20u32);
        Ok(((), 0))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn several_changed_map_entries_share_one_content_batch() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(BatchedGeneral)
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs_general::<BatchedGeneral>()
            .expect("outputs"),
        0
    );
    let stats = runtime.stats();
    assert_eq!(stats.content_applies, 2);
    assert_eq!(stats.content_batches, 1);
}

// ---------------------------------------------------------------------------
// A single source payload update installs content without whole equality
// ---------------------------------------------------------------------------

#[view(source)]
pub struct SourceDeltaMap(Map<u32, u32>);

#[test]
fn a_single_source_payload_update_skips_whole_content_equality() {
    let runtime = Runtime::new();
    runtime.register_view(<SourceDeltaMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| edit.view::<SourceDeltaMap>().field0().insert(1, 10))
        .expect("seed");
    let before = runtime.stats().source_content_applies;
    runtime
        .edit(|edit| edit.view::<SourceDeltaMap>().field0().insert(1, 20))
        .expect("update one existing entry");
    assert!(
        runtime.stats().source_content_applies > before,
        "one content-moving source unit must skip whole-content equality"
    );
}

// ---------------------------------------------------------------------------
// Graph add/remove cancellation is proven by unit equality, not whole equality
// ---------------------------------------------------------------------------

#[view(source)]
pub struct SourceGraphDelta(Graph<u32, u32>);

#[test]
fn a_source_graph_cancellation_uses_unit_equality() {
    let runtime = Runtime::new();
    runtime.register_view(<SourceGraphDelta as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| {
            let mut view = edit.view::<SourceGraphDelta>();
            let mut graph = view.field0();
            graph.add_node(1, 10);
            graph.add_node(2, 20);
        })
        .expect("seed");
    let revision = runtime.revision();
    let equality_checks = runtime.stats().source_equality_checks;
    runtime
        .edit(|edit| {
            let mut view = edit.view::<SourceGraphDelta>();
            let mut graph = view.field0();
            let edge = graph.add_edge(1, 2, 5);
            graph.remove_edge(edge);
        })
        .expect("add and remove");
    assert_eq!(runtime.revision(), revision);
    assert_eq!(
        runtime.stats().source_equality_checks,
        equality_checks,
        "unit equality must prove the graph returned to its committed content"
    );
}

// ---------------------------------------------------------------------------
// Keyed components: one independent invocation per key
// ---------------------------------------------------------------------------

#[irrigation]
pub mod keyed_producer {
    #[component(callable, keyed = u32)]
    pub fn double(key: u32) -> u32 {
        key * 2
    }
}

#[test]
fn a_keyed_component_calls_one_invocation_per_key() {
    let runtime = Runtime::new();
    let outputs = keyed_producer::Double::call_keyed(&runtime, [1u32, 2, 3]);
    let outputs: Vec<u32> = outputs
        .into_iter()
        .map(|output| output.expect("keyed output"))
        .collect();
    assert_eq!(outputs, vec![2, 4, 6]);
}

static KEYED_ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static KEYED_MAX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn wait_for_keyed_overlap() {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    let active = KEYED_ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
    KEYED_MAX.fetch_max(active, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    KEYED_ACTIVE.fetch_sub(1, Ordering::SeqCst);
}

#[irrigation]
pub mod parallel_keyed {
    #[component(callable, keyed = u32)]
    pub fn slow(key: u32) -> u32 {
        super::wait_for_keyed_overlap();
        key
    }
}

#[test]
fn keyed_calls_run_concurrently() {
    KEYED_MAX.store(0, std::sync::atomic::Ordering::SeqCst);
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        ..irrigation::RuntimeConfig::default()
    });
    let outputs = parallel_keyed::Slow::call_keyed(&runtime, [1u32, 2]);
    let outputs: Vec<u32> = outputs
        .into_iter()
        .map(|output| output.expect("parallel keyed output"))
        .collect();
    assert_eq!(outputs, vec![1, 2]);
    assert!(
        KEYED_MAX.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "two keyed producers must overlap"
    );
}

#[test]
fn keyed_many_keys_keep_order_with_bounded_workers() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 4,
        ..irrigation::RuntimeConfig::default()
    });
    let keys: Vec<u32> = (0..200).collect();
    let outputs = keyed_producer::Double::call_keyed(&runtime, keys.clone());
    let outputs: Vec<u32> = outputs
        .into_iter()
        .map(|output| output.expect("keyed output"))
        .collect();
    assert_eq!(
        outputs,
        keys.into_iter().map(|key| key * 2).collect::<Vec<_>>()
    );
}

#[view(derived)]
pub struct PlannedMap(Map<u32, u32>);

#[irrigation]
pub mod planned_component {
    use super::PlannedMap;
    use irrigation::ViewSchema;

    #[component(
        callable,
        plans = [irrigation::WriteScope::Field(
            PlannedMap::view_id(),
            irrigation::FieldId(0)
        )]
    )]
    pub fn write(_tag: u32) -> u32 {
        PlannedMap.field0().define(1, 10, "planned");
        0
    }
}

#[test]
fn a_component_registers_its_declared_plans_before_the_body() {
    let runtime = Runtime::new();
    runtime.register_view(<PlannedMap as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .call::<planned_component::Write>(planned_component::WriteArgs { _tag: 0 })
        .expect("planned component");
    assert!(runtime.stats().planned_ports > 0);
    assert_eq!(
        runtime.snapshot().view::<PlannedMap>().field0(),
        vec![(1, 10)]
    );
}

#[view(source)]
pub struct StructureVersion(Map<u32, u32>);

#[test]
fn a_map_payload_update_keeps_the_key_structure_version() {
    let runtime = Runtime::new();
    runtime.register_view(<StructureVersion as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| edit.view::<StructureVersion>().field0().insert(1, 10))
        .expect("seed");
    let before = runtime.stats().structure_refreshes;
    runtime
        .edit(|edit| edit.view::<StructureVersion>().field0().insert(1, 20))
        .expect("payload update");
    let after = runtime.stats().structure_refreshes;
    assert_eq!(
        before, after,
        "a payload update must not refresh the key structure"
    );
}

#[view(source)]
pub struct KeyRows(Map<u32, u32>);

#[view(source)]
pub struct KeySourceSet(Set<u32>);

#[irrigation]
pub mod keyed_rows {
    use super::KeyRows;
    use irrigation::ViewSchema;

    #[component(
        callable,
        keyed = u32,
        keys = KeyRows,
        plans = [irrigation::WriteScope::entity(
            &irrigation::UnitKey::entity(
                KeyRows::view_id(),
                irrigation::FieldId(0),
                irrigation::UnitKind::Value,
                irrigation::any_key(key)
            )
        )]
    )]
    pub fn write(key: u32) -> u32 {
        KeyRows.field0().define(key, key * 10, "keyed row");
        key
    }
}

#[test]
fn keyed_rows_commit_per_key_in_key_order() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.register_view(<KeyRows as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| {
            let mut rows = edit.view::<KeyRows>();
            let mut field = rows.field0();
            field.insert(1, 0);
            field.insert(2, 0);
            field.insert(3, 0);
        })
        .expect("seed rows");
    let outputs = keyed_rows::Write::call_all(&runtime);
    assert!(outputs.iter().all(std::result::Result::is_ok));
    assert_eq!(
        runtime.snapshot().view::<KeyRows>().field0(),
        vec![(1, 10), (2, 20), (3, 30)]
    );
}

static KEYED_ROW_READY: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<()>>> =
    std::sync::OnceLock::new();
static KEYED_ROW_RELEASE: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Receiver<()>>> =
    std::sync::OnceLock::new();

fn pause_keyed_row(key: u32) {
    if key == 1 {
        if let Some(ready) = KEYED_ROW_READY.get() {
            let _ = ready.lock().expect("ready").send(());
        }
        if let Some(release) = KEYED_ROW_RELEASE.get() {
            let _ = release.lock().expect("release").recv();
        }
    }
}

#[irrigation]
pub mod keyed_rows_slow {
    use super::KeyRows;
    use irrigation::ViewSchema;

    #[component(
        callable,
        keyed = u32,
        plans = [irrigation::WriteScope::entity(
            &irrigation::UnitKey::entity(
                KeyRows::view_id(),
                irrigation::FieldId(0),
                irrigation::UnitKind::Value,
                irrigation::any_key(key)
            )
        )]
    )]
    pub fn write(key: u32) -> u32 {
        KeyRows.field0().define(key, key * 10, "slow keyed row");
        super::pause_keyed_row(key);
        key
    }
}

#[test]
fn one_slow_key_does_not_block_another_keys_commit() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let _ = KEYED_ROW_READY.set(std::sync::Mutex::new(ready_tx));
    let _ = KEYED_ROW_RELEASE.set(std::sync::Mutex::new(release_rx));

    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.register_view(<KeyRows as irrigation::ViewSchema>::DESCRIPTOR);
    let writer_runtime = runtime.clone();
    let writer =
        std::thread::spawn(move || keyed_rows_slow::Write::call_keyed(&writer_runtime, [1u32, 2]));
    ready_rx.recv().expect("key 1 wrote");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let rows = runtime.snapshot().view::<KeyRows>().field0();
        if rows.iter().any(|(key, value)| *key == 2 && *value == 20) {
            break;
        }
        assert!(Instant::now() < deadline, "key 2 never committed");
        std::thread::sleep(Duration::from_millis(1));
    }
    release_tx.send(()).expect("release key 1");
    let outputs = writer.join().expect("writer thread");
    assert!(outputs.iter().all(std::result::Result::is_ok));
    assert_eq!(
        runtime.snapshot().view::<KeyRows>().field0(),
        vec![(1, 10), (2, 20)]
    );
}

#[view(source)]
pub struct KeyGraph(Graph<u32, u32>);

#[irrigation]
pub mod graph_keys {
    use super::KeyGraph;

    #[component(callable, keyed = u64, keys = (KeyGraph, field0_nodes))]
    pub fn echo(key: u64) -> u64 {
        key
    }
}

#[test]
fn call_all_enumerates_graph_node_handles() {
    let runtime = Runtime::new();
    runtime.register_view(<KeyGraph as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| {
            let mut view = edit.view::<KeyGraph>();
            let mut graph = view.field0();
            graph.add_node(10, 1);
            graph.add_node(20, 2);
        })
        .expect("seed graph");
    let outputs = graph_keys::Echo::call_all(&runtime);
    let outputs: Vec<u64> = outputs
        .into_iter()
        .map(|output| output.expect("graph key output"))
        .collect();
    assert_eq!(outputs, vec![10, 20]);
}

#[incremental_data]
pub struct SchemaRows {
    #[collection(Map)]
    pub rows: Map<u32, u32>,
    #[collection(Set)]
    pub marked: Set<u32>,
    #[index]
    pub note: u64,
}

#[test]
fn incremental_data_declares_a_view_schema() {
    fn assert_schema<T: irrigation::IncrementalData>() {}
    assert_schema::<SchemaRows>();
    assert_eq!(
        <SchemaRows as irrigation::ViewSchema>::DESCRIPTOR
            .fields
            .len(),
        3
    );
}

/// `define_graph` refuses an edge whose endpoint is not in the same definition. The
/// docs used to promise such edges were dropped, while the unit loop panicked on them --
/// so the documented drop never happened and one of the two spellings was dead.
#[test]
fn a_whole_graph_definition_refuses_an_edge_to_a_missing_node() {
    let runtime = Runtime::new();
    runtime.install(derived_graph::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<GraphMode>().set_mode(3))
        .expect("edit");
    let error = runtime
        .read_outputs::<derived_graph::Root>()
        .expect_err("a dangling edge fails the write");
    let message = format!("{error:?}");
    assert!(
        message.contains("graph edge endpoints exist in the same definition"),
        "the refusal names the rule: {message}"
    );
}

/// Removing a reduced element renumbers everything after it, and the cell at the old last
/// position must be retracted -- restaging alone would leave it serving an element the field
/// no longer holds.
#[test]
fn removing_a_reduced_element_leaves_no_stale_tail() {
    let runtime = Runtime::new();
    runtime.install(totals_reader::Total).expect("install");
    runtime
        .edit(|edit| {
            let mut totals = edit.view::<Totals>();
            let mut values = totals.values();
            values.push(10);
            values.push(20);
            values.push(30);
        })
        .expect("seed");
    assert_eq!(
        runtime.read().expect("snapshot").view::<Totals>().values(),
        vec![10, 20, 30]
    );

    runtime
        .edit(|edit| {
            let mut totals = edit.view::<Totals>();
            let mut values = totals.values();
            values.remove(0);
        })
        .expect("remove");

    let snapshot = runtime.read().expect("snapshot");
    let values = snapshot.view::<Totals>().values();
    assert_eq!(
        values,
        vec![20, 30],
        "the shifted elements take their new positions and the tail is gone"
    );
    assert_eq!(
        runtime
            .read_outputs::<totals_reader::Total>()
            .expect("aggregate"),
        50,
        "the aggregate follows the removal rather than the stale cells"
    );
}

/// A provider-driven removal moves the **presence** cell's routing generation, not only the
/// payload's.
///
/// Both cells of one entity belong to one release, so a provider that retracts an entity has to
/// advance the generation a presence subscriber reads. The existing handover gate asserts this for
/// the `Value` unit only; the `Membership` cell is a different unit and was unasserted, so the
/// asymmetry could have been reintroduced without a failure.
#[test]
fn a_provider_removal_advances_the_presence_generation() {
    let runtime = Runtime::new();
    runtime.install(graph_node_map_gate::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<graph_node_map_gate::Root>()
            .expect("read"),
        (vec![(1, 10), (2, 20)], Some(10), Some(20))
    );

    let membership = |node: u64| {
        irrigation::UnitKey::entity(
            <GraphNodeMap as irrigation::ViewSchema>::view_id(),
            irrigation::FieldId(0),
            irrigation::UnitKind::Membership,
            irrigation::any_key(node),
        )
    };
    let before = runtime.routing_generation(&membership(2));

    // mode 2 defines a graph with one node, so node 2's entry is retracted by the provider.
    runtime
        .edit(|edit| edit.view::<GraphMode>().set_mode(2))
        .expect("edit");
    let (entries, _, second) = runtime
        .read_outputs::<graph_node_map_gate::Root>()
        .expect("read");
    assert_eq!(entries, vec![(1, 10)], "the entry is gone");
    assert_eq!(second, None);

    assert!(
        runtime.routing_generation(&membership(2)) > before,
        "the presence cell's generation moves with the payload's on a provider removal"
    );
}
