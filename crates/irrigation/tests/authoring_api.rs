//! Public authoring API gates.
//!
//! These tests exercise the surface a user writes: Views, components, edits,
//! snapshots, and the handles generated from declarations. They are deliberately
//! separate from the kernel gates so a failure points at authoring ergonomics
//! rather than at scheduling.

use irrigation::{Context, Runtime, component, view};

#[view(source)]
pub struct Numbers {
    values: List<u32>,
    strict: bool,
}

#[component]
pub fn total(numbers: &Numbers) -> u32 {
    numbers.values().to_vec().iter().sum()
}

#[test]
fn two_editor_scopes_commit_both_writes() {
    let runtime = Runtime::new();
    runtime.install(Total).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<Numbers>().values().push(1);
            edit.view::<Numbers>().values().push(2);
        })
        .expect("edit");
    assert_eq!(
        runtime.snapshot().view::<Numbers>().values(),
        vec![1, 2],
        "the second editor must start from the first editor's staged content"
    );
}

#[test]
fn a_scalar_write_then_a_collection_write_compose() {
    let runtime = Runtime::new();
    runtime.install(Total).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<Numbers>().set_strict(true);
            let mut numbers = edit.view::<Numbers>();
            let mut values = numbers.values();
            values.push(7);
        })
        .expect("edit");
    let snapshot = runtime.snapshot();
    let numbers = snapshot.view::<Numbers>();
    assert!(numbers.strict());
    assert_eq!(numbers.values(), vec![7]);
}

#[test]
fn a_no_op_pair_of_editors_publishes_nothing() {
    let runtime = Runtime::new();
    runtime.install(Total).expect("install");
    let revision = runtime.revision();
    runtime
        .edit(|edit| {
            edit.view::<Numbers>();
            edit.view::<Numbers>();
        })
        .expect("edit");
    assert_eq!(
        runtime.revision(),
        revision,
        "two untouched editors must not publish a revision"
    );
}

#[view(derived)]
pub struct Mirror {
    value: u64,
    scores: Map<u32, u32>,
}

struct MirrorProducer;

impl irrigation::GeneralComponent for MirrorProducer {
    type State = ();
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00AF_0001);
    const NAME: &'static str = "authoring_api::MirrorProducer";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<Numbers as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<Mirror as irrigation::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let strict = Numbers::STRICT.read_in(cx)?;
        cx.write(Mirror::VALUE).set(if strict { 7 } else { 0 });
        cx.write(Mirror::SCORES).entry(1).set(10);
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

#[component]
pub fn read_mirror(mirror: &Mirror) -> (u64, Option<u32>) {
    (mirror.value(), mirror.scores().get(&1))
}

#[test]
fn field_constants_read_and_write_through_a_general_component() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(MirrorProducer)
        .expect("producer");
    runtime.install(ReadMirror).expect("root");
    runtime
        .edit(|edit| edit.view::<Numbers>().set_strict(true))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<ReadMirror>().expect("read"),
        (7, Some(10))
    );
}

#[test]
fn field_constants_carry_the_declared_units() {
    let runtime = Runtime::new();
    runtime.install(Total).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<Numbers>().values().push(4);
        })
        .expect("edit");
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.view::<Numbers>().values(), vec![4]);
    assert_eq!(
        Numbers::VALUES.view(),
        <Numbers as irrigation::ViewSchema>::view_id()
    );
    assert_eq!(Numbers::VALUES.id(), irrigation::FieldId(0));
}

#[component(callable)]
pub fn scaled(numbers: &Numbers, factor: u32) -> u32 {
    numbers.values().to_vec().iter().sum::<u32>() * factor
}

#[component]
pub fn use_scaled(numbers: &Numbers) -> u32 {
    scaled(numbers, 3)
}

#[test]
fn callable_components_accept_view_references_and_owned_parameters() {
    let runtime = Runtime::new();
    runtime.install(UseScaled).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<Numbers>().values().push(2);
            edit.view::<Numbers>().values().push(3);
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<UseScaled>().expect("read"), 15);
    assert_eq!(
        runtime
            .call::<Scaled>(ScaledArgs {
                numbers: Numbers,
                factor: 2,
            })
            .expect("host call"),
        10
    );
}

#[test]
fn installed_handle_reads_without_repeating_the_root_type() {
    let runtime = Runtime::new();
    runtime
        .edit(|edit| {
            edit.view::<Numbers>().values().push(4);
        })
        .expect("seed");
    let app = runtime.install_root(Total).expect("install");
    assert_eq!(app.read().expect("read"), 4);
    assert_eq!(app.revision(), runtime.revision());
    let projected = app.projection(0).expect("projection");
    assert_eq!(projected.downcast_ref::<u32>(), Some(&4));
}

#[view(derived)]
pub struct Gadget {
    value: u64,
}

#[derive(Debug)]
pub struct GadgetProducer;

#[irrigation::general_component(writes(Gadget), reads(Numbers))]
impl GadgetProducer {
    fn run(cx: &mut irrigation::GeneralCx) -> irrigation::Result<()> {
        let strict = Numbers::STRICT.read_in(cx)?;
        cx.write(Gadget::VALUE).set(if strict { 11 } else { 0 });
        Ok(())
    }
}

#[component]
pub fn read_gadget(gadget: &Gadget) -> u64 {
    gadget.value()
}

#[test]
fn general_component_function_form_builds_the_trait_impl() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(GadgetProducer)
        .expect("producer");
    runtime.install(ReadGadget).expect("root");
    runtime
        .edit(|edit| edit.view::<Numbers>().set_strict(true))
        .expect("edit");
    assert_eq!(runtime.read_outputs::<ReadGadget>().expect("read"), 11);
}

#[view(derived)]
pub struct Gadget2 {
    value: u64,
}

#[derive(Debug)]
pub struct GadgetProducer2;

#[irrigation::general_component(writes(Gadget2), reads(Numbers))]
impl irrigation::GeneralComponent for GadgetProducer2 {
    type State = ();
    type Output = ();

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let strict = Numbers::STRICT.read_in(cx)?;
        cx.write(Gadget2::VALUE).set(if strict { 13 } else { 0 });
        Ok(((), ()))
    }
}

#[component]
pub fn read_gadget2(gadget: &Gadget2) -> u64 {
    gadget.value()
}

#[test]
fn general_component_impl_form_fills_identity_and_views() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(GadgetProducer2)
        .expect("producer");
    runtime.install(ReadGadget2).expect("root");
    runtime
        .edit(|edit| edit.view::<Numbers>().set_strict(true))
        .expect("edit");
    assert_eq!(runtime.read_outputs::<ReadGadget2>().expect("read"), 13);
}

#[component]
pub fn first_or_zero(numbers: &Numbers) -> u32 {
    if let Some(first) = numbers.values().get(0) {
        first
    } else {
        0
    }
}

#[component]
pub fn early_exit(numbers: &Numbers) -> u32 {
    if numbers.values().is_empty() {
        return 99;
    }
    numbers.values().to_vec().iter().sum()
}

#[test]
fn if_let_and_return_lower_and_keep_their_result() {
    let runtime = Runtime::new();
    runtime.install(FirstOrZero).expect("if-let root");
    assert_eq!(runtime.read_outputs::<FirstOrZero>().expect("read"), 0);
    runtime
        .edit(|edit| {
            edit.view::<Numbers>().values().push(8);
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<FirstOrZero>().expect("read"), 8);

    let runtime = Runtime::new();
    runtime.install(EarlyExit).expect("return root");
    assert_eq!(runtime.read_outputs::<EarlyExit>().expect("empty"), 99);
    runtime
        .edit(|edit| {
            edit.view::<Numbers>().values().push(4);
            edit.view::<Numbers>().values().push(5);
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<EarlyExit>().expect("read"), 9);
}

#[view(source)]
pub struct NamedList(#[field(name = "entries")] List<u32>);

#[view(source)]
pub struct Scores {
    scores: Map<u32, u32>,
}

#[test]
fn a_tuple_field_can_be_named_and_read_as_value() {
    let runtime = Runtime::new();
    runtime
        .edit(|edit| {
            edit.view::<NamedList>().entries().push(3);
        })
        .expect("edit");
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.view::<NamedList>().entries(), vec![3]);
    assert_eq!(snapshot.view::<NamedList>().value(), vec![3]);
    assert_eq!(
        NamedList::ENTRIES.id(),
        irrigation::FieldId(0),
        "the renamed constant is the same declared field"
    );
}

#[component]
pub fn folded(_numbers: &Numbers) -> u32 {
    Numbers::VALUES.fold(0, |total, value| total + value)
}

#[test]
fn a_list_field_folds_in_order() {
    let runtime = Runtime::new();
    runtime.install(Folded).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<Numbers>().values().push(2);
            edit.view::<Numbers>().values().push(5);
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<Folded>().expect("read"), 7);
}

#[test]
fn typed_and_entry_observers_fire_for_the_declared_units() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runtime = Runtime::new();
    runtime.install(Total).expect("install");
    let seen = Arc::new(AtomicUsize::new(0));
    let typed_seen = Arc::clone(&seen);
    let _typed = runtime
        .observe_view::<Numbers>(move |view| {
            if !view.values().is_empty() {
                typed_seen.fetch_add(1, Ordering::Relaxed);
            }
        })
        .expect("observe");
    let entry_seen = Arc::new(AtomicUsize::new(0));
    let tracked_entry = Arc::clone(&entry_seen);
    let _entry = Scores::SCORES
        .entry(1)
        .observe(&runtime, move |_snapshot| {
            tracked_entry.fetch_add(1, Ordering::Relaxed);
        })
        .expect("entry observe");
    runtime
        .edit(|edit| {
            edit.view::<Numbers>().values().push(1);
        })
        .expect("seed numbers");
    runtime
        .edit(|edit| {
            edit.view::<Numbers>().set_strict(true);
        })
        .expect("strict change");
    runtime
        .edit(|edit| {
            edit.view::<Scores>().scores().insert(1, 10);
        })
        .expect("map seed");
    runtime
        .edit(|edit| {
            edit.view::<Scores>().scores().insert(1, 20);
        })
        .expect("map change");
    assert!(seen.load(Ordering::Relaxed) >= 1);
    assert!(entry_seen.load(Ordering::Relaxed) >= 1);
}

#[component]
pub fn read_context(context: Context<u32, u32>) -> u32 {
    context.read()
}

#[component]
pub fn context_root(_scores: &Scores) -> u32 {
    read_context(Scores::SCORES.entry(1).context())
}

#[test]
fn a_field_entry_can_build_a_tracked_context_argument() {
    let runtime = Runtime::new();
    runtime.install(ContextRoot).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<Scores>().scores().insert(1, 41);
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<ContextRoot>().expect("read"), 41);
    runtime
        .edit(|edit| {
            edit.view::<Scores>().scores().insert(1, 42);
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<ContextRoot>().expect("read"), 42);
}

#[component(root)]
pub fn explicit_root() -> u32 {
    7
}

#[component(callable)]
pub fn zero_call() -> u32 {
    9
}

#[test]
fn explicit_component_kinds_do_not_depend_on_the_parameter_heuristic() {
    let runtime = Runtime::new();
    runtime.install(ExplicitRoot).expect("explicit root");
    assert_eq!(runtime.read_outputs::<ExplicitRoot>().expect("read"), 7);
    assert_eq!(ExplicitRoot::OUTPUT_0, 0);
    let called = runtime
        .call::<ZeroCall>(__ZeroCallArgs {})
        .expect("call a zero-argument callable");
    assert_eq!(called, 9);
    assert_eq!(
        runtime
            .call::<ZeroCall>(ZeroCallArgs {})
            .expect("public args name"),
        9
    );
}

#[test]
fn public_invocation_names_replace_the_underscored_aliases() {
    let runtime = Runtime::new();
    runtime.register_view(<Numbers as irrigation::ViewSchema>::DESCRIPTOR);
    let first = runtime
        .call::<Scaled>(ScaledArgs {
            numbers: Numbers,
            factor: 2,
        })
        .expect("public spec");
    let second = runtime
        .call::<__ScaledSpec>(__ScaledArgs {
            numbers: Numbers,
            factor: 2,
        })
        .expect("legacy alias");
    assert_eq!(first, second);
}

#[test]
fn edit_view_scopes_one_editor_and_commits_it() {
    let runtime = Runtime::new();
    runtime.install(Total).expect("install");
    runtime
        .edit_view::<Numbers>(|inputs| {
            let mut values = inputs.values();
            values.push(6);
            values.push(7);
        })
        .expect("edit");
    assert_eq!(runtime.snapshot().view::<Numbers>().values(), vec![6, 7]);
}

#[view(source)]
pub struct LeftRows {
    rows: Map<u32, u64>,
}

#[view(source)]
pub struct RightRows {
    rows: Map<u32, u64>,
}

#[view(derived)]
pub struct JoinedRows {
    rows: Map<u32, u64>,
}

#[component]
pub fn read_joined(joined: &JoinedRows) -> u64 {
    joined.rows().get(&1).unwrap_or(0)
}

#[test]
fn a_closure_join_publishes_through_typed_map_fields() {
    let runtime = Runtime::new();
    let join = irrigation::Join::new(
        LeftRows::ROWS,
        RightRows::ROWS,
        |left: &u64, right: &u64| left * right,
    )
    .publish_to(JoinedRows::ROWS);
    runtime.install_general_producer(join).expect("join");
    runtime.install(ReadJoined).expect("root");
    runtime
        .edit(|edit| {
            edit.view::<LeftRows>().rows().insert(1, 3);
            edit.view::<RightRows>().rows().insert(1, 4);
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<ReadJoined>().expect("read"), 12);
    runtime
        .edit(|edit| {
            edit.view::<RightRows>().rows().insert(1, 5);
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<ReadJoined>().expect("read"), 15);
}

#[test]
fn two_closure_joins_over_the_same_fields_are_rejected() {
    let runtime = Runtime::new();
    let first = irrigation::Join::new(
        LeftRows::ROWS,
        RightRows::ROWS,
        |left: &u64, right: &u64| left + right,
    )
    .publish_to(JoinedRows::ROWS);
    runtime.install_general_producer(first).expect("first join");
    // A second join over the same fields would define the same outputs; the
    // runtime must reject it rather than merge the two producers.
    let second = irrigation::Join::new(
        LeftRows::ROWS,
        RightRows::ROWS,
        |left: &u64, right: &u64| left + right,
    )
    .publish_to(JoinedRows::ROWS);
    assert!(runtime.install_general_producer(second).is_err());
}

#[view(derived)]
pub struct GraphOut {
    graph: Graph<u32, u32>,
}

#[derive(Debug)]
pub struct GraphProducer;

#[irrigation::general_component(writes(GraphOut), reads(Numbers))]
impl GraphProducer {
    fn run(cx: &mut irrigation::GeneralCx) -> irrigation::Result<()> {
        let _strict = Numbers::STRICT.read_in(cx)?;
        cx.write(GraphOut::GRAPH)
            .define_graph(&[(1, 10), (2, 20)], &[(7, 1, 2, 5)]);
        Ok(())
    }
}

#[component]
pub fn read_graph(graph: &GraphOut) -> (usize, usize, Option<u32>) {
    let graph = graph.graph();
    (graph.nodes().len(), graph.edges().len(), graph.node(2))
}

#[test]
fn a_typed_graph_writer_defines_nodes_edges_and_topology() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(GraphProducer)
        .expect("producer");
    runtime.install(ReadGraph).expect("root");
    runtime
        .edit(|edit| edit.view::<Numbers>().set_strict(true))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<ReadGraph>().expect("read"),
        (2, 1, Some(20))
    );
}

#[view(derived)]
pub struct ReducedOut {
    total: Reduced<irrigation::Sum<u64>>,
}

#[derive(Debug)]
pub struct ReducedProducer;

#[irrigation::general_component(writes(ReducedOut), reads(Numbers))]
impl ReducedProducer {
    fn run(cx: &mut irrigation::GeneralCx) -> irrigation::Result<()> {
        let _strict = Numbers::STRICT.read_in(cx)?;
        cx.write(ReducedOut::TOTAL).define_all([1u64, 2, 3]);
        Ok(())
    }
}

#[component]
pub fn read_reduced(output: &ReducedOut) -> u64 {
    output.total().aggregate()
}

#[test]
fn a_typed_reduced_writer_publishes_a_lawful_aggregate() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(ReducedProducer)
        .expect("producer");
    runtime.install(ReadReduced).expect("root");
    runtime
        .edit(|edit| edit.view::<Numbers>().set_strict(true))
        .expect("edit");
    assert_eq!(runtime.read_outputs::<ReadReduced>().expect("read"), 6);
}

#[test]
fn a_general_root_handle_reads_its_typed_output() {
    let runtime = Runtime::new();
    let app = runtime
        .install_general_root(GadgetProducer2)
        .expect("general root");
    runtime
        .edit(|edit| edit.view::<Numbers>().set_strict(true))
        .expect("edit");
    app.read().expect("read");
    assert_eq!(runtime.snapshot().view::<Gadget2>().value(), 13);
    let producer = runtime
        .install_general_producer(GraphProducer)
        .expect("producer");
    assert_eq!(producer.revision(), runtime.revision());
}
