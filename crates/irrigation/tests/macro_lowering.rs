//! Lowering gates for the allow-listed macros, the iterator-chain
//! classification, typed tree discriminants, generated attributes, and binder
//! shadowing.
//!
//! Every gate here exercises a macro expansion end to end: the defect it pins
//! is a frozen value or a trace that names something the source never read, so
//! a unit test of the token stream is not enough.

use irrigation::{NodeId, Runtime, SiteId, irrigation, tree_view, view};

// ---------------------------------------------------------------------------
// A tracked local captured by a macro must be observed
// ---------------------------------------------------------------------------

#[view(source)]
pub struct Seed {
    seed: u32,
    flag: bool,
    width: usize,
}

#[view(source)]
pub struct Numbers {
    values: List<u32>,
}

#[view(source)]
pub struct Shadowed {
    wide: u64,
    narrow: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    x: u32,
    y: u32,
}

#[irrigation]
mod lowered_macros {
    use super::Seed;

    #[component]
    pub fn formatted(input: &Seed) -> String {
        let seed = input.seed();
        format!("seed={seed}")
    }

    #[component]
    pub fn padded(input: &Seed) -> String {
        let seed = input.seed();
        let width = input.width();
        format!("{seed:>width$}")
    }

    #[component]
    pub fn collected(input: &Seed) -> Vec<u32> {
        let seed = input.seed();
        let width = input.width();
        let repeated = vec![seed; width];
        repeated
    }

    #[component]
    pub fn branch(input: &Seed) -> u32 {
        let seed = input.seed();
        if matches!(seed, 1..=4) { 1 } else { 2 }
    }

    #[component]
    pub fn guarded(input: &Seed) -> u32 {
        let seed = input.seed();
        assert!(seed > 0);
        seed
    }

    #[component]
    pub fn inline_branch(input: &Seed) -> u32 {
        if matches!(input.seed(), 1..=4) { 1 } else { 2 }
    }

    #[component]
    pub fn formatted_argument(input: &Seed) -> String {
        format!("value={}", input.seed())
    }

    #[component]
    pub fn collected_argument(input: &Seed) -> Vec<u32> {
        vec![input.seed()]
    }

    #[component]
    pub fn guarded_argument(input: &Seed) -> u32 {
        assert!(input.seed() > 0);
        input.seed()
    }
}

fn runtime<R: irrigation::RootComponent>(root: R) -> Runtime {
    let runtime = Runtime::new();
    runtime.install(root).expect("install");
    runtime
}

#[test]
fn a_format_capture_observes_the_tracked_local() {
    let runtime = runtime(lowered_macros::Formatted);
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(1);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Formatted>()
            .expect("outputs"),
        "seed=1"
    );
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(7);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Formatted>()
            .expect("outputs"),
        "seed=7"
    );
}

#[test]
fn a_format_spec_capture_observes_every_named_local() {
    let runtime = runtime(lowered_macros::Padded);
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Seed>();
            view.set_seed(7);
            view.set_width(3);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Padded>()
            .expect("outputs"),
        "  7"
    );
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_width(5);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Padded>()
            .expect("outputs"),
        "    7"
    );
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(9);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Padded>()
            .expect("outputs"),
        "    9"
    );
}

#[test]
fn a_vec_repeat_observes_its_value_and_count() {
    let runtime = runtime(lowered_macros::Collected);
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Seed>();
            view.set_seed(2);
            view.set_width(3);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Collected>()
            .expect("outputs"),
        vec![2, 2, 2]
    );
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(5);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Collected>()
            .expect("outputs"),
        vec![5, 5, 5]
    );
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_width(1);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Collected>()
            .expect("outputs"),
        vec![5]
    );
}

#[test]
fn an_inline_matches_scrutinee_is_tracked() {
    let runtime = runtime(lowered_macros::InlineBranch);
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(9);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::InlineBranch>()
            .expect("outputs"),
        2
    );
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(3);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::InlineBranch>()
            .expect("outputs"),
        1
    );
}

#[test]
fn macro_arguments_are_lowered_as_expressions() {
    let formatted = runtime(lowered_macros::FormattedArgument);
    formatted
        .edit(|edit| {
            edit.view::<Seed>().set_seed(3);
        })
        .expect("edit");
    assert_eq!(
        formatted
            .read_outputs::<lowered_macros::FormattedArgument>()
            .expect("outputs"),
        "value=3"
    );
    formatted
        .edit(|edit| {
            edit.view::<Seed>().set_seed(4);
        })
        .expect("edit");
    assert_eq!(
        formatted
            .read_outputs::<lowered_macros::FormattedArgument>()
            .expect("outputs"),
        "value=4"
    );

    let collected = runtime(lowered_macros::CollectedArgument);
    collected
        .edit(|edit| {
            edit.view::<Seed>().set_seed(3);
        })
        .expect("edit");
    assert_eq!(
        collected
            .read_outputs::<lowered_macros::CollectedArgument>()
            .expect("outputs"),
        vec![3]
    );
    collected
        .edit(|edit| {
            edit.view::<Seed>().set_seed(4);
        })
        .expect("edit");
    assert_eq!(
        collected
            .read_outputs::<lowered_macros::CollectedArgument>()
            .expect("outputs"),
        vec![4]
    );

    let guarded = runtime(lowered_macros::GuardedArgument);
    guarded
        .edit(|edit| {
            edit.view::<Seed>().set_seed(1);
        })
        .expect("edit");
    assert_eq!(
        guarded
            .read_outputs::<lowered_macros::GuardedArgument>()
            .expect("outputs"),
        1
    );
    guarded
        .edit(|edit| {
            edit.view::<Seed>().set_seed(0);
        })
        .expect("edit");
    assert!(matches!(
        guarded.read_outputs::<lowered_macros::GuardedArgument>(),
        Err(irrigation::ComputeError::Panicked { .. })
    ));
}

#[test]
fn a_matches_branch_flips_with_the_tracked_local() {
    let runtime = runtime(lowered_macros::Branch);
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(9);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Branch>()
            .expect("outputs"),
        2
    );
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(3);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Branch>()
            .expect("outputs"),
        1
    );
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(8);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Branch>()
            .expect("outputs"),
        2
    );
}

#[test]
fn an_assert_condition_is_rechecked_when_the_local_changes() {
    let runtime = runtime(lowered_macros::Guarded);
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(1);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<lowered_macros::Guarded>()
            .expect("outputs"),
        1
    );
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(0);
        })
        .expect("edit");
    assert!(matches!(
        runtime.read_outputs::<lowered_macros::Guarded>(),
        Err(irrigation::ComputeError::Panicked { .. })
    ));
}

// ---------------------------------------------------------------------------
// Attributes below the macro gate the generated items as one unit
// ---------------------------------------------------------------------------

#[view(source)]
#[cfg(not(any()))]
pub struct AlwaysVisible {
    value: u64,
}

#[view(source)]
#[cfg(any())]
pub struct NeverCompiled {
    value: u64,
}

#[irrigation]
mod attribute_gated {
    use super::AlwaysVisible;

    #[component]
    #[cfg(not(any()))]
    pub fn visible(view: &AlwaysVisible) -> u64 {
        view.value()
    }

    #[component]
    #[cfg(any())]
    pub fn hidden(view: &AlwaysVisible) -> u64 {
        view.value() + 1
    }
}

#[test]
fn a_forwarded_cfg_keeps_a_visible_component_working() {
    let runtime = runtime(attribute_gated::Visible);
    runtime
        .edit(|edit| {
            edit.view::<AlwaysVisible>().set_value(3);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<attribute_gated::Visible>()
            .expect("outputs"),
        3
    );
}

// ---------------------------------------------------------------------------
// The reachability fixed point follows its captured seed
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
        fixed.non_bottom()
    }
}

#[test]
fn a_fixed_point_follows_the_seed_captured_by_a_vec_macro() {
    let runtime = runtime(reachability::Reachable);
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
    // The seed is read through `vec![seed]` only; the fixed point must not keep
    // the value from the first revision.
    runtime
        .edit(|edit| {
            edit.view::<GraphInput>().set_seed(2);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<reachability::Reachable>()
            .expect("outputs"),
        vec![2, 3]
    );
}

// ---------------------------------------------------------------------------
// A second-collection-adapter chain stays lazy
// ---------------------------------------------------------------------------

#[irrigation]
mod chained_adapters {
    use super::Numbers;

    #[component]
    pub fn evens(numbers: &Numbers) -> u64 {
        let values = numbers.values().to_vec();
        values
            .iter()
            .filter(|value| **value % 2 == 0)
            .map(|value| u64::from(*value) * 10)
            .sum::<u64>()
    }

    #[component]
    pub fn shifted(numbers: &Numbers) -> u64 {
        let values = numbers.values().to_vec();
        values
            .iter()
            .map(|value| *value + 1)
            .map(|value| u64::from(value) * 2)
            .sum::<u64>()
    }
}

#[test]
fn a_filter_then_map_chain_runs_lazily() {
    let runtime = Runtime::new();
    runtime.install(chained_adapters::Evens).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Numbers>();
            let mut values = view.values();
            values.push(1);
            values.push(2);
            values.push(3);
            values.push(4);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<chained_adapters::Evens>()
            .expect("outputs"),
        60
    );
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Numbers>();
            let mut values = view.values();
            values.push(6);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<chained_adapters::Evens>()
            .expect("outputs"),
        120
    );
}

#[test]
fn two_maps_in_a_row_stay_lazy() {
    let runtime = Runtime::new();
    runtime.install(chained_adapters::Shifted).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Numbers>();
            let mut values = view.values();
            values.push(1);
            values.push(2);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<chained_adapters::Shifted>()
            .expect("outputs"),
        10
    );
}

// ---------------------------------------------------------------------------
// Binder shadowing must not observe the outer local
// ---------------------------------------------------------------------------

#[irrigation]
mod shadowing {
    use super::Shadowed;

    #[component]
    pub fn match_arm(input: &Shadowed) -> u64 {
        let _n = input.wide();
        match input.narrow() {
            _n => _n + 1,
        }
    }

    #[component]
    pub fn closure_parameter(input: &Shadowed) -> u64 {
        let _n = input.wide();
        let values = vec![1u64, 2, 3];
        let shifted: Vec<u64> = values.iter().map(|_n| *_n + 1).collect();
        shifted.iter().sum()
    }

    #[component]
    pub fn trivial_let(input: &Shadowed) -> u64 {
        let _n = input.wide();
        let _n = 5u64;
        let out = _n + 1;
        out
    }
}

#[test]
fn a_match_arm_binding_observes_its_scrutinee_not_the_shadowed_local() {
    let runtime = Runtime::new();
    runtime.install(shadowing::MatchArm).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Shadowed>();
            view.set_wide(1);
            view.set_narrow(10);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<shadowing::MatchArm>()
            .expect("outputs"),
        11
    );
    // The arm body is the third lowered expression site: the `wide` read, the
    // match scrutinee, then `_n + 1`. The `_n` it reads is the pattern binding,
    // so the body observes the scrutinee -- the value the binding projects --
    // and never the shadowed local of the same name.
    let body = site_node(&runtime, 2);
    let scrutinee = site_node(&runtime, 1);
    let explanation = runtime.explain_node(body);
    let observed: Vec<NodeId> = explanation
        .dependencies
        .iter()
        .map(|dependency| dependency.node)
        .collect();
    assert_eq!(
        observed,
        vec![scrutinee],
        "an arm binding must observe the scrutinee, not the shadowed local: {explanation:?}"
    );

    // Changing the value the binding projects re-runs the body.
    runtime
        .edit(|edit| edit.view::<Shadowed>().set_narrow(20))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<shadowing::MatchArm>()
            .expect("outputs"),
        21
    );

    // Changing the shadowed local it does not read does not.
    let executed = runtime.explain_node(body).executed_at;
    runtime
        .edit(|edit| edit.view::<Shadowed>().set_wide(2))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<shadowing::MatchArm>()
            .expect("outputs"),
        21
    );
    assert_eq!(
        runtime.explain_node(body).executed_at,
        executed,
        "the body reads the scrutinee, so the shadowed local must not wake it"
    );
}

#[test]
fn a_trivial_let_binding_shadows_the_tracked_local() {
    let runtime = Runtime::new();
    runtime.install(shadowing::TrivialLet).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Shadowed>();
            view.set_wide(1);
            view.set_narrow(10);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<shadowing::TrivialLet>()
            .expect("outputs"),
        6
    );
    // `_n` resolves to the second binding, so `_n + 1` did not observe the
    // first (tracked) one.
    let sum = site_node(&runtime, 1);
    let explanation = runtime.explain_node(sum);
    assert!(
        explanation.dependencies.is_empty(),
        "a trivial let left the outer tracked local in scope: {explanation:?}"
    );
}

#[test]
fn a_closure_parameter_shadows_the_tracked_local() {
    let runtime = Runtime::new();
    runtime
        .install(shadowing::ClosureParameter)
        .expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Shadowed>();
            view.set_wide(1);
            view.set_narrow(10);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<shadowing::ClosureParameter>()
            .expect("outputs"),
        9
    );
    let closure_node = site_node(&runtime, 2);
    let wide = site_node(&runtime, 0);
    let explanation = runtime.explain_node(closure_node);
    assert!(
        !explanation
            .dependencies
            .iter()
            .any(|dependency| dependency.node == wide),
        "a closure parameter left the outer tracked local in scope: {explanation:?}"
    );
}

/// Resolves the node created for one lowered expression site. Site counters are
/// sequential within a component, so a gate can name the expression it asserts
/// on without depending on node identity allocation.
fn site_node(runtime: &Runtime, counter: u32) -> NodeId {
    let site = SiteId(0x8000_0000 | counter);
    runtime
        .dump()
        .into_iter()
        .map(|node| runtime.explain_node(node.id))
        .find(|explanation| explanation.site == site)
        .unwrap_or_else(|| panic!("no expression site {counter} was created"))
        .node
}

// ---------------------------------------------------------------------------
// A struct-update base is an expression position like any other
// ---------------------------------------------------------------------------

#[irrigation]
mod struct_update {
    use super::{Point, Seed};

    #[component]
    pub fn rest_base(input: &Seed) -> u32 {
        let base = Point {
            x: input.seed(),
            y: input.width() as u32,
        };
        let point = Point { x: 0, ..base };
        point.y
    }
}

/// The change version of the installed root's output projection.
fn output_version(runtime: &Runtime) -> u64 {
    let mut versions = runtime
        .dump()
        .into_iter()
        .filter(|node| node.kind == "InvocationOutput")
        .map(|node| node.changed_at);
    let version = versions.next().expect("the root has one output projection");
    assert!(
        versions.next().is_none(),
        "the root has one output projection"
    );
    version
}

#[test]
fn a_struct_update_base_is_lowered() {
    let runtime = runtime(struct_update::RestBase);
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Seed>();
            view.set_seed(11);
            view.set_width(3);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<struct_update::RestBase>()
            .expect("outputs"),
        3
    );
    let before = output_version(&runtime);

    // `base.y` is the field the update keeps; the base is read only through
    // `..base`, so a change there must move the result.
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_width(7);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<struct_update::RestBase>()
            .expect("outputs"),
        7
    );
    let after = output_version(&runtime);
    assert!(
        after > before,
        "a changed struct-update base must move the output version"
    );

    // `base.x` is read by the update only to be overwritten: the base moves,
    // the result does not, so the output keeps its version.
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_seed(12);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<struct_update::RestBase>()
            .expect("outputs"),
        7
    );
    assert_eq!(
        output_version(&runtime),
        after,
        "an unchanged output must keep its change version"
    );

    // Restating the value restates nothing.
    runtime
        .edit(|edit| {
            edit.view::<Seed>().set_width(7);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<struct_update::RestBase>()
            .expect("outputs"),
        7
    );
    assert_eq!(
        output_version(&runtime),
        after,
        "a restated value must keep the change version"
    );
}

// ---------------------------------------------------------------------------
// A write inside a match arm is restated when the arm is taken again
// ---------------------------------------------------------------------------

#[view(source)]
pub struct ArmMode {
    mode: u32,
}

#[view(derived)]
pub struct ArmTick(u32);

#[irrigation]
mod arm_write_probe {
    use super::{ArmMode, ArmTick};

    #[component]
    pub fn root(mode: &ArmMode) -> u32 {
        match mode.mode() {
            0 => ArmTick.define_field0(1),
            _ => ArmTick.define_field0(2),
        }
        ArmTick.field0()
    }
}

#[test]
fn a_write_inside_a_match_arm_is_restated_on_re_entry() {
    use irrigation::ViewSchema;

    let runtime = Runtime::new();
    runtime.register_view(<ArmTick as ViewSchema>::DESCRIPTOR);
    runtime.install(arm_write_probe::Root).expect("install");

    assert_eq!(
        runtime
            .read_outputs::<arm_write_probe::Root>()
            .expect("build"),
        1
    );

    runtime
        .edit(|edit| edit.view::<ArmMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<arm_write_probe::Root>()
            .expect("other"),
        2
    );

    runtime
        .edit(|edit| edit.view::<ArmMode>().set_mode(0))
        .expect("edit");
    let rebuilt = runtime
        .read_outputs::<arm_write_probe::Root>()
        .expect("rebuild");
    assert_eq!(rebuilt, 1, "re-entering the arm must restate the write");
}

// ---------------------------------------------------------------------------
// A derived tree View rejects edits; a foreign node answers no discriminant
// ---------------------------------------------------------------------------

#[tree_view(root = Element, view = Rich)]
pub mod rich {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Element {
        Items { values: Vec<Node> },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Node {
        Literal(Literal),
        Name(String),
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Literal {
        Int(i64),
        Text(String),
    }
}

#[tree_view(root = Only, view = Published, kind = derived)]
pub mod published {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Only {
        Leaf { value: u64 },
    }
}

#[irrigation]
mod tree_reads {
    use super::rich;

    #[component]
    pub fn node_count(tree: &rich::Rich) -> usize {
        match tree.root() {
            Some(root) => root.values().len(),
            None => 0,
        }
    }
}

#[test]
#[should_panic(expected = "is derived")]
fn a_derived_tree_view_rejects_edits() {
    let runtime = Runtime::new();
    let _ = runtime.edit(|edit| {
        edit.view::<published::Published>();
    });
}

#[test]
fn a_foreign_node_answers_no_discriminant_and_no_payload() {
    let runtime = runtime(tree_reads::NodeCount);
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<rich::Rich>();
            tree.set_root(&rich::Element::Items {
                values: vec![rich::Node::Literal(rich::Literal::Text(String::from(
                    "leak",
                )))],
            });
        })
        .expect("edit");
    let snapshot = runtime.read().expect("read");
    let root = snapshot.view::<rich::Rich>().root().expect("root");
    let item = root.values()[0].clone();
    let literal = item.literal().expect("literal child");
    assert_eq!(literal.text(), Some(String::from("leak")));

    // The Literal node is type 2; wrapped as an `Node` (type 1) its variant
    // index would pass the old guard and read the wrong slot.
    let foreign = rich::NodeRef::from_tree(literal.tree().clone());
    assert_eq!(foreign.kind(), None);
    assert_eq!(foreign.name(), None);
}

// ---------------------------------------------------------------------------
// A lazy iterator bound to a local is still classified as one (macros-14)
// ---------------------------------------------------------------------------

#[irrigation]
mod bound_adapters {
    use super::Numbers;

    #[component]
    pub fn doubled(numbers: &Numbers) -> u64 {
        let values = numbers.values().to_vec();
        let chain = values.iter();
        chain.map(|value| u64::from(*value) * 2).sum::<u64>()
    }
}

#[test]
fn a_bound_iterator_chain_is_lowered_as_a_chain() {
    let runtime = Runtime::new();
    runtime.install(bound_adapters::Doubled).expect("install");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Numbers>();
            let mut values = view.values();
            values.push(1);
            values.push(2);
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<bound_adapters::Doubled>()
            .expect("outputs"),
        6
    );
}
