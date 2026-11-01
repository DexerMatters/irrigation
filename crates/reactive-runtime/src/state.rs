//! Runtime state: the node arena, View regions, invocations, and scopes.
//!
//! All of this lives behind one mutex. User code never runs while it is held:
//! a node claims its slot, releases the lock, executes, and re-acquires the
//! lock to commit. That rule is what keeps the metadata lock short and makes
//! the atomic commit protocol possible.

use std::any::Any;
use std::sync::Arc;

use reactive_core::error::ComputeError;
use reactive_core::frame::CancelToken;
use reactive_core::frame::Dep;
use reactive_core::hash::FxHashMap;
use reactive_core::ids::{
    ComponentId, NodeId, Revision, ScopeId, SiteId, Version, FieldId, ViewId,
};
use reactive_core::key::{AnyKey, KeyTable};
use reactive_core::value::Value;
use reactive_core::descriptor::{UnitKey, ViewDescriptor};

use crate::task::{Task, TaskId, TaskMode, TaskStatus};

/// Execution state of one memoized node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeState {
    /// No value has been produced yet.
    Vacant,
    /// A task is computing the node.
    Running {
        /// The task that claimed the node.
        owner: TaskId,
        /// Claim generation, used to reject stale commits.
        generation: u64,
    },
    /// A committed value is available.
    Ready,
    /// The last attempt failed.
    Failed,
}

/// What a node represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// One lowered expression site in one scope.
    Site,
    /// One logical View unit.
    Unit,
    /// The body of one component invocation.
    InvocationBody,
    /// One output projection of a component invocation.
    InvocationOutput,
    /// A collection or control-region operator.
    Operator,
    /// A derived View's membership/content unit.
    RegionContent,
}

/// One memoized computation.
#[derive(Debug)]
pub struct NodeRecord {
    /// Human-readable kind for diagnostics.
    pub label: &'static str,
    /// What the node represents.
    pub kind: NodeKind,
    /// Identity scope for site nodes.
    pub scope: ScopeId,
    /// Expression site for site nodes.
    pub site: SiteId,
    /// Logical unit for unit nodes.
    pub unit: Option<UnitKey>,
    /// The last committed value.
    pub value: Option<Value>,
    /// Revision at which the value last changed.
    pub changed_at: Version,
    /// Revision for which the value has been validated.
    pub verified_at: Revision,
    /// Revision at which the node last executed its closure.
    pub executed_at: Revision,
    /// Revision at which the node's trace was last validated or replayed.
    pub validated_at: Revision,
    /// Revision at which the node was last invalidated.
    pub suspect_at: Revision,
    /// Ordered observations from the last execution.
    pub trace: Vec<Dep>,
    /// Nodes whose trace currently contains this node.
    pub dependents: Vec<NodeId>,
    /// Execution state.
    pub state: NodeState,
    /// Failure from the last attempt, if any.
    pub failure: Option<ComputeError>,
    /// Revision at which the failure happened.
    pub failed_at: Revision,
    /// Units this node currently defines.
    pub definitions: Vec<DefinitionRecord>,
    /// Index into `State::generals` for a custom component node.
    pub general: Option<usize>,
    /// Retained state of a custom component, committed with its output.
    pub general_state: Option<crate::general::ErasedState>,
    /// Tasks suspended on this node, to be queued when it completes.
    pub waiters: Vec<TaskId>,
}

impl NodeRecord {
    /// Builds a vacant site node.
    #[must_use]
    pub fn site(scope: ScopeId, site: SiteId) -> Self {
        Self {
            label: "site",
            kind: NodeKind::Site,
            scope,
            site,
            unit: None,
            value: None,
            changed_at: 0,
            verified_at: 0,
            executed_at: 0,
            validated_at: 0,
            suspect_at: 0,
            trace: Vec::new(),
            dependents: Vec::new(),
            state: NodeState::Vacant,
            failure: None,
            failed_at: 0,
            definitions: Vec::new(),
            general: None,
            general_state: None,
            waiters: Vec::new(),
        }
    }

    /// Builds a vacant unit node.
    #[must_use]
    pub fn unit(label: &'static str, unit: UnitKey) -> Self {
        Self {
            label,
            kind: NodeKind::Unit,
            scope: ScopeId::ROOT,
            site: SiteId(0),
            unit: Some(unit),
            ..Self::site(ScopeId::ROOT, SiteId(0))
        }
    }

    /// Builds a vacant node of an arbitrary kind.
    #[must_use]
    pub fn empty(label: &'static str, kind: NodeKind) -> Self {
        Self {
            label,
            kind,
            ..Self::site(ScopeId::ROOT, SiteId(0))
        }
    }

    /// Returns `true` when the node has a committed value for `revision`.
    #[must_use]
    pub fn is_verified(&self, revision: Revision) -> bool {
        self.verified_at == revision && self.value.is_some()
    }

    /// Returns `true` when a worker currently owns the node.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self.state, NodeState::Running { .. })
    }
}

/// One committed definition: which provider defined which unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionRecord {
    /// The defined unit.
    pub unit: UnitKey,
    /// The defining node.
    pub provider: NodeId,
}

/// One in-flight demand.
pub struct TaskSlot {
    /// The demand body, taken out while an attempt is executing.
    pub body: Option<Box<dyn Task>>,
    /// Scheduling state.
    pub status: TaskStatus,
    /// Whether the task runs on a worker or on its calling thread.
    pub mode: TaskMode,
    /// Whether the task holds a read permit on the revision gate.
    pub gate: bool,
    /// The task's cancellation token.
    pub cancel: CancelToken,
    /// Nodes the task claimed and still owns after suspending.
    pub held: Vec<NodeId>,
}

impl std::fmt::Debug for TaskSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskSlot")
            .field("status", &self.status)
            .field("mode", &self.mode)
            .field("gate", &self.gate)
            .field("held", &self.held)
            .finish_non_exhaustive()
    }
}

/// Two providers that claimed one unit during one revision.
#[derive(Clone, Debug)]
pub struct PendingConflict {
    /// The contested unit.
    pub unit: UnitKey,
    /// The owner that held the unit when the second claim arrived.
    pub first: NodeId,
    /// The provider that claimed it later.
    pub second: NodeId,
}

/// A unit installed in a derived View region.
#[derive(Debug)]
pub struct UnitSlot {
    /// The node holding the unit's value.
    pub node: NodeId,
    /// Whether the unit is currently defined.
    pub present: bool,
}

/// The provider that owns one unit.
#[derive(Clone, Debug)]
pub struct ProviderRef {
    /// The defining node.
    pub node: NodeId,
    /// Human-readable defining site.
    pub label: &'static str,
}

/// A derived View's materialized region.
pub struct Region {
    /// The View schema.
    pub descriptor: &'static ViewDescriptor,
    /// Immutable content at the current revision.
    pub content: Arc<dyn Any + Send + Sync>,
    /// Node holding the content, used for versioning.
    pub content_node: NodeId,
    /// Installed units by key.
    pub units: KeyTable<UnitSlot>,
    /// Owner of each unit, for unique-definition enforcement.
    pub owners: KeyTable<ProviderRef>,
}

impl std::fmt::Debug for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Region")
            .field("view", &self.descriptor.name)
            .field("units", &self.units.len())
            .finish()
    }
}

/// One component invocation record.
#[derive(Clone)]
pub struct Invocation {
    /// Type-erased argument tuple, compared with real equality.
    pub args: Arc<dyn Any + Send + Sync>,
    /// Identity scope of the invocation.
    pub scope: ScopeId,
    /// The body node.
    pub body: NodeId,
    /// Output projection nodes.
    pub outputs: Vec<NodeId>,
}

impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Invocation")
            .field("scope", &self.scope)
            .field("body", &self.body)
            .field("outputs", &self.outputs)
            .finish()
    }
}

/// Buckets of typed keys interned into scopes, keyed by `(parent, hash)`.
type KeyedScopes = FxHashMap<(u64, u64), Vec<(Arc<dyn AnyKey>, ScopeId)>>;

/// Stable child-scope allocation.
///
/// Scopes are allocated from a counter, so identity never depends on a hash.
/// `child` interns an exact `(parent, discriminant)` pair; `child_keyed`
/// interns a typed key and compares keys by equality, so hash collisions only
/// cost a bucket scan.
#[derive(Default)]
pub struct ScopeTable {
    by_discriminant: FxHashMap<(u64, u64), ScopeId>,
    by_key: KeyedScopes,
    next: u64,
}

impl std::fmt::Debug for ScopeTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopeTable")
            .field("scopes", &self.len())
            .field("next", &self.next)
            .finish()
    }
}

impl ScopeTable {
    fn allocate(&mut self) -> ScopeId {
        self.next += 1;
        ScopeId(self.next)
    }

    /// Returns the child scope for `(parent, discriminant)`, allocating it once.
    pub fn child(&mut self, parent: ScopeId, discriminant: u64) -> ScopeId {
        if let Some(scope) = self.by_discriminant.get(&(parent.raw(), discriminant)) {
            return *scope;
        }
        let scope = self.allocate();
        self.by_discriminant
            .insert((parent.raw(), discriminant), scope);
        scope
    }

    /// Returns the child scope for a typed key, comparing keys by equality.
    pub fn child_keyed(&mut self, parent: ScopeId, key: Arc<dyn AnyKey>) -> ScopeId {
        let lookup = (parent.raw(), key.key_hash());
        if let Some(bucket) = self.by_key.get(&lookup) {
            for (existing, scope) in bucket {
                if existing.key_eq(key.as_ref()) {
                    return *scope;
                }
            }
        }
        let scope = self.allocate();
        self.by_key.entry(lookup).or_default().push((key, scope));
        scope
    }

    /// Drops interned scopes that no live node references.
    pub fn prune(&mut self, live: &std::collections::HashSet<ScopeId>) -> usize {
        let before = self.len();
        self.by_discriminant.retain(|_, scope| live.contains(scope));
        self.by_key.retain(|_, bucket| {
            bucket.retain(|(_, scope)| live.contains(scope));
            !bucket.is_empty()
        });
        before - self.len()
    }

    /// Number of allocated scopes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_discriminant.len() + self.by_key.values().map(Vec::len).sum::<usize>()
    }

    /// Returns `true` when no scope has been allocated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The installed root component.
#[derive(Clone)]
pub struct RootRecord {
    /// Component identity.
    pub component: ComponentId,
    /// Component name.
    pub name: &'static str,
    /// Body node.
    pub body: NodeId,
    /// Output projection nodes.
    pub outputs: Vec<NodeId>,
    /// Runs the component body.
    pub run: fn() -> Value,
    /// Projects one output.
    pub project: fn(&Value, usize) -> Value,
    /// Compares one output projection.
    pub projection_eq: fn(usize, &Value, &Value) -> bool,
    /// Compares the whole output product.
    pub body_eq: fn(&Value, &Value) -> bool,
    /// Rebuilds the output product from demanded projections.
    pub reconstruct: fn(&[Value]) -> Value,
}

impl std::fmt::Debug for RootRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootRecord")
            .field("component", &self.component)
            .field("name", &self.name)
            .field("outputs", &self.outputs)
            .finish()
    }
}

type SnapshotCallback = Arc<dyn Fn(&crate::snapshot::Snapshot) + Send + Sync>;
type ChangesCallback =
    Arc<dyn Fn(&crate::snapshot::Snapshot, &crate::snapshot::ChangeSummary) + Send + Sync>;

/// An observer callback.
#[derive(Clone)]
pub enum Observer {
    /// Receives the snapshot only.
    Plain(SnapshotCallback),
    /// Receives the snapshot and a summary of the changed View versions.
    WithChanges(ChangesCallback),
}

impl std::fmt::Debug for Observer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plain(_) => f.write_str("Observer::Plain"),
            Self::WithChanges(_) => f.write_str("Observer::WithChanges"),
        }
    }
}

/// One active observation root.
pub struct SubscriptionRecord {
    /// Subscription identity.
    pub id: u64,
    /// The observed View.
    pub view: ViewId,
    /// Callback invoked after each change.
    pub callback: Observer,
    /// Content version last delivered.
    pub last_version: Version,
    /// Watched units; empty means the whole View content.
    pub units: Vec<UnitKey>,
    /// Per-unit versions last delivered, aligned with `units`.
    pub last_versions: Vec<Version>,
    /// Whether the subscription is still active.
    pub active: Arc<std::sync::atomic::AtomicBool>,
}

/// Version of one unit's node, or zero when it has never been created.
pub fn unit_version(state: &State, unit: &UnitKey) -> Version {
    state
        .units
        .get(unit)
        .copied()
        .or_else(|| {
            state
                .regions
                .get(&unit.view)
                .and_then(|region| region.units.get(unit))
                .map(|slot| slot.node)
        })
        .map(|node| state.node(node).changed_at)
        .unwrap_or(0)
}

impl std::fmt::Debug for SubscriptionRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriptionRecord")
            .field("id", &self.id)
            .field("view", &self.view)
            .finish()
    }
}

/// Counters used by diagnostics and tests.
#[derive(Debug, Default)]
pub struct Stats {
    /// Nodes created.
    pub nodes_created: std::sync::atomic::AtomicU64,
    /// Nodes executed.
    pub executions: std::sync::atomic::AtomicU64,
    /// Trace validations performed.
    pub validations: std::sync::atomic::AtomicU64,
    /// Revisions published.
    pub revisions: std::sync::atomic::AtomicU64,
    /// Nodes marked suspect by invalidation.
    pub invalidations: std::sync::atomic::AtomicU64,
}

/// Everything the runtime protects with its metadata lock.
#[derive(Debug, Default)]
pub struct State {
    /// Node arena.
    pub nodes: Vec<NodeRecord>,
    /// Expression-site nodes by scope.
    pub sites: FxHashMap<(ScopeId, SiteId), NodeId>,
    /// Logical units by key.
    pub units: KeyTable<NodeId>,
    /// Scope allocation.
    pub scopes: ScopeTable,
    /// Component invocations by `(component, argument hash)`.
    pub invocations: FxHashMap<(ComponentId, u64), Vec<Invocation>>,
    /// Derived View regions.
    pub regions: FxHashMap<ViewId, Region>,
    /// Registered View descriptors.
    pub views: FxHashMap<ViewId, &'static ViewDescriptor>,
    /// Current revision.
    pub revision: Revision,
    /// Tasks waiting on nodes, for cycle detection.
    pub wait_graph: FxHashMap<TaskId, NodeId>,
    /// In-flight demand tasks.
    pub tasks: FxHashMap<TaskId, TaskSlot>,
    /// Next task identity.
    pub next_task: TaskId,
    /// Next claim generation.
    pub next_generation: u64,
    /// Active subscriptions.
    pub subscriptions: Vec<SubscriptionRecord>,
    /// Next subscription id.
    pub next_subscription: u64,
    /// Next entity id allocated by source edits.
    pub next_entity: u64,
    /// Next invocation identity, used to derive a unique scope per invocation.
    pub next_invocation: u64,
    /// Installed root.
    pub root: Option<RootRecord>,
    /// Installed general components, indexed by their node's `general` field.
    pub(crate) generals: Vec<std::sync::Arc<dyn crate::general::ErasedGeneral>>,
    /// The installed `GeneralComponent` root, if any.
    pub general_root: Option<NodeId>,
    /// Conflicting definitions awaiting end-of-revision resolution.
    pub pending_conflicts: Vec<PendingConflict>,
    /// Fields currently populated by the keyed derived protocol.
    pub keyed_fields: std::collections::HashSet<(ViewId, FieldId)>,
    /// Diagnostics counters.
    pub stats: Stats,
    /// Last observer-refresh failure, recorded by `edit`.
    ///
    /// A failed refresh does not fail the transaction; it is reported here so
    /// the host can distinguish "the edit committed" from "an observer could
    /// not be refreshed".
    pub notification_error: Option<ComputeError>,
}

impl State {
    /// Allocates a node record.
    pub fn alloc(&mut self, record: NodeRecord) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(record);
        self.stats
            .nodes_created
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        id
    }

    /// Returns a node record.
    #[must_use]
    pub fn node(&self, id: NodeId) -> &NodeRecord {
        &self.nodes[id.index()]
    }

    /// Returns a mutable node record.
    pub fn node_mut(&mut self, id: NodeId) -> &mut NodeRecord {
        &mut self.nodes[id.index()]
    }

    /// Allocates a fresh entity identity for a source collection edit.
    pub fn alloc_entity(&mut self) -> u64 {
        self.next_entity += 1;
        self.next_entity
    }

    /// Registers a demand task and returns its identity.
    pub fn alloc_task(
        &mut self,
        body: Option<Box<dyn Task>>,
        mode: TaskMode,
        gate: bool,
        cancel: CancelToken,
    ) -> TaskId {
        self.next_task += 1;
        let id = self.next_task;
        self.tasks.insert(
            id,
            TaskSlot {
                body,
                status: TaskStatus::Queued,
                mode,
                gate,
                cancel,
                held: Vec::new(),
            },
        );
        id
    }

    /// Adds a reverse dependency edge.
    pub fn add_dependent(&mut self, dependency: NodeId, dependent: NodeId) {
        let record = self.node_mut(dependency);
        if !record.dependents.contains(&dependent) {
            record.dependents.push(dependent);
        }
    }

    /// Removes a reverse dependency edge.
    pub fn remove_dependent(&mut self, dependency: NodeId, dependent: NodeId) {
        let record = self.node_mut(dependency);
        record.dependents.retain(|node| *node != dependent);
    }
}

/// Marks `seeds` and everything reachable through reverse edges suspect at
/// `revision`.
///
/// This is the invalidation cost the plan calls `E_v`: it can visit the whole
/// old reachable cone even when equality later stops execution.
pub fn invalidate(state: &mut State, seeds: impl IntoIterator<Item = NodeId>, revision: Revision) {
    let mut stack: Vec<NodeId> = seeds.into_iter().collect();
    let mut seen: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    let mut marked = 0u64;
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        let record = state.node_mut(node);
        if record.suspect_at >= revision {
            continue;
        }
        record.suspect_at = revision;
        marked += 1;
        for dependent in &record.dependents {
            stack.push(*dependent);
        }
    }
    state
        .stats
        .invalidations
        .fetch_add(marked, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use reactive_core::key::any_key;
    use std::hash::{Hash, Hasher};

    /// Two different keys that deliberately share one hash.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Colliding(u64);

    impl Hash for Colliding {
        fn hash<H: Hasher>(&self, state: &mut H) {
            0u64.hash(state);
        }
    }

    #[test]
    fn keyed_child_scopes_disambiguate_equal_hashes() {
        let mut table = ScopeTable::default();
        let first = table.child_keyed(ScopeId::ROOT, any_key(Colliding(1)));
        let second = table.child_keyed(ScopeId::ROOT, any_key(Colliding(2)));
        assert_ne!(first, second, "equal hashes must not share a scope");
        assert_eq!(
            table.child_keyed(ScopeId::ROOT, any_key(Colliding(1))),
            first
        );
        let other_parent = table.child(ScopeId::ROOT, 1);
        assert_ne!(
            table.child_keyed(other_parent, any_key(Colliding(1))),
            first,
            "the parent is part of the scope identity"
        );
    }

    #[test]
    fn scope_ids_are_allocated_not_hashed() {
        let mut table = ScopeTable::default();
        let first = table.child(ScopeId::ROOT, 7);
        let second = table.child(ScopeId::ROOT, 8);
        assert_ne!(first, second);
        assert_ne!(first, ScopeId::ROOT);
        assert_eq!(table.child(ScopeId::ROOT, 7), first);
        assert_eq!(table.len(), 2);
    }
}
