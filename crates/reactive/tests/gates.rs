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

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[test]
fn a_cancelled_demand_publishes_nothing() {
    let runtime = Runtime::new();
    runtime.install(model_analysis::Analyze).expect("install");
    let token = reactive::CancelToken::new(1);
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

#[reactive]
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
    assert_eq!(runtime.read_outputs::<branching::Pick>().expect("outputs"), 11);
    let before = runtime.memory_report();
    assert!(before.nodes > 0);
    assert!(before.trace_edges > 0);

    // Switch branches: the old branch's nodes become unreachable.
    runtime
        .edit(|edit| {
            edit.view::<Toggle>().set_flag(false);
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<branching::Pick>().expect("outputs"), 22);
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
    assert_eq!(runtime.read_outputs::<branching::Pick>().expect("outputs"), 32);

    // Switching back reconstructs the abandoned branch.
    runtime
        .edit(|edit| {
            edit.view::<Toggle>().set_flag(true);
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<branching::Pick>().expect("outputs"), 11);
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

#[reactive]
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
    assert_eq!(runtime.read_outputs::<table::Lookup>().expect("outputs"), None);

    // An unrelated key must not wake the consumer.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Table>();
            let mut entries = view.entries();
            entries.insert(2, 20);
        })
        .expect("edit");
    let probe = reactive_test::StatsProbe::start(&runtime);
    assert_eq!(runtime.read_outputs::<table::Lookup>().expect("outputs"), None);
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
    assert_eq!(runtime.read_outputs::<table::Lookup>().expect("outputs"), None);
}

// ---------------------------------------------------------------------------
// GeneralComponent: custom incremental algorithms
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Counter {
    value: u64,
}

fn counter_unit() -> reactive::UnitKey {
    reactive::UnitKey::field(
        <Counter as reactive::ViewSchema>::DESCRIPTOR.id,
        reactive::FieldId(0),
        reactive::UnitKind::Value,
    )
}

/// Tracks the counter with an incremental update: the change batch names the
/// unit, so the update re-reads exactly that unit.
struct Tracker;

impl reactive::GeneralComponent for Tracker {
    type State = (u64, u64, u64);
    type Output = (u64, u64, u64);

    const ID: reactive::ComponentId = reactive::ComponentId(0x00AA_0001);
    const NAME: &'static str = "tests::Tracker";
    const VIEWS: &'static [&'static reactive::ViewDescriptor] =
        &[<Counter as reactive::ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> reactive::DependencyUpdates {
        reactive::DependencyUpdates::Incremental
    }

    fn initialize(
        &self,
        cx: &mut reactive::GeneralCx,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(counter_unit())?;
        let state = (value, 1, 0);
        Ok((state, (value * 2, 1, 0)))
    }

    fn update(
        &self,
        cx: &mut reactive::GeneralCx,
        state: &Self::State,
        changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
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

impl reactive::GeneralComponent for Resetter {
    type State = u64;
    type Output = (u64, u64);

    const ID: reactive::ComponentId = reactive::ComponentId(0x00AA_0002);
    const NAME: &'static str = "tests::Resetter";
    const VIEWS: &'static [&'static reactive::ViewDescriptor] =
        &[<Counter as reactive::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut reactive::GeneralCx,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(counter_unit())?;
        Ok((value + 1, (value, 1)))
    }

    fn update(
        &self,
        _cx: &mut reactive::GeneralCx,
        _state: &Self::State,
        _changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        panic!("the default policy must not call update");
    }
}

#[test]
fn general_components_initialize_update_and_commit_state() {
    let runtime = Runtime::new();
    runtime.install_general(Tracker).expect("install");
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
    runtime.install_general(Resetter).expect("install");
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
    runtime.install_general(Tracker).expect("install");
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

impl reactive::JoinSpec for PairJoin {
    type Key = u32;
    type Left = u64;
    type Right = u64;
    type Joined = u64;

    fn left() -> (reactive::ViewId, reactive::FieldId) {
        (
            <LeftTable as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
        )
    }

    fn right() -> (reactive::ViewId, reactive::FieldId) {
        (
            <RightTable as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
        )
    }

    fn combine(left: &Self::Left, right: &Self::Right) -> Self::Joined {
        JOIN_COMBINES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    runtime.register_view(<LeftTable as reactive::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<RightTable as reactive::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general(reactive::EquiJoin::<PairJoin>::new())
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
            .read_outputs_general::<reactive::EquiJoin<PairJoin>>()
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
            .read_outputs_general::<reactive::EquiJoin<PairJoin>>()
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
            .read_outputs_general::<reactive::EquiJoin<PairJoin>>()
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
            .read_outputs_general::<reactive::EquiJoin<PairJoin>>()
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
        let runtime = Runtime::with_config(reactive::RuntimeConfig {
            workers,
            stack_size: 32 * 1024 * 1024,
            ..reactive::RuntimeConfig::default()
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
        let results: Vec<_> = handles.into_iter().map(|handle| handle.join().expect("join")).collect();
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

impl reactive::Reduction for Sum {
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

#[reactive]
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

// ---------------------------------------------------------------------------
// Cross-thread single-flight under contention
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Seeds {
    values: List<u32>,
}

#[reactive]
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
        let runtime = Runtime::with_config(reactive::RuntimeConfig {
            workers,
            stack_size: 32 * 1024 * 1024,
            ..reactive::RuntimeConfig::default()
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

#[test]
fn snapshots_can_be_taken_without_a_root() {
    let runtime = Runtime::new();
    runtime.register_view(<Table as reactive::ViewSchema>::DESCRIPTOR);
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

impl reactive::AsyncGeneralComponent for AsyncTracker {
    type State = u64;
    type Output = u64;

    const ID: reactive::ComponentId = reactive::ComponentId(0x00AA_0003);
    const NAME: &'static str = "tests::AsyncTracker";
    const VIEWS: &'static [&'static reactive::ViewDescriptor] =
        &[<Counter as reactive::ViewSchema>::DESCRIPTOR];

    fn initialize<'a>(
        &'a self,
        cx: &'a mut reactive::GeneralCx,
    ) -> reactive::ComponentFuture<'a, (Self::State, Self::Output)> {
        Box::pin(async move {
            let value: u64 = cx.read_typed(counter_unit())?;
            Ok((value, value * 2))
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a mut reactive::GeneralCx,
        state: &'a Self::State,
        changes: &'a reactive::ChangeBatch,
    ) -> reactive::ComponentFuture<'a, (Self::State, Self::Output)> {
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
        .install_general(reactive::AsyncAdapter::new(AsyncTracker))
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs_general::<reactive::AsyncAdapter<AsyncTracker>>()
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
            .read_outputs_general::<reactive::AsyncAdapter<AsyncTracker>>()
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

fn switch_unit(field: u16) -> reactive::UnitKey {
    reactive::UnitKey::field(
        <Switch as reactive::ViewSchema>::DESCRIPTOR.id,
        reactive::FieldId(field),
        reactive::UnitKind::Value,
    )
}

/// Reads either `left` or `right` depending on `flag`, and switches its
/// observed unit when the flag changes.
struct Selector;

impl reactive::GeneralComponent for Selector {
    type State = (bool, u64);
    type Output = u64;

    const ID: reactive::ComponentId = reactive::ComponentId(0x00AA_0004);
    const NAME: &'static str = "tests::Selector";
    const VIEWS: &'static [&'static reactive::ViewDescriptor] =
        &[<Switch as reactive::ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> reactive::DependencyUpdates {
        reactive::DependencyUpdates::Incremental
    }

    fn initialize(
        &self,
        cx: &mut reactive::GeneralCx,
    ) -> reactive::Result<(Self::State, Self::Output)> {
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
        cx: &mut reactive::GeneralCx,
        state: &Self::State,
        changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
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
    runtime.install_general(Selector).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Switch>();
            view.set_flag(false);
            view.set_left(10);
            view.set_right(20);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<Selector>()
            .expect("outputs"),
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
        runtime
            .read_outputs_general::<Selector>()
            .expect("outputs"),
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
        runtime
            .read_outputs_general::<Selector>()
            .expect("outputs"),
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
        runtime
            .read_outputs_general::<Selector>()
            .expect("outputs"),
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

#[reactive]
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
    let seen: Arc<Mutex<Vec<reactive::ChangeSummary>>> = Arc::new(Mutex::new(Vec::new()));
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
    assert_eq!(summaries[0].view, <Model as reactive::ViewSchema>::view_id());
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

#[reactive]
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
        matches!(error, reactive::ComputeError::DuplicateDefinition { .. }),
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

#[reactive]
mod bomb {
    use super::{explode, Bomb};

    #[component]
    pub fn risky(bomb: &Bomb) -> u64 {
        if bomb.armed() {
            explode()
        } else {
            7
        }
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
        matches!(error, reactive::ComputeError::Panicked { .. }),
        "expected a panic error, got {error:?}"
    );

    // The single-flight slot is released and the failure is not permanent.
    runtime
        .edit(|edit| {
            edit.view::<Bomb>().set_armed(false);
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<bomb::Risky>().expect("recovers"),
        7
    );
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
        first
            .read_outputs::<model_analysis::Analyze>()
            .expect("a"),
        2
    );
    assert_eq!(
        second
            .read_outputs::<model_analysis::Analyze>()
            .expect("b"),
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

#[reactive]
mod parser {
    use super::{parse_number, TextInput};

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

#[reactive]
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

#[reactive]
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
    let _ = runtime.read_outputs::<branch_scope::Root>().expect("outputs");
    assert_eq!(
        runtime.read().expect("snapshot").view::<BranchSlot>().field0(),
        vec![(1, 10)]
    );

    runtime
        .edit(|edit| {
            edit.view::<BranchFlag>().set_on(false);
        })
        .expect("edit");
    let _ = runtime.read_outputs::<branch_scope::Root>().expect("outputs");
    assert_eq!(
        runtime.read().expect("snapshot").view::<BranchSlot>().field0(),
        Vec::<(u32, u32)>::new(),
        "the retired branch's definition must be retracted"
    );

    runtime
        .edit(|edit| {
            edit.view::<BranchFlag>().set_on(true);
        })
        .expect("edit");
    let _ = runtime.read_outputs::<branch_scope::Root>().expect("outputs");
    assert_eq!(
        runtime.read().expect("snapshot").view::<BranchSlot>().field0(),
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

#[reactive]
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
    let runtime = Runtime::with_config(reactive::RuntimeConfig {
        workers: 2,
        stack_size: 16 * 1024 * 1024,
        ..reactive::RuntimeConfig::default()
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

#[reactive]
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
    let runtime = Runtime::with_config(reactive::RuntimeConfig {
        workers: 2,
        stack_size: 16 * 1024 * 1024,
        ..reactive::RuntimeConfig::default()
    });
    runtime.install(diamond::Combine).expect("install");
    // (value, (doubled, parity)): `doubled` changes every step; `parity` holds
    // at 0 for 2->4 and changes at 5. A consumer that combined a new `doubled`
    // with a stale `parity` would fail here.
    for (value, expected) in [(2, (4, 0)), (4, (8, 0)), (5, (10, 1)), (6, (12, 0)), (6, (12, 0))] {
        runtime
            .edit(|edit| {
                edit.view::<DiamondInput>().set_value(value);
            })
            .expect("edit");
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let runtime = &runtime;
                    scope.spawn(move || {
                        runtime.read_outputs::<diamond::Combine>().expect("outputs")
                    })
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
    use std::sync::{Arc, Mutex};
    let runtime = Runtime::new();
    runtime.install(handover::Root).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<HandoverFlag>().set_on(false);
        })
        .expect("edit");
    let _ = runtime.read_outputs::<handover::Root>().expect("initial demand");

    type Observed = Vec<Vec<(u32, u32)>>;
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

#[reactive]
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
    runtime.install(derived_list_provider::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<DerivedListFlag>().set_on(true))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<derived_list_provider::Root>().expect("live read"),
        (1, vec![1, 2, 3], 3, Some(3))
    );
    assert_eq!(runtime.snapshot().view::<DerivedSlot>().field0(), vec![1, 2, 3]);

    runtime
        .edit(|edit| edit.view::<DerivedListFlag>().set_on(false))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<derived_list_provider::Root>().expect("live read"),
        (2, vec![4], 1, None)
    );
    assert_eq!(runtime.snapshot().view::<DerivedSlot>().field0(), vec![4]);
}

// ---------------------------------------------------------------------------
// Edit outcome vs observer refresh (N4, F3)
// ---------------------------------------------------------------------------

/// A general root whose demand always fails.
struct BrokenRoot;

impl reactive::GeneralComponent for BrokenRoot {
    type State = u64;
    type Output = u64;

    const ID: reactive::ComponentId = reactive::ComponentId(0x00DD_0001);
    const NAME: &'static str = "gates::BrokenRoot";

    fn initialize(
        &self,
        _cx: &mut reactive::GeneralCx,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        Err(reactive::ComputeError::internal("deliberate failure"))
    }

    fn update(
        &self,
        _cx: &mut reactive::GeneralCx,
        _state: &Self::State,
        _changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
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
    runtime.install_general(BrokenRoot).expect("install general");
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

#[reactive]
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
    runtime.install(conditional_definition::Root).expect("install");
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

#[reactive]
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
    let runtime = Runtime::with_config(reactive::RuntimeConfig {
        workers: 2,
        stack_size: 16 * 1024 * 1024,
        ..reactive::RuntimeConfig::default()
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
        Err(reactive::ComputeError::Cycle { .. }) => {}
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

fn audit_flag_unit() -> reactive::UnitKey {
    reactive::UnitKey::field(
        <AuditFlag as reactive::ViewSchema>::DESCRIPTOR.id,
        reactive::FieldId(0),
        reactive::UnitKind::Value,
    )
}

fn audit_outer_unit() -> reactive::UnitKey {
    reactive::UnitKey::field(
        <AuditOuter as reactive::ViewSchema>::DESCRIPTOR.id,
        reactive::FieldId(0),
        reactive::UnitKind::Value,
    )
}

fn audit_missing_unit() -> reactive::UnitKey {
    reactive::UnitKey::field(
        <AuditMissing as reactive::ViewSchema>::DESCRIPTOR.id,
        reactive::FieldId(0),
        reactive::UnitKind::Value,
    )
}

struct InstallAudit;

impl reactive::GeneralComponent for InstallAudit {
    type State = u64;
    type Output = u64;

    const ID: reactive::ComponentId = reactive::ComponentId(0x00DD_0002);
    const NAME: &'static str = "gates::InstallAudit";
    const VIEWS: &'static [&'static reactive::ViewDescriptor] = &[
        <AuditFlag as reactive::ViewSchema>::DESCRIPTOR,
        <AuditOuter as reactive::ViewSchema>::DESCRIPTOR,
    ];

    fn initialize(
        &self,
        cx: &mut reactive::GeneralCx,
    ) -> reactive::Result<(Self::State, Self::Output)> {
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
        _cx: &mut reactive::GeneralCx,
        _state: &Self::State,
        _changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        unreachable!("the audit component resets")
    }
}

#[test]
fn a_failed_definition_install_is_atomic() {
    let runtime = Runtime::new();
    runtime.install_general(InstallAudit).expect("install");
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
        matches!(failed, Err(reactive::ComputeError::DefinitionDenied { .. })),
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

#[reactive]
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
    let second = runtime.call::<call_conflict::__SecondSpec>(call_conflict::__SecondArgs { tag: 0 });
    assert!(
        matches!(
            second,
            Err(reactive::ComputeError::DuplicateDefinition { .. })
        ),
        "expected a duplicate definition, got {second:?}"
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

#[reactive]
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
    assert!(
        after + 100 <= before,
        "units before={before} after={after}"
    );

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

#[reactive]
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

#[reactive]
mod structure_map {
    use super::{StructureMode, StructureTable};

    #[component]
    pub fn root(mode: &StructureMode) -> (Vec<u32>, usize, Vec<(u32, u64)>) {
        reactive::ensure_view(<StructureTable as reactive::ViewSchema>::DESCRIPTOR);
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
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<structure_map::Root>()
            .expect("read")
    };
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

#[reactive]
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
    assert!(matches!(error, reactive::ComputeError::Internal { .. }));
}

#[reactive]
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
    JOIN_COMBINES.store(0, std::sync::atomic::Ordering::SeqCst);
    let runtime = Runtime::new();
    runtime.register_view(<LeftTable as reactive::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<RightTable as reactive::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general(reactive::EquiJoin::<PairJoin>::new())
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
            .read_outputs_general::<reactive::EquiJoin<PairJoin>>()
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
            .read_outputs_general::<reactive::EquiJoin<PairJoin>>()
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
}

// ---------------------------------------------------------------------------
// Phase 1: one structure contract (F1 completion)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ConsumerTable {
    entries: Map<u32, u64>,
}

#[reactive]
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

#[reactive]
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

#[reactive]
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
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<keyed_lists::Root>()
            .expect("read")
    };
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
    assert_eq!(
        read(&runtime),
        (vec![1], vec![10], Some(10), None)
    );
    let units = runtime.dump();
    let membership = |id: &str| {
        units
            .iter()
            .find(|node| {
                node.kind == "Unit"
                    && node
                        .unit
                        .as_deref()
                        .is_some_and(|unit| {
                            unit.contains("Membership") && unit.contains(id)
                        })
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

#[reactive]
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
    assert_eq!(
        KEYED_MAP_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        2
    );

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

#[reactive]
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
    let read = |runtime: &Runtime| {
        runtime
            .read_outputs::<keyed_sets::Root>()
            .expect("read")
    };
    assert_eq!(
        read(&runtime),
        (true, true, false, vec![1, 2])
    );

    // Adding a third member changes only that member's membership unit.
    runtime
        .edit(|edit| edit.view::<KeyedMode>().set_mode(1))
        .expect("edit");
    let revision = runtime.revision();
    assert_eq!(
        read(&runtime),
        (true, true, true, vec![1, 2, 3])
    );
    let units = runtime.dump();
    let membership = |id: &str| {
        units
            .iter()
            .find(|node| {
                node.kind == "Unit"
                    && node
                        .unit
                        .as_deref()
                        .is_some_and(|unit| {
                            unit.contains("Membership") && unit.contains(id)
                        })
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
    assert_eq!(
        read(&runtime),
        (false, true, false, vec![2])
    );
    let units = runtime.dump();
    let membership = |id: &str| {
        units
            .iter()
            .find(|node| {
                node.kind == "Unit"
                    && node
                        .unit
                        .as_deref()
                        .is_some_and(|unit| {
                            unit.contains("Membership") && unit.contains(id)
                        })
            })
            .expect("membership unit")
    };
    assert_eq!(membership("Some(\"1\")").changed_at, removal);
    assert!(membership("Some(\"2\")").changed_at < removal);
}

static KEYED_FILTER_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static KEYED_FLAT_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[reactive]
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

#[reactive]
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

#[reactive]
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

#[reactive]
mod map_to_list_gate {
    use super::{CrossList, CrossTable};

    #[component]
    pub fn root(table: &CrossTable) -> (Vec<u64>, Option<u64>, Option<u64>) {
        reactive::ensure_view(<CrossList as reactive::ViewSchema>::DESCRIPTOR);
        let target = CrossList.field0();
        table.entries().to_list(&target, |key, value| {
            Some((u64::from(*key), *value * 2))
        });
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
    let view = format!("{:?}", <CrossList as reactive::ViewSchema>::DESCRIPTOR.id);
    let unit = runtime
        .dump()
        .into_iter()
        .find(|node| {
            node.kind == "Unit"
                && node
                    .unit
                    .as_deref()
                    .is_some_and(|unit| {
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

#[reactive]
mod list_to_map_gate {
    use super::{CrossMap, KeyedList, KeyedMode};

    #[component]
    pub fn root(mode: &KeyedMode) -> (Vec<(u32, u64)>, Option<u64>, Option<u64>) {
        reactive::ensure_view(<CrossMap as reactive::ViewSchema>::DESCRIPTOR);
        if mode.mode() == 1 {
            KeyedList.define_keyed([(1u64, 11u64)]);
        } else {
            KeyedList.define_keyed([(1u64, 10u64), (2, 20)]);
        }
        let target = CrossMap.field0();
        KeyedList.field0().to_map(&target, |id, value| Some((id as u32, *value)));
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
#[reactive]
mod derived_graph {
    use super::{DerivedGraph, GraphMode};

    #[component]
    pub fn root(
        mode: &GraphMode,
    ) -> (
        Vec<u64>,
        Vec<u64>,
        Option<u64>,
        Option<reactive::EdgePayload<u64>>,
        Vec<(u64, u64)>,
    ) {
        if mode.mode() == 1 {
            DerivedGraph.define_graph(
                &[(1u64, 11u64), (2, 20)],
                &[(10u64, 1, 2, 100u64)],
            );
        } else if mode.mode() == 2 {
            DerivedGraph.define_graph(&[(1u64, 10u64)], &[]);
        } else {
            DerivedGraph.define_graph(
                &[(1u64, 10u64), (2, 20)],
                &[(10u64, 1, 2, 100u64)],
            );
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
            Some(reactive::EdgePayload {
                from: 1,
                to: 2,
                value: 100,
            }),
            vec![(10, 2)],
        )
    );
    let topology_version = |runtime: &Runtime| {
        let view = format!("{:?}", <DerivedGraph as reactive::ViewSchema>::DESCRIPTOR.id);
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
            Some(reactive::EdgePayload {
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

impl reactive::JoinSpec for TargetJoin {
    type Key = u32;
    type Left = u64;
    type Right = u64;
    type Joined = u64;

    fn left() -> (reactive::ViewId, reactive::FieldId) {
        (
            <LeftTable as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
        )
    }

    fn right() -> (reactive::ViewId, reactive::FieldId) {
        (
            <RightTable as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
        )
    }

    fn combine(left: &u64, right: &u64) -> u64 {
        left * right
    }

    fn target() -> Option<(reactive::ViewId, reactive::FieldId)> {
        Some((
            <JoinTarget as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
        ))
    }
}

#[test]
fn join_view_updates_only_changed_keys() {
    let runtime = Runtime::new();
    runtime.register_view(<LeftTable as reactive::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<RightTable as reactive::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<JoinTarget as reactive::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general(reactive::EquiJoin::<TargetJoin>::new())
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
            .read_outputs_general::<reactive::EquiJoin<TargetJoin>>()
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
            .read_outputs_general::<reactive::EquiJoin<TargetJoin>>()
            .expect("join"),
        [(1, 22), (2, 60)].into_iter().collect()
    );
    assert_eq!(
        runtime.snapshot().view::<JoinTarget>().field0(),
        vec![(1, 22), (2, 60)]
    );
    let view = format!("{:?}", <JoinTarget as reactive::ViewSchema>::DESCRIPTOR.id);
    let unit = |id: &str| {
        runtime
            .dump()
            .into_iter()
            .find(|node| {
                node.kind == "Unit"
                    && node.unit.as_deref().is_some_and(|unit| {
                        unit.contains(&view)
                            && unit.contains("Value")
                            && unit.contains(id)
                    })
            })
            .expect("target value unit")
    };
    assert_eq!(unit("Some(\"1\")").changed_at, revision);
    assert!(unit("Some(\"2\")").changed_at < revision);
}

#[view(derived)]
pub struct GraphNodeMap(Map<u64, u64>);

#[reactive]
mod graph_node_map_gate {
    use super::{DerivedGraph, GraphMode, GraphNodeMap};

    #[component]
    pub fn root(mode: &GraphMode) -> (Vec<(u64, u64)>, Option<u64>, Option<u64>) {
        reactive::ensure_view(<GraphNodeMap as reactive::ViewSchema>::DESCRIPTOR);
        if mode.mode() == 1 {
            DerivedGraph.define_graph(
                &[(1u64, 11u64), (2, 20)],
                &[(10u64, 1, 2, 100u64)],
            );
        } else if mode.mode() == 2 {
            DerivedGraph.define_graph(&[(1u64, 10u64)], &[]);
        } else {
            DerivedGraph.define_graph(
                &[(1u64, 10u64), (2, 20)],
                &[(10u64, 1, 2, 100u64)],
            );
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
    let view = format!("{:?}", <GraphNodeMap as reactive::ViewSchema>::DESCRIPTOR.id);
    let unit = |id: &str| {
        runtime
            .dump()
            .into_iter()
            .find(|node| {
                node.kind == "Unit"
                    && node.unit.as_deref().is_some_and(|unit| {
                        unit.contains(&view)
                            && unit.contains("Value")
                            && unit.contains(id)
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
    runtime.register_view(<ConsumerTable as reactive::ViewSchema>::DESCRIPTOR);
    let seen = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&seen);
    let subscription = runtime
        .observe_entry::<ConsumerTable, u32>(
            reactive::FieldId(0),
            1u32,
            move |_snapshot| {
                sink.fetch_add(1, Ordering::SeqCst);
            },
        )
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
            .call::<call_projection_gate::__PairSpec>(
                call_projection_gate::__PairArgs { tag },
            )
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

#[test]
fn the_plan_vocabulary_is_public() {
    let site = reactive::plan::Site::new(reactive::SiteId(7), reactive::plan::SiteShape::Read);
    assert_eq!(site.id, reactive::SiteId(7));
    assert_eq!(format!("{:?}", site.shape), "Read");
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

#[reactive]
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
    runtime.install(routing_generation_gate::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<RouteFlag>().set_on(true))
        .expect("seed");
    let _ = runtime
        .read_outputs::<routing_generation_gate::Root>()
        .expect("read");
    let view = <RouteSlot as reactive::ViewSchema>::DESCRIPTOR.id;
    let unit = reactive::UnitKey::field(view, reactive::FieldId(0), reactive::UnitKind::Value);
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

fn scoped_value(key: u32) -> reactive::UnitKey {
    reactive::UnitKey::entity(
        <ScopedTable as reactive::ViewSchema>::DESCRIPTOR.id,
        reactive::FieldId(0),
        reactive::UnitKind::Value,
        reactive::any_key(key),
    )
}

struct ScopedReader;

impl reactive::GeneralComponent for ScopedReader {
    type State = ([u64; 2], usize);
    type Output = ([u64; 2], usize);

    const ID: reactive::ComponentId = reactive::ComponentId(0x00DD_0003);
    const NAME: &'static str = "gates::ScopedReader";
    const VIEWS: &'static [&'static reactive::ViewDescriptor] = &[
        <ScopedTable as reactive::ViewSchema>::DESCRIPTOR,
        <ScopedOut as reactive::ViewSchema>::DESCRIPTOR,
    ];

    fn dependency_updates(&self) -> reactive::DependencyUpdates {
        reactive::DependencyUpdates::Incremental
    }

    fn initialize(
        &self,
        cx: &mut reactive::GeneralCx,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        let mut values = [0u64; 2];
        cx.begin_scope(1);
        values[0] = cx.read_typed(scoped_value(1))?;
        cx.end_scope();
        cx.begin_scope(2);
        values[1] = cx.read_typed(scoped_value(2))?;
        cx.end_scope();
        cx.retain(1);
        cx.output().entry(
            <ScopedOut as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
            1u32,
            values[0],
        );
        Ok(((values, 0), (values, 0)))
    }

    fn update(
        &self,
        cx: &mut reactive::GeneralCx,
        state: &Self::State,
        changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        let batch = changes.changes().len();
        cx.begin_scope(1);
        let first: u64 = cx.read_typed(scoped_value(1))?;
        cx.end_scope();
        cx.retain(1);
        let values = [first, state.0[1]];
        cx.output().entry(
            <ScopedOut as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
            1u32,
            first,
        );
        Ok(((values, batch), (values, batch)))
    }
}

#[test]
fn retired_dependency_scopes_filter_notifications() {
    let runtime = Runtime::new();
    runtime.install_general(ScopedReader).expect("install");
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

#[reactive]
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
        assert_eq!(
            runtime.read_outputs::<churn::Root>().expect("read"),
            value
        );
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
// Strict components and declared primitives (N6 partial)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct StrictKey {
    value: u64,
}

fn strict_double(value: u64) -> u64 {
    value * 2
}

#[reactive]
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
            .call::<call_projection_gate::__PairSpec>(
                call_projection_gate::__PairArgs { tag },
            )
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
            .call::<call_projection_gate::__PairSpec>(
                call_projection_gate::__PairArgs { tag },
            )
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

impl reactive::JoinSpec for WeightJoin {
    type Key = u32;
    type Left = i64;
    type Right = i64;
    type Joined = i64;

    fn left() -> (reactive::ViewId, reactive::FieldId) {
        (
            <LeftWeights as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
        )
    }

    fn right() -> (reactive::ViewId, reactive::FieldId) {
        (
            <RightWeights as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
        )
    }

    fn combine(left: &i64, right: &i64) -> i64 {
        left * right
    }

    fn target() -> Option<(reactive::ViewId, reactive::FieldId)> {
        Some((
            <JoinedWeights as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
        ))
    }
}

#[test]
fn signed_weight_join_applies_the_bilinear_rule() {
    let runtime = Runtime::new();
    runtime.register_view(<LeftWeights as reactive::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<RightWeights as reactive::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<JoinedWeights as reactive::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general(reactive::EquiJoin::<WeightJoin>::new())
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
        .read_outputs_general::<reactive::EquiJoin<WeightJoin>>()
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
        .read_outputs_general::<reactive::EquiJoin<WeightJoin>>()
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
        .read_outputs_general::<reactive::EquiJoin<WeightJoin>>()
        .expect("read");
    assert!(!joined.contains_key(&2));
    eprintln!("target: {:?}", runtime.snapshot().view::<JoinedWeights>().field0());
    assert!(!joined.contains_key(&2));
    assert_eq!(
        runtime.snapshot().view::<JoinedWeights>().field0(),
        vec![(1, 35)]
    );
}
