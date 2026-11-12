//! Unit tests for `src/graph.rs`.

use super::*;

#[test]
fn graph_delta_store_reports_node_and_edge_moves() {
    let graph: GraphStore<u32, u32> = GraphStore::new();
    let delta = GraphDelta::from_edits(vec![
        GraphEdit::AddNode { id: 1, value: 10 },
        GraphEdit::AddNode { id: 2, value: 20 },
        GraphEdit::AddEdge {
            from: 1,
            to: 2,
            value: 7,
        },
        GraphEdit::SetNode { id: 1, value: 11 },
    ]);
    let (graph, applied) = graph.apply_delta(&delta);
    assert!(applied.content_changed);
    assert_eq!(graph.node_count(), 2);
    assert_eq!(graph.edge_count(), 1);
    assert_eq!(applied.changed.first(), Some(&GraphKey::Node(1)));
    assert_eq!(applied.changed.last(), Some(&GraphKey::Node(1)));
    assert!(
        applied
            .changed
            .iter()
            .any(|key| matches!(key, GraphKey::Edge(_)))
    );

    let edge = graph.edges().next().expect("edge").0;
    let delta = GraphDelta::from_edits(vec![
        GraphEdit::RemoveNode { id: 1 },
        GraphEdit::RemoveEdge { id: edge },
    ]);
    let (graph, applied) = graph.apply_delta(&delta);
    assert_eq!(graph.node_count(), 1);
    assert_eq!(graph.edge_count(), 0);
    assert!(applied.removed.contains(&GraphKey::Node(1)));
    assert!(applied.removed.contains(&GraphKey::Edge(edge)));
}

#[test]
fn adding_and_removing_edges_updates_adjacency() {
    let graph: GraphStore<&str, u32> = GraphStore::new();
    let graph = graph.add_node(1, "a").add_node(2, "b");
    let (graph, edge) = graph.add_edge(1, 2, 7);

    assert_eq!(graph.outgoing(1).unwrap().collect::<Vec<_>>(), vec![edge]);
    assert_eq!(graph.incoming(2).unwrap().collect::<Vec<_>>(), vec![edge]);
    assert_eq!(graph.edge(edge).map(|e| e.value), Some(7));

    let graph = graph.remove_edge(edge);
    assert_eq!(graph.edge_count(), 0);
    assert!(graph.outgoing(1).is_none(), "no empty adjacency entry");
    assert!(graph.incoming(2).is_none(), "no empty adjacency entry");
}

#[test]
fn equality_is_about_the_graph_not_the_index_or_the_allocator() {
    let churned: GraphStore<u32, u32> = {
        let graph = GraphStore::new().add_node(1, 10).add_node(2, 20);
        let (graph, edge) = graph.add_edge(1, 2, 5);
        assert_eq!(edge, 0);
        graph.remove_edge(edge)
    };
    let canonical: GraphStore<u32, u32> = GraphStore::new().add_node(1, 10).add_node(2, 20);
    assert_eq!(
        churned, canonical,
        "a consumed edge id and a drained adjacency are not graph state"
    );

    let (_, edge_again) = churned.add_edge(1, 2, 5);
    assert_eq!(edge_again, 1, "the allocator keeps its high-water mark");
    let (_, first) = canonical.add_edge(1, 2, 5);
    assert_eq!(first, 0);
}

#[test]
fn adjacency_index_has_one_entry_per_endpoint() {
    let graph: GraphStore<(), u32> = GraphStore::new();
    let graph = graph.add_node(1, ()).add_node(2, ());
    let (graph, first) = graph.add_edge(1, 1, 1);
    let (graph, second) = graph.add_edge(1, 2, 2);
    assert_eq!(
        graph.outgoing(1).unwrap().collect::<Vec<_>>(),
        vec![first, second]
    );

    let graph = graph.remove_edge(first);
    assert_eq!(
        graph.outgoing(1).unwrap().collect::<Vec<_>>(),
        vec![second],
        "a self-loop disappears from the outgoing index"
    );
    assert!(
        graph.incoming(1).is_none(),
        "a drained incoming index is dropped"
    );

    let graph = graph.remove_edge(second);
    assert!(graph.outgoing(1).is_none());
    assert!(graph.incoming(2).is_none());
}

#[test]
fn removing_a_node_retracts_incident_edges() {
    let graph: GraphStore<&str, u32> = GraphStore::new();
    let graph = graph.add_node(1, "a").add_node(2, "b").add_node(3, "c");
    let (graph, _) = graph.add_edge(1, 2, 1);
    let (graph, _) = graph.add_edge(3, 2, 2);
    let (graph, _) = graph.add_edge(2, 1, 3);

    let graph = graph.remove_node(2);
    assert_eq!(graph.node_count(), 2);
    assert_eq!(graph.edge_count(), 0);
    assert!(!graph.contains_node(2));
}

#[test]
fn removing_only_a_node_leaves_its_edges_as_dangling_ones() {
    let graph: GraphStore<&str, u32> = GraphStore::new();
    let graph = graph.add_node(1, "a").add_node(2, "b");
    let (graph, edge) = graph.add_edge(1, 2, 7);

    let graph = graph.remove_node_only(1);
    assert!(!graph.contains_node(1));
    assert_eq!(graph.node_count(), 1);
    assert_eq!(graph.edge(edge).map(|e| e.value), Some(7));
    assert_eq!(
        graph.outgoing(1).unwrap().collect::<Vec<_>>(),
        vec![edge],
        "the index is a projection of the edges that survive"
    );
}

#[test]
fn incident_edges_lists_each_edge_once() {
    let graph: GraphStore<(), u32> = GraphStore::new();
    let graph = graph.add_node(1, ()).add_node(2, ());
    let (graph, loop_edge) = graph.add_edge(1, 1, 1);
    let (graph, incoming) = graph.add_edge(2, 1, 2);
    let (graph, outgoing) = graph.add_edge(1, 2, 3);
    assert_eq!(
        graph.incident_edges(1),
        vec![loop_edge, incoming, outgoing],
        "a self-loop is incident once, and edges are in id order"
    );
    assert_eq!(graph.incident_edges(2), vec![incoming, outgoing]);
    assert!(graph.incident_edges(3).is_empty());
}

#[test]
fn parallel_edges_and_self_loops_are_representable() {
    let graph: GraphStore<(), u32> = GraphStore::new();
    let graph = graph.add_node(1, ());
    let (graph, first) = graph.add_edge(1, 1, 1);
    let (graph, second) = graph.add_edge(1, 1, 2);
    assert_ne!(first, second);
    assert_eq!(graph.edge_count(), 2);
    assert_eq!(graph.outgoing(1).unwrap().count(), 2);
}

#[test]
fn payload_edits_do_not_change_topology() {
    let graph: GraphStore<u32, u32> = GraphStore::new();
    let graph = graph.add_node(1, 10).add_node(2, 20);
    let (graph, edge) = graph.add_edge(1, 2, 5);
    let edited = graph.set_edge(edge, 9);
    assert_eq!(edited.edge(edge).map(|e| e.value), Some(9));
    assert_eq!(edited.outgoing(1).unwrap().collect::<Vec<_>>(), vec![edge]);
}
