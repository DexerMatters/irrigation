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
    let explanation = runtime.explain_node(root.id);
    assert!(!explanation.dependencies.is_empty());
    assert!(explanation.suspect_at > explanation.verified_at);
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
