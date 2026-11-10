//! Persistent general graphs with indexed adjacency.
//!
//! Edges are entities in their own right: parallel edges and self-loops are
//! allowed, and an edge's payload is independent of its endpoints. Deleting a
//! node retracts its incident edges, which costs at least the affected degree.

use irrigation_core::change::Delta;

use crate::delta::{AppliedDelta, DeltaStore};
use crate::persistent::PMap;

/// Identity of an edge in a [`GraphStore`].
pub type EdgeId = u64;

/// Identity of a graph node.
pub type GraphNodeId = u64;

/// One edge with its endpoints and payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge<N, E> {
    /// Source node.
    pub from: GraphNodeId,
    /// Destination node.
    pub to: GraphNodeId,
    /// Edge payload, such as a weight.
    pub value: E,
    /// Phantom marker so node payload types appear in the edge type.
    pub marker: std::marker::PhantomData<N>,
}

impl<N, E> Edge<N, E> {
    /// Builds an edge.
    #[must_use]
    pub fn new(from: GraphNodeId, to: GraphNodeId, value: E) -> Self {
        Self {
            from,
            to,
            value,
            marker: std::marker::PhantomData,
        }
    }
}

/// A persistent directed graph with in/out adjacency indexes.
#[derive(Clone, Debug)]
pub struct GraphStore<N, E> {
    nodes: PMap<GraphNodeId, N>,
    edges: PMap<EdgeId, Edge<N, E>>,
    outgoing: PMap<GraphNodeId, PMap<EdgeId, ()>>,
    incoming: PMap<GraphNodeId, PMap<EdgeId, ()>>,
    next_edge: EdgeId,
}

/// Removes one edge id from an adjacency index.
///
/// The index is a projection of `edges`, so a node keeps an entry exactly while
/// it has an incident edge: removing the last one drops the key instead of
/// leaving an empty map behind.
fn remove_from_index(
    index: &PMap<GraphNodeId, PMap<EdgeId, ()>>,
    node: GraphNodeId,
    edge: EdgeId,
) -> PMap<GraphNodeId, PMap<EdgeId, ()>> {
    let remaining = index
        .get(&node)
        .cloned()
        .unwrap_or_default()
        .remove(&edge)
        .0;
    if remaining.is_empty() {
        index.remove(&node).0
    } else {
        index.insert(node, remaining).0
    }
}

impl<N: PartialEq, E: PartialEq> PartialEq for GraphStore<N, E> {
    fn eq(&self, other: &Self) -> bool {
        // The adjacency indexes are a projection of `edges`, and `next_edge` is
        // the allocator's high-water mark. Neither is part of the graph value:
        // a transaction that adds and removes an edge again, or that consumes
        // an id, leaves the graph unchanged.
        self.nodes == other.nodes && self.edges == other.edges
    }
}

impl<N: Eq, E: Eq> Eq for GraphStore<N, E> {}

impl<N, E> Default for GraphStore<N, E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N, E> GraphStore<N, E> {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: PMap::new(),
            edges: PMap::new(),
            outgoing: PMap::new(),
            incoming: PMap::new(),
            next_edge: 0,
        }
    }

    /// Number of nodes.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of edges.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Looks up a node payload.
    #[must_use]
    pub fn node(&self, id: GraphNodeId) -> Option<&N> {
        self.nodes.get(&id)
    }

    /// Looks up an edge.
    #[must_use]
    pub fn edge(&self, id: EdgeId) -> Option<&Edge<N, E>> {
        self.edges.get(&id)
    }

    /// Iterates over nodes in id order.
    pub fn nodes(&self) -> impl Iterator<Item = (GraphNodeId, &N)> {
        self.nodes.iter().map(|(id, value)| (*id, value))
    }

    /// Iterates over edges in id order.
    pub fn edges(&self) -> impl Iterator<Item = (EdgeId, &Edge<N, E>)> {
        self.edges.iter().map(|(id, edge)| (*id, edge))
    }

    /// Outgoing edge ids of a node, in edge-id order.
    #[must_use]
    pub fn outgoing(&self, node: GraphNodeId) -> Option<impl Iterator<Item = EdgeId> + '_> {
        self.outgoing
            .get(&node)
            .map(|edges| edges.iter().map(|(id, ())| *id))
    }

    /// Incoming edge ids of a node, in edge-id order.
    #[must_use]
    pub fn incoming(&self, node: GraphNodeId) -> Option<impl Iterator<Item = EdgeId> + '_> {
        self.incoming
            .get(&node)
            .map(|edges| edges.iter().map(|(id, ())| *id))
    }

    /// Edge ids incident to one node, each once, in edge-id order.
    ///
    /// A self-loop is both outgoing and incoming; it appears exactly once.
    #[must_use]
    pub fn incident_edges(&self, node: GraphNodeId) -> Vec<EdgeId> {
        let mut edges: Vec<EdgeId> = Vec::new();
        if let Some(outgoing) = self.outgoing(node) {
            edges.extend(outgoing);
        }
        if let Some(incoming) = self.incoming(node) {
            edges.extend(incoming);
        }
        edges.sort_unstable();
        edges.dedup();
        edges
    }

    /// Returns `true` when the node exists.
    #[must_use]
    pub fn contains_node(&self, node: GraphNodeId) -> bool {
        self.nodes.contains_key(&node)
    }
}

impl<N: Clone, E: Clone> GraphStore<N, E> {
    /// Adds a node, returning the new graph.
    #[must_use]
    pub fn add_node(&self, id: GraphNodeId, value: N) -> Self {
        let (nodes, _) = self.nodes.insert(id, value);
        Self {
            nodes,
            ..self.clone()
        }
    }

    /// Replaces a node payload.
    #[must_use]
    pub fn set_node(&self, id: GraphNodeId, value: N) -> Self {
        let (nodes, _) = self.nodes.insert(id, value);
        Self {
            nodes,
            ..self.clone()
        }
    }

    /// Adds an edge, returning the new graph and the new edge id.
    #[must_use]
    pub fn add_edge(&self, from: GraphNodeId, to: GraphNodeId, value: E) -> (Self, EdgeId) {
        let id = self.next_edge;
        let edge = Edge::new(from, to, value);
        let (edges, _) = self.edges.insert(id, edge);
        let (out_for, _) = self.outgoing.insert(
            from,
            self.outgoing
                .get(&from)
                .cloned()
                .unwrap_or_default()
                .insert(id, ())
                .0,
        );
        let (in_for, _) = self.incoming.insert(
            to,
            self.incoming
                .get(&to)
                .cloned()
                .unwrap_or_default()
                .insert(id, ())
                .0,
        );
        (
            Self {
                nodes: self.nodes.clone(),
                edges,
                outgoing: out_for,
                incoming: in_for,
                next_edge: id + 1,
            },
            id,
        )
    }

    /// Inserts or replaces an edge with an explicit identity.
    #[must_use]
    pub fn insert_edge(&self, id: EdgeId, from: GraphNodeId, to: GraphNodeId, value: E) -> Self {
        let mut graph = self.clone();
        if graph.edges.get(&id).is_some() {
            graph = graph.remove_edge(id);
        }
        let edge = Edge::new(from, to, value);
        let (edges, _) = graph.edges.insert(id, edge);
        let (out_for, _) = graph.outgoing.insert(
            from,
            graph
                .outgoing
                .get(&from)
                .cloned()
                .unwrap_or_default()
                .insert(id, ())
                .0,
        );
        let (in_for, _) = graph.incoming.insert(
            to,
            graph
                .incoming
                .get(&to)
                .cloned()
                .unwrap_or_default()
                .insert(id, ())
                .0,
        );
        Self {
            nodes: graph.nodes,
            edges,
            outgoing: out_for,
            incoming: in_for,
            next_edge: graph.next_edge.max(id + 1),
        }
    }

    /// Removes an edge.
    #[must_use]
    pub fn remove_edge(&self, id: EdgeId) -> Self {
        let Some(edge) = self.edges.get(&id).cloned() else {
            return self.clone();
        };
        let (edges, _) = self.edges.remove(&id);
        Self {
            nodes: self.nodes.clone(),
            edges,
            outgoing: remove_from_index(&self.outgoing, edge.from, id),
            incoming: remove_from_index(&self.incoming, edge.to, id),
            next_edge: self.next_edge,
        }
    }

    /// Removes a node and every incident edge.
    ///
    /// The work is at least proportional to the node's degree, as the plan
    /// requires for a documented deletion rule.
    #[must_use]
    pub fn remove_node(&self, id: GraphNodeId) -> Self {
        let mut graph = self.clone();
        for edge in graph.incident_edges(id) {
            graph = graph.remove_edge(edge);
        }
        let (nodes, _) = graph.nodes.remove(&id);
        Self { nodes, ..graph }
    }

    /// Removes one node record, leaving its incident edges in place.
    ///
    /// The node half of an entity retraction: the adjacency indexes are a
    /// projection of `edges`, so the entries that named this node's edges stay —
    /// those edges still exist, they just name a node that is gone. Removing a
    /// node *and* every edge incident to it is [`Self::remove_node`].
    #[must_use]
    pub fn remove_node_only(&self, id: GraphNodeId) -> Self {
        let (nodes, _) = self.nodes.remove(&id);
        Self {
            nodes,
            ..self.clone()
        }
    }

    /// Replaces an edge payload.
    #[must_use]
    pub fn set_edge(&self, id: EdgeId, value: E) -> Self {
        let Some(edge) = self.edges.get(&id).cloned() else {
            return self.clone();
        };
        let updated = Edge::new(edge.from, edge.to, value);
        let (edges, _) = self.edges.insert(id, updated);
        Self {
            nodes: self.nodes.clone(),
            edges,
            outgoing: self.outgoing.clone(),
            incoming: self.incoming.clone(),
            next_edge: self.next_edge,
        }
    }
}

/// One changed graph identity, preserving the node/edge distinction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GraphKey {
    /// A node identity.
    Node(GraphNodeId),
    /// An edge identity.
    Edge(EdgeId),
}

/// One graph edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphEdit<N, E> {
    /// Add a node with a payload.
    AddNode {
        /// Node identity.
        id: GraphNodeId,
        /// Node payload.
        value: N,
    },
    /// Remove a node and its incident edges.
    RemoveNode {
        /// Node identity.
        id: GraphNodeId,
    },
    /// Replace a node payload.
    SetNode {
        /// Node identity.
        id: GraphNodeId,
        /// New payload.
        value: N,
    },
    /// Add an edge.
    AddEdge {
        /// Source node.
        from: GraphNodeId,
        /// Destination node.
        to: GraphNodeId,
        /// Edge payload.
        value: E,
    },
    /// Remove an edge.
    RemoveEdge {
        /// Edge identity.
        id: EdgeId,
    },
    /// Replace an edge payload.
    SetEdge {
        /// Edge identity.
        id: EdgeId,
        /// New payload.
        value: E,
    },
}

/// An ordered script of graph edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphDelta<N, E> {
    edits: Vec<GraphEdit<N, E>>,
}

impl<N, E> Default for GraphDelta<N, E> {
    fn default() -> Self {
        Self { edits: Vec::new() }
    }
}

impl<N, E> GraphDelta<N, E> {
    /// The empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a script from edits.
    #[must_use]
    pub fn from_edits(edits: Vec<GraphEdit<N, E>>) -> Self {
        Self { edits }
    }

    /// Appends an edit.
    pub fn push(&mut self, edit: GraphEdit<N, E>) {
        self.edits.push(edit);
    }
}

impl<N, E> Delta for GraphDelta<N, E>
where
    N: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
{
    type Target = GraphStore<N, E>;

    fn empty() -> Self {
        Self::new()
    }

    fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    fn apply(&self, target: &Self::Target) -> Self::Target {
        let mut graph = target.clone();
        for edit in &self.edits {
            graph = match edit {
                GraphEdit::AddNode { id, value } | GraphEdit::SetNode { id, value } => {
                    graph.add_node(*id, value.clone())
                }
                GraphEdit::RemoveNode { id } => graph.remove_node(*id),
                GraphEdit::AddEdge { from, to, value } => {
                    graph.add_edge(*from, *to, value.clone()).0
                }
                GraphEdit::RemoveEdge { id } => graph.remove_edge(*id),
                GraphEdit::SetEdge { id, value } => graph.set_edge(*id, value.clone()),
            };
        }
        graph
    }

    fn compose(&self, later: &Self) -> Self {
        let mut edits = self.edits.clone();
        edits.extend(later.edits.iter().cloned());
        Self { edits }
    }
}

impl<N, E> DeltaStore for GraphStore<N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    type Key = GraphKey;
    type Delta = GraphDelta<N, E>;

    fn apply_delta(&self, delta: &Self::Delta) -> (Self, AppliedDelta<GraphKey>) {
        let mut next = self.clone();
        let mut applied = AppliedDelta::empty();
        for edit in &delta.edits {
            match edit {
                GraphEdit::AddNode { id, value } | GraphEdit::SetNode { id, value } => {
                    let changed = next.nodes.get(id) != Some(value);
                    next = next.set_node(*id, value.clone());
                    if changed {
                        applied.changed.push(GraphKey::Node(*id));
                        applied.content_changed = true;
                    }
                }
                GraphEdit::RemoveNode { id } => {
                    let incident: Vec<EdgeId> = next.incident_edges(*id);
                    if next.nodes.get(id).is_some() {
                        next = next.remove_node(*id);
                        applied.removed.push(GraphKey::Node(*id));
                        applied
                            .removed
                            .extend(incident.into_iter().map(GraphKey::Edge));
                        applied.content_changed = true;
                    }
                }
                GraphEdit::AddEdge { from, to, value } => {
                    let (graph, id) = next.add_edge(*from, *to, value.clone());
                    next = graph;
                    applied.changed.push(GraphKey::Edge(id));
                    applied.content_changed = true;
                }
                GraphEdit::RemoveEdge { id } => {
                    if next.edges.get(id).is_some() {
                        next = next.remove_edge(*id);
                        applied.removed.push(GraphKey::Edge(*id));
                        applied.content_changed = true;
                    }
                }
                GraphEdit::SetEdge { id, value } => {
                    if let Some(edge) = next.edges.get(id)
                        && edge.value != *value
                    {
                        next = next.set_edge(*id, value.clone());
                        applied.changed.push(GraphKey::Edge(*id));
                        applied.content_changed = true;
                    }
                }
            }
        }
        (next, applied)
    }
}

#[cfg(test)]
mod tests {
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
}
