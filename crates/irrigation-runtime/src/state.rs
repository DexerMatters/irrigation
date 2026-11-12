//! Runtime state: the node arena, View regions, invocations, and scopes.
//!
//! All of this lives behind one mutex. User code never runs while it is held:
//! a node claims its slot, releases the lock, executes, and re-acquires the
//! lock to commit. That rule is what keeps the metadata lock short and makes
//! the atomic commit protocol possible.

use std::any::Any;
use std::sync::Arc;

use irrigation_core::descriptor::{UnitKey, UnitKind, ViewDescriptor, WriteScope};
use irrigation_core::error::ComputeError;
use irrigation_core::frame::CancelToken;
use irrigation_core::frame::{Dep, Requirement};
use irrigation_core::hash::{FxHashMap, FxHashSet};
use irrigation_core::ids::{
    ComponentId, FieldId, NodeId, Revision, ScopeId, SiteId, Version, ViewId,
};
use irrigation_core::key::{AnyKey, KeyTable};
use irrigation_core::value::Value;

use crate::inner::DefinitionDraft;
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
    /// Slot generation, packed into the `NodeId`.
    pub generation: u32,
    /// Whether the slot was reclaimed and awaits reuse.
    pub freed: bool,
    /// What the node represents.
    pub kind: NodeKind,
    /// Identity scope for site nodes.
    pub scope: ScopeId,
    /// Expression site for site nodes.
    pub site: SiteId,
    /// Logical unit for unit nodes.
    pub unit: Option<UnitKey>,
    /// Component that owns this node, when it is a component body or output.
    pub component: Option<ComponentId>,
    /// The last committed value.
    pub value: Option<Value>,
    /// Revision at which the value last changed.
    ///
    /// A `Revision`, not a value version: every writer stamps it with the
    /// revision that changed the value, and the readers that ask "did this change
    /// at the current revision" compare it with one.
    pub changed_at: Revision,
    /// Revision at which this cell's payload was retracted.
    ///
    /// A retracted cell keeps its node (subscribers hold the id and compare
    /// versions), but it no longer holds a live unit: the observer must not
    /// count it among a field's rows, and it must not report its removal as a
    /// content change every revision afterwards. An install clears the marker,
    /// so a re-published unit is live again.
    pub retired_at: Revision,
    /// Monotonic value version. Unlike `changed_at`, this advances even when a
    /// value changes inside one revision, and it does not advance for an equal
    /// redefinition.
    pub value_version: u64,
    /// Revision for which the value has been validated.
    pub verified_at: Revision,
    /// Routing generation: bumped when a unit's provider changes.
    pub routing_at: Revision,
    /// Revision at which the node last executed its closure.
    pub executed_at: Revision,
    /// Revision at which the node's trace was last validated or replayed.
    pub validated_at: Revision,
    /// Revision at which the node was last invalidated.
    pub suspect_at: Revision,
    /// Monotonic stamp of the last invalidation.
    ///
    /// Revision alone cannot distinguish "verified before an invalidation" from
    /// "verified after it" inside one revision. This stamp can: a node is clean
    /// only while `suspect_validity <= verified_validity`.
    pub suspect_validity: u64,
    /// Stamp of the last certification; equals `suspect_validity` when clean.
    pub verified_validity: u64,
    /// Global validity stamp at the moment this attempt claimed the node.
    pub attempt_validity: u64,
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
    /// Writable scopes this node currently claims.
    pub claims: Vec<WriteScope>,
    /// Views whose content this node replaced on its last run.
    ///
    /// A general component publishes content instead of defining units; both
    /// are writes to a View, and both are attributed to the component.
    pub content_writes: Vec<irrigation_core::ids::ViewId>,
    /// Whether a whole-region replacement owns this cell.
    ///
    /// A replacement with no cell write beside it cannot say which cell's
    /// payload it changed, so it opens the cell's next generation and leaves
    /// the payload absent for the next read to derive from the committed
    /// content. It is not the same state as an invalidation: a retracted cell
    /// has no payload either, and its reader must observe the retraction rather
    /// than have the cell re-derived underneath it.
    ///
    /// The replacement allocates the version and stamps `changed_at` when it
    /// sets this marker, because a payload it owns changed at that revision; the
    /// marker only says the payload has not been installed yet. Deriving it
    /// clears the marker without moving either: the change belongs to the
    /// replacement, not to whichever later revision happened to read the cell.
    pub(crate) content_stale: bool,
    /// Index into `State::generals` for a custom component node.
    pub general: Option<usize>,
    /// Retained state of a custom component, committed with its output.
    pub general_state: Option<crate::general::ErasedState>,
    /// Units read under a dependency scope by a general component.
    pub general_unit_scopes: FxHashMap<UnitKey, u64>,
    /// Dependency scopes retained by the last general-component run.
    pub general_retained_scopes: FxHashSet<u64>,
    /// Tasks suspended on this node, to be queued when it completes.
    pub waiters: Vec<TaskId>,
    /// Future wakers waiting for this node to be certified.
    pub wakers: Vec<std::task::Waker>,
}

impl NodeRecord {
    /// Builds a vacant site node.
    #[must_use]
    pub fn site(scope: ScopeId, site: SiteId) -> Self {
        Self {
            label: "site",
            generation: 0,
            freed: false,
            kind: NodeKind::Site,
            scope,
            site,
            unit: None,
            component: None,
            value: None,
            changed_at: 0,
            retired_at: 0,
            value_version: 0,
            verified_at: 0,
            routing_at: 0,
            executed_at: 0,
            validated_at: 0,
            suspect_at: 0,
            suspect_validity: 0,
            verified_validity: 0,
            attempt_validity: 0,
            trace: Vec::new(),
            dependents: Vec::new(),
            state: NodeState::Vacant,
            failure: None,
            failed_at: 0,
            definitions: Vec::new(),
            claims: Vec::new(),
            content_writes: Vec::new(),
            content_stale: false,
            general: None,
            general_state: None,
            general_unit_scopes: FxHashMap::default(),
            general_retained_scopes: FxHashSet::default(),
            waiters: Vec::new(),
            wakers: Vec::new(),
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

    /// Returns `true` when a worker currently owns the node.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self.state, NodeState::Running { .. })
    }

    /// Returns `true` when no invalidation happened after the last
    /// certification.
    #[must_use]
    pub fn has_valid_trace(&self) -> bool {
        self.suspect_validity <= self.verified_validity
    }

    /// Marks the node certified for a revision without changing its value.
    ///
    /// A certified node is current for that revision, which is also what clears
    /// a whole-region replacement's staleness marker: the cell's payload was
    /// published after the replacement that invalidated it.
    pub fn certify(&mut self, revision: Revision) {
        self.verified_at = revision;
        self.verified_validity = self.suspect_validity;
        self.content_stale = false;
    }

    /// Installs one cell payload as the value of its generation.
    ///
    /// Every writer of a cell moves these fields through [`Self::install_value`],
    /// [`Self::retire_value`], [`Self::begin_generation`], or
    /// [`Self::publish_fill`] — an install, a retraction, a replacement, and the
    /// read that finally derives a replacement's payload — so a payload is
    /// always the one `value_version` names, at the revision `changed_at`
    /// stamps. A node that is not a cell (a site, a component body) publishes
    /// its value through the commit instead.
    pub fn install_value(
        &mut self,
        payload: Option<Value>,
        value_version: u64,
        revision: Revision,
    ) {
        self.value = payload;
        self.value_version = value_version;
        self.changed_at = revision;
        self.retired_at = 0;
        self.state = NodeState::Ready;
        self.failure = None;
        self.suspect_at = 0;
        self.certify(revision);
    }

    /// Removes a cell's payload as a change of its own generation.
    ///
    /// Retraction is the inverse of installation, so it moves the payload, the
    /// version and the stamp together like any other change: the entity's last
    /// generation held a value, this one holds none. Certification is left to
    /// the caller, which also has to tell the invalidation pass which
    /// observations the removal invalidated.
    pub fn retire_value(&mut self, value_version: u64, revision: Revision) {
        self.value = None;
        self.value_version = value_version;
        self.changed_at = revision;
        self.retired_at = revision;
        self.content_stale = false;
    }

    /// Opens a cell's next generation without its payload.
    ///
    /// The version and the stamp move now, because the change happened now; the
    /// payload arrives with the read that can derive it. Leaving the previous
    /// generation's payload behind would let a reader answer with a value the
    /// cell no longer commits, so the cell holds no value at all until the
    /// generation is filled. The invalidation that follows the replacement is
    /// what marks the readers, and it walks them even when this cell already
    /// looks suspect: the cell's own stamp says nothing about the readers that
    /// re-certified after it.
    pub fn begin_generation(&mut self, value_version: u64, revision: Revision) {
        self.value = None;
        self.value_version = value_version;
        self.changed_at = revision;
        self.content_stale = true;
        // The cell holds no value for this revision, whatever its validity
        // stamps say.
        self.verified_at = 0;
    }

    /// Publishes the payload the generation opened by [`Self::begin_generation`]
    /// was waiting for.
    ///
    /// The change belongs to the replacement, not to the read that can finally
    /// derive the payload, so the generation's `changed_at` is kept. The version
    /// does move: what the cell publishes changed from "not installed" to the
    /// payload, and a reader that recorded the unfilled generation — a negative
    /// observation, typically — must fail validation instead of validating
    /// against a value it never saw. Certification is the reading revision,
    /// because the payload comes from content that revision has committed.
    pub fn publish_fill(
        &mut self,
        payload: Option<Value>,
        value_version: u64,
        changed_at: Revision,
        verified_at: Revision,
    ) {
        self.value = payload;
        self.value_version = value_version;
        self.changed_at = changed_at;
        self.retired_at = 0;
        self.state = NodeState::Ready;
        self.failure = None;
        self.suspect_at = 0;
        self.certify(verified_at);
    }
}

/// One committed definition: which provider defined which unit.
#[derive(Clone)]
pub struct DefinitionRecord {
    /// The defined unit.
    pub unit: UnitKey,
    /// The defining node.
    pub provider: NodeId,
    /// The committed value of the definition, kept so a route conflict can
    /// restore the selected provider's post-state.
    pub value: Value,
}

impl std::fmt::Debug for DefinitionRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefinitionRecord")
            .field("unit", &self.unit)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

impl PartialEq for DefinitionRecord {
    fn eq(&self, other: &Self) -> bool {
        self.unit == other.unit && self.provider == other.provider
    }
}

impl Eq for DefinitionRecord {}

/// One live write claim held by a computation.
#[derive(Clone, Debug)]
pub struct WriteClaim {
    /// The claiming node.
    pub owner: NodeId,
    /// The task executing the owner.
    pub task: TaskId,
    /// The scope being written.
    pub scope: WriteScope,
    /// Human-readable defining site.
    pub label: &'static str,
    /// Source location of the claim.
    pub location: Option<&'static std::panic::Location<'static>>,
}

/// A port this computation intends to write before it claims it.
#[derive(Clone, Debug)]
pub struct PlannedPort {
    /// The computation that will write it.
    pub owner: NodeId,
    /// The task executing the owner.
    pub task: TaskId,
    /// The scope being planned.
    pub scope: WriteScope,
    /// Human-readable defining site.
    pub label: &'static str,
    /// Source location of the plan.
    pub location: Option<&'static std::panic::Location<'static>>,
}

/// One write group held until the end of its demand.
#[derive(Debug)]
pub struct PendingWrite {
    /// The computation that staged it.
    pub owner: NodeId,
    /// The demand task that owns the group.
    pub task: TaskId,
    /// Revision of the attempt.
    pub revision: Revision,
    /// Claim generation of the attempt.
    pub generation: u64,
    /// Drafts that install together.
    pub drafts: Vec<DefinitionDraft>,
    /// Units the attempt no longer defines.
    pub retractions: Vec<UnitKey>,
    /// Whether the group restates the owner's whole definition set.
    pub restate_all: bool,
    /// Whether definitions must be declared by the provider's scope.
    pub enforce_views: bool,
    /// Requirements that were false when the group was staged.
    pub requirements: Vec<Requirement>,
    /// The owner's read set at staging time.
    pub trace: Vec<Dep>,
    /// Claims held by the group.
    pub claims: Vec<WriteScope>,
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
    /// Attempts restarted because their trace was invalidated before publish.
    pub retries: u32,
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
    /// Source location of the later claim.
    pub second_location: Option<&'static std::panic::Location<'static>>,
    /// The owner entry the later claim displaced. Restoring it re-establishes
    /// the earlier provider's committed post-state when the unit is not handed
    /// to the later one.
    pub first_owner: ProviderRef,
}

/// A unit installed in a derived View region.
#[derive(Debug)]
pub struct UnitSlot {
    /// The node holding the unit's value.
    pub node: NodeId,
}

/// The provider that owns one unit.
#[derive(Clone, Debug)]
pub struct ProviderRef {
    /// The defining node.
    pub node: NodeId,
    /// Human-readable defining site.
    pub label: &'static str,
    /// Source location of the definition call, when available.
    pub location: Option<&'static std::panic::Location<'static>>,
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
    /// Bumped whenever any unit's provider changes.
    pub routing_version: u64,
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
    /// Revision at which this invocation was last demanded.
    pub last_used: Revision,
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
    parents: FxHashMap<ScopeId, (ScopeId, String)>,
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
        self.parents
            .insert(scope, (parent, format!("discriminant {discriminant}")));
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
        self.parents
            .insert(scope, (parent, format!("key {}", key.key_display())));
        self.by_key.entry(lookup).or_default().push((key, scope));
        scope
    }

    /// Drops interned scopes that no live node references.
    pub fn prune(&mut self, live: &FxHashSet<ScopeId>) -> usize {
        let before = self.len();
        self.by_discriminant.retain(|_, scope| live.contains(scope));
        self.by_key.retain(|_, bucket| {
            bucket.retain(|(_, scope)| live.contains(scope));
            !bucket.is_empty()
        });
        self.parents.retain(|scope, _| live.contains(scope));
        before - self.len()
    }

    /// Returns one scope's parent, when it has one.
    #[must_use]
    pub fn parent(&self, scope: ScopeId) -> Option<ScopeId> {
        self.parents.get(&scope).map(|(parent, _)| *parent)
    }

    /// Returns the ancestry of one scope, outermost first.
    #[must_use]
    pub fn path(&self, scope: ScopeId) -> Vec<String> {
        let mut path = Vec::new();
        let mut current = scope;
        for _ in 0..64 {
            let Some((parent, label)) = self.parents.get(&current) else {
                break;
            };
            path.push(format!("{current:?} under {parent:?}: {label}"));
            current = *parent;
        }
        path.reverse();
        path
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
    ///
    /// For a routing subscription these are routing generations rather than
    /// value versions.
    pub last_versions: Vec<Version>,
    /// Whether `units` are watched by routing generation instead of value.
    pub routing: bool,
    /// Whether the subscription is still active.
    pub active: Arc<std::sync::atomic::AtomicBool>,
}

/// Resolves the node that currently holds a unit.
///
/// A host edit installs into the global table and a provider into its region;
/// both are installed units, so one lookup answers both.
pub(crate) fn current_unit_node(state: &State, unit: &UnitKey) -> Option<NodeId> {
    state.units.get(unit).copied().or_else(|| {
        state
            .regions
            .get(&unit.view)
            .and_then(|region| region.units.get(unit))
            .map(|slot| slot.node)
    })
}

/// Projects one unit out of a region's committed content.
///
/// This is the runtime's own projection, for the callers that bring no
/// accessor of their own, so it answers only the units the content vtable
/// spells: one field value, one entity's payload or presence, and one entity's
/// child slot. An index the accessor derives from order (a rank, an adjacency)
/// and a whole-field payload on a collection field belong to the accessor that
/// materialized them.
pub(crate) fn content_projection(region: &Region, unit: &UnitKey) -> Option<Value> {
    if unit.kind == UnitKind::Structure && unit.entity.is_none() {
        // A field-level structure unit initializes from the family's structure
        // projection. A family without one — a tree's node slots — has no
        // field-level projection at all, which is what this arm keeps distinct
        // from whatever the generic getter answers for the same field.
        return (region.descriptor.content.structure)(&region.content, unit.field)
            .map(|structure| structure.value);
    }
    (region.descriptor.content.get)(&region.content, unit)
}

/// Routing generation of one unit's node, or zero when it has no provider.
pub fn routing_version(state: &State, unit: &UnitKey) -> Version {
    current_unit_node(state, unit).map_or(0, |node| state.node(node).routing_at)
}

/// Version of one unit: its cell's change stamp, or the committed content's
/// when no cell exists, or zero when neither defines it.
///
/// This is the one answer to "what version does this unit have". A unit
/// subscription seeds `last_versions` from it and the delivery comparison asks
/// it again, so both halves of a notification are the same function of the same
/// state. A unit the runtime never materialized has no cell; that is the
/// revision `content_version` names, and the revision its first read stamps
/// the cell with, so the seed and the creation agree. Zero names a unit that
/// neither the cell table nor the content defines, so a subscription does not
/// read a later appearance as anything but a change.
pub fn unit_version(state: &State, unit: &UnitKey) -> Version {
    unit_version_of(state, unit).unwrap_or(0)
}

/// [`unit_version`], keeping "no answer" distinct from "revision zero".
///
/// A cell's stamp can legitimately be zero — a cell born at the initial content
/// revision — so a caller that needs to tell "defined at zero" from "not
/// defined" asks here. It is the one derivation; the scalar form is its
/// `unwrap_or(0)`, and the observation surface reads it too, so a host that
/// seeds a watermark from the board agrees with the runtime's own
/// subscriptions.
pub fn unit_version_of(state: &State, unit: &UnitKey) -> Option<Version> {
    match current_unit_node(state, unit) {
        Some(node) => Some(state.node(node).changed_at),
        None => content_version(state, unit),
    }
}

/// The revision the committed content gives one unit its answer at, or `None`
/// when the runtime cannot derive that unit from the content.
///
/// This is what [`unit_version`] answers while no cell exists, and the revision
/// a cell born from a content projection takes with it (`ensure_unit_node`), so
/// a subscription's seed and the first materialization's stamp are the same
/// number. Only the units the content vtable spells have one: an adjacency
/// index, a whole-field payload, and a field's protocol certificate belong to
/// the accessor that materializes them, and a cell it creates keeps the version
/// it was born with.
pub(crate) fn content_version(state: &State, unit: &UnitKey) -> Option<Version> {
    let region = state.regions.get(&unit.view)?;
    content_projection(region, unit)?;
    Some(state.node(region.content_node).changed_at)
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
    /// Nanoseconds spent marking invalidation cones.
    pub invalidation_ns: std::sync::atomic::AtomicU64,
    /// Automatic collections triggered on demand entry.
    pub auto_collects: std::sync::atomic::AtomicU64,
    /// Site nodes created on first interning.
    pub site_creations: std::sync::atomic::AtomicU64,
    /// Site-node lookups that reused an existing identity.
    pub site_reuses: std::sync::atomic::AtomicU64,
    /// Invocation records evicted by the cache cap.
    pub invocations_evicted: std::sync::atomic::AtomicU64,
    /// Trace validations attempted, successful or not.
    pub validation_attempts: std::sync::atomic::AtomicU64,
    /// Trace validations rejected at a moved observation.
    pub validation_failures: std::sync::atomic::AtomicU64,
    /// Closure attempts that ended in failure or cancellation.
    pub execution_failures: std::sync::atomic::AtomicU64,
    /// Successful commits whose value was equal (cutoff).
    pub cutoffs: std::sync::atomic::AtomicU64,
    /// Pooled attempts that suspended on another task's node.
    pub suspensions: std::sync::atomic::AtomicU64,
    /// Demand tasks that can evaluate, started over the runtime's lifetime.
    pub demands: std::sync::atomic::AtomicU64,
    /// Subscription callbacks that fired.
    pub notifications: std::sync::atomic::AtomicU64,
    /// General-component runs elided because no observed unit moved.
    pub no_change_runs: std::sync::atomic::AtomicU64,
    /// Write scopes claimed.
    pub write_claims: std::sync::atomic::AtomicU64,
    /// Overlapping live claims rejected.
    pub write_conflicts: std::sync::atomic::AtomicU64,
    /// Write groups deferred to the end of a demand.
    pub deferred_writes: std::sync::atomic::AtomicU64,
    /// Demand attempts restarted by commit-time validation.
    pub demand_retries: std::sync::atomic::AtomicU64,
    /// Reader waits on a live writer claim.
    pub reader_waits: std::sync::atomic::AtomicU64,
    /// Reader waits on a planned provider.
    pub reader_plan_waits: std::sync::atomic::AtomicU64,
    /// Planned ports registered by running computations.
    pub planned_ports: std::sync::atomic::AtomicU64,
    /// Planned ports withdrawn before their claim.
    pub unplanned_ports: std::sync::atomic::AtomicU64,
    /// Content values rebuilt because a definition unit actually moved.
    pub content_applies: std::sync::atomic::AtomicU64,
    /// Equal definition restatements that skipped content application.
    pub content_restatements: std::sync::atomic::AtomicU64,
    /// Changed units that move only a projection, not field content.
    pub content_projections: std::sync::atomic::AtomicU64,
    /// Batched content applications performed by definition commits.
    pub content_batches: std::sync::atomic::AtomicU64,
    /// Whole-content values published, one call per field commit.
    ///
    /// A write group that retracts or installs many units of one field must
    /// publish that field's content once; one commit per unit makes the group
    /// quadratic in the field's size. This is the counter that shows which one
    /// it is.
    pub content_commits: std::sync::atomic::AtomicU64,
    /// Node-commit payloads not built because no observer wanted node detail.
    pub observe_payloads_skipped: std::sync::atomic::AtomicU64,
    /// Source content installed from one changed content-moving unit.
    pub source_content_applies: std::sync::atomic::AtomicU64,
    /// Source content replacements that needed whole-content equality.
    pub source_equality_checks: std::sync::atomic::AtomicU64,
    /// Structure projections refreshed after a commit.
    pub structure_refreshes: std::sync::atomic::AtomicU64,
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
    /// Next monotonic validity stamp.
    pub next_validity: u64,
    /// Next monotonic value version.
    pub next_value_version: u64,
    /// Views declared (via `ensure_view`) by each scope's computations.
    pub declared_views: FxHashMap<ScopeId, std::collections::HashSet<ViewId>>,
    /// The component that owns each body scope.
    pub scope_components: FxHashMap<ScopeId, irrigation_core::ids::ComponentId>,
    /// The invocation body node that owns each body scope.
    pub scope_bodies: FxHashMap<ScopeId, NodeId>,
    /// Planned provider components for units, installed before values exist.
    pub route_plans: FxHashMap<UnitKey, irrigation_core::ids::ComponentId>,
    /// Ports a running computation has announced it will write.
    pub planned_ports: Vec<PlannedPort>,
    /// Reclaimed arena slots available for reuse.
    pub free_nodes: Vec<u32>,
    /// Revision of the last reclamation.
    pub last_collect: Revision,
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
    /// Names of components the runtime has seen, so a projection can name them.
    pub component_names: FxHashMap<irrigation_core::ids::ComponentId, &'static str>,
    /// One producing component per derived output View.
    pub(crate) producers: FxHashMap<ViewId, NodeId>,
    /// The materialized cell of every unit key, grouped by View.
    ///
    /// A cell stores a projection of its View's content, so a whole-region
    /// replacement has to find every cell it may have moved. The global unit
    /// table cannot answer that per View, and scanning it would make one
    /// replacement cost the whole runtime.
    pub(crate) cells: FxHashMap<ViewId, KeyTable<NodeId>>,
    /// Conflicting definitions awaiting end-of-revision resolution.
    pub pending_conflicts: Vec<PendingConflict>,
    /// Live write claims. Empty in the common case.
    ///
    /// Arbitration tests this list through [`WriteScope::covers`] and
    /// [`WriteScope::overlaps`]; the maps below are lookup aids, not policy.
    pub claims: Vec<WriteClaim>,
    /// Field/View claims, the only broader scopes an entity can overlap.
    pub broad_claims: Vec<WriteClaim>,
    /// Exact-scope claim lookup.
    pub claim_index: FxHashMap<WriteScope, WriteClaim>,
    /// Write groups deferred to the end of their demand.
    pub pending_writes: Vec<PendingWrite>,
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
    /// Allocates a node record, reusing a reclaimed slot when possible.
    ///
    /// A slot whose generation is exhausted is retired instead of wrapping, so
    /// a stale identity can never name a later node in the same slot. Retired
    /// slots stay on the free list, which keeps a later reclamation pass from
    /// recycling them. The arena index space is finite as well; exhausting it
    /// fails loudly.
    pub fn alloc(&mut self, mut record: NodeRecord) -> NodeId {
        let mut retired: smallvec::SmallVec<[u32; 4]> = smallvec::SmallVec::new();
        let index = loop {
            let Some(index) = self.free_nodes.pop() else {
                break self.push_slot(record);
            };
            let slot = index as usize;
            let generation = self.nodes[slot].generation;
            if generation == NodeId::MAX_GENERATION {
                retired.push(index);
                continue;
            }
            record.generation = generation + 1;
            record.freed = false;
            self.nodes[slot] = record;
            break index;
        };
        self.free_nodes.extend(retired);
        self.stats
            .nodes_created
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        NodeId::new(index, self.nodes[index as usize].generation)
    }

    /// Appends a new slot at generation zero.
    ///
    /// # Panics
    ///
    /// Panics when every representable arena index is in use.
    fn push_slot(&mut self, mut record: NodeRecord) -> u32 {
        let index = self.nodes.len();
        assert!(
            index <= NodeId::INDEX_MASK as usize,
            "node arena exhausted: {index} slots exceed {} index bits",
            NodeId::INDEX_BITS
        );
        record.generation = 0;
        record.freed = false;
        self.nodes.push(record);
        index as u32
    }

    /// Returns a node record.
    ///
    /// # Panics
    ///
    /// Panics when the identity refers to a reclaimed or reused slot.
    #[must_use]
    pub fn node(&self, id: NodeId) -> &NodeRecord {
        let record = &self.nodes[id.index()];
        assert_eq!(
            record.generation,
            id.generation(),
            "stale NodeId {id:?} for slot {}",
            id.index()
        );
        assert!(!record.freed, "reclaimed NodeId {id:?}");
        record
    }

    /// Returns a mutable node record.
    ///
    /// # Panics
    ///
    /// Panics when the identity refers to a reclaimed or reused slot.
    pub fn node_mut(&mut self, id: NodeId) -> &mut NodeRecord {
        let generation = self.nodes[id.index()].generation;
        assert_eq!(
            generation,
            id.generation(),
            "stale NodeId {id:?} for slot {}",
            id.index()
        );
        assert!(!self.nodes[id.index()].freed, "reclaimed NodeId {id:?}");
        &mut self.nodes[id.index()]
    }

    /// Allocates a fresh entity identity for a source collection edit.
    pub fn alloc_entity(&mut self) -> u64 {
        self.next_entity += 1;
        self.next_entity
    }

    /// Allocates a fresh invalidation stamp.
    pub fn alloc_validity(&mut self) -> u64 {
        self.next_validity += 1;
        self.next_validity
    }

    /// Allocates a fresh value version.
    pub fn alloc_value_version(&mut self) -> u64 {
        self.next_value_version += 1;
        self.next_value_version
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
                retries: 0,
            },
        );
        id
    }

    /// Whether a live provider claim covers `unit`.
    ///
    /// A region records one owner per claim identity, and an entity's value and
    /// membership units share that identity ([`WriteScope::entity`]), so a write
    /// to either answers to the same claim. Ownership is a hash lookup: scanning
    /// a provider's definition list would make a write linear in the View's
    /// unit count.
    #[must_use]
    pub(crate) fn unit_owned_by_live_provider(&self, unit: &UnitKey) -> bool {
        let Some(region) = self.regions.get(&unit.view) else {
            return false;
        };
        let WriteScope::Entity(owner) = WriteScope::entity(unit) else {
            return false;
        };
        region.owners.get(&owner).is_some()
    }

    /// Records the node holding one materialized cell of a View.
    pub(crate) fn record_cell(&mut self, unit: &UnitKey, node: NodeId) {
        self.cells
            .entry(unit.view)
            .or_default()
            .insert(Arc::new(unit.clone()), node);
    }

    /// Drops the recorded cell of one unit.
    pub(crate) fn forget_cell(&mut self, unit: &UnitKey) {
        if let Some(cells) = self.cells.get_mut(&unit.view) {
            cells.remove(unit);
            if cells.is_empty() {
                self.cells.remove(&unit.view);
            }
        }
    }

    /// Records a live write claim for `owner`.
    pub fn register_claim(
        &mut self,
        owner: NodeId,
        task: TaskId,
        scope: WriteScope,
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) {
        let scope = match &scope {
            WriteScope::Entity(unit) => WriteScope::entity(unit),
            other => other.clone(),
        };
        self.node_mut(owner).claims.push(scope.clone());
        let claim = WriteClaim {
            owner,
            task,
            scope: scope.clone(),
            label,
            location,
        };
        if matches!(claim.scope, WriteScope::Field(..) | WriteScope::View(_)) {
            self.broad_claims.push(claim.clone());
        }
        self.claim_index.insert(scope, claim.clone());
        self.claims.push(claim);
    }

    /// Claims a group of scopes atomically.
    ///
    /// Every scope is checked for a conflicting live claim before any claim is
    /// registered. A conflict therefore leaves the group's earlier scopes
    /// unclaimed, which is what lets two multi-port writers either conflict on
    /// the whole group or both proceed.
    ///
    /// Returns the existing and requested provider descriptions on conflict.
    pub fn claim_group(
        &mut self,
        owner: NodeId,
        task: TaskId,
        scopes: &[WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<usize, (String, String, String)> {
        for scope in scopes {
            if let Some(existing) = self.conflicting_claim(scope, owner) {
                return Err((
                    scope.describe(),
                    format!("{} ({:?})", existing.label, existing.location),
                    format!("{label} ({location:?})"),
                ));
            }
        }
        let mut claimed = 0usize;
        let mut already: std::collections::HashSet<WriteScope> = self
            .nodes
            .get(owner.index())
            .map(|record| record.claims.iter().cloned().collect())
            .unwrap_or_default();
        for scope in scopes {
            if already.insert(scope.clone()) {
                self.register_claim(owner, task, scope.clone(), label, location);
                claimed += 1;
            }
        }
        Ok(claimed)
    }

    /// Returns the invocation body whose identity scope is `scope`.
    ///
    /// A planned port belongs to the invocation body, not to the expression-site
    /// node that happened to call `plan_ports`; the site commits as soon as its
    /// expression returns and would retire the plan with it.
    #[must_use]
    pub fn invocation_body(&self, scope: ScopeId) -> Option<NodeId> {
        self.scope_bodies.get(&scope).copied().or_else(|| {
            self.invocations
                .values()
                .flat_map(|entries| entries.iter())
                .find(|invocation| invocation.scope == scope)
                .map(|invocation| invocation.body)
        })
    }

    /// Returns a live plan overlapping `scope` and owned by someone else.
    #[must_use]
    pub fn conflicting_plan(&self, scope: &WriteScope, owner: NodeId) -> Option<&PlannedPort> {
        self.planned_ports
            .iter()
            .find(|plan| plan.owner != owner && plan.scope.overlaps(scope))
    }

    /// Announces a group of ports this computation intends to write.
    ///
    /// A plan is not a claim: it installs nothing and does not conflict. It is
    /// visible to readers so they can wait for the running provider instead of
    /// returning the previous content.
    pub fn plan_group(
        &mut self,
        owner: NodeId,
        task: TaskId,
        scopes: &[WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<usize, (String, String, String)> {
        for scope in scopes {
            if let Some(existing) = self.conflicting_plan(scope, owner) {
                return Err((
                    scope.describe(),
                    format!("{} ({:?})", existing.label, existing.location),
                    format!("{label} ({location:?})"),
                ));
            }
        }
        let mut added = 0usize;
        for scope in scopes {
            let already = self
                .planned_ports
                .iter()
                .any(|plan| plan.owner == owner && &plan.scope == scope);
            if !already {
                self.planned_ports.push(PlannedPort {
                    owner,
                    task,
                    scope: scope.clone(),
                    label,
                    location,
                });
                added += 1;
            }
        }
        Ok(added)
    }

    /// Withdraws a group of planned ports owned by `owner`.
    pub fn unplan_group(&mut self, owner: NodeId, scopes: &[WriteScope]) -> usize {
        let before = self.planned_ports.len();
        self.planned_ports
            .retain(|plan| plan.owner != owner || !scopes.iter().any(|scope| scope == &plan.scope));
        before - self.planned_ports.len()
    }

    /// Returns a live plan covering `unit`, if any.
    #[must_use]
    pub fn planned_covering(&self, unit: &UnitKey) -> Option<&PlannedPort> {
        self.planned_ports
            .iter()
            .find(|plan| plan.scope.covers(unit))
    }

    /// Returns a claim that covers `unit`, if any.
    #[must_use]
    pub fn claim_covering(&self, unit: &UnitKey) -> Option<&WriteClaim> {
        self.claims.iter().find(|claim| claim.scope.covers(unit))
    }

    /// Returns a claim overlapping `scope` and owned by someone else.
    #[must_use]
    pub fn conflicting_claim(&self, scope: &WriteScope, owner: NodeId) -> Option<&WriteClaim> {
        // Entity claims are the hot path. Their exact identity is indexed, and
        // only a broad Field/View claim can overlap them. Scanning every entity
        // claim for every unit would make one large output group quadratic.
        if matches!(scope, WriteScope::Entity(_)) {
            let normalized = match scope {
                WriteScope::Entity(unit) => WriteScope::entity(unit),
                other => other.clone(),
            };
            if let Some(claim) = self.claim_index.get(&normalized)
                && claim.owner != owner
            {
                return Some(claim);
            }
            return self
                .broad_claims
                .iter()
                .find(|claim| claim.owner != owner && claim.scope.overlaps(scope));
        }
        self.claims
            .iter()
            .find(|claim| claim.owner != owner && claim.scope.overlaps(scope))
    }

    /// Releases every claim owned by `owner`.
    pub fn release_claims(&mut self, owner: NodeId) {
        let scopes = self
            .nodes
            .get(owner.index())
            .map_or_else(Vec::new, |record| record.claims.clone());
        for scope in scopes {
            self.claim_index.remove(&scope);
        }
        self.claims.retain(|claim| claim.owner != owner);
        self.broad_claims.retain(|claim| claim.owner != owner);
        self.planned_ports.retain(|plan| plan.owner != owner);
        if let Some(record) = self.nodes.get_mut(owner.index()) {
            record.claims.clear();
        }
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
pub fn invalidate(
    state: &mut State,
    seeds: impl IntoIterator<Item = NodeId>,
    revision: Revision,
) -> u64 {
    invalidate_tracked(state, seeds, revision).0
}

/// Whether a node holds committed data rather than a computed result.
///
/// A unit and a region's content are the data a commit installs; every other
/// kind is a result derived from them. Invalidation asks the difference twice —
/// a data node keeps its certification and only its readers are marked — so the
/// question has one owner.
fn is_data_node(record: &NodeRecord) -> bool {
    matches!(record.kind, NodeKind::Unit | NodeKind::RegionContent)
        && record.value.is_some()
        && record.has_valid_trace()
}

/// [`invalidate`], also reporting whether it marked a node that `revision` had
/// already finalized.
///
/// A node verified at `revision` with a value is a result this revision
/// published. Marking it suspect after its commit has landed is only correct
/// when something re-reads it before the demand's result escapes; the deferred
/// drain installs after the last commit, so it asks for this flag instead. The
/// seeds themselves are excluded: their values just committed and are the new
/// state, not a stale survivor of it.
///
/// The exclusion is about the seeds only. A seed's own suspect state never
/// decides whether its readers are visited: a node whose value already moved
/// this revision is skipped without restamping, but its dependents are still
/// walked, because a reader that re-certified after the earlier invalidation is
/// clean and this pass is the only one that can mark it.
#[must_use]
pub(crate) fn invalidate_tracked(
    state: &mut State,
    seeds: impl IntoIterator<Item = NodeId>,
    revision: Revision,
) -> (u64, bool) {
    // One stamp for the whole batch. It advances even when the revision does
    // not, so a node verified earlier in this revision is distinguishable from
    // one verified after this invalidation.
    let validity = state.alloc_validity();
    // A seed with no readers invalidates nobody, and a *data* seed needs no
    // suspect stamp either: its value is what a commit just installed, so it is
    // certified for this revision and every later read takes the certified
    // path. The difference is what the pass walks: the nodes a commit touched,
    // or every unit it published. A definition commit installs thousands of
    // cells at once, and marking them made a bulk publication pay per cell
    // instead of per reader.
    let mut stack: smallvec::SmallVec<[NodeId; 16]> = seeds
        .into_iter()
        .filter(|node| {
            let record = state.node(*node);
            !(record.dependents.is_empty() && is_data_node(record))
        })
        .collect();
    let roots: FxHashSet<NodeId> = stack.iter().copied().collect();
    let mut seen: FxHashSet<NodeId> = FxHashSet::default();
    let mut marked = 0u64;
    let mut finalized = false;
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        // The node keeps its stamp; its readers are what this pass still owes.
        let already_suspect = {
            let record = state.node(node);
            record.suspect_at >= revision && record.suspect_validity > record.verified_validity
        };
        if already_suspect {
            for dependent in &state.node(node).dependents {
                stack.push(*dependent);
            }
            continue;
        }
        let record = state.node_mut(node);
        // Data nodes are the seeds of invalidation: their value just committed,
        // so they stay certified at the new stamp and only their dependents are
        // marked suspect. Computed nodes reached through reverse edges are the
        // ones that must re-run.
        let data_seed = is_data_node(record);
        record.suspect_at = revision;
        record.suspect_validity = validity;
        if data_seed {
            record.verified_validity = validity;
        }
        marked += 1;
        if !roots.contains(&node)
            && record.value.is_some()
            && record.verified_at == revision
            && !record.is_running()
        {
            finalized = true;
        }
        for dependent in &record.dependents {
            stack.push(*dependent);
        }
    }
    state
        .stats
        .invalidations
        .fetch_add(marked, std::sync::atomic::Ordering::Relaxed);
    (marked, finalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use irrigation_core::descriptor::UnitKind;
    use irrigation_core::key::any_key;
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

    #[test]
    fn field_claim_conflicts_with_singleton_claim_in_either_order() {
        let view = ViewId(3);
        let field = FieldId(1);
        let singleton = UnitKey::field(view, field, UnitKind::Value);
        let field_scope = WriteScope::Field(view, field);
        let singleton_scope = WriteScope::entity(&singleton);

        for field_first in [true, false] {
            let mut state = State::default();
            let first = state.alloc(NodeRecord::unit("first", singleton.clone()));
            let second = state.alloc(NodeRecord::unit("second", singleton.clone()));
            let (first_scope, second_scope) = if field_first {
                (field_scope.clone(), singleton_scope.clone())
            } else {
                (singleton_scope.clone(), field_scope.clone())
            };
            state.register_claim(first, 1, first_scope, "first", None);

            let conflict = state
                .conflicting_claim(&second_scope, second)
                .expect("a whole-field claim and its singleton are one unit");
            assert_eq!(conflict.owner, first);
            assert_eq!(
                state.claim_covering(&singleton).map(|claim| claim.owner),
                Some(first)
            );
        }
    }

    #[test]
    fn one_entity_is_one_claim_across_unit_kinds() {
        let view = ViewId(3);
        let field = FieldId(1);
        let value = UnitKey::entity(view, field, UnitKind::Value, any_key(9u32));
        let membership = UnitKey::entity(view, field, UnitKind::Membership, any_key(9u32));
        let mut state = State::default();
        let first = state.alloc(NodeRecord::unit("first", value.clone()));
        let second = state.alloc(NodeRecord::unit("second", membership.clone()));
        state.register_claim(first, 1, WriteScope::Entity(value), "first", None);

        let conflict = state.conflicting_claim(&WriteScope::Entity(membership), second);
        assert_eq!(conflict.map(|claim| claim.owner), Some(first));
    }

    #[test]
    fn exhausted_generation_retires_the_slot_instead_of_wrapping() {
        let unit = UnitKey::field(ViewId(3), FieldId(1), UnitKind::Value);
        let mut state = State::default();
        let first = state.alloc(NodeRecord::unit("first", unit.clone()));
        assert_eq!(first.index(), 0);
        state.nodes[0].generation = NodeId::MAX_GENERATION;
        state.nodes[0].freed = true;
        state.free_nodes.push(0);

        let second = state.alloc(NodeRecord::unit("second", unit.clone()));
        assert_eq!(second.index(), 1, "an exhausted slot is retired");
        assert_eq!(second.generation(), 0);
        assert_eq!(state.nodes[0].generation, NodeId::MAX_GENERATION);
        assert_eq!(state.free_nodes, vec![0], "the retired slot stays reserved");

        let third = state.alloc(NodeRecord::unit("third", unit));
        assert_eq!(third.index(), 2, "the retired slot is never handed out");
        assert_eq!(state.free_nodes, vec![0]);
    }

    #[test]
    fn claim_group_conflicts_without_partial_claims() {
        let view = ViewId(9);
        let field = FieldId(0);
        let first =
            WriteScope::Entity(UnitKey::entity(view, field, UnitKind::Value, any_key(1u32)));
        let second_unit = UnitKey::entity(view, field, UnitKind::Value, any_key(2u32));
        let second = WriteScope::Entity(second_unit.clone());
        let mut state = State::default();
        let owner = state.alloc(NodeRecord::unit(
            "owner",
            UnitKey::field(view, field, UnitKind::Value),
        ));
        let other = state.alloc(NodeRecord::unit(
            "other",
            UnitKey::field(view, field, UnitKind::Value),
        ));
        let third = state.alloc(NodeRecord::unit(
            "third",
            UnitKey::field(view, field, UnitKind::Value),
        ));
        state.register_claim(owner, 0, first.clone(), "first", None);

        let conflict = state
            .claim_group(other, 1, &[first, second.clone()], "group", None)
            .expect_err("overlap must reject the whole group");
        assert!(!conflict.0.is_empty());
        assert!(
            state.claim_covering(&second_unit).is_none(),
            "the rejected group left a partial claim"
        );
        state
            .claim_group(third, 2, std::slice::from_ref(&second), "third", None)
            .expect("the uncontested scope is still free");
        assert!(state.claim_covering(&second_unit).is_some());
    }

    #[test]
    fn a_plan_is_visible_until_its_owner_commits() {
        let view = ViewId(9);
        let field = FieldId(0);
        let unit = UnitKey::entity(view, field, UnitKind::Value, any_key(7u32));
        let scope = WriteScope::Entity(unit.clone());
        let mut state = State::default();
        let owner = state.alloc(NodeRecord::unit("owner", unit.clone()));
        state
            .plan_group(owner, 0, &[scope], "plan", None)
            .expect("plan");
        assert!(state.planned_covering(&unit).is_some());
        state.release_claims(owner);
        assert!(
            state.planned_covering(&unit).is_none(),
            "release_claims must retire the plan too"
        );
    }

    #[test]
    fn a_plan_can_be_withdrawn_before_its_claim() {
        let view = ViewId(9);
        let field = FieldId(0);
        let planned = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
        let kept = UnitKey::entity(view, field, UnitKind::Value, any_key(2u32));
        let mut state = State::default();
        let owner = state.alloc(NodeRecord::unit("owner", planned.clone()));
        let scopes = [
            WriteScope::Entity(planned.clone()),
            WriteScope::Entity(kept.clone()),
        ];
        state
            .plan_group(owner, 0, &scopes, "plan", None)
            .expect("plan");
        state.unplan_group(owner, std::slice::from_ref(&scopes[0]));
        assert!(state.planned_covering(&planned).is_none());
        assert!(state.planned_covering(&kept).is_some());
    }

    #[test]
    fn a_claim_group_dedupes_the_ownership_pair() {
        let view = ViewId(9);
        let field = FieldId(0);
        let value = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
        let membership = UnitKey::entity(view, field, UnitKind::Membership, any_key(1u32));
        let other = UnitKey::entity(view, field, UnitKind::Value, any_key(2u32));
        let mut state = State::default();
        let owner = state.alloc(NodeRecord::unit("owner", value.clone()));
        let scopes = [
            WriteScope::entity(&value),
            WriteScope::entity(&membership),
            WriteScope::entity(&other),
        ];
        let claimed = state
            .claim_group(owner, 0, &scopes, "group", None)
            .expect("claim group");
        assert_eq!(claimed, 2, "one entity is one claim across its unit kinds");
        assert_eq!(state.node(owner).claims.len(), 2);
        assert!(state.claim_covering(&value).is_some());
        assert!(state.claim_covering(&other).is_some());
    }

    #[test]
    fn a_broad_claim_still_blocks_one_entity() {
        let view = ViewId(9);
        let field = FieldId(0);
        let unit = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
        let mut state = State::default();
        let first = state.alloc(NodeRecord::unit("first", unit.clone()));
        let second = state.alloc(NodeRecord::unit("second", unit.clone()));
        state.register_claim(first, 0, WriteScope::Field(view, field), "field", None);
        let conflict = state.conflicting_claim(&WriteScope::entity(&unit), second);
        assert_eq!(conflict.map(|claim| claim.owner), Some(first));
        state.release_claims(first);
        assert!(
            state
                .conflicting_claim(&WriteScope::entity(&unit), second)
                .is_none()
        );
    }

    #[test]
    fn overlapping_plans_conflict_before_any_claim() {
        let view = ViewId(9);
        let field = FieldId(0);
        let unit = UnitKey::entity(view, field, UnitKind::Value, any_key(1u32));
        let scope = WriteScope::entity(&unit);
        let mut state = State::default();
        let first = state.alloc(NodeRecord::unit("first", unit.clone()));
        let second = state.alloc(NodeRecord::unit("second", unit));
        state
            .plan_group(first, 0, std::slice::from_ref(&scope), "first", None)
            .expect("first plan");
        let conflict = state
            .plan_group(second, 1, std::slice::from_ref(&scope), "second", None)
            .expect_err("overlap must conflict");
        assert!(!conflict.0.is_empty());
        state.unplan_group(first, std::slice::from_ref(&scope));
        state
            .plan_group(second, 1, std::slice::from_ref(&scope), "second", None)
            .expect("handover after withdrawal");
    }
}
