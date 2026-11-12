//! Incremental general graphs.
//!
//! Topology and payloads are separate observations. `neighbors(v)` reads one
//! node's adjacency unit; `edge(id)` reads one edge's payload unit. Changing a
//! weight therefore does not invalidate a consumer that reads only topology,
//! and adding an unrelated edge does not invalidate unrelated adjacency.
//!
//! Node deletion retracts incident edges. That cost is at least the affected
//! degree, which the plan documents as the deletion rule; the whole-field verbs
//! pay it. A single node unit is retracted on its own, so the edge units it did
//! not name keep their payloads and the content keeps the edge as a dangling one.

use std::any::Any;
use std::marker::PhantomData;

use irrigation_core::descriptor::{FieldKind, UnitKey, UnitKind, WriteScope};
use irrigation_core::error::{ComputeError, PreconditionReason};
use irrigation_core::ids::{FieldId, ViewId};
use irrigation_core::key::any_key;
use irrigation_core::ops::panic_engine;
use irrigation_core::value::{Value, value_of};
use irrigation_runtime::edit::Edit;
use irrigation_runtime::field::{ViewFieldHandle, structure_unit};
use irrigation_runtime::helpers::{
    PortPrecondition, define_unit, read_unit_value, require_present, retract_unit,
    try_read_unit_value, write_group,
};
use irrigation_store::field::ViewField;
use irrigation_store::graph::{GraphNodeId, GraphStore};
use irrigation_store::persistent::{PMap, PSet};

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
#[derive(Clone, PartialEq, Eq)]
pub struct EdgePayload<E> {
    /// Source node.
    pub from: u64,
    /// Destination node.
    pub to: u64,
    /// Edge payload.
    pub value: E,
}

/// Prints the payload alone, **by design**.
///
/// The observation wire uses this as a graph edge's canvas label, so printing the
/// endpoints here would put `EdgePayload { from: 1, to: 2, .. }` on the canvas in
/// place of the edge's own label. The cost is that `assert_eq!` on an edge shows
/// only the payload; print `from`/`to` explicitly when that matters.
impl<E: std::fmt::Debug> std::fmt::Debug for EdgePayload<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.value, f)
    }
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

/// One graph value cell's payload: the graph's cell grammar, stated once.
///
/// A `NodeKey` cell holds the node payload `N`. An `EdgeKey` cell holds
/// `Option<EdgePayload<E>>`: the outer `None` names absence, so a whole-cell
/// write can remove an edge without a retraction, while a present cell holds
/// the whole record. [`ViewField::define`] consumes that shape, and
/// [`ViewField::get`], the read accessors' initializers and the runtime's own
/// projection of committed content (the host-write mask included) all produce
/// it. Presence is the one unit kind the family does not answer: the generated
/// dispatcher reads it through [`ViewField::contains`] and writes it through
/// the payload draft, never through `get` or `define`.
fn node_cell<N: Clone, E>(storage: &GraphStore<N, E>, node: u64) -> Option<N> {
    storage.node(node).cloned()
}

fn edge_cell<N, E: Clone>(storage: &GraphStore<N, E>, edge: u64) -> Option<EdgePayload<E>> {
    storage.edge(edge).map(|edge| EdgePayload {
        from: edge.from,
        to: edge.to,
        value: edge.value.clone(),
    })
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

    fn len(storage: &Self::Storage) -> Option<usize> {
        Some(storage.node_count())
    }

    fn define(
        storage: &Self::Storage,
        key: &dyn irrigation_core::key::AnyKey,
        value: Value,
    ) -> Self::Storage {
        if let Some(node) = key.as_any().downcast_ref::<NodeKey>() {
            if let Some(payload) = value.downcast_ref::<N>() {
                return storage.set_node(node.0, payload.clone());
            }
        }
        if let Some(edge) = key.as_any().downcast_ref::<EdgeKey>() {
            if let Some(payload) = value.downcast_ref::<Option<EdgePayload<E>>>() {
                return match payload {
                    Some(payload) => {
                        storage.insert_edge(edge.0, payload.from, payload.to, payload.value.clone())
                    }
                    None => storage.remove_edge(edge.0),
                };
            }
        }
        storage.clone()
    }

    fn retract(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> Self::Storage {
        if let Some(node) = key.as_any().downcast_ref::<NodeKey>() {
            // Retracting one entity retires one entity. The incident edge units
            // are still installed and keep serving their payloads, so removing
            // them here would leave cells the content no longer backs; the
            // whole-field verbs stage every edge unit and can cascade.
            return storage.remove_node_only(node.0);
        }
        if let Some(edge) = key.as_any().downcast_ref::<EdgeKey>() {
            return storage.remove_edge(edge.0);
        }
        storage.clone()
    }

    fn contains(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> bool {
        if let Some(node) = key.as_any().downcast_ref::<NodeKey>() {
            storage.contains_node(node.0)
        } else if let Some(edge) = key.as_any().downcast_ref::<EdgeKey>() {
            storage.edge(edge.0).is_some()
        } else {
            false
        }
    }

    fn structure(storage: &Self::Storage) -> Option<irrigation_core::descriptor::Structure> {
        Some(irrigation_core::descriptor::Structure {
            value: value_of(topology_of(storage)),
            eq: |left, right| {
                left.downcast_ref::<GraphTopology>() == right.downcast_ref::<GraphTopology>()
            },
        })
    }

    fn get(storage: &Self::Storage, key: &dyn irrigation_core::key::AnyKey) -> Option<Value> {
        if let Some(node) = key.as_any().downcast_ref::<NodeKey>() {
            node_cell(storage, node.0).map(value_of)
        } else if let Some(edge) = key.as_any().downcast_ref::<EdgeKey>() {
            edge_cell(storage, edge.0).map(|cell| value_of(Some(cell)))
        } else {
            None
        }
    }

    fn define_many(storage: &Self::Storage, drafts: &[(UnitKey, Value)]) -> Self::Storage {
        if drafts.iter().any(|(key, _)| key.entity.is_none()) {
            let mut next = storage.clone();
            for (key, value) in drafts {
                let entity = key.entity.as_deref().unwrap_or(&());
                if key.kind == UnitKind::Membership && value.downcast_ref::<bool>() == Some(&false)
                {
                    next = Self::retract(&next, entity);
                } else if key.kind != UnitKind::Membership {
                    next = Self::define(&next, entity, value.clone());
                }
            }
            return next;
        }

        let mut next = storage.clone();
        for (key, value) in drafts {
            let Some(entity) = key.entity.as_deref() else {
                continue;
            };
            if let Some(node) = entity.as_any().downcast_ref::<NodeKey>() {
                if key.kind == UnitKind::Membership {
                    if value.downcast_ref::<bool>() == Some(&false) {
                        next = next.remove_node_only(node.0);
                    }
                } else if let Some(payload) = value.downcast_ref::<N>() {
                    next = next.set_node(node.0, payload.clone());
                }
                continue;
            }
            if let Some(edge) = entity.as_any().downcast_ref::<EdgeKey>() {
                if key.kind == UnitKind::Membership {
                    if value.downcast_ref::<bool>() == Some(&false) {
                        next = next.remove_edge(edge.0);
                    }
                } else if let Some(payload) = value.downcast_ref::<Option<EdgePayload<E>>>() {
                    next = match payload {
                        Some(payload) => next.insert_edge(
                            edge.0,
                            payload.from,
                            payload.to,
                            payload.value.clone(),
                        ),
                        None => next.remove_edge(edge.0),
                    };
                }
            }
        }
        next
    }

    fn unit_eq(old: &Self::Storage, new: &Self::Storage, unit: &UnitKey) -> Option<bool> {
        if unit.entity.is_none() && unit.kind == UnitKind::Value {
            return Some(old == new);
        }
        if unit.kind == UnitKind::Structure {
            return Some(topology_of(old) == topology_of(new));
        }
        let entity = unit.entity.as_deref()?;
        if let Some(node) = entity.as_any().downcast_ref::<NodeKey>() {
            if unit.kind == UnitKind::Adjacency {
                return Some(adjacency_of(old, node.0) == adjacency_of(new, node.0));
            }
            if unit.kind == UnitKind::IncomingAdjacency {
                return Some(
                    incoming_adjacency_of(old, node.0) == incoming_adjacency_of(new, node.0),
                );
            }
            return match unit.kind {
                UnitKind::Value => Some(old.node(node.0) == new.node(node.0)),
                UnitKind::Membership => {
                    Some(old.contains_node(node.0) == new.contains_node(node.0))
                }
                _ => None,
            };
        }
        if let Some(edge) = entity.as_any().downcast_ref::<EdgeKey>() {
            return match unit.kind {
                UnitKind::Value => {
                    let old_edge = old
                        .edge(edge.0)
                        .map(|edge| (edge.from, edge.to, edge.value.clone()));
                    let new_edge = new
                        .edge(edge.0)
                        .map(|edge| (edge.from, edge.to, edge.value.clone()));
                    Some(old_edge == new_edge)
                }
                UnitKind::Membership => {
                    Some(old.edge(edge.0).is_some() == new.edge(edge.0).is_some())
                }
                _ => None,
            };
        }
        None
    }

    fn unit_moves_content(unit: &UnitKey) -> bool {
        // Topology, membership and adjacency project the node/edge content.
        unit.kind == UnitKind::Value && unit.entity.is_some()
    }
}

/// The endpoints of one edge write, and whether the write requires them.
#[derive(Clone, Copy, Debug)]
struct EdgeEnds {
    from: u64,
    to: u64,
    required: bool,
}

impl EdgeEnds {
    fn required(from: u64, to: u64) -> Self {
        Self {
            from,
            to,
            required: true,
        }
    }

    fn trusted(from: u64, to: u64) -> Self {
        Self {
            from,
            to,
            required: false,
        }
    }
}

/// Tracked read handle for a graph field.
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

impl<N, E> Copy for GraphRef<N, E> {}

impl<N, E> Clone for GraphRef<N, E> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<N, E> GraphRef<N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    /// Builds a handle for one graph field.
    #[must_use]
    pub const fn new(
        view: ViewId,
        field: FieldId,
        content: fn(&dyn Any) -> GraphStore<N, E>,
    ) -> Self {
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
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Value,
            any_key(NodeKey(node)),
        )
    }

    fn edge_key(&self, edge: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Value,
            any_key(EdgeKey(edge)),
        )
    }

    fn edge_membership_key(&self, edge: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Membership,
            any_key(EdgeKey(edge)),
        )
    }

    fn contains_edge(&self, edge: u64) -> bool {
        read_unit_value(self.edge_membership_key(edge), |content| {
            Some((self.content)(content).edge(edge).is_some())
        })
    }

    fn adjacency_key(&self, node: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Adjacency,
            any_key(NodeKey(node)),
        )
    }

    fn incoming_adjacency_key(&self, node: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::IncomingAdjacency,
            any_key(NodeKey(node)),
        )
    }

    /// Reads the graph's topology: its nodes and edges in order.
    ///
    /// The answer is the `Structure` unit's value, which the field's owner
    /// publishes with the graph it defines. There is no separate published
    /// manifest to compare it against: the abandoned `define_topology` /
    /// `content_topology` split was removed, and `nodes` / `edges` read this
    /// same whole graph.
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
    ///
    /// The payload read is total: a member whose cells disagree — a
    /// membership-only draft, or a retraction between the two reads — is
    /// absent, not an engine failure.
    #[must_use]
    pub fn node(&self, node: u64) -> Option<N> {
        if !self.contains_node(node) {
            return None;
        }
        try_read_unit_value(self.node_key(node), |content| {
            node_cell(&(self.content)(content), node)
        })
    }

    /// Reads one edge with its endpoints.
    ///
    /// As in [`Self::node`], a retraction between the membership read and the
    /// payload read is absent rather than an engine failure.
    #[must_use]
    pub fn edge(&self, edge: u64) -> Option<EdgePayload<E>> {
        if !self.contains_edge(edge) {
            return None;
        }
        try_read_unit_value(self.edge_key(edge), |content| {
            Some(edge_cell(&(self.content)(content), edge))
        })
        .flatten()
    }

    /// Reads one edge payload without its endpoints.
    #[must_use]
    pub fn edge_weight(&self, edge: u64) -> Option<E> {
        self.edge(edge).map(|edge| edge.value)
    }

    /// Defines one node and its membership.
    ///
    /// The write claims the node's owner pair. If the node already exists, this
    /// is an upsert; use a checked variant if the caller needs absence.
    #[track_caller]
    pub fn define_node(&self, node: u64, payload: N) {
        self.stage_node(node, payload, "graph_node", &[]);
    }

    /// Stages one node's payload and membership under `label`.
    fn stage_node(
        &self,
        node: u64,
        payload: N,
        label: &'static str,
        preconditions: &[PortPrecondition<'_>],
    ) {
        let node_unit = self.node_key(node);
        let membership = self.membership_key(node);
        write_group(
            WriteScope::entity(&membership),
            label,
            preconditions,
            |_| (),
            |()| {
                // The entity draft installs the node's membership cell in the
                // same commit. A `Membership = true` draft beside it would
                // restate a presence the payload already states, and a graph
                // spells presence through its payload, so the writer stages
                // one draft.
                define_unit(node_unit, payload, label);
            },
        );
    }

    /// Defines one edge, requiring both endpoint nodes to be present.
    ///
    /// The requirement is structural: when the endpoints are absent the edge
    /// write defers to the end of the demand, so a root body may create the
    /// nodes after the edge. A checked caller can fail instead by using the
    /// explicit requirement API.
    #[track_caller]
    pub fn define_edge(&self, edge: u64, from: u64, to: u64, payload: E) {
        self.stage_edge(
            edge,
            EdgeEnds::required(from, to),
            payload,
            "graph_edge",
            &[],
        );
    }

    /// Requires both endpoint memberships at commit.
    ///
    /// The endpoints belong to other owner pairs, so reading them here is not a
    /// self-read: this read is the edge write's dependency on a host provider
    /// that creates them. The condition itself is staged as a requirement, never
    /// read against the commit that installs the edge.
    fn require_endpoints(&self, from: u64, to: u64) {
        let from_port = self.membership_key(from);
        let to_port = self.membership_key(to);
        if !(self.contains_node(from) && self.contains_node(to)) {
            let _ =
                irrigation_runtime::helpers::provide_ports(&[from_port.clone(), to_port.clone()]);
            let _ = self.contains_node(from) && self.contains_node(to);
        }
        require_present(&[from_port, to_port], "graph edge endpoints exist");
    }

    /// Stages one edge's payload and membership under `label`.
    fn stage_edge(
        &self,
        edge: u64,
        ends: EdgeEnds,
        payload: E,
        label: &'static str,
        preconditions: &[PortPrecondition<'_>],
    ) {
        let edge_unit = self.edge_key(edge);
        let membership = self.edge_membership_key(edge);
        write_group(
            WriteScope::entity(&membership),
            label,
            preconditions,
            |_| (),
            |()| {
                if ends.required {
                    self.require_endpoints(ends.from, ends.to);
                }
                // As in `stage_node`: the payload draft is what installs the
                // edge's membership, so no presence draft is staged.
                define_unit(
                    edge_unit,
                    Some(EdgePayload {
                        from: ends.from,
                        to: ends.to,
                        value: payload,
                    }),
                    label,
                );
            },
        );
    }

    /// Defines one edge without publishing an endpoint requirement.
    ///
    /// Use this when the same write group defines both endpoints (a builder
    /// emitting a complete graph fragment). [`Self::define_edge`] is the checked
    /// form for an edge whose endpoints may come from another writer or a port
    /// provider (`irrigation_runtime::PortProvider`, which this crate does not
    /// depend on); it waits or defers instead of trusting the caller.
    ///
    /// The edge still claims its own port, so two writers cannot define the
    /// same edge. The caller is responsible for both node definitions being in
    /// the same demand; the page and snapshots only draw edges whose endpoints
    /// exist in the committed content.
    #[track_caller]
    pub fn define_edge_trusted(
        &self,
        edge: u64,
        from: u64,
        to: u64,
        payload: E,
        label: &'static str,
    ) {
        self.stage_edge(edge, EdgeEnds::trusted(from, to), payload, label, &[]);
    }

    /// Defines one node whose identity is the emission expression that runs it.
    ///
    /// The identity is [`irrigation_core::ops::emission_id`]: a computation site
    /// interned per `(invocation scope, expression site)`. A re-execution of the
    /// same invocation reuses the site, and therefore the entity; a different
    /// invocation gets a different one. The caller never names the id.
    ///
    /// Emit from a statement of its own. A call inside an iterator closure shares
    /// the enclosing expression's site, so two emissions in one closure would be
    /// one entity.
    #[track_caller]
    pub fn emit_node(&self, payload: N) -> u64 {
        let node = irrigation_core::ops::emission_id();
        self.define_node(node, payload);
        node
    }

    /// Defines one edge whose identity is the emission expression that runs it.
    ///
    /// The checked form: both endpoints were committed by earlier emissions, which
    /// is the normal shape when a component emits its own node, recurses, and then
    /// links the child. Use [`Self::emit_edge_trusted`] only when an endpoint is
    /// staged by the same expression.
    #[track_caller]
    pub fn emit_edge(&self, from: u64, to: u64, payload: E) -> u64 {
        let edge = irrigation_core::ops::emission_id();
        self.define_edge(edge, from, to, payload);
        edge
    }

    /// Defines one edge owned by the emission expression, without endpoint
    /// requirements.
    ///
    /// For an edge whose endpoint is a node the same expression also stages; the
    /// text and content rules are [`Self::define_edge_trusted`]'s.
    #[track_caller]
    pub fn emit_edge_trusted(&self, from: u64, to: u64, payload: E) -> u64 {
        let edge = irrigation_core::ops::emission_id();
        self.define_edge_trusted(edge, from, to, payload, "emitted_edge");
        edge
    }

    /// Defines a node shared by every invocation that names the same key.
    ///
    /// The identity is a root-level site interned by key equality, so the node has
    /// one owner even though many invocations call this. The payload must be a
    /// function of the key: the first evaluation stores the value and later
    /// callers validate it. The node retracts when the last caller stops demanding
    /// it.
    #[track_caller]
    pub fn emit_shared<K>(&self, key: K, payload: N) -> u64
    where
        K: std::any::Any + std::hash::Hash + Eq + Send + Sync + std::fmt::Debug + 'static,
    {
        let node = irrigation_core::ops::shared_node(key);
        let id = u64::from(node.0);
        irrigation_core::ops::eval_node(node, || self.define_node(id, payload));
        id
    }

    /// Inserts a node that must not already exist.
    ///
    /// The absence is a requirement on the write group rather than a read: the
    /// same commit installs the node's membership, so a tracked read of it would
    /// make this computation observe a unit it defines, and the demand's
    /// self-invalidation retry would then re-check the absence against the node
    /// this insert just published.
    #[track_caller]
    pub fn insert_node(&self, node: u64, payload: N, label: &'static str) {
        let ports = [self.membership_key(node)];
        self.stage_node(
            node,
            payload,
            label,
            &[PortPrecondition::absent(&ports, "graph node absent")],
        );
    }

    /// Updates a node that must already exist.
    ///
    /// Presence is a requirement on the write group: the same commit installs
    /// the node's payload and membership, so a tracked read of the membership
    /// cell would make the group observe a unit its own commit moves.
    #[track_caller]
    pub fn update_node(&self, node: u64, payload: N, label: &'static str) {
        let ports = [self.membership_key(node)];
        self.stage_node(
            node,
            payload,
            label,
            &[PortPrecondition::present(&ports, "graph node present")],
        );
    }

    /// Inserts an edge that must not already exist.
    #[track_caller]
    pub fn insert_edge(&self, edge: u64, from: u64, to: u64, payload: E, label: &'static str) {
        let ports = [self.edge_membership_key(edge)];
        self.stage_edge(
            edge,
            EdgeEnds::required(from, to),
            payload,
            label,
            &[PortPrecondition::absent(&ports, "graph edge absent")],
        );
    }

    /// Updates an edge that must already exist.
    ///
    /// Presence is a requirement on the write group, not a read of the edge's
    /// own membership cell: the update installs that cell, so reading it would
    /// make the group observe a unit its own commit moves. The requirement
    /// names one cell, so a node or edge elsewhere in the graph does not
    /// invalidate a one-edge writer.
    #[track_caller]
    pub fn update_edge(&self, edge: u64, from: u64, to: u64, payload: E, label: &'static str) {
        let ports = [self.edge_membership_key(edge)];
        self.stage_edge(
            edge,
            EdgeEnds::required(from, to),
            payload,
            label,
            &[PortPrecondition::present(&ports, "graph edge present")],
        );
    }

    /// Removes a node and every incident edge.
    ///
    /// The whole field is claimed first, so a concurrent edge write cannot add
    /// an incident edge between the projection and the retraction.
    #[track_caller]
    pub fn remove_node(&self, node: u64, label: &'static str) {
        write_group(
            WriteScope::Field(self.view, self.field),
            label,
            &[],
            |content| (self.content)(content),
            |base| self.remove_node_from(&base, node, label),
        );
    }

    fn remove_node_from(&self, base: &GraphStore<N, E>, node: u64, label: &'static str) {
        let nodes: Vec<(u64, N)> = base
            .nodes()
            .filter(|(id, _)| *id != node)
            .map(|(id, payload)| (id, payload.clone()))
            .collect();
        let edges: Vec<(u64, u64, u64, E)> = topology_of(base)
            .edges
            .iter()
            .filter(|(_, (from, to))| *from != node && *to != node)
            .filter_map(|(edge, (from, to))| {
                base.edge(*edge)
                    .map(|payload| (*edge, *from, *to, payload.value.clone()))
            })
            .collect();
        self.define_graph_from(base, &nodes, &edges, label);
    }

    /// Removes a node only when it has no incident edges.
    #[track_caller]
    pub fn remove_node_if_empty(&self, node: u64, label: &'static str) {
        write_group(
            WriteScope::Field(self.view, self.field),
            label,
            &[],
            |content| (self.content)(content),
            |base| {
                if topology_of(&base)
                    .edges
                    .iter()
                    .any(|(_, (from, to))| *from == node || *to == node)
                {
                    panic_engine(ComputeError::PreconditionUnmet {
                        condition: String::from("graph node has no incident edges"),
                        ports: vec![format!("{:?}", self.membership_key(node))],
                        reason: PreconditionReason::Contradiction,
                    });
                }
                self.remove_node_from(&base, node, label);
            },
        );
    }

    /// Defines a map entry per node from this graph's payloads.
    ///
    /// The target map is published as a whole field
    /// ([`crate::map::MapRef::define_all`]), so a removed node retracts its
    /// entry even when the calling body does not restate its output.
    pub fn node_map(&self, target: &crate::map::MapRef<u64, N>) {
        let mut items = Vec::new();
        for node in self.nodes() {
            if let Some(payload) = self.node(node) {
                items.push((node, payload));
            }
        }
        target.define_all(items, "graph_node_map");
    }

    /// Reads the outgoing adjacency of one node as `(edge, target)` pairs.
    ///
    /// Derived graphs compute it from the topology unit, so an edge edit
    /// invalidates only topology readers. Source graphs keep their per-node
    /// adjacency units.
    #[must_use]
    pub fn outgoing(&self, node: u64) -> Vec<(u64, u64)> {
        if irrigation_runtime::helpers::field_protocol(self.view, self.field) {
            return self
                .topology()
                .edges
                .iter()
                .filter(|(_, (from, _))| *from == node)
                .map(|(edge, (_, to))| (*edge, *to))
                .collect();
        }
        let value: Vec<(u64, u64)> = read_unit_value(self.adjacency_key(node), |content| {
            Some(adjacency_of(&(self.content)(content), node))
        });
        value
    }

    /// Reads the incoming adjacency of one node as `(edge, source)` pairs.
    #[must_use]
    pub fn incoming(&self, node: u64) -> Vec<(u64, u64)> {
        if irrigation_runtime::helpers::field_protocol(self.view, self.field) {
            return self
                .topology()
                .edges
                .iter()
                .filter(|(_, (_, to))| *to == node)
                .map(|(edge, (from, _))| (*edge, *from))
                .collect();
        }
        let value: Vec<(u64, u64)> =
            read_unit_value(self.incoming_adjacency_key(node), |content| {
                Some(incoming_adjacency_of(&(self.content)(content), node))
            });
        value
    }

    /// Defines a whole derived graph: node payloads, edge payloads, topology.
    ///
    /// Every edge's endpoints must appear in `nodes`: an edge that names a missing
    /// endpoint is a caller mistake and fails the write with `PreconditionUnmet`
    /// naming the edge and both endpoints. It used to claim such edges were dropped,
    /// while the unit loop panicked on them -- so the documented drop never happened.
    #[track_caller]
    pub fn define_graph(
        &self,
        nodes: &[(u64, N)],
        edges: &[(u64, u64, u64, E)],
        label: &'static str,
    ) {
        write_group(
            WriteScope::Field(self.view, self.field),
            label,
            &[],
            |content| (self.content)(content),
            |previous| self.define_graph_from(&previous, nodes, edges, label),
        );
    }

    fn define_graph_from(
        &self,
        previous: &GraphStore<N, E>,
        nodes: &[(u64, N)],
        edges: &[(u64, u64, u64, E)],
        label: &'static str,
    ) {
        let node_ids: irrigation_store::persistent::PSet<u64> =
            nodes.iter().map(|(id, _)| *id).collect();
        // An edge whose endpoints are not both in this definition is refused, and it
        // is refused here rather than while building topology and again while defining
        // the unit: those two spellings disagreed -- one dropped the edge silently, the
        // other panicked -- so the silent one was dead and this function's doc, which
        // promised the drop, described behaviour that could not happen.
        for (edge, from, to, _) in edges {
            if !node_ids.contains(from) || !node_ids.contains(to) {
                panic_engine(ComputeError::PreconditionUnmet {
                    condition: String::from("graph edge endpoints exist in the same definition"),
                    ports: vec![format!("{edge}"), format!("{from}"), format!("{to}")],
                    reason: PreconditionReason::Contradiction,
                });
            }
        }
        let mut topology_edges: PMap<u64, (u64, u64)> = PMap::new();
        for (edge, from, to, _) in edges {
            topology_edges = topology_edges.insert(*edge, (*from, *to)).0;
        }
        // A whole-graph definition replaces the graph: retract node and edge
        // units that the new definition no longer contains. This makes the
        // write independent of when the previous branch site is reclaimed.
        let stale = topology_of(previous);
        for id in stale.nodes.iter() {
            retract_unit(self.node_key(*id), label);
        }
        for edge in stale.edges.iter().map(|(edge, _)| *edge) {
            retract_unit(self.edge_key(edge), label);
        }
        retract_unit(self.topology_unit(), label);
        for (id, payload) in nodes {
            define_unit(self.node_key(*id), payload.clone(), label);
        }
        for (edge, from, to, payload) in edges {
            define_unit(
                self.edge_key(*edge),
                Some(EdgePayload {
                    from: *from,
                    to: *to,
                    value: payload.clone(),
                }),
                label,
            );
        }
        define_unit(
            self.topology_unit(),
            GraphTopology {
                nodes: node_ids,
                edges: topology_edges,
            },
            label,
        );
    }

    /// Reads the neighbours of one node, preserving edge order.
    #[must_use]
    pub fn neighbors(&self, node: u64) -> Vec<u64> {
        self.outgoing(node).into_iter().map(|(_, to)| to).collect()
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
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Value,
            any_key(NodeKey(node)),
        )
    }

    fn edge_key(&self, edge: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Value,
            any_key(EdgeKey(edge)),
        )
    }

    fn edge_membership_key(&self, edge: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Membership,
            any_key(EdgeKey(edge)),
        )
    }

    fn adjacency_key(&self, node: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::Adjacency,
            any_key(NodeKey(node)),
        )
    }

    fn incoming_adjacency_key(&self, node: u64) -> UnitKey {
        UnitKey::entity(
            self.view,
            self.field,
            UnitKind::IncomingAdjacency,
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
        let incident = self.storage.incident_edges(node);
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
            self.edit.set(self.edge_membership_key(edge), false);
        }
        for neighbour in neighbours {
            self.mark_dirty(neighbour);
        }
        self.mark_dirty(node);
    }

    /// Adds an edge, returning its identity.
    ///
    /// Edge membership is staged with the payload, mirroring [`Self::add_node`]:
    /// a reader that observed the edge's absence depends on that membership
    /// cell and must be woken when the edge arrives.
    pub fn add_edge(&mut self, from: u64, to: u64, value: E) -> u64 {
        let (next, edge) = self.storage.add_edge(from, to, value.clone());
        *self.storage = next;
        self.edit
            .set(self.edge_key(edge), Some(EdgePayload { from, to, value }));
        self.edit.set(self.edge_membership_key(edge), true);
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
        self.edit
            .set(self.edge_key(edge), Option::<EdgePayload<E>>::None);
        self.edit.set(self.edge_membership_key(edge), false);
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
        self.edit
            .set(self.edge_key(edge), Some(EdgePayload { from, to, value }));
    }

    fn stage_structure(&mut self) {
        let topology = topology_of(self.storage);
        self.edit
            .set(structure_unit(self.view, self.field), topology);
        for node in self.dirty.clone() {
            let adjacency = adjacency_of(self.storage, node);
            self.edit.set(self.adjacency_key(node), adjacency);
            let incoming = incoming_adjacency_of(self.storage, node);
            self.edit.set(self.incoming_adjacency_key(node), incoming);
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

fn incoming_adjacency_of<N, E>(storage: &GraphStore<N, E>, node: u64) -> Vec<(u64, u64)> {
    storage
        .incoming(node)
        .map(|edges| {
            edges
                .filter_map(|edge| storage.edge(edge).map(|payload| (edge, payload.from)))
                .collect()
        })
        .unwrap_or_default()
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
#[path = "../tests/unit/graph/tests.rs"]
mod tests;
