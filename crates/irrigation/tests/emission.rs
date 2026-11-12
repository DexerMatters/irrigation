//! Emission identity gates: who owns an entity, and when it is reused.
//!
//! A component body's expressions are computation sites, and an emission's
//! identity is its site. These tests pin the three properties the STLC host
//! relies on: one expression gets one identity, a re-execution reuses it, and a
//! different invocation cannot collide with it.

use irrigation::{Runtime, ViewSchema, component, view};

#[view(source)]
pub struct Knob {
    pub value: u32,
}

#[view(derived)]
pub struct Emitted {
    pub graph: irrigation::Graph<u64, u64>,
}

#[view(derived)]
pub struct Shared {
    pub graph: irrigation::Graph<u64, u64>,
}

/// Emits two nodes and reads a source, so an edit re-executes the body while the
/// invocation arguments stay the same.
#[component(callable)]
pub fn emit_pair(knob: &Knob, tag: u64) -> (u64, u64) {
    let _ = knob.value();
    let graph = Emitted.graph();
    let first = graph.emit_node(tag);
    let second = graph.emit_node(tag + 1);
    (first, second)
}

/// Emits one node shared by every invocation that names the same key.
#[component(callable)]
pub fn share(knob: &Knob, key: u64) -> u64 {
    let _ = knob.value();
    Shared.graph().emit_shared(key, key.saturating_mul(10))
}

/// Uses the shared node only when the source says so, so an edit can drop the
/// last caller and retire it.
#[component]
pub fn shared_root(knob: &Knob) -> u64 {
    if knob.value() > 0 { share(knob, 9) } else { 0 }
}

#[component]
pub fn emission_root(knob: &Knob) -> (u64, u64, usize, u64) {
    let (first, second) = emit_pair(knob, 10);
    Emitted.graph().emit_edge(first, second, 7);
    let shared = share(knob, 3);
    let graph = Emitted.graph();
    (first, second, graph.node_count(), shared)
}

fn board() -> Runtime {
    let runtime = Runtime::new();
    runtime.register_view(<Emitted as ViewSchema>::DESCRIPTOR);
    runtime.register_view(<Shared as ViewSchema>::DESCRIPTOR);
    runtime.install(EmissionRoot).expect("install");
    runtime
        .edit(|edit| edit.view::<Knob>().set_value(0))
        .expect("seed");
    let _ = runtime.read_outputs::<EmissionRoot>().expect("demand");
    runtime
}

fn outputs(runtime: &Runtime) -> (u64, u64, usize, u64) {
    runtime.read_outputs::<EmissionRoot>().expect("read")
}

#[test]
fn one_expression_owns_one_entity() {
    let runtime = board();
    let (first, second, nodes, shared) = outputs(&runtime);
    assert_ne!(first, second, "two emission expressions are two entities");
    assert_eq!(nodes, 2, "the graph holds exactly what was emitted");
    let snapshot = runtime.snapshot();
    let graph = snapshot.view::<Shared>();
    assert_eq!(graph.graph_nodes().len(), 1, "one shared key is one node");
    assert_eq!(
        graph.graph_node(shared),
        Some(30),
        "the shared payload is the first caller's"
    );
}

#[test]
fn re_executing_an_invocation_reuses_its_entities() {
    let runtime = board();
    let before = outputs(&runtime);
    runtime
        .edit(|edit| edit.view::<Knob>().set_value(1))
        .expect("edit");
    let after = outputs(&runtime);
    assert_eq!(after, before, "the same invocation keeps its identities");
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.view::<Emitted>().graph_nodes().len(), 2);
    assert_eq!(snapshot.view::<Emitted>().graph_edges().len(), 1);
}

#[test]
fn different_invocations_do_not_collide() {
    let runtime = board();
    let (first, second, _, _) = outputs(&runtime);
    let other = runtime
        .call::<EmitPair>(EmitPairArgs {
            knob: Knob,
            tag: 11,
        })
        .expect("host call");
    assert_ne!(
        other.0, first,
        "a different argument is a different invocation"
    );
    assert_ne!(other.1, second);
}

#[test]
fn equal_shared_keys_share_one_node() {
    let runtime = board();
    let first = runtime
        .call::<Share>(ShareArgs { knob: Knob, key: 3 })
        .expect("same key");
    let second = runtime
        .call::<Share>(ShareArgs { knob: Knob, key: 4 })
        .expect("other key");
    assert_ne!(first, second, "different keys are different shared nodes");
    let snapshot = runtime.snapshot();
    let graph = snapshot.view::<Shared>();
    assert_eq!(graph.graph_nodes().len(), 2);
}

#[test]
fn a_shared_node_retires_with_its_last_caller() {
    let runtime = Runtime::new();
    runtime.register_view(<Shared as ViewSchema>::DESCRIPTOR);
    runtime.install(SharedRoot).expect("install");
    runtime
        .edit(|edit| edit.view::<Knob>().set_value(1))
        .expect("seed");
    let _ = runtime.read_outputs::<SharedRoot>().expect("demand");
    assert_eq!(
        runtime.snapshot().view::<Shared>().graph_nodes().len(),
        1,
        "the shared key is one node while a caller demands it"
    );
    runtime
        .edit(|edit| edit.view::<Knob>().set_value(0))
        .expect("drop the caller");
    let _ = runtime.read_outputs::<SharedRoot>().expect("demand");
    assert!(
        runtime.snapshot().view::<Shared>().graph_nodes().is_empty(),
        "the node retracts when its last caller stops demanding it"
    );
}

#[test]
fn an_emitted_edge_connects_the_emitted_nodes() {
    let runtime = board();
    let (first, second, _, _) = outputs(&runtime);
    let snapshot = runtime.snapshot();
    let graph = snapshot.view::<Emitted>();
    let edge = graph
        .graph_edges()
        .into_iter()
        .find_map(|edge| graph.graph_edge(edge))
        .expect("one edge");
    assert_eq!((edge.from, edge.to, edge.value), (first, second, 7));
}
