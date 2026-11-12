//! Unit tests for `src/graph.rs`.

use super::*;
use irrigation_core::descriptor::FieldDescriptor;

#[test]
fn graph_unit_eq_compares_one_node_or_edge() {
    let view = ViewId(1);
    let field = FieldId(0);
    let node_key = UnitKey::entity(view, field, UnitKind::Value, any_key(NodeKey(1)));
    let old: GraphStore<u32, u32> = GraphStore::new().add_node(1, 10).add_node(2, 20);
    let same: GraphStore<u32, u32> = GraphStore::new().add_node(1, 10).add_node(2, 20);
    let next: GraphStore<u32, u32> = GraphStore::new().add_node(1, 11).add_node(2, 20);
    assert_eq!(
        <Graph<u32, u32> as ViewField>::unit_eq(&old, &same, &node_key),
        Some(true)
    );
    assert_eq!(
        <Graph<u32, u32> as ViewField>::unit_eq(&old, &next, &node_key),
        Some(false)
    );
    let structure = UnitKey::field(view, field, UnitKind::Structure);
    assert_eq!(
        <Graph<u32, u32> as ViewField>::unit_eq(&old, &next, &structure),
        Some(true)
    );
}

#[test]
fn graph_define_many_batches_node_and_edge_payloads() {
    let view = ViewId(1);
    let field = FieldId(0);
    let drafts = vec![
        (
            UnitKey::entity(view, field, UnitKind::Value, any_key(NodeKey(1))),
            value_of(10u32),
        ),
        (
            UnitKey::entity(view, field, UnitKind::Value, any_key(NodeKey(2))),
            value_of(20u32),
        ),
        (
            UnitKey::entity(view, field, UnitKind::Value, any_key(EdgeKey(10))),
            value_of(Some(EdgePayload {
                from: 1,
                to: 2,
                value: 5u32,
            })),
        ),
    ];
    let storage = <Graph<u32, u32> as ViewField>::define_many(
        &<Graph<u32, u32> as ViewField>::empty(),
        &drafts,
    );
    assert_eq!(storage.node_count(), 2);
    assert_eq!(storage.edge_count(), 1);
    assert_eq!(storage.edge(10).map(|edge| edge.value), Some(5));
    assert_eq!(
        storage.outgoing(1).expect("outgoing").collect::<Vec<_>>(),
        vec![10]
    );
}

#[test]
fn topology_separates_payloads() {
    let graph: GraphStore<u32, u32> = GraphStore::new();
    let graph = graph.add_node(1, 10).add_node(2, 20);
    let (graph, edge) = graph.add_edge(1, 2, 5);
    let topology = topology_of(&graph);
    assert_eq!(topology.node_count(), 2);
    assert_eq!(topology.edge_count(), 1);
    let edited = graph.set_edge(edge, 9);
    assert_eq!(topology_of(&edited), topology);
    assert_eq!(adjacency_of(&edited, 1), vec![(edge, 2)]);
}

#[test]
fn graph_structure_projects_topology() {
    let graph: GraphStore<u32, u32> = GraphStore::new().add_node(1, 10).add_node(2, 20);
    let (graph, _) = graph.add_edge(1, 2, 5);
    let structure = <Graph<u32, u32> as ViewField>::structure(&graph).expect("structure");
    let topology = structure
        .value
        .downcast_ref::<GraphTopology>()
        .expect("topology");
    assert_eq!(topology.node_count(), 2);
    assert_eq!(topology.edge_count(), 1);
    assert!((structure.eq)(&structure.value, &structure.value));
}

#[test]
fn graph_field_kind_is_stable() {
    assert_eq!(<Graph<u32, u32> as ViewField>::KIND, FieldKind::Graph);
    let _ = FieldDescriptor {
        name: "edges",
        kind: <Graph<u32, u32> as ViewField>::KIND,
    };
}

#[test]
fn a_value_cell_answers_the_payload_its_definition_consumed() {
    let graph: GraphStore<u32, u32> = GraphStore::new().add_node(1, 10).add_node(2, 20);
    let (graph, edge) = graph.add_edge(1, 2, 5);

    let node =
        <Graph<u32, u32> as ViewField>::get(&graph, &NodeKey(1)).expect("a node is a value cell");
    assert_eq!(node.downcast_ref::<u32>(), Some(&10));

    // An edge cell holds the `Option<EdgePayload>` shape every writer
    // stages, so its definition consumes exactly what `get` answers.
    let cell = <Graph<u32, u32> as ViewField>::get(&graph, &EdgeKey(edge))
        .expect("an edge is a value cell");
    let payload = cell
        .downcast_ref::<Option<EdgePayload<u32>>>()
        .expect("an edge cell holds Option<EdgePayload>")
        .clone()
        .expect("a present edge");
    assert_eq!((payload.from, payload.to, payload.value), (1, 2, 5));

    let defined = <Graph<u32, u32> as ViewField>::define(
        &graph,
        &EdgeKey(edge),
        value_of(Some(EdgePayload {
            from: 1,
            to: 2,
            value: 9u32,
        })),
    );
    assert_eq!(defined.edge(edge).map(|edge| edge.value), Some(9));

    let removed = <Graph<u32, u32> as ViewField>::define(
        &defined,
        &EdgeKey(edge),
        value_of(Option::<EdgePayload<u32>>::None),
    );
    assert_eq!(removed.edge(edge), None, "the outer none is absence");
    assert!(
        <Graph<u32, u32> as ViewField>::get(&removed, &EdgeKey(edge)).is_none(),
        "an absent edge has no cell to project"
    );
}

#[test]
fn adjacency_tracks_parallel_edges() {
    let graph: GraphStore<(), u32> = GraphStore::new();
    let graph = graph.add_node(1, ()).add_node(2, ());
    let (graph, first) = graph.add_edge(1, 2, 1);
    let (graph, second) = graph.add_edge(1, 2, 2);
    assert_eq!(adjacency_of(&graph, 1), vec![(first, 2), (second, 2)]);
}
