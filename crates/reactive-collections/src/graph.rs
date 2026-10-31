//! Incremental general graphs.
//!
//! Topology and payloads are separate observations. `neighbors(v)` reads one
//! node's adjacency unit; `edge(id)` reads one edge's payload unit. Changing a
//! weight therefore does not invalidate a consumer that reads only topology,
//! and adding an unrelated edge does not invalidate unrelated adjacency.
//!
//! Node deletion retracts incident edges. That cost is at least the affected
//! degree, which the plan documents as the deletion rule.

use std::any::Any;
use std::marker::PhantomData;

use reactive_core::ids::{FieldId, ViewId};
use reactive_core::key::any_key;
use reactive_core::value::{Value, value_of};
use reactive_runtime::edit::Edit;
use reactive_runtime::field::{ViewFieldHandle, structure_unit};
use reactive_runtime::helpers::read_unit_value;
use reactive_core::descriptor::{FieldKind, UnitKey, UnitKind};
use reactive_store::field::ViewField;
use reactive_store::graph::{GraphStore, GraphNodeId};
use reactive_store::persistent::{PMap, PSet};

/// Identity of a graph node as a unit key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct NodeKey(pub GraphNodeId);

/// Identity of a graph edge as a unit key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct EdgeKey(pub u64);

/// Topology of a graph View: which nodes and edges exist and where they point.
///
/// Payloads are deliberately absent: a weight edit must not change topology.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct GraphTopology {
    /// Existing node identities.
    pub nodes: PSet<u64>,
    /// Edge identity to `(from, to)`.
    pub edges: PMap<u64, (u64, u64)>,
}

impl GraphTopology {
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
}

/// One edge with its endpoints and payload.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EdgePayload<E> {
    /// Source node.
    pub from: u64,
    /// Destination node.
    pub to: u64,
    /// Edge payload.
    pub value: E,
}

/// A general graph field.
///
/// Parallel edges and self-loops are representable because edges are entities
/// in their own right.
pub struct Graph<N, E>(PhantomData<fn() -> (N, E)>);

impl<N, E> std::fmt::Debug for Graph<N, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Graph")
    }
}

impl<N, E> ViewField for Graph<N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    type Storage = GraphStore<N, E>;

    const KIND: FieldKind = FieldKind::Graph;

    fn empty() -> Self::Storage {
        GraphStore::new()
    }

    fn define(
        storage: &Self::Storage,
        key: &dyn reactive_core::key::AnyKey,
        _value: Value,
    ) -> Self::Storage {
        let _ = key;
        storage.clone()
    }

    fn retract(storage: &Self::Storage, _key: &dyn reactive_core::key::AnyKey) -> Self::Storage {
        storage.clone()
    }

    fn contains(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> bool {
        if let Some(node) = key.as_any().downcast_ref::<NodeKey>() {
            storage.contains_node(node.0)
        } else if let Some(edge) = key.as_any().downcast_ref::<EdgeKey>() {
            storage.edge(edge.0).is_some()
        } else {
            false
        }
    }

    fn get(storage: &Self::Storage, key: &dyn reactive_core::key::AnyKey) -> Option<Value> {
        if let Some(node) = key.as_any().downcast_ref::<NodeKey>() {
            storage.node(node.0).map(|value| value_of(value.clone()))
        } else if let Some(edge) = key.as_any().downcast_ref::<EdgeKey>() {
            storage.edge(edge.0).map(|edge| {
                value_of(EdgePayload {
                    from: edge.from,
                    to: edge.to,
                    value: edge.value.clone(),
                })
            })
        } else {
            None
        }
    }
}

/// Tracked read handle for a graph field.
#[derive(Clone)]
pub struct GraphRef<N, E> {
    view: ViewId,
    field: FieldId,
    content: fn(&dyn Any) -> GraphStore<N, E>,
}

impl<N, E> PartialEq for GraphRef<N, E> {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view && self.field == other.field
    }
}

impl<N, E> Eq for GraphRef<N, E> {}

impl<N, E> std::fmt::Debug for GraphRef<N, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphRef")
            .field("view", &self.view)
            .field("field", &self.field)
            .finish_non_exhaustive()
    }
}

impl<N, E> GraphRef<N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    /// Builds a handle for one graph field.
    #[must_use]
    pub fn new(view: ViewId, field: FieldId, content: fn(&dyn Any) -> GraphStore<N, E>) -> Self {
        Self {
            view,
            field,
            content,
        }
    }

    fn topology_unit(&self) -> UnitKey {
        structure_unit(self.view, self.field)
    }

    fn membership_key(&self, node: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Membership,
            any_key(NodeKey(node)),
        )
    }

    fn node_key(&self, node: u64) -> UnitKey {
        UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(NodeKey(node)))
    }

    fn edge_key(&self, edge: u64) -> UnitKey {
        UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(EdgeKey(edge)))
    }

    fn adjacency_key(&self, node: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Adjacency,
            any_key(NodeKey(node)),
        )
    }

    /// Reads the graph's topology.
    #[must_use]
    pub fn topology(&self) -> GraphTopology {
        read_unit_value(self.topology_unit(), |content| {
            Some(topology_of(&(self.content)(content)))
        })
    }

    /// Node identities in order.
    #[must_use]
    pub fn nodes(&self) -> Vec<u64> {
        self.topology().nodes.iter().copied().collect()
    }

    /// Edge identities in order.
    #[must_use]
    pub fn edges(&self) -> Vec<u64> {
        self.topology().edges.iter().map(|(id, _)| *id).collect()
    }

    /// Number of nodes.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.topology().node_count()
    }

    /// Number of edges.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.topology().edge_count()
    }

    /// Reports node presence.
    #[must_use]
    pub fn contains_node(&self, node: u64) -> bool {
        read_unit_value(self.membership_key(node), |content| {
            Some((self.content)(content).contains_node(node))
        })
    }

    /// Reads one node payload.
    #[must_use]
    pub fn node(&self, node: u64) -> Option<N> {
        if !self.contains_node(node) {
            return None;
        }
        let value: N = read_unit_value(self.node_key(node), |content| {
            (self.content)(content).node(node).cloned()
        });
        Some(value)
    }

    /// Reads one edge with its endpoints.
    #[must_use]
    pub fn edge(&self, edge: u64) -> Option<EdgePayload<E>> {
        let value: Option<EdgePayload<E>> =
            read_unit_value(self.edge_key(edge), |content| {
                Some((self.content)(content).edge(edge).map(|edge| EdgePayload {
                    from: edge.from,
                    to: edge.to,
                    value: edge.value.clone(),
                }))
            });
        value
    }

    /// Reads one edge payload without its endpoints.
    #[must_use]
    pub fn edge_weight(&self, edge: u64) -> Option<E> {
        self.edge(edge).map(|edge| edge.value)
    }

    /// Reads the outgoing adjacency of one node as `(edge, target)` pairs.
    #[must_use]
    pub fn outgoing(&self, node: u64) -> Vec<(u64, u64)> {
        let value: Vec<(u64, u64)> = read_unit_value(self.adjacency_key(node), |content| {
            Some(adjacency_of(&(self.content)(content), node))
        });
        value
    }

    /// Reads the neighbours of one node, preserving edge order.
    #[must_use]
    pub fn neighbors(&self, node: u64) -> Vec<u64> {
        self.outgoing(node).into_iter().map(|(_, to)| to).collect()
    }

    /// Number of outgoing edges.
    #[must_use]
    pub fn out_degree(&self, node: u64) -> usize {
        self.outgoing(node).len()
    }
}

impl<N, E> ViewFieldHandle for Graph<N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    type Handle = GraphRef<N, E>;
    type Editor<'a> = GraphEditor<'a, N, E>;

    fn handle(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> Self::Storage,
    ) -> Self::Handle {
        GraphRef::new(view, field, content)
    }

    fn editor<'a>(
        edit: &'a mut Edit,
        view: ViewId,
        field: FieldId,
        storage: &'a mut Self::Storage,
    ) -> Self::Editor<'a> {
        GraphEditor {
            edit,
            view,
            field,
            storage,
            dirty: Vec::new(),
        }
    }
}

/// Transactional editor for a graph field.
pub struct GraphEditor<'a, N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    edit: &'a mut Edit,
    view: ViewId,
    field: FieldId,
    storage: &'a mut GraphStore<N, E>,
    dirty: Vec<u64>,
}

impl<N, E> std::fmt::Debug for GraphEditor<'_, N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphEditor").finish_non_exhaustive()
    }
}

impl<N, E> GraphEditor<'_, N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    fn membership_key(&self, node: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Membership,
            any_key(NodeKey(node)),
        )
    }

    fn node_key(&self, node: u64) -> UnitKey {
        UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(NodeKey(node)))
    }

    fn edge_key(&self, edge: u64) -> UnitKey {
        UnitKey::entity(self.view, self.field, UnitKind::Value, any_key(EdgeKey(edge)))
    }

    fn adjacency_key(&self, node: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Adjacency,
            any_key(NodeKey(node)),
        )
    }

    fn mark_dirty(&mut self, node: u64) {
        if !self.dirty.contains(&node) {
            self.dirty.push(node);
        }
    }

    /// Adds a node with a payload.
    pub fn add_node(&mut self, node: u64, value: N) {
        *self.storage = self.storage.add_node(node, value.clone());
        self.edit.set(self.membership_key(node), true);
        self.edit.set(self.node_key(node), value);
        self.mark_dirty(node);
    }

    /// Replaces a node payload.
    pub fn set_node(&mut self, node: u64, value: N) {
        if !self.storage.contains_node(node) {
            return;
        }
        *self.storage = self.storage.set_node(node, value.clone());
        self.edit.set(self.node_key(node), value);
    }

    /// Removes a node and retracts its incident edges.
    pub fn remove_node(&mut self, node: u64) {
        if !self.storage.contains_node(node) {
            return;
        }
        let mut incident: Vec<u64> = Vec::new();
        if let Some(outgoing) = self.storage.outgoing(node) {
            incident.extend(outgoing);
        }
        if let Some(incoming) = self.storage.incoming(node) {
            for edge in incoming {
                if !incident.contains(&edge) {
                    incident.push(edge);
                }
            }
        }
        let mut neighbours = Vec::new();
        for edge in &incident {
            if let Some(edge) = self.storage.edge(*edge) {
                neighbours.push(edge.from);
                neighbours.push(edge.to);
            }
        }
        *self.storage = self.storage.remove_node(node);
        self.edit.set(self.membership_key(node), false);
        for edge in incident {
            self.edit
                .set(self.edge_key(edge), Option::<EdgePayload<E>>::None);
        }
        for neighbour in neighbours {
            self.mark_dirty(neighbour);
        }
        self.mark_dirty(node);
    }

    /// Adds an edge, returning its identity.
    pub fn add_edge(&mut self, from: u64, to: u64, value: E) -> u64 {
        let (next, edge) = self.storage.add_edge(from, to, value.clone());
        *self.storage = next;
        self.edit.set(
            self.edge_key(edge),
            Some(EdgePayload {
                from,
                to,
                value,
            }),
        );
        self.mark_dirty(from);
        self.mark_dirty(to);
        edge
    }

    /// Removes an edge.
    pub fn remove_edge(&mut self, edge: u64) {
        let Some(existing) = self.storage.edge(edge) else {
            return;
        };
        let (from, to) = (existing.from, existing.to);
        *self.storage = self.storage.remove_edge(edge);
        self.edit.set(self.edge_key(edge), Option::<EdgePayload<E>>::None);
        self.mark_dirty(from);
        self.mark_dirty(to);
    }

    /// Replaces an edge payload without touching topology.
    pub fn set_edge(&mut self, edge: u64, value: E) {
        let Some(existing) = self.storage.edge(edge) else {
            return;
        };
        let (from, to) = (existing.from, existing.to);
        *self.storage = self.storage.set_edge(edge, value.clone());
        self.edit.set(
            self.edge_key(edge),
            Some(EdgePayload {
                from,
                to,
                value,
            }),
        );
    }

    fn stage_structure(&mut self) {
        let topology = topology_of(self.storage);
        self.edit
            .set(structure_unit(self.view, self.field), topology);
        for node in self.dirty.clone() {
            let adjacency = adjacency_of(self.storage, node);
            self.edit.set(self.adjacency_key(node), adjacency);
        }
    }
}

impl<N, E> Drop for GraphEditor<'_, N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.stage_structure();
    }
}

fn topology_of<N, E>(storage: &GraphStore<N, E>) -> GraphTopology {
    GraphTopology {
        nodes: storage.nodes().map(|(id, _)| id).collect(),
        edges: storage
            .edges()
            .map(|(id, edge)| (id, (edge.from, edge.to)))
            .collect(),
    }
}

fn adjacency_of<N, E>(storage: &GraphStore<N, E>, node: u64) -> Vec<(u64, u64)> {
    storage
        .outgoing(node)
        .map(|edges| {
            edges
                .filter_map(|edge| storage.edge(edge).map(|payload| (edge, payload.to)))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reactive_core::descriptor::FieldDescriptor;

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
    fn graph_field_kind_is_stable() {
        assert_eq!(<Graph<u32, u32> as ViewField>::KIND, FieldKind::Graph);
        let _ = FieldDescriptor {
            name: "edges",
            kind: <Graph<u32, u32> as ViewField>::KIND,
        };
    }

    #[test]
    fn adjacency_tracks_parallel_edges() {
        let graph: GraphStore<(), u32> = GraphStore::new();
        let graph = graph.add_node(1, ()).add_node(2, ());
        let (graph, first) = graph.add_edge(1, 2, 1);
        let (graph, second) = graph.add_edge(1, 2, 2);
        assert_eq!(
            adjacency_of(&graph, 1),
            vec![(first, 2), (second, 2)]
        );
    }
}
