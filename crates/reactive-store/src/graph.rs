//! Persistent general graphs with indexed adjacency.
//!
//! Edges are entities in their own right: parallel edges and self-loops are
//! allowed, and an edge's payload is independent of its endpoints. Deleting a
//! node retracts its incident edges, which costs at least the affected degree.

use reactive_core::change::Delta;

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
        let (out_for, _) = self
            .outgoing
            .insert(from, self.outgoing.get(&from).cloned().unwrap_or_default().insert(id, ()).0);
        let (in_for, _) = self
            .incoming
            .insert(to, self.incoming.get(&to).cloned().unwrap_or_default().insert(id, ()).0);
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

    /// Removes an edge.
    #[must_use]
    pub fn remove_edge(&self, id: EdgeId) -> Self {
        let Some(edge) = self.edges.get(&id).cloned() else {
            return self.clone();
        };
        let (edges, _) = self.edges.remove(&id);
        let outgoing = self
            .outgoing
            .get(&edge.from)
            .cloned()
            .unwrap_or_default()
            .remove(&id)
            .0;
        let incoming = self
            .incoming
            .get(&edge.to)
            .cloned()
            .unwrap_or_default()
            .remove(&id)
            .0;
        Self {
            nodes: self.nodes.clone(),
            edges,
            outgoing: self.outgoing.insert(edge.from, outgoing).0,
            incoming: self.incoming.insert(edge.to, incoming).0,
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
        let mut incident: Vec<EdgeId> = Vec::new();
        if let Some(outgoing) = graph.outgoing.get(&id) {
            incident.extend(outgoing.iter().map(|(edge, ())| *edge));
        }
        if let Some(incoming) = graph.incoming.get(&id) {
            for (edge, ()) in incoming.iter() {
                if !incident.contains(edge) {
                    incident.push(*edge);
                }
            }
        }
        for edge in incident {
            graph = graph.remove_edge(edge);
        }
        let (nodes, _) = graph.nodes.remove(&id);
        let (outgoing, _) = graph.outgoing.remove(&id);
        let (incoming, _) = graph.incoming.remove(&id);
        Self {
            nodes,
            edges: graph.edges,
            outgoing,
            incoming,
            next_edge: graph.next_edge,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(graph.outgoing(1).unwrap().count(), 0);
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
