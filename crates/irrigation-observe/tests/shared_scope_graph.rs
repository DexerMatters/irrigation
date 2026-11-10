//! The shared-graph fixture: one Graph View, two writers, and tag filtering.
//!
//! The runtime owns a graph field's `Structure` unit and refreshes it from the
//! committed content, so the topology is the whole graph, not a name-only
//! manifest. A name resolver therefore reads the topology, filters the IDs that
//! belong to its subgraph, and only then reads those payloads. This fixture
//! proves that the two writers compose, that the filtering keeps the resolver's
//! answer stable across type edits, and that the projection reports both
//! contributors and the arbitration counters.

use irrigation::{Runtime, RuntimeStats, ViewSchema, irrigation, view};
use irrigation_observe::wire::StatsWire;

/// Name IDs live below 1_000; type IDs start there. The real host uses tag bits
/// and an edge-kind tag; the range keeps this fixture readable.
const NAME_MAX: u64 = 1_000;

#[view(source)]
pub struct Mode {
    names: u8,
    types: u8,
}

#[view(derived)]
pub struct Shared {
    graph: irrigation::Graph<u64, u64>,
}

#[irrigation]
mod fixture {
    use super::{Mode, NAME_MAX, Shared};

    /// The name writer: nodes 1..=3, edge 10.
    #[component]
    pub fn names(tag: u8) -> u32 {
        let _ = tag;
        let graph = Shared.graph();
        let extra = u64::from(Mode.names());
        graph.define_node(1, 10);
        graph.define_node(2, 20);
        graph.define_node(3, 30);
        if extra > 0 {
            graph.define_node(3 + extra, 40);
        }
        graph.define_edge(10, 1, 2, 100);
        3
    }

    /// The type writer: type nodes and typing edges, no name entities.
    #[component]
    pub fn types(tag: u8) -> u32 {
        let _ = tag;
        let graph = Shared.graph();
        let extra = u64::from(Mode.types());
        graph.define_node(1_000, 1_000);
        graph.define_node(1_001, 1_010);
        if extra > 0 {
            graph.define_node(1_002, 1_020);
        }
        graph.define_edge(2_000, 1, 1_000, 5);
        graph.define_edge(2_001, 1_000, 1_001, 7);
        if extra > 0 {
            graph.define_edge(2_002, 1_001, 1_002, 9);
        }
        2
    }

    /// A resolver-like reader: read the topology, filter name IDs, read only
    /// those payloads.
    #[component]
    pub fn names_only(tag: u8) -> (Vec<u64>, Vec<u64>) {
        let _ = tag;
        let graph = Shared.graph();
        let nodes = graph
            .nodes()
            .into_iter()
            .filter(|id| *id < NAME_MAX)
            .collect();
        let edges = graph
            .edges()
            .into_iter()
            .filter(|id| *id < NAME_MAX)
            .collect();
        (nodes, edges)
    }

    #[component]
    pub fn root(mode: &Mode) -> (Vec<u64>, Vec<u64>, Vec<u64>, Vec<u64>, u32, u32) {
        let _ = mode;
        let name_count = names(1);
        let type_count = types(2);
        let (name_nodes, name_edges) = names_only(3);
        let content = Shared.graph().topology();
        (
            name_nodes,
            name_edges,
            content.nodes.iter().copied().collect(),
            content.edges.iter().map(|(id, _)| *id).collect(),
            name_count,
            type_count,
        )
    }
}

fn runtime() -> Runtime {
    let runtime = Runtime::new();
    runtime.register_view(<Shared as ViewSchema>::DESCRIPTOR);
    runtime.install(fixture::Root).expect("install");
    runtime
        .edit(|edit| {
            let mut mode = edit.view::<Mode>();
            mode.set_names(0);
            mode.set_types(0);
        })
        .expect("seed");
    runtime
}

/// The revision a component last **executed** at.
///
/// `verified_at` also moves when a component is revalidated without running;
/// the claim here is about waking the writer, so execution is the right fact.
fn executed(runtime: &Runtime, name: &str) -> irrigation::Revision {
    let suffix = format!("::fixture::{name}");
    runtime
        .component_facts()
        .into_iter()
        .find(|component| component.name.ends_with(&suffix))
        .map_or_else(
            || panic!("no component for {name}"),
            |component| component.executed_at,
        )
}

#[test]
fn the_content_is_the_union_and_the_reader_filters_names() {
    let runtime = runtime();
    let (name_nodes, name_edges, content_nodes, content_edges, ..) =
        runtime.read_outputs::<fixture::Root>().expect("read");
    assert_eq!(name_nodes, vec![1, 2, 3]);
    assert_eq!(name_edges, vec![10]);
    assert_eq!(content_nodes, vec![1, 2, 3, 1000, 1001]);
    assert_eq!(content_edges, vec![10, 2000, 2001]);

    let snapshot = runtime.read().expect("snapshot");
    let view = snapshot.view::<Shared>();
    assert_eq!(view.graph_nodes(), content_nodes);
    assert_eq!(view.graph_edges(), content_edges);
    assert_eq!(view.graph_edge(2000).expect("typing edge").from, 1);
    assert_eq!(view.graph_edge(2000).expect("typing edge").to, 1000);
}

#[test]
fn both_writers_are_contributors() {
    let runtime = runtime();
    let _ = runtime.read_outputs::<fixture::Root>().expect("read");
    let facts = runtime
        .view_facts()
        .into_iter()
        .find(|facts| facts.name.ends_with("Shared"))
        .expect("the shared View");
    assert_eq!(facts.contributors.len(), 2, "{:?}", facts.contributors);
    assert!(facts.producer.is_some(), "a primary contributor remains");
    let components = runtime.component_facts();
    let names: Vec<&str> = facts
        .contributors
        .iter()
        .map(|id| {
            components
                .iter()
                .find(|component| component.id == *id)
                .map_or("?", |component| component.name)
        })
        .collect();
    assert!(names.iter().any(|name| name.ends_with("::names")));
    assert!(names.iter().any(|name| name.ends_with("::types")));
    assert!(
        components
            .iter()
            .filter(|component| {
                component.name.ends_with("::names") || component.name.ends_with("::types")
            })
            .all(|component| component
                .writes
                .iter()
                .any(|view| view == &Shared::view_id())),
        "both writers report the shared View"
    );
}

#[test]
fn a_name_edit_does_not_wake_the_type_writer() {
    let runtime = runtime();
    let _ = runtime.read_outputs::<fixture::Root>().expect("read");
    let types_before = executed(&runtime, "types");
    let names_before = executed(&runtime, "names");

    runtime
        .edit(|edit| edit.view::<Mode>().set_names(1))
        .expect("edit names");
    let (name_nodes, _, content_nodes, _, _, _) =
        runtime.read_outputs::<fixture::Root>().expect("read");
    assert_eq!(name_nodes, vec![1, 2, 3, 4]);
    assert_eq!(content_nodes, vec![1, 2, 3, 4, 1000, 1001]);
    assert!(executed(&runtime, "names") > names_before);
    assert_eq!(executed(&runtime, "types"), types_before);
}

#[test]
fn a_type_edit_keeps_the_reader_answer_and_does_not_wake_the_name_writer() {
    let runtime = runtime();
    let before = runtime.read_outputs::<fixture::Root>().expect("read");
    let names_before = executed(&runtime, "names");
    let types_before = executed(&runtime, "types");
    let reader_before = executed(&runtime, "names_only");

    runtime
        .edit(|edit| edit.view::<Mode>().set_types(1))
        .expect("edit types");
    let after = runtime.read_outputs::<fixture::Root>().expect("read");
    // Name facts are unchanged; the content grew.
    assert_eq!(after.0, before.0);
    assert_eq!(after.1, before.1);
    assert_eq!(after.2, vec![1, 2, 3, 1000, 1001, 1002]);
    assert_eq!(after.3, vec![10, 2000, 2001, 2002]);
    assert_eq!(executed(&runtime, "names"), names_before);
    assert!(executed(&runtime, "types") > types_before);
    // The reader reads the topology, which moved, so it re-executes; its answer
    // is unchanged, which is the cutoff the resolver relies on.
    assert!(executed(&runtime, "names_only") > reader_before);
}

#[test]
fn arbitration_counters_are_exposed() {
    let runtime = runtime();
    let _ = runtime.read_outputs::<fixture::Root>().expect("read");
    let stats: RuntimeStats = runtime.stats();
    assert!(stats.write_claims > 0, "{stats:?}");
    let wire = StatsWire::from_stats(&stats);
    assert_eq!(wire.write_claims, stats.write_claims);
    assert_eq!(wire.deferred_writes, stats.deferred_writes);
    assert_eq!(wire.demand_retries, stats.demand_retries);
    assert_eq!(wire.reader_waits, stats.reader_waits);
}
