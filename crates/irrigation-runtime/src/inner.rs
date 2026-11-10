//! The evaluation kernel: claims, ordered validation, execution, and commit.
//!
//! The algorithm is the sequential specification from the plan, refined for
//! multiple workers:
//!
//! ```text
//! demand(node, revision):
//!     if verified at revision: return cached value
//!     if another task owns it: suspend (cycle-checked)
//!     if not suspect at revision: mark verified and return cached value
//!     claim single-flight ownership
//!     if a trace exists: validate observations in order
//!         if every observation is unchanged: mark verified and return
//!     execute with a fresh recorder
//!     stage values, trace, and definitions
//!     commit atomically, preserving changed_at for equal values
//! ```

mod inspect;

use irrigation_core::hash::FxHashSet;
use parking_lot::{Condvar, Mutex, MutexGuard};
use smallvec::SmallVec;
use std::any::Any;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use irrigation_core::descriptor::{
    FieldDescriptor, FieldKind, UnitKey, UnitKind, ViewDescriptor, WriteScope,
};
use irrigation_core::error::{ComputeError, Result};
use irrigation_core::frame::{self, CancelToken, Dep, Frame, FrameGuard, Requirement};
use irrigation_core::hash::FxHashMap;
use irrigation_core::ids::{
    ComponentId, FieldId, NodeId, Revision, ScopeId, SiteId, Version, ViewId,
};
use irrigation_core::key::{AnyKey, any_key};
use irrigation_core::ops::{RuntimeOps, catch_engine, panic_engine};
use irrigation_core::value::{Value, value_of};

use crate::state::{
    DefinitionRecord, Invocation, NodeKind, NodeRecord, NodeState, ProviderRef, Region, State,
    UnitSlot, content_projection, current_unit_node, invalidate, invalidate_tracked,
};
use crate::task::{self, TaskId, TaskMode, TaskStatus};

/// A definition staged by a computation.
pub struct DefinitionDraft {
    /// The unit being defined.
    pub unit: UnitKey,
    /// The defined value.
    pub value: Value,
    /// Semantic equality for the value type.
    pub eq: fn(&Value, &Value) -> bool,
    /// Human-readable defining site.
    pub label: &'static str,
    /// Source location of the definition call, when available.
    pub location: Option<&'static std::panic::Location<'static>>,
}

impl std::fmt::Debug for DefinitionDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefinitionDraft")
            .field("unit", &self.unit)
            .field("label", &self.label)
            .finish()
    }
}

/// What one commit left behind.
///
/// A commit that publishes and then asks for a retry is not an aborted attempt:
/// the state a run staged belongs to the publication it produced. The outcome
/// carries that distinction instead of encoding it in where the error came from.
enum CommitOutcome {
    /// The value, trace, and write group published.
    Published(Value),
    /// The write group published, but an observation moved before the install
    /// began: the attempt re-runs against the state it just published.
    SelfInvalidated,
}

/// How much write arbitration the runtime performs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteArbitration {
    /// No claims. This is a one-release migration escape hatch.
    Off,
    /// Claims, requirements, deferred drains, and retries.
    #[default]
    Claims,
    /// Claims plus explicit `plan_route` declarations for every write and
    /// requirement port.
    Strict,
}

/// What one commit states about the content delta its replacement publishes.
///
/// A content write that arrives beside unit writes may have named its delta: an
/// editor stages every cell it touched, so the replacement is their sum. The
/// claim is about the transaction, not about the content, so the publisher
/// checks it per cell — but only a commit that stages cells at all can be the
/// sum of anything. A content write with no unit write beside it names nothing,
/// and every materialized cell the new content projects is the replacement's.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CellDelta<'a> {
    /// Units the same commit writes itself, by their own key; a replacement
    /// owes none of them. A retraction names both cells of the entity it
    /// clears, because it writes both.
    pub installed: &'a FxHashSet<UnitKey>,
    /// Whether the commit staged cells beside the replacement, which is what
    /// makes it their sum rather than an unrelated whole-region write.
    pub stated: bool,
}

/// Runtime configuration.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Worker threads used for parallel regions.
    pub workers: usize,
    /// Stack size of each worker thread.
    pub stack_size: usize,
    /// Maximum nested execution depth.
    pub max_depth: u32,
    /// How long an inline wait parks before re-checking a node.
    ///
    /// Pooled tasks do not poll: they suspend and are queued when the node
    /// completes. Only nested and workerless demands use this slice.
    pub wait_slice: Duration,
    /// Automatically reclaim after this many revisions on demand entry.
    ///
    /// `None` keeps reclamation manual.
    pub auto_collect_every: Option<u64>,
    /// Optional cap on retained component invocations.
    ///
    /// When exceeded, the least recently used invocations whose bodies are not
    /// referenced by any live trace are evicted. `None` keeps the
    /// reachability-only policy.
    pub max_invocations: Option<usize>,
    /// Maximum times one demand may restart because its trace was invalidated
    /// before it could publish.
    pub max_retries: u32,
    /// Write-arbitration policy.
    pub write_arbitration: WriteArbitration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            workers: 1,
            stack_size: 256 * 1024 * 1024,
            max_depth: 250_000,
            wait_slice: Duration::from_millis(20),
            auto_collect_every: None,
            max_invocations: None,
            max_retries: 32,
            write_arbitration: WriteArbitration::Claims,
        }
    }
}

/// The shared runtime state and its synchronization.
pub struct Inner {
    /// Configuration.
    pub config: RuntimeConfig,
    /// Metadata and node arena.
    pub state: Mutex<State>,
    /// Explicit provider-selection policy for live definition conflicts.
    pub route_selector: Mutex<Option<Arc<dyn crate::runtime::RouteSelector>>>,
    /// Host-installed demand recipes for missing ports, by View.
    pub port_providers: Mutex<FxHashMap<ViewId, Arc<dyn crate::runtime::PortProvider>>>,
    /// Wakes waiters when a node completes.
    pub wake: Condvar,
    /// Serializes edits against active derived-computation revisions.
    pub gate: crate::gate::Gate,
    /// Cached revision for lock-free fast paths.
    pub(crate) revision: AtomicU64,
    /// Weak self-reference used when building frames.
    self_ref: OnceLock<Weak<Inner>>,
    /// Workers that execute derived computation.
    pub pool: crate::scheduler::Pool,
    /// The most recently started demand, targeted by `cancel`.
    pub(crate) active_task: Mutex<Option<TaskId>>,
    /// Passive event sink. Set once, immediately after construction, before
    /// anything can demand.
    #[cfg(feature = "observe")]
    pub observer: OnceLock<Arc<dyn crate::observe::RuntimeObserver>>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeInner")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Whether `id` still names a live arena slot.
fn node_is_live(state: &State, id: NodeId) -> bool {
    state
        .nodes
        .get(id.index())
        .is_some_and(|record| record.generation == id.generation() && !record.freed)
}

/// The committed presence a membership cell stores, when it holds one.
fn membership_of(state: &State, node: NodeId) -> Option<bool> {
    state
        .node(node)
        .value
        .as_ref()
        .and_then(|value| value.downcast_ref::<bool>().copied())
}

/// The keyed/whole protocol one draft declares, if it declares one.
///
/// A `Structure` install publishes the keyed protocol; a `Value` install
/// publishes the whole one. Entity drafts publish members, not a protocol —
/// except on a graph field, where the per-entity writers are the only writers
/// a shared graph has: they install nodes and edges one at a time and never
/// stage the runtime-refreshed topology unit, and a field-level draft staged by
/// a writer would claim that unit from every other writer. The field is keyed
/// all the same, so its adjacency readers dispatch on the topology instead of a
/// cell nobody maintains.
fn draft_protocol(unit: &UnitKey, fields: &[FieldDescriptor]) -> Option<bool> {
    if unit.entity.is_some() {
        let graph = fields
            .get(usize::from(unit.field.0))
            .is_some_and(|field| field.kind == FieldKind::Graph);
        return graph.then_some(true);
    }
    match unit.kind {
        UnitKind::Structure => Some(true),
        UnitKind::Value => Some(false),
        _ => None,
    }
}

/// Collects the observed units whose version moved since the previous run.
///
/// Scope-filtered: a unit observed under a scope the component asked not to
/// retain is reported only when it is the unit itself, never through a
/// dependency of an unrelated scope.
fn observed_changes(
    state: &State,
    trace: &[Dep],
    unit_scopes: &FxHashMap<UnitKey, u64>,
    retained: &FxHashSet<u64>,
) -> Vec<crate::general::Change> {
    let mut changes = Vec::new();
    for dep in trace {
        let record = state.node(dep.node);
        if record.value_version == dep.version && record.routing_at == dep.routing {
            continue;
        }
        let Some(unit) = &record.unit else {
            continue;
        };
        if unit_scopes
            .get(unit)
            .is_some_and(|scope| !retained.contains(scope))
        {
            continue;
        }
        changes.push(crate::general::Change {
            unit: unit.clone(),
            previous: dep.version,
            current: record.value_version,
            routing_changed: record.value_version == dep.version,
        });
    }
    changes
}

/// Whether the unit is still a current definition of its certified provider.
///
/// A producer keeps every unit it does not explicitly retract, so a unit may
/// outlive the revision that staged it. Such a unit keeps its old `verified_at`
/// while its value stays authoritative, and a reader must not treat it as
/// undefined just because something marked it suspect. Ownership is a hash lookup: scanning the
/// provider's definition list here would make every read linear in the View size.
fn unit_definition_is_current(state: &State, node: NodeId, revision: Revision) -> bool {
    let record = state.node(node);
    let Some(unit) = &record.unit else {
        return false;
    };
    let Some(provider) = state.producers.get(&unit.view).copied() else {
        return false;
    };
    if state.node(provider).verified_at != revision || !state.node(provider).has_valid_trace() {
        return false;
    }
    state
        .regions
        .get(&unit.view)
        .and_then(|region| region.owners.get(&owner_key(unit)))
        .is_some_and(|owner| owner.node == provider)
}

/// Returns the producer of a unit whose value cannot be trusted yet.
///
/// A unit materialized from stale View content must re-derive from its producer
/// before a reader is allowed to re-certify it lazily.
fn bad_producer(state: &State, node: NodeId, revision: Revision) -> Option<NodeId> {
    let unit = state.node(node).unit.as_ref()?;
    let producer = state.producers.get(&unit.view).copied()?;
    (state.node(producer).verified_at != revision || !state.node(producer).has_valid_trace())
        .then_some(producer)
}

/// Records that `task` owns `node`'s claim until it commits, fails, or closes.
///
/// Ownership is registered when the claim is taken, not when its frame opens:
/// ordered validation runs user code between those two points. A task with no
/// slot is a detached evaluation, whose claims live on its thread until the
/// evaluation that took them ends.
fn hold_claim(state: &mut State, task: TaskId, node: NodeId) {
    let Some(slot) = state.tasks.get_mut(&task) else {
        task::hold_detached_claim(node);
        return;
    };
    if !slot.held.contains(&node) {
        slot.held.push(node);
    }
}

/// Drops a claim the task has published or failed.
fn forget_claim(state: &mut State, task: TaskId, node: NodeId) {
    let Some(slot) = state.tasks.get_mut(&task) else {
        task::forget_detached_claim(node);
        return;
    };
    slot.held.retain(|held| *held != node);
}

/// Adds the attempt's executing nodes to the claims its task still owns.
///
/// A suspension snapshots the active stack; that snapshot must extend the claim
/// set rather than replace it, because a node claimed for ordered validation is
/// not on the stack yet.
fn merge_held(state: &mut State, task: TaskId, nodes: &[NodeId]) {
    let Some(slot) = state.tasks.get_mut(&task) else {
        for node in nodes {
            task::hold_detached_claim(*node);
        }
        return;
    };
    for node in nodes {
        if !slot.held.contains(node) {
            slot.held.push(*node);
        }
    }
}

impl Inner {
    /// Creates the shared runtime.
    #[must_use]
    pub fn new(config: RuntimeConfig) -> Arc<Self> {
        irrigation_core::ops::quiet_engine_panics();
        let pool = crate::scheduler::Pool::new(config.workers, config.stack_size);
        let inner = Arc::new(Self {
            config,
            state: Mutex::new(State::default()),
            route_selector: Mutex::new(None),
            port_providers: Mutex::new(FxHashMap::default()),
            wake: Condvar::new(),
            gate: crate::gate::Gate::new(),
            revision: AtomicU64::new(0),
            self_ref: OnceLock::new(),
            pool,
            active_task: Mutex::new(None),
            #[cfg(feature = "observe")]
            observer: OnceLock::new(),
        });
        let _ = inner.self_ref.set(Arc::downgrade(&inner));
        inner
    }

    /// Creates the shared runtime with an installed observer.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn new_observed(
        config: RuntimeConfig,
        observer: Arc<dyn crate::observe::RuntimeObserver>,
    ) -> Arc<Self> {
        let inner = Self::new(config);
        let _ = inner.observer.set(observer);
        inner
    }

    /// Returns a strong reference to this runtime.
    #[must_use]
    pub fn arc(&self) -> Arc<Self> {
        self.self_ref
            .get()
            .and_then(Weak::upgrade)
            .expect("runtime inner outlived its strong owner")
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock()
    }

    /// Current revision.
    #[must_use]
    pub fn current_revision(&self) -> Revision {
        self.revision.load(Ordering::Acquire)
    }

    /// Publishes a new revision after an edit transaction.
    pub(crate) fn publish_revision(&self, revision: Revision) {
        self.revision.store(revision, Ordering::Release);
    }

    /// Registers a View descriptor and materializes its region.
    pub fn register_view(&self, descriptor: &'static ViewDescriptor) {
        let mut state = self.lock();
        if let Some(existing) = state.views.get(&descriptor.id) {
            // Identity is the full 64-bit name hash; a collision between two
            // different names is rejected instead of aliasing the namespaces.
            assert!(
                existing.name == descriptor.name,
                "two View names share one ViewId: `{}` and `{}`",
                existing.name,
                descriptor.name
            );
            return;
        }
        state.views.insert(descriptor.id, descriptor);
        let content = (descriptor.content.empty)();
        let content_node = state.alloc(NodeRecord::empty("view content", NodeKind::RegionContent));
        {
            let revision = state.revision;
            let value_version = state.alloc_value_version();
            state.node_mut(content_node).install_value(
                Some(content.clone()),
                value_version,
                revision,
            );
        }
        state.units.insert(
            Arc::new(UnitKey::field(
                descriptor.id,
                irrigation_core::ids::FieldId(0),
                UnitKind::Content,
            )),
            content_node,
        );
        state.regions.insert(
            descriptor.id,
            Region {
                descriptor,
                content,
                content_node,
                units: irrigation_core::key::KeyTable::new(),
                owners: irrigation_core::key::KeyTable::new(),
                routing_version: 0,
            },
        );
    }

    /// Returns the content of a registered View.
    #[must_use]
    pub fn view_content(&self, view: ViewId) -> Option<Arc<dyn Any + Send + Sync>> {
        self.lock()
            .regions
            .get(&view)
            .map(|region| Arc::clone(&region.content))
    }

    /// Returns the descriptor of a registered View.
    #[must_use]
    pub fn view_descriptor(&self, view: ViewId) -> Option<&'static ViewDescriptor> {
        self.lock().views.get(&view).copied()
    }

    /// Allocates or returns the child scope.
    pub fn child_scope(&self, parent: ScopeId, discriminant: u64, _label: &'static str) -> ScopeId {
        self.lock().scopes.child(parent, discriminant)
    }

    /// Whether a field currently uses the keyed derived protocol.
    pub fn field_is_keyed(&self, view: ViewId, field: FieldId) -> bool {
        self.lock().keyed_fields.contains(&(view, field))
    }

    /// Allocates or returns the child scope for a typed key.
    pub fn child_scope_keyed(
        &self,
        parent: ScopeId,
        key: Arc<dyn AnyKey>,
        _label: &'static str,
    ) -> ScopeId {
        self.lock().scopes.child_keyed(parent, key)
    }

    /// Returns (creating on first use) the node for one expression site.
    pub fn site_node(&self, scope: ScopeId, site: SiteId) -> NodeId {
        let mut state = self.lock();
        if let Some(node) = state.sites.get(&(scope, site)) {
            state
                .stats
                .site_reuses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return *node;
        }
        let node = state.alloc(NodeRecord::site(scope, site));
        state
            .stats
            .site_creations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        state.sites.insert((scope, site), node);
        node
    }

    /// Returns (creating on first use) a unit node, initialized on creation.
    pub fn unit_node_init(
        &self,
        view: ViewId,
        key: Arc<dyn AnyKey>,
        init: &mut dyn FnMut(&dyn Any) -> Option<Value>,
    ) -> NodeId {
        let mut state = self.lock();
        let typed = Self::typed_key(view, key);
        if let Some(node) = state.units.get(&typed).copied() {
            let stale = Self::cell_is_stale(&state, &typed, node);
            // A cell an earlier caller created without a projection is this
            // accessor's to fill, exactly like one a replacement opened: both
            // hold no answer, and both are answered by the committed content.
            let unfilled = !stale
                && !state.unit_owned_by_live_provider(&typed)
                && Self::cell_is_unfilled(&state, node);
            if stale || unfilled {
                let initial = init(Self::region_content(&state, view).as_ref());
                if stale || initial.is_some() {
                    Self::republish_cell(&mut state, node, initial);
                }
            }
            return node;
        }
        let initial = init(Self::region_content(&state, view).as_ref());
        Self::ensure_unit_node(&mut state, typed, initial)
    }

    /// Returns one View's committed content, or an empty value when it has no
    /// region yet: the caller's initializer decides what that means.
    fn region_content(state: &State, view: ViewId) -> Arc<dyn Any + Send + Sync> {
        state.regions.get(&view).map_or_else(
            || Arc::new(()) as Arc<dyn Any + Send + Sync>,
            |region| Arc::clone(&region.content) as Arc<dyn Any + Send + Sync>,
        )
    }

    /// Returns (creating on first use) a unit's node, with no initializer.
    ///
    /// A caller that brings no projection cannot re-derive a stale cell, so this
    /// is the vacant-cell form of [`Self::unit_node_init`].
    fn cell_node(&self, view: ViewId, key: Arc<dyn AnyKey>) -> NodeId {
        let mut state = self.lock();
        let typed = Self::typed_key(view, key);
        Self::ensure_unit_node(&mut state, typed, None)
    }

    /// Returns the unit key a caller addressed, or the View's field-0 value.
    fn typed_key(view: ViewId, key: Arc<dyn AnyKey>) -> UnitKey {
        match key.as_any().downcast_ref::<UnitKey>() {
            Some(unit) => unit.clone(),
            None => UnitKey::field(view, irrigation_core::ids::FieldId(0), UnitKind::Value),
        }
    }

    /// Returns the cell node of one unit, creating it vacant on first use.
    pub(crate) fn ensure_unit_node(
        state: &mut State,
        key: UnitKey,
        initial: Option<Value>,
    ) -> NodeId {
        if let Some(node) = state.units.get(&key) {
            return *node;
        }
        // A cell is born at the revision the committed content gave its unit,
        // whether or not the projection is available yet: the payload has been
        // the content's answer since the content changed, and a subscription
        // seeded from the same projection must not read the creation as a
        // change of its own. A unit the content cannot spell is born at the
        // zero `unit_version` answered before the cell existed, and keeps that
        // version until an accessor's projection or a definition moves it.
        let changed_at = crate::state::content_version(state, &key).unwrap_or(0);
        let mut record = NodeRecord::unit("unit", key.clone());
        record.changed_at = changed_at;
        if let Some(value) = initial {
            let value_version = state.alloc_value_version();
            record.install_value(Some(value), value_version, changed_at);
        }
        let node = state.alloc(record);
        state.units.insert(Arc::new(key.clone()), node);
        state.record_cell(&key, node);
        node
    }

    /// Whether a cell has never held an answer.
    ///
    /// A cell created without a projection — a generic read, a receipt, a unit
    /// no provider has defined yet — holds none, and the accessor that owns the
    /// projection is the one that can fill it. A retracted cell holds none
    /// either, but it has a generation of its own: its reader observes the
    /// retraction rather than a re-derived payload.
    fn cell_is_unfilled(state: &State, node: NodeId) -> bool {
        let record = state.node(node);
        record.value.is_none() && record.value_version == 0
    }

    /// Whether a materialized cell must be re-derived from its View's content.
    ///
    /// Only a cell a whole-region replacement left for the next read qualifies:
    /// a provider-owned cell is the provider's to publish, and a retracted cell
    /// must stay uncertified, because its reader observes the retraction rather
    /// than a re-derived payload.
    fn cell_is_stale(state: &State, unit: &UnitKey, node: NodeId) -> bool {
        state.node(node).content_stale && !state.unit_owned_by_live_provider(unit)
    }

    /// Re-derives one cell a whole-region replacement left for the next read.
    ///
    /// Returns whether the cell is current afterwards. A cell the content
    /// vtable spells is republished from the committed content. A cell only the
    /// accessor that materialized it can project — a rank or adjacency index, a
    /// whole-field payload — stays stale for that accessor: its pre-replacement
    /// payload is not this revision's answer, so the caller must not serve it.
    pub(crate) fn refresh_cell(state: &mut State, unit: &UnitKey, node: NodeId) -> bool {
        if !state.node(node).content_stale {
            return true;
        }
        if state.unit_owned_by_live_provider(unit) || !Self::content_spells(state, unit) {
            return false;
        }
        let initial = state
            .regions
            .get(&unit.view)
            .and_then(|region| content_projection(region, unit));
        Self::republish_cell(state, node, initial);
        true
    }

    /// Whether the content vtable spells one cell.
    ///
    /// A cell is spelled when the vtable answers that unit for that field: a
    /// field that holds one value (a scalar, or a tree's entity-less root
    /// cell), one entity's payload or presence, or one entity's child slot. A
    /// whole-field payload or an index the accessor computes belongs to the
    /// accessor that materialized it.
    fn content_spells(state: &State, unit: &UnitKey) -> bool {
        let Some(region) = state.regions.get(&unit.view) else {
            return false;
        };
        let Some(field) = region
            .descriptor
            .fields
            .get(usize::from(unit.field.0))
            .map(|field| field.kind)
        else {
            return false;
        };
        match unit.kind {
            // One entity's payload or presence is a projection of the content.
            UnitKind::Value | UnitKind::Membership if unit.entity.is_some() => true,
            // A field that holds one value projects that value: a scalar
            // field's cell, and a tree's entity-less root cell.
            UnitKind::Value => matches!(field, FieldKind::Scalar | FieldKind::Tree),
            // A structure cell is the field's projection when it has no
            // entity, and a family's child slot when it has one; the vtable
            // spells the child slot, and the field-level projection is
            // refreshed with the content instead of here.
            UnitKind::Structure => unit.entity.is_some(),
            // An index the accessor computes from order, a protocol
            // certificate, and the content unit itself are not projections.
            UnitKind::Membership
            | UnitKind::Rank
            | UnitKind::Adjacency
            | UnitKind::IncomingAdjacency
            | UnitKind::Content => false,
        }
    }

    /// The revision the committed content already answers one draft's value at.
    ///
    /// The content is the cutoff authority on the definition path exactly as it
    /// is on the edit path: a definition that restates what the content projects
    /// is the cell's birth, not a generation of its own. The value has been the
    /// content's answer since the content changed, so a subscription seeded from
    /// that content must not read the definition as a change. `None` means the
    /// content does not hold the draft, and installing it moves the unit.
    fn content_answer(
        state: &State,
        region: &Region,
        unit: &UnitKey,
        value: &Value,
        eq: fn(&Value, &Value) -> bool,
    ) -> Option<Revision> {
        let holds = if unit.kind == UnitKind::Membership {
            let present = (region.descriptor.content.contains)(&region.content, unit);
            eq(&value_of(present), value)
        } else {
            (region.descriptor.content.get)(&region.content, unit)
                .is_some_and(|current| eq(&current, value))
        };
        holds.then(|| state.node(region.content_node).changed_at)
    }

    /// Writes a re-derived cell's payload into the generation it belongs to.
    ///
    /// The replacement that opened this generation already allocated its version
    /// and stamped the revision it changed at, and a read that can now project
    /// the payload is installing that change rather than making one. The stamp
    /// is therefore kept, and only the payload observer's identity moves: a
    /// reader that recorded the unfilled generation recorded the absence, so it
    /// belongs to a version this fill retires.
    fn republish_cell(state: &mut State, node: NodeId, initial: Option<Value>) {
        let verified_at = state.revision;
        let changed_at = state.node(node).changed_at;
        let value_version = state.alloc_value_version();
        let dependents = state.node(node).dependents.clone();
        state
            .node_mut(node)
            .publish_fill(initial, value_version, changed_at, verified_at);
        // The payload is new to the graph even though its stamp is not: a
        // dependent that observed the unfilled cell, or the generation it
        // replaced, holds a result this payload contradicts, and a cell it
        // already certified for this revision would not revisit. Mark the
        // readers instead, so the next demand re-executes them.
        if !dependents.is_empty() {
            invalidate(state, dependents, verified_at);
        }
    }

    /// Claims a writable scope for the current computation.
    ///
    /// The claim is held by the executing node and released by commit, fail,
    /// cancellation, retirement, or a task teardown.
    pub fn claim_write(
        &self,
        scope: WriteScope,
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        self.claim_writes(std::slice::from_ref(&scope), label, location)
    }

    /// Announces a group of ports the current computation intends to write.
    pub fn plan_writes(
        &self,
        scopes: &[WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        if self.config.write_arbitration == WriteArbitration::Off || scopes.is_empty() {
            return Ok(());
        }
        let frame = frame::current().ok_or(ComputeError::OutsideComputation {
            operation: "write plan",
        })?;
        let task = task::current_or_detached();
        let mut state = self.lock();
        let owner = state
            .invocation_body(frame.scope())
            .unwrap_or_else(|| frame.node());
        match state.plan_group(owner, task, scopes, label, location) {
            Ok(added) => {
                if added > 0 {
                    state
                        .stats
                        .planned_ports
                        .fetch_add(added as u64, Ordering::Relaxed);
                }
                Ok(())
            }
            Err((port, first, second)) => {
                drop(state);
                Err(ComputeError::WriteConflict {
                    port,
                    first,
                    second,
                })
            }
        }
    }

    /// Withdraws planned ports for the current computation.
    pub fn unplan_writes(&self, scopes: &[WriteScope]) -> Result<()> {
        let frame = frame::current().ok_or(ComputeError::OutsideComputation {
            operation: "write plan withdrawal",
        })?;
        let mut state = self.lock();
        let owner = state
            .invocation_body(frame.scope())
            .unwrap_or_else(|| frame.node());
        let removed = state.unplan_group(owner, scopes);
        if removed > 0 {
            state
                .stats
                .unplanned_ports
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Claims a group of writable scopes atomically.
    ///
    /// All conflicts and strict declarations are checked before any claim is
    /// registered, so a rejected group has no partial ownership.
    pub fn claim_writes(
        &self,
        scopes: &[WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        if self.config.write_arbitration == WriteArbitration::Off || scopes.is_empty() {
            return Ok(());
        }
        let frame = frame::current().ok_or(ComputeError::OutsideComputation {
            operation: "write claim",
        })?;
        let owner = frame.node();
        let task = task::current_or_detached();
        let mut state = self.lock();
        if self.config.write_arbitration == WriteArbitration::Strict {
            let component = component_of(&state, frame.scope());
            for scope in scopes {
                let declared = component
                    .is_some_and(|component| planned_for_component(&state, scope, component));
                if !declared {
                    drop(state);
                    return Err(ComputeError::WriteOutOfScope {
                        port: scope.describe(),
                    });
                }
            }
        }
        match state.claim_group(owner, task, scopes, label, location) {
            Ok(claimed) => {
                if claimed > 0 {
                    state
                        .stats
                        .write_claims
                        .fetch_add(claimed as u64, Ordering::Relaxed);
                }
                drop(state);
                #[cfg(feature = "observe")]
                for scope in scopes {
                    self.emit(crate::observe::RuntimeEvent::Arbitration {
                        kind: "write_claim",
                        node: owner,
                        port: scope.describe(),
                    });
                }
                Ok(())
            }
            Err((conflict_port, first, second)) => {
                state.stats.write_conflicts.fetch_add(1, Ordering::Relaxed);
                drop(state);
                #[cfg(feature = "observe")]
                self.emit(crate::observe::RuntimeEvent::Arbitration {
                    kind: "write_conflict",
                    node: owner,
                    port: conflict_port.clone(),
                });
                Err(ComputeError::WriteConflict {
                    port: conflict_port,
                    first,
                    second,
                })
            }
        }
    }

    /// Waits for a live writer claiming one of `ports`.
    pub fn wait_for_port(&self, ports: &[UnitKey]) -> Result<bool> {
        let owner = {
            let state = self.lock();
            ports.iter().find_map(|port| {
                state
                    .claim_covering(port)
                    .filter(|claim| state.node(claim.owner).is_running())
                    .map(|claim| claim.owner)
            })
        };
        let Some(owner) = owner else {
            return Ok(false);
        };
        let me = task::current_or_detached();
        self.wait_for(owner, me)?;
        Ok(true)
    }

    /// Runs host-installed providers for missing ports.
    pub fn provide_ports(&self, ports: &[UnitKey]) -> Result<bool> {
        let providers: Vec<(Arc<dyn crate::runtime::PortProvider>, UnitKey)> = {
            let registered = self.port_providers.lock();
            ports
                .iter()
                .filter_map(|port| {
                    registered
                        .get(&port.view)
                        .cloned()
                        .map(|provider| (provider, port.clone()))
                })
                .collect()
        };
        if providers.is_empty() {
            return Ok(false);
        }
        for (provider, port) in providers {
            provider.provide(&port)?;
        }
        Ok(true)
    }

    /// Records a structural precondition on the current write group.
    ///
    /// The condition is the caller's evaluated predicate about `ports`; the
    /// requirement asserts that those ports are present at commit. A false
    /// condition therefore defers the write group or fails it — a state in
    /// which the ports are absent never satisfies the write that observed the
    /// absence.
    pub fn require_condition(
        &self,
        condition: bool,
        ports: &[UnitKey],
        description: &'static str,
    ) -> Result<()> {
        if self.config.write_arbitration == WriteArbitration::Strict {
            let state = self.lock();
            for port in ports {
                if !port_declared(&state, port) {
                    return Err(ComputeError::WriteOutOfScope {
                        port: format!("{port:?}"),
                    });
                }
            }
        }
        if frame::current().is_none() {
            return Err(ComputeError::OutsideComputation {
                operation: "write requirement",
            });
        }
        if ports.is_empty() {
            // Nothing can be re-evaluated at commit, so honour the condition
            // now instead of registering a requirement no port can meet.
            if condition {
                return Ok(());
            }
            return Err(ComputeError::PreconditionUnmet {
                condition: description.to_string(),
                ports: Vec::new(),
                reason: irrigation_core::error::PreconditionReason::Contradiction,
            });
        }
        frame::require(Requirement::present(ports.to_vec(), description));
        Ok(())
    }

    /// Reads a unit at the current revision and records the observation.
    pub fn read_unit(&self, node: NodeId) -> Result<Value> {
        let me = task::current_or_detached();
        loop {
            match self.read_unit_step(node) {
                ReadStep::Ready(observation) => {
                    self.record_read(node, &observation);
                    return Ok(observation.value.expect("a ready read carries a value"));
                }
                ReadStep::Absent(observation) => {
                    self.record_read(node, &observation);
                    return Err(ComputeError::Absent { node });
                }
                ReadStep::Fail(error) => return Err(error),
                ReadStep::Retry => continue,
                ReadStep::Wait(target) => self.wait_for(target, me)?,
                ReadStep::Demand(producer) => {
                    self.demand_producer(producer)?;
                }
            }
        }
    }

    /// Decides one step of the unit-read protocol.
    ///
    /// The blocking read and the receipt path share this decision, so a derived
    /// unit is never served before its producer has run; only how the reader
    /// waits differs.
    fn read_unit_step(&self, node: NodeId) -> ReadStep {
        let reader_node = frame::current().map(|frame| frame.node());
        let (pending_producer, definition_current, unit_provider, claim_owner, planned_owner) = {
            let state = self.lock();
            let revision = state.revision;
            let unit = state.node(node).unit.clone();
            let claim_owner = unit.as_ref().and_then(|unit| {
                state
                    .claim_covering(unit)
                    .filter(|claim| {
                        Some(claim.owner) != reader_node && state.node(claim.owner).is_running()
                    })
                    .map(|claim| claim.owner)
            });
            let planned_owner = unit.as_ref().and_then(|unit| {
                state
                    .planned_covering(unit)
                    .filter(|plan| {
                        Some(plan.owner) != reader_node
                            && !task::stack_contains(plan.task)
                            && state.node(plan.owner).is_running()
                    })
                    .map(|plan| plan.owner)
            });
            (
                bad_producer(&state, node, revision),
                unit_definition_is_current(&state, node, revision),
                unit.as_ref()
                    .and_then(|unit| state.producers.get(&unit.view).copied()),
                claim_owner,
                planned_owner,
            )
        };
        let mut state = self.lock();
        let revision = state.revision;
        if let Some(owner) = claim_owner {
            state.stats.reader_waits.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "observe")]
            let port = state
                .node(node)
                .unit
                .as_ref()
                .map_or_else(|| format!("{node:?}"), |unit| format!("{unit:?}"));
            drop(state);
            #[cfg(feature = "observe")]
            self.emit(crate::observe::RuntimeEvent::Arbitration {
                kind: "reader_wait",
                node: owner,
                port,
            });
            return ReadStep::Wait(owner);
        }
        if let Some(owner) = planned_owner {
            state.stats.reader_waits.fetch_add(1, Ordering::Relaxed);
            state
                .stats
                .reader_plan_waits
                .fetch_add(1, Ordering::Relaxed);
            drop(state);
            #[cfg(feature = "observe")]
            let port = {
                let state = self.lock();
                state
                    .node(node)
                    .unit
                    .as_ref()
                    .map_or_else(|| format!("{node:?}"), |unit| format!("{unit:?}"))
            };
            #[cfg(feature = "observe")]
            self.emit(crate::observe::RuntimeEvent::Arbitration {
                kind: "reader_wait",
                node: owner,
                port,
            });
            return ReadStep::Wait(owner);
        }
        if let Some(producer) = pending_producer {
            return ReadStep::Demand(producer);
        }
        // A cell a whole-region replacement owns holds a projection of the new
        // content. This read brings no initializer, so re-derive it from the
        // content where the vtable spells the unit. Where it does not — a rank
        // or adjacency index, a whole-field payload — only the accessor that
        // materialized it can, and this read has no answer for the revision:
        // the pre-replacement payload is not one, and retrying until the
        // attempt gives up is not either. A provider-owned cell is not the
        // replacement's: the provider protocol above decides when it runs.
        if let Some(unit) = state.node(node).unit.clone()
            && state.node(node).content_stale
            && !state.unit_owned_by_live_provider(&unit)
            && !Self::refresh_cell(&mut state, &unit, node)
        {
            let record = state.node(node);
            return ReadStep::Absent(Observation {
                value: None,
                version: record.value_version,
                routing: record.routing_at,
                validity: record.verified_validity,
                provider: unit_provider,
            });
        }
        let record = state.node_mut(node);
        if let Some(failure) = &record.failure {
            if record.failed_at == revision {
                return ReadStep::Fail(failure.clone());
            }
            record.failure = None;
            record.state = NodeState::Vacant;
            return ReadStep::Retry;
        }
        // A running node may still carry the previous revision's value; serving
        // it would let the reader certify a value the owner is replacing.
        if record.is_running() {
            return ReadStep::Wait(node);
        }
        if record.verified_at == revision && record.has_valid_trace() {
            return match &record.value {
                Some(value) => ReadStep::Ready(Observation {
                    value: Some(value.clone()),
                    version: record.value_version,
                    routing: record.routing_at,
                    validity: record.verified_validity,
                    provider: unit_provider,
                }),
                None => ReadStep::Absent(Observation {
                    value: None,
                    version: record.value_version,
                    routing: record.routing_at,
                    validity: record.verified_validity,
                    provider: unit_provider,
                }),
            };
        }
        if (record.suspect_at <= record.verified_at || definition_current)
            && record.has_valid_trace()
            && record.value.is_some()
        {
            record.certify(revision);
            return ReadStep::Ready(Observation {
                value: record.value.clone(),
                version: record.value_version,
                routing: record.routing_at,
                validity: record.verified_validity,
                provider: unit_provider,
            });
        }
        // The unit exists as a node but no producer has defined it at this
        // revision. That is a semantic answer, not a failure: the reader records
        // it as a negative observation, so a later definition invalidates it.
        ReadStep::Absent(Observation {
            value: None,
            version: record.value_version,
            routing: record.routing_at,
            validity: record.verified_validity,
            provider: unit_provider,
        })
    }

    /// Records one completed unit read, including the View's provider.
    ///
    /// The observation is recorded for a negative read too: a reader that saw an
    /// absent unit is tied to that unit, so a later definition — or a provider
    /// handover — invalidates it instead of leaving a value derived from a
    /// false absence in the graph.
    ///
    /// The provider is an observation of its own: a handover must invalidate the
    /// reader even when the unit's value is unchanged. Once per run, since the
    /// trace lists producers rather than reads.
    fn record_read(&self, node: NodeId, observation: &Observation) {
        frame::record_dep(Dep {
            node,
            version: observation.version,
            routing: observation.routing,
            validity: observation.validity,
        });
        let Some(provider_node) = observation.provider else {
            return;
        };
        let (version, routing, validity) = {
            let state = self.lock();
            let record = state.node(provider_node);
            (
                record.value_version,
                record.routing_at,
                record.verified_validity,
            )
        };
        frame::record_provider_dep(Dep {
            node: provider_node,
            version,
            routing,
            validity,
        });
    }

    /// Demands the general component that produces a View.
    fn demand_producer(&self, producer: NodeId) -> Result<Value> {
        let mut compute = || {
            Err(ComputeError::internal(
                "a general component producer does not use a compute closure",
            ))
        };
        let eq = {
            let state = self.lock();
            let general = state
                .node(producer)
                .general
                .map(|index| Arc::clone(&state.generals[index]));
            move |left: &Value, right: &Value| {
                general
                    .as_ref()
                    .is_some_and(|general| general.output_eq(left, right))
            }
        };
        self.eval(producer, &mut compute, &eq)
    }

    /// Re-certifies a general component whose observed units are all unchanged.
    ///
    /// Observed units may look unverified only because their producers have not
    /// run yet. Producers are certified first (in a scratch frame, so the walk
    /// never becomes part of any trace); if every observed version survives, the
    /// component is re-certified without running its body.
    fn certify_observed(&self, trace: &[Dep], revision: Revision) -> Result<()> {
        let views: Vec<ViewId> = {
            let state = self.lock();
            trace
                .iter()
                .filter_map(|dep| {
                    let record = state.node(dep.node);
                    let unit = record.unit.as_ref()?;
                    (record.verified_at != revision || !record.has_valid_trace())
                        .then_some(unit.view)
                })
                .collect()
        };
        if views.is_empty() {
            return Ok(());
        }
        let mut seen = irrigation_core::hash::FxHashSet::default();
        for view in views {
            ensure_view_current(self, view, &mut seen, 0)?;
        }
        Ok(())
    }

    /// Demands a node, executing `compute` when reuse is impossible.
    ///
    /// The claim decision, the frame construction, and the commit tail live in
    /// separate functions on purpose: this frame stays live for every ancestor
    /// demand in a recursive computation, so it must not carry the locals of
    /// the claim protocol or of commit. As a single function it was a 4.6 KB
    /// debug-build frame, and four of those are live per component-call level.
    pub fn eval(
        &self,
        node: NodeId,
        compute: &mut dyn FnMut() -> Result<Value>,
        eq: &dyn Fn(&Value, &Value) -> bool,
    ) -> Result<Value> {
        // A slot-less evaluation has no task teardown to release the claims it
        // takes, so this call is its teardown.
        let _claims = DetachedClaims::new(self);
        match self.begin_eval(node)? {
            Begin::Done(value) => Ok(value),
            Begin::General { index, claim } => self.eval_general(node, index, claim),
            Begin::Run(claim) => self.run_claimed(node, compute, eq, claim),
        }
    }

    /// Runs a custom component under its own active-node marker.
    #[inline(never)]
    fn eval_general(&self, node: NodeId, index: usize, claim: Claim) -> Result<Value> {
        task::active_push(node);
        let result = self.run_general(node, index, claim.revision, claim.owner, claim.generation);
        task::active_pop(node);
        result
    }

    /// Claims the node and decides whether its cached value can be reused.
    ///
    /// Kept out of [`Inner::eval`] so the claim protocol's state is not part of
    /// the stack frame that a recursive demand keeps alive.
    #[inline(never)]
    fn begin_eval(&self, node: NodeId) -> Result<Begin> {
        if frame::current().is_some_and(|frame| frame.cancel().is_cancelled()) {
            return Err(ComputeError::Cancelled);
        }
        let me = task::current_or_detached();
        let generation = loop {
            let action = {
                let mut state = self.lock();
                let revision = state.revision;
                let validity = state.next_validity;
                let record = state.node_mut(node);
                if let Some(failure) = &record.failure {
                    if record.failed_at == revision {
                        Action::Fail(failure.clone())
                    } else {
                        record.failure = None;
                        record.state = NodeState::Vacant;
                        record.attempt_validity = validity;
                        let generation = state.next_generation + 1;
                        state.next_generation = generation;
                        state.node_mut(node).state = NodeState::Running {
                            owner: me,
                            generation,
                        };
                        Action::Claim(generation)
                    }
                } else if record.verified_at == revision
                    && record.value.is_some()
                    && record.has_valid_trace()
                {
                    Action::Return(
                        record.value.clone().expect("checked"),
                        record.value_version,
                        record.routing_at,
                        record.verified_validity,
                    )
                } else if record.is_running() {
                    match record.state {
                        NodeState::Running { owner, generation } if owner == me => {
                            if task::active_contains(node) {
                                Action::Fail(ComputeError::Cycle {
                                    path: vec![format!(
                                        "re-entrant demand of {} ({node:?})",
                                        record.label
                                    )],
                                })
                            } else {
                                Action::Resume(generation)
                            }
                        }
                        _ => Action::Wait,
                    }
                } else if record.suspect_at <= record.verified_at
                    && record.has_valid_trace()
                    && record.value.is_some()
                {
                    record.certify(revision);
                    record.validated_at = revision;
                    Action::Return(
                        record.value.clone().expect("checked"),
                        record.value_version,
                        record.routing_at,
                        record.verified_validity,
                    )
                } else {
                    state.next_generation += 1;
                    let generation = state.next_generation;
                    state.node_mut(node).attempt_validity = validity;
                    state.node_mut(node).state = NodeState::Running {
                        owner: me,
                        generation,
                    };
                    Action::Claim(generation)
                }
            };

            match action {
                Action::Return(value, version, routing, validity) => {
                    frame::record_dep(Dep {
                        node,
                        version,
                        routing,
                        validity,
                    });
                    return Ok(Begin::Done(value));
                }
                Action::Fail(error) => return Err(error),
                Action::Wait => self.wait_for(node, me)?,
                Action::Claim(generation) => {
                    // Ownership starts here, not when the frame opens: ordered
                    // validation runs user code, and an error, panic, or
                    // cancellation on that path must still release the claim.
                    let mut state = self.lock();
                    hold_claim(&mut state, me, node);
                    drop(state);
                    #[cfg(feature = "observe")]
                    self.emit(crate::observe::RuntimeEvent::NodeClaimed { node, task: me });
                    break generation;
                }
                Action::Resume(generation) => {
                    let mut state = self.lock();
                    hold_claim(&mut state, me, node);
                    break generation;
                }
            }
        };

        // Ordered validation. Re-certifying a node without running it needs its
        // observed inputs to be final, which is a property of every traced node
        // rather than of one component kind: certify the producers of the views
        // this node observed (depth-first, in a scratch frame) before comparing
        // versions. Without this a consumer could validate against a producer
        // that has not run yet and mistake an unchanged observation for a stale
        // one.
        let (revision, scope, trace, has_value) = {
            let state = self.lock();
            let record = state.node(node);
            (
                state.revision,
                record.scope,
                record.trace.clone(),
                record.value.is_some(),
            )
        };
        if has_value {
            self.certify_observed(&trace, revision)?;
        }
        if has_value && self.validate_trace(&trace, revision) {
            let (value, version, routing, validity) = {
                let mut state = self.lock();
                let general = state.node(node).general.is_some();
                if general {
                    state.stats.no_change_runs.fetch_add(1, Ordering::Relaxed);
                }
                let record = state.node_mut(node);
                record.state = NodeState::Ready;
                record.certify(revision);
                record.validated_at = revision;
                record.suspect_at = 0;
                let observed = (
                    record
                        .value
                        .clone()
                        .expect("validated node without a value"),
                    record.value_version,
                    record.routing_at,
                    record.verified_validity,
                );
                forget_claim(&mut state, me, node);
                observed
            };
            frame::record_dep(Dep {
                node,
                version,
                routing,
                validity,
            });
            #[cfg(feature = "observe")]
            self.emit(crate::observe::RuntimeEvent::NodeValidated {
                node,
                deps: trace.len() as u32,
            });
            return Ok(Begin::Done(value));
        }

        let claim = Claim {
            revision,
            scope,
            generation,
            owner: me,
        };
        // Custom components run their own algorithm instead of the caller's
        // closure; their state and output commit atomically.
        let general_index = self.lock().node(node).general;
        if let Some(index) = general_index {
            return Ok(Begin::General { index, claim });
        }
        Ok(Begin::Run(claim))
    }

    /// Pushes the execution frame for one claimed node.
    ///
    /// The frame object is large in debug builds; constructing it in its own
    /// function keeps that temporary out of the frame that stays live across
    /// the body's recursive demands.
    #[inline(never)]
    fn open_frame(&self, node: NodeId, claim: Claim) -> Result<ClaimedFrame> {
        let parent = frame::current();
        let (depth, cancel) = match &parent {
            Some(parent) => (parent.depth(), parent.cancel().clone()),
            None => (0, CancelToken::default()),
        };
        if depth + 1 > self.config.max_depth {
            return self.fail(
                node,
                ComputeError::DepthExceeded {
                    limit: self.config.max_depth,
                },
                claim.revision,
            );
        }
        // Frames are thread-local execution state: `RefCell` is intentional and
        // the frame never crosses a thread boundary.
        #[allow(clippy::arc_with_non_send_sync)]
        let frame = Arc::new(Frame::computing(
            self.arc(),
            node,
            claim.scope,
            depth + 1,
            cancel.clone(),
        ));
        task::active_push(node);
        let guard = FrameGuard::push(Arc::clone(&frame));
        Ok(ClaimedFrame {
            frame,
            guard,
            cancel,
            started: std::time::Instant::now(),
        })
    }

    /// Runs a claimed node's caller-supplied closure and hands off to commit.
    ///
    /// Only the locals that must survive the body's recursive demands live
    /// here; everything the commit needs is read after the body returns.
    #[inline(never)]
    fn run_claimed(
        &self,
        node: NodeId,
        compute: &mut dyn FnMut() -> Result<Value>,
        eq: &dyn Fn(&Value, &Value) -> bool,
        claim: Claim,
    ) -> Result<Value> {
        let claimed = self.open_frame(node, claim)?;
        let result = catch_engine(compute);
        self.finish_eval(node, claimed, result, eq, claim)
    }

    /// Takes the execution result, commits the write group, and records the
    /// observation in the caller.
    #[inline(never)]
    fn finish_eval(
        &self,
        node: NodeId,
        claimed: ClaimedFrame,
        result: Result<Result<Value>>,
        eq: &dyn Fn(&Value, &Value) -> bool,
        claim: Claim,
    ) -> Result<Value> {
        let ClaimedFrame {
            frame,
            guard,
            cancel,
            started,
        } = claimed;
        let definitions: Vec<DefinitionDraft> = frame.take_staged();
        let requirements = frame.take_requirements();
        let retractions = frame.take_retractions();
        let deps = frame.take_deps();
        drop(guard);
        task::active_pop(node);

        if cancel.is_cancelled() {
            return self.fail(node, ComputeError::Cancelled, claim.revision);
        }
        match result {
            Ok(Ok(value)) => {
                // A lowered component's body restates its whole output on every
                // run, so its commit replaces the definition set. Only a custom
                // component writes deltas.
                let committed = self.commit(
                    node,
                    value,
                    deps,
                    definitions,
                    requirements,
                    Vec::new(),
                    (retractions, true),
                    true,
                    true,
                    None,
                    FxHashMap::default(),
                    FxHashSet::default(),
                    eq,
                    claim.revision,
                    claim.owner,
                    claim.generation,
                    &cancel,
                )?;
                let committed = match committed {
                    CommitOutcome::Published(committed) => committed,
                    CommitOutcome::SelfInvalidated => return Err(ComputeError::Retry),
                };
                #[cfg(feature = "observe")]
                let changed_at = self.changed_at(node);
                let version = self.value_version_at(node);
                let routing = self.routing_at(node);
                let validity = self.validity_at(node);
                frame::record_dep(Dep {
                    node,
                    version,
                    routing,
                    validity,
                });
                #[cfg(feature = "observe")]
                self.note_commit(node, changed_at, claim.revision, started);
                #[cfg(not(feature = "observe"))]
                let _ = (started, version);
                Ok(committed)
            }
            Ok(Err(error)) | Err(error) => {
                // A suspension is scheduling, not failure: the claim is kept
                // and the task is queued again when the awaited node completes.
                if error.is_suspended() || error.is_retry() {
                    return Err(error);
                }
                self.fail(node, error, claim.revision)
            }
        }
    }

    /// Runs one custom component: initialize or update, then commit the new
    /// state, output, trace, and definitions atomically.
    fn run_general(
        &self,
        node: NodeId,
        index: usize,
        revision: Revision,
        owner: TaskId,
        generation: u64,
    ) -> Result<Value> {
        #[cfg(feature = "observe")]
        let started = std::time::Instant::now();
        let (general, old_trace, old_state, scope, old_unit_scopes, old_retained) = {
            let state = self.lock();
            let record = state.node(node);
            let general = Arc::clone(&state.generals[index]);
            (
                general,
                record.trace.clone(),
                record.general_state.clone(),
                record.scope,
                record.general_unit_scopes.clone(),
                record.general_retained_scopes.clone(),
            )
        };
        // The batch is lazy: it is materialized when the body first asks, which
        // is after the body's own reads have pulled the producers it observes.
        // No pre-walk is needed to make it accurate, and evaluation order no
        // longer decides what update sees.
        let changes = {
            let weak = Arc::downgrade(&self.arc());
            let trace = old_trace;
            crate::general::ChangeBatch::lazy(move || {
                let Some(inner) = weak.upgrade() else {
                    return Vec::new();
                };
                let state = inner.lock();
                observed_changes(&state, &trace, &old_unit_scopes, &old_retained)
            })
        };
        let parent = frame::current();
        let (depth, cancel) = match &parent {
            Some(parent) => (parent.depth(), parent.cancel().clone()),
            None => (0, CancelToken::default()),
        };
        if depth + 1 > self.config.max_depth {
            return self.fail(
                node,
                ComputeError::DepthExceeded {
                    limit: self.config.max_depth,
                },
                revision,
            );
        }
        // Frames are thread-local execution state; see the note in `eval`.
        #[allow(clippy::arc_with_non_send_sync)]
        let frame = Arc::new(Frame::computing(
            self.arc(),
            node,
            scope,
            depth + 1,
            cancel.clone(),
        ));
        let guard = FrameGuard::push(Arc::clone(&frame));
        let mut cx = crate::general::GeneralCx::new(self.arc(), revision, scope);
        let output_views = general.output_views();
        if !output_views.is_empty() {
            cx.restrict_output_views(output_views);
        }
        let use_update = general.policy() == crate::general::DependencyUpdates::Incremental
            && old_state.is_some();
        let result = catch_engine(|| match (&old_state, use_update) {
            (Some(state), true) => general.update(&mut cx, state, &changes),
            _ => general.initialize(&mut cx),
        });
        let pending_claims = cx.take_pending_claims();
        if !pending_claims.is_empty() {
            let scopes: Vec<WriteScope> = pending_claims.iter().map(WriteScope::entity).collect();
            if let Err(error) = frame
                .runtime()
                .claim_writes(&scopes, "general output", None)
            {
                cx.set_definition_error(format!("{error}"));
            }
        }
        let (unit_scopes, retained_scopes) = cx.take_scopes();
        let definition_error = cx.take_definition_error();
        let content_updates = cx.take_content_updates();
        let (mut retractions, restate_all) = cx.take_definition_mode();
        let definitions: Vec<DefinitionDraft> = frame.take_staged();
        let requirements = frame.take_requirements();
        let frame_retractions = frame.take_retractions();
        retractions.extend(frame_retractions);
        let deps = frame.take_deps();
        drop(guard);
        // A component that retains its state outside the node record stages it
        // for this attempt; the commit tail below promotes it exactly on success
        // and drops it on every other exit, so a cancelled or retried attempt
        // leaves the previous state valid.
        if frame_cancelled(&parent) {
            crate::general::discard_local_state(node);
            return self.fail(node, ComputeError::Cancelled, revision);
        }
        if let Some(reason) = definition_error {
            crate::general::discard_local_state(node);
            return self.fail(
                node,
                ComputeError::DefinitionDenied {
                    unit: String::from("general component output"),
                    reason,
                },
                revision,
            );
        }
        match result {
            Ok(Ok((state, output))) => {
                let eq = |left: &Value, right: &Value| general.output_eq(left, right);
                let committed = self.commit(
                    node,
                    output,
                    deps,
                    definitions,
                    requirements,
                    content_updates,
                    (retractions, restate_all),
                    false,
                    false,
                    Some(state),
                    unit_scopes,
                    retained_scopes,
                    &eq,
                    revision,
                    owner,
                    generation,
                    &cancel,
                );
                let published = match committed {
                    Ok(CommitOutcome::Published(committed)) => Some(committed),
                    Ok(CommitOutcome::SelfInvalidated) => None,
                    Err(error) => {
                        crate::general::discard_local_state(node);
                        return Err(error);
                    }
                };
                // The group published either way, so the successor this run
                // staged belongs to that publication. Only an attempt that
                // published nothing is discarded; dropping a published run's
                // state leaves the retry reading the trace of the output it
                // published against the state before it.
                crate::general::commit_local_state(node);
                let Some(committed) = published else {
                    return Err(ComputeError::Retry);
                };
                #[cfg(feature = "observe")]
                let changed_at = self.changed_at(node);
                let version = self.value_version_at(node);
                let routing = self.routing_at(node);
                let validity = self.validity_at(node);
                frame::record_dep(Dep {
                    node,
                    version,
                    routing,
                    validity,
                });
                #[cfg(feature = "observe")]
                self.note_commit(node, changed_at, revision, started);
                #[cfg(not(feature = "observe"))]
                let _ = version;
                Ok(committed)
            }
            Ok(Err(error)) | Err(error) => {
                crate::general::discard_local_state(node);
                if error.is_suspended() || error.is_retry() {
                    return Err(error);
                }
                self.fail(node, error, revision)
            }
        }
    }

    /// Returns a node's routing generation.
    #[must_use]
    pub fn routing_at(&self, node: NodeId) -> Version {
        self.lock().node(node).routing_at
    }

    /// Returns the stamp at which the node was last certified.
    #[must_use]
    pub fn validity_at(&self, node: NodeId) -> u64 {
        self.lock().node(node).verified_validity
    }

    /// Returns the monotonic version of the node's value.
    #[must_use]
    pub fn value_version_at(&self, node: NodeId) -> u64 {
        self.lock().node(node).value_version
    }

    /// Returns the provider node that currently owns `unit`, if any.
    #[must_use]
    pub fn provider_of(&self, unit: &UnitKey) -> Option<NodeId> {
        let state = self.lock();
        state
            .regions
            .get(&unit.view)
            .and_then(|region| region.owners.get(&owner_key(unit)))
            .map(|provider| provider.node)
    }

    /// Captures the current provider recipe for one View region.
    #[must_use]
    pub fn routing_recipe(&self, view: ViewId) -> Option<crate::runtime::RoutingRecipe> {
        let state = self.lock();
        let region = state.regions.get(&view)?;
        let mut providers = Vec::new();
        for (key, _) in region.units.iter() {
            let Some(unit) = key.as_any().downcast_ref::<UnitKey>() else {
                continue;
            };
            if let Some(provider) = region.owners.get(&owner_key(unit)) {
                providers.push((unit.clone(), provider.node));
            }
        }
        let planned = state
            .route_plans
            .iter()
            .filter(|(unit, _)| unit.view == view)
            .map(|(unit, component)| (unit.clone(), *component))
            .collect();
        Some(crate::runtime::RoutingRecipe {
            stamp: crate::runtime::RoutingStamp {
                view,
                version: region.routing_version,
            },
            providers,
            planned,
        })
    }

    /// Returns a unit's routing generation.
    #[must_use]
    pub fn routing_generation(&self, unit: &UnitKey) -> Version {
        let state = self.lock();
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
            .map_or(0, |node| state.node(node).routing_at)
    }

    /// Returns the revision at which a node's value last changed.
    #[must_use]
    pub fn changed_at(&self, node: NodeId) -> Version {
        self.lock().node(node).changed_at
    }

    /// Validates a trace in order.
    ///
    /// Returns `true` only when every observation is certified for `revision`
    /// and carries the version recorded at execution time.
    fn validate_trace(&self, trace: &[Dep], revision: Revision) -> bool {
        let mut state = self.lock();
        state
            .stats
            .validation_attempts
            .fetch_add(1, Ordering::Relaxed);
        for dep in trace {
            let (verified, clean, changed, routing) = {
                let record = state.node(dep.node);
                (
                    record.verified_at == revision,
                    record.suspect_at <= record.verified_at
                        && record.has_valid_trace()
                        && record.value.is_some(),
                    record.value_version,
                    record.routing_at,
                )
            };
            if !clean {
                state
                    .stats
                    .validation_failures
                    .fetch_add(1, Ordering::Relaxed);
                return false;
            }
            if !verified {
                state.node_mut(dep.node).certify(revision);
            }
            if changed != dep.version || routing != dep.routing {
                state
                    .stats
                    .validation_failures
                    .fetch_add(1, Ordering::Relaxed);
                return false;
            }
        }
        state.stats.validations.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Whether this attempt still owns the node it is publishing.
    fn claim_is_current(state: &State, node: NodeId, owner: TaskId, generation: u64) -> bool {
        matches!(
            state.node(node).state,
            NodeState::Running {
                owner: current_owner,
                generation: current_generation,
            } if current_owner == owner && current_generation == generation
        )
    }

    /// Whether every observation the attempt made is still current.
    fn trace_is_current(state: &State, node: NodeId, deps: &[Dep]) -> bool {
        state.next_validity == state.node(node).attempt_validity
            || deps.iter().all(|dep| {
                let record = state.node(dep.node);
                record.value.is_some()
                    && record.has_valid_trace()
                    && record.value_version == dep.version
                    && record.routing_at == dep.routing
            })
    }

    /// Publishes one attempt's value, trace, definitions, and content atomically.
    ///
    /// The commit runs in three phases. The first revalidates the claim with the
    /// lock held and reads what host code needs; the second releases the lock to
    /// run the host's route selector and the value cutoff `eq` — user code never
    /// runs under the metadata lock; the third revalidates and publishes the
    /// whole write group in one critical section.
    #[allow(clippy::too_many_arguments)]
    fn commit(
        &self,
        node: NodeId,
        value: Value,
        deps: Vec<Dep>,
        definitions: Vec<DefinitionDraft>,
        requirements: Vec<Requirement>,
        content_updates: Vec<(irrigation_core::ids::ViewId, Arc<dyn Any + Send + Sync>)>,
        definition_mode: (Vec<UnitKey>, bool),
        enforce_views: bool,
        deferrable: bool,
        retained_state: Option<crate::general::ErasedState>,
        unit_scopes: FxHashMap<UnitKey, u64>,
        retained_scopes: FxHashSet<u64>,
        eq: &dyn Fn(&Value, &Value) -> bool,
        revision: Revision,
        owner: TaskId,
        generation: u64,
        cancel: &CancelToken,
    ) -> Result<CommitOutcome> {
        let route_selector = self.route_selector.lock().clone();
        // Phase one: revalidate the claim and read the state host code needs.
        let (previous_value, collisions) = {
            let state = self.lock();
            if !Self::claim_is_current(&state, node, owner, generation) {
                return Err(ComputeError::internal(
                    "commit from a claim that is no longer owned",
                ));
            }
            if cancel.is_cancelled() {
                drop(state);
                return self.fail(node, ComputeError::Cancelled, revision);
            }
            if state.revision != revision {
                drop(state);
                return self.fail(
                    node,
                    ComputeError::internal(
                        "input revision changed while a computation was running",
                    ),
                    revision,
                );
            }
            if !Self::trace_is_current(&state, node, &deps) {
                drop(state);
                return Err(ComputeError::Retry);
            }
            let previous_value = state.node(node).value.clone();
            let collisions = if route_selector.is_some() {
                route_collisions(&state, node, &definitions)
            } else {
                Vec::new()
            };
            (previous_value, collisions)
        };
        // Phase two: host code, without the metadata lock.
        let decisions = decide_routes(route_selector.as_ref(), &collisions);
        let changed = previous_value.as_ref().is_none_or(|old| !eq(old, &value));

        // Phase three: revalidate and publish in one critical section.
        let mut state = self.lock();
        // A commit from a claim that is no longer ours must not publish. This is
        // the generation check the plan requires; it also makes the retirement
        // race harmless if a guard is ever missed.
        if !Self::claim_is_current(&state, node, owner, generation) {
            return Err(ComputeError::internal(
                "commit from a claim that is no longer owned",
            ));
        }
        if cancel.is_cancelled() {
            drop(state);
            return self.fail(node, ComputeError::Cancelled, revision);
        }
        if state.revision != revision {
            drop(state);
            return self.fail(
                node,
                ComputeError::internal("input revision changed while a computation was running"),
                revision,
            );
        }
        // The lock was released in phase two, so every observation is checked
        // again before anything is published. A rejected attempt must not
        // install a definition or replace View content on the way out.
        if !Self::trace_is_current(&state, node, &deps) {
            drop(state);
            return Err(ComputeError::Retry);
        }
        // Everything the install writes or retracts takes a validity stamp
        // above this mark, so a stale observation carrying an older stamp was
        // moved by someone else while this attempt was running.
        let install_mark = state.next_validity;
        // A replacement that stages the content the View already holds installs
        // nothing, so it is not a write: claiming its ports would fail a body
        // over a group that publishes no content at all.
        let content_views: Vec<ViewId> = content_updates
            .iter()
            .filter(|(view, content)| {
                state.views.get(view).is_some_and(|descriptor| {
                    let committed = state
                        .regions
                        .get(view)
                        .is_some_and(|region| (descriptor.content.eq)(&region.content, content));
                    !committed
                })
            })
            .map(|(view, _)| *view)
            .collect();
        let group = WriteGroup {
            provider: node,
            drafts: &definitions,
            retractions: &definition_mode.0,
            restate_all: definition_mode.1,
            content_views: &content_views,
        };
        // A computation that tracked-read a port its own write group installs
        // is self-dependent: the install invalidates the read the write was
        // computed from, so the demand either re-executes against the value
        // this commit just published or exhausts the retry budget. The plan
        // makes that a program error rather than a retry, and the read may sit
        // in a different site node of the same attempt, so it is decided here
        // over the attempt's whole site trace.
        if let Some(port) = self_dependent_port(&state, node, &deps, group) {
            let error = ComputeError::PreconditionUnmet {
                condition: String::from("writer reads a port it writes"),
                ports: vec![format!("{port:?}")],
                reason: irrigation_core::error::PreconditionReason::SelfDependent,
            };
            drop(state);
            return self.fail(node, error, revision);
        }
        // Structural requirements are checked at commit. A false requirement
        // either defers the whole write group to the end of the demand or fails
        // the attempt; it never publishes a definition that was not true. A
        // group that cannot be deferred fails here: its retained state and
        // output commit with its definitions or not at all.
        let unmet = requirements
            .iter()
            .find(|requirement| !requirement_holds(&state, requirement, group));
        let deferred = if let Some(requirement) = unmet {
            // A deferred group is drained by the demand task that owns it. An
            // evaluation with no live task — a parallel region's scoped thread —
            // has no such boundary: no close and no drain will ever match its
            // identity, so the group and its claims would stay pending for ever.
            if !deferrable
                || definitions.is_empty()
                || !content_updates.is_empty()
                || !state.tasks.contains_key(&owner)
            {
                let error = precondition_error(requirement);
                drop(state);
                return self.fail(node, error, revision);
            }
            true
        } else {
            false
        };
        // The arbitration event leaves this section with the batch instead of
        // firing inside it: an observer is host code and must not run while the
        // metadata guard is held.
        let (definitions, definition_mode, _requirements, deferred_port) = if deferred {
            let port = definitions
                .first()
                .map_or_else(|| format!("{node:?}"), |draft| format!("{:?}", draft.unit));
            let pending = crate::state::PendingWrite {
                owner: node,
                task: owner,
                revision,
                generation,
                drafts: definitions,
                retractions: definition_mode.0,
                restate_all: definition_mode.1,
                enforce_views,
                requirements,
                trace: deps.clone(),
                claims: state.node(node).claims.clone(),
            };
            state.pending_writes.push(pending);
            state.stats.deferred_writes.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "observe")]
            let port = Some(port);
            #[cfg(not(feature = "observe"))]
            let port: Option<String> = {
                let _ = port;
                None
            };
            (Vec::new(), (Vec::new(), false), Vec::new(), port)
        } else {
            (definitions, definition_mode, requirements, None)
        };
        let mut changed_nodes = Vec::new();
        if !deferred {
            // The whole write group is validated before any of it is applied,
            // so a rejected install cannot leave View content behind.
            if let Err(error) = Self::validate_definitions(
                &state,
                node,
                &definitions,
                &definition_mode.0,
                enforce_views,
            ) {
                drop(state);
                return self.fail(node, error, revision);
            }
        }
        // A content replacement joins the same critical section: it is applied
        // only after the definitions were proved installable, and the
        // definitions are installed against the replacement content. It is the
        // same whole-region transition a host `set_content` stages, so both go
        // through the one publisher: content, its version, and the structure
        // derived from it move together, and a replacement of a View the group
        // installs no unit into is still a change its observers see.
        // The units this group drafts are installed immediately after the
        // replacement, so the replacement owes them no generation of their own.
        // The set is the exact units installed: an entity's presence cell does
        // not stand in for a payload the group never staged.
        let installed: FxHashSet<UnitKey> =
            definitions.iter().map(|draft| draft.unit.clone()).collect();
        // The drafts are installed after this replacement, so the content it
        // publishes cannot project them yet: the reconcile that checks a
        // transaction's own writes against its content belongs to the edit
        // path, where the writes and the content commit together.
        for (view, content) in content_updates {
            let eq = state
                .views
                .get(&view)
                .copied()
                .map(|descriptor| descriptor.content.eq);
            let applied = eq.is_some_and(|eq| {
                Self::publish_region_content(
                    &mut state,
                    view,
                    content,
                    eq,
                    revision,
                    CellDelta {
                        installed: &installed,
                        stated: false,
                    },
                    &mut changed_nodes,
                )
            });
            debug_assert!(
                applied,
                "a staged content replacement targets a registered View"
            );
        }
        state.node_mut(node).content_writes = content_views;
        if !deferred {
            Self::install_definitions(
                &mut state,
                node,
                definitions,
                definition_mode,
                revision,
                enforce_views,
                &decisions,
                &mut changed_nodes,
            );
        }

        let old_trace = state.node(node).trace.clone();
        let value_version = changed.then(|| state.alloc_value_version());
        for dep in &old_trace {
            state.remove_dependent(dep.node, node);
        }
        let new_dependencies: FxHashSet<NodeId> = deps.iter().map(|dep| dep.node).collect();
        for dep in &deps {
            state.add_dependent(dep.node, node);
        }
        {
            let record = state.node_mut(node);
            record.value = Some(value.clone());
            record.trace = deps;
            record.state = NodeState::Ready;
            record.certify(revision);
            record.executed_at = revision;
            record.validated_at = revision;
            record.suspect_at = 0;
            record.failure = None;
            if let Some(retained) = retained_state {
                record.general_state = Some(retained);
            }
            record.general_unit_scopes = unit_scopes;
            record.general_retained_scopes = retained_scopes;
            if changed {
                record.changed_at = revision;
                record.value_version = value_version.expect("changed");
            }
        }
        if !deferred {
            state.release_claims(node);
        }
        // The claim published; the task no longer owns it, so a later exit must
        // not release a claim that is now another task's to take.
        forget_claim(&mut state, owner, node);
        state.stats.executions.fetch_add(1, Ordering::Relaxed);
        if changed {
            let dependents = state.node(node).dependents.clone();
            changed_nodes.extend(dependents);
        }
        // Retire observations that the new trace no longer contains. A retired
        // branch must release its definitions and its value, so it re-executes
        // and restages if it becomes reachable again.
        let retired: Vec<NodeId> = old_trace
            .iter()
            .filter(|dep| !new_dependencies.contains(&dep.node))
            .map(|dep| dep.node)
            .collect();
        for retired in retired {
            self.retire_unreachable(&mut state, retired, revision, &mut changed_nodes);
        }
        let mut invalidated = 0;
        if !changed_nodes.is_empty() {
            let started = std::time::Instant::now();
            invalidated = invalidate(&mut state, changed_nodes.iter().copied(), revision);
            state.stats.invalidation_ns.fetch_add(
                started.elapsed().as_nanos() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        // A commit that installs a cell its own body observed — `let base =
        // list.to_vec(); list.insert_at(base.len(), ..)` — invalidates the
        // trace it is publishing. That read was of the pre-write state, which
        // is the from-scratch answer for this attempt; re-running the body
        // would apply the write to the state this commit just published.
        // Re-certify instead, and retry only for an observation that moved
        // before this commit began installing.
        let external = Self::trace_moved_before(&state, node, install_mark);
        if !external {
            let record = state.node_mut(node);
            record.certify(revision);
            record.suspect_at = 0;
        }
        let seeds = changed_nodes;
        let mut waiters = Self::wake_waiters(&mut state, node);
        drop(state);
        #[cfg(feature = "observe")]
        if let Some(port) = deferred_port {
            self.emit(crate::observe::RuntimeEvent::Arbitration {
                kind: "deferred_write",
                node,
                port,
            });
        }
        #[cfg(not(feature = "observe"))]
        let _ = deferred_port;
        #[cfg(feature = "observe")]
        self.emit_invalidated(revision, &seeds, invalidated);
        #[cfg(not(feature = "observe"))]
        let _ = (invalidated, seeds);
        self.wake.notify_all();
        self.enqueue(std::mem::take(&mut waiters.tasks));
        waiters.wake();
        self.emit_woken(std::mem::take(&mut waiters.woken));
        if external {
            return Ok(CommitOutcome::SelfInvalidated);
        }
        Ok(CommitOutcome::Published(value))
    }

    /// Whether the attempt observed a unit that moved before this commit began
    /// installing.
    ///
    /// The trace was certified under this lock before the install, so an
    /// observation that no longer matches its recorded version was moved either
    /// by an earlier commit — the attempt must re-run against that state — or by
    /// this commit's own install. Every cell the install writes or retracts, and
    /// every dependent the install invalidates, takes a validity stamp above
    /// `install_mark`, so an observation still carrying an older stamp is not
    /// this attempt's to re-certify.
    fn trace_moved_before(state: &State, node: NodeId, install_mark: u64) -> bool {
        state.node(node).trace.iter().any(|dep| {
            let record = state.node(dep.node);
            record.suspect_validity <= install_mark
                && (record.value_version != dep.version
                    || record.routing_at != dep.routing
                    || !record.has_valid_trace())
        })
    }

    /// Releases everything a node that is no longer reachable owns.
    ///
    /// Shared nodes (still referenced by another trace) are left alone. The
    /// node's value is cleared so that if it becomes reachable again it
    /// re-executes and restages its definitions.
    fn retire_unreachable(
        &self,
        state: &mut State,
        node: NodeId,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        if node == NodeId::NONE || !state.node(node).dependents.is_empty() {
            return;
        }
        if matches!(
            state.node(node).kind,
            NodeKind::Unit | NodeKind::RegionContent
        ) {
            // Unit and content nodes never register a write claim.
            return;
        }
        // A node owned by a live task, or with waiters, is not dead: clearing
        // it would hand a second task the same claim. Leave it, claim included;
        // a later commit or reclamation retires it once the claim is released.
        if state.node(node).is_running() || !state.node(node).waiters.is_empty() {
            return;
        }
        state.release_claims(node);
        let definitions = state.node(node).definitions.clone();
        for definition in definitions {
            let view = definition.unit.view;
            Self::with_taken_region(state, view, |state, region| {
                Self::retract_unit(
                    state,
                    region,
                    node,
                    &definition.unit,
                    revision,
                    changed_nodes,
                    false,
                );
            });
        }
        let trace = state.node(node).trace.clone();
        for dep in &trace {
            state.remove_dependent(dep.node, node);
        }
        {
            let record = state.node_mut(node);
            record.value = None;
            record.trace.clear();
            record.definitions.clear();
            record.state = NodeState::Vacant;
            record.verified_at = 0;
            record.verified_validity = 0;
            record.suspect_validity = 0;
            record.suspect_at = 0;
            record.changed_at = 0;
            record.value_version = 0;
            record.failure = None;
        }
        for dep in trace {
            self.retire_unreachable(state, dep.node, revision, changed_nodes);
        }
    }

    fn fail<T>(&self, node: NodeId, error: ComputeError, revision: Revision) -> Result<T> {
        let mut state = self.lock();
        state
            .stats
            .execution_failures
            .fetch_add(1, Ordering::Relaxed);
        state.release_claims(node);
        let claim_owner = match state.node(node).state {
            NodeState::Running { owner, .. } => Some(owner),
            _ => None,
        };
        {
            let record = state.node_mut(node);
            if error.is_cancelled() {
                record.state = NodeState::Vacant;
                record.failure = None;
            } else {
                record.state = NodeState::Failed;
                record.failure = Some(error.clone());
                record.failed_at = revision;
            }
        }
        if let Some(owner) = claim_owner {
            forget_claim(&mut state, owner, node);
        }
        let mut waiters = Self::wake_waiters(&mut state, node);
        drop(state);
        #[cfg(feature = "observe")]
        self.emit(crate::observe::RuntimeEvent::NodeFailed {
            node,
            kind: crate::observe::error_kind(&error),
        });
        self.wake.notify_all();
        self.enqueue(std::mem::take(&mut waiters.tasks));
        waiters.wake();
        self.emit_woken(std::mem::take(&mut waiters.woken));
        Err(error)
    }

    /// Runs `body` with one View's region temporarily out of the map.
    ///
    /// The region has to leave the map because the mutation needs the rest of
    /// the state as well. A View's content vtable is the one host callback that
    /// still runs under the metadata lock — it is confined to the descriptor's
    /// pure content operations, and every function that reaches it goes through
    /// here — so a panic in it must not leave the View without its data: the
    /// region goes back before the panic continues unwinding. Route selection
    /// and the value cutoff `eq` are resolved before this point, without the
    /// lock.
    fn with_taken_region<R>(
        state: &mut State,
        view: ViewId,
        body: impl FnOnce(&mut State, &mut Region) -> R,
    ) -> Option<R> {
        let mut region = state.regions.remove(&view)?;
        let outcome = catch_engine(std::panic::AssertUnwindSafe(|| body(state, &mut region)));
        let replaced = state.regions.insert(view, region);
        debug_assert!(
            replaced.is_none(),
            "a View region was replaced while it was taken"
        );
        Some(outcome.unwrap_or_else(|error| panic_engine(error)))
    }

    /// Checks one unit of a write group against the group's authority.
    ///
    /// Installation and retraction share it: a unit is writable only from a
    /// registered View, from a View the provider declared, and from the provider
    /// component its route plan names.
    fn validate_unit(
        state: &State,
        provider: NodeId,
        unit: &UnitKey,
        enforce_views: bool,
    ) -> Result<()> {
        if !state.regions.contains_key(&unit.view) {
            return Err(ComputeError::DefinitionDenied {
                unit: format!("{unit:?}"),
                reason: String::from("view is not registered"),
            });
        }
        if enforce_views {
            let declared = state
                .declared_views
                .get(&state.node(provider).scope)
                .is_some_and(|views| views.contains(&unit.view));
            if !declared {
                return Err(ComputeError::DefinitionDenied {
                    unit: format!("{unit:?}"),
                    reason: String::from(
                        "the provider did not declare this View (call `ensure_view` first)",
                    ),
                });
            }
        }
        if let Some(planned) = state.route_plans.get(unit) {
            let provider_component = component_of(state, state.node(provider).scope);
            if provider_component != Some(*planned) {
                return Err(ComputeError::DefinitionDenied {
                    unit: format!("{unit:?}"),
                    reason: String::from("the unit is planned for a different provider component"),
                });
            }
        }
        Ok(())
    }

    /// Checks one unit a write group retracts against the group's authority.
    ///
    /// Retraction mirrors installation: the units a group removes answer to the
    /// same authority as the units it installs, and another live provider's unit
    /// is a conflict rather than a silent removal.
    fn validate_retraction(state: &State, provider: NodeId, unit: &UnitKey) -> Result<()> {
        let owner = state
            .regions
            .get(&unit.view)
            .and_then(|region| region.owners.get(&owner_key(unit)));
        if let Some(owner) = owner
            && !retraction_may_take_over(state, provider, owner, unit)
        {
            return Err(ComputeError::DefinitionDenied {
                unit: format!("{unit:?}"),
                reason: String::from("the unit is owned by another live provider"),
            });
        }
        Ok(())
    }

    /// Checks every draft and explicit retraction of one write group before any
    /// of it is installed.
    ///
    /// The install itself calls this again; the commit calls it first so View
    /// content only joins a write group that can no longer be rejected.
    fn validate_definitions(
        state: &State,
        provider: NodeId,
        drafts: &[DefinitionDraft],
        retractions: &[UnitKey],
        enforce_views: bool,
    ) -> Result<()> {
        for draft in drafts {
            Self::validate_unit(state, provider, &draft.unit, enforce_views)?;
            if draft_states_absence(draft) {
                // A `Membership = false` draft is the presence spelling of the
                // entity's retraction, so it answers to the retraction's
                // authority: another live provider's entity is a conflict, not a
                // silent removal.
                Self::validate_retraction(state, provider, &payload_key(&draft.unit))?;
            }
        }
        for unit in retractions {
            Self::validate_unit(state, provider, unit, enforce_views)?;
            Self::validate_retraction(state, provider, unit)?;
        }
        let restaged: FxHashSet<UnitKey> = drafts.iter().map(|draft| draft.unit.clone()).collect();
        if restaged.len() != drafts.len() {
            let mut seen: std::collections::HashMap<
                UnitKey,
                Option<&'static std::panic::Location<'static>>,
            > = std::collections::HashMap::new();
            for draft in drafts {
                if let Some(first_location) = seen.get(&draft.unit) {
                    return Err(ComputeError::DuplicateDefinition {
                        unit: format!("{:?}", draft.unit),
                        first: describe_provider(draft.label, *first_location),
                        second: describe_provider(draft.label, draft.location),
                    });
                }
                seen.insert(draft.unit.clone(), draft.location);
            }
        }
        Ok(())
    }

    /// Installs one write group into its View regions.
    ///
    /// The caller has already run [`Self::validate_definitions`] on the same
    /// group; every region is known to exist and every draft is known to be the
    /// only one for its unit.
    #[allow(clippy::too_many_arguments)]
    fn install_definitions(
        state: &mut State,
        provider: NodeId,
        drafts: Vec<DefinitionDraft>,
        definition_mode: (Vec<UnitKey>, bool),
        revision: Revision,
        enforce_views: bool,
        decisions: &[RouteDecision],
        changed_nodes: &mut Vec<NodeId>,
    ) {
        debug_assert!(
            Self::validate_definitions(state, provider, &drafts, &definition_mode.0, enforce_views)
                .is_ok(),
            "a write group is validated before it is installed"
        );
        let (retractions, restate_all) = definition_mode;
        let retracted: FxHashSet<UnitKey> = retractions.into_iter().collect();
        // Presence is decided once per write group, from its final drafts: a
        // `Membership = false` draft states that the entity is absent, so the
        // group retracts it instead of installing a presence cell. The decision
        // is normalised to the owner key every retraction names, which releases
        // the provider's records for both cells and takes the drafts out of the
        // install below.
        let withdrawn = withdrawn_entities(&drafts);
        // A retraction may name either of an entity's cells, and the record it
        // releases is the ownership pair. Comparing full keys would let a
        // presence-only retraction leave the payload's definition installed.
        let released: FxHashSet<UnitKey> = retracted
            .iter()
            .map(owner_key)
            .chain(withdrawn.iter().cloned())
            .collect();
        let old: Vec<DefinitionRecord> = state.node(provider).definitions.clone();
        let restaged: FxHashSet<UnitKey> = drafts
            .iter()
            .filter(|draft| !draft_withdrawn(draft, &withdrawn))
            .map(|draft| draft.unit.clone())
            .collect();

        // A field-level `Value` draft publishes its field as one value, not as a
        // delta against entities: the entity cells this provider staged for that
        // field and did not restate belong to the value that was replaced. A
        // delta publisher that stops listing an id would otherwise leave its
        // membership and payload cells serving the dropped member forever.
        let replaced: FxHashSet<(ViewId, FieldId)> = drafts
            .iter()
            .filter(|draft| draft.unit.entity.is_none() && draft.unit.kind == UnitKind::Value)
            .map(|draft| (draft.unit.view, draft.unit.field))
            .collect();
        let superseded: FxHashSet<UnitKey> = old
            .iter()
            .filter(|record| {
                record.unit.entity.is_some()
                    && replaced.contains(&(record.unit.view, record.unit.field))
                    && !restaged.contains(&record.unit)
            })
            .map(|record| record.unit.clone())
            .collect();

        // Retractions of one View batch their content publish, and the fields
        // they move are refreshed once by the install pass that follows.
        let mut stale_entities: FxHashMap<ViewId, Vec<UnitKey>> = FxHashMap::default();
        let mut retracted_dirty: FxHashMap<ViewId, Vec<FieldId>> = FxHashMap::default();
        // Retractions first: a unit this provider no longer produces must
        // disappear atomically with the new value. A unit is removed only when
        // it is retracted explicitly, superseded by a whole-field replacement,
        // or the run restates everything; absence alone keeps the committed
        // value.
        for record in &old {
            if restaged.contains(&record.unit) {
                continue;
            }
            if withdrawn.contains(&owner_key(&record.unit)) {
                // The group's own presence decision clears this pair, after the
                // drafts it does install are rewritten into the content.
                continue;
            }
            if !restate_all
                && !released.contains(&owner_key(&record.unit))
                && !superseded.contains(&record.unit)
            {
                continue;
            }
            let view = record.unit.view;
            if record.unit.entity.is_some() {
                stale_entities
                    .entry(view)
                    .or_default()
                    .push(record.unit.clone());
                continue;
            }
            let outcome = Self::with_taken_region(state, view, |state, region| {
                Self::retract_unit(
                    state,
                    region,
                    provider,
                    &record.unit,
                    revision,
                    changed_nodes,
                    false,
                );
            });
            debug_assert!(outcome.is_some(), "a validated View has its region");
        }

        // Explicit retractions that are not part of this provider's old record
        // set: a branch site that was not taken last revision can retract the
        // unit it used to own without waiting for the old branch node to be
        // reclaimed.
        let mut explicit_entities: FxHashMap<ViewId, Vec<UnitKey>> = FxHashMap::default();
        for unit in &retracted {
            if restaged.contains(unit) || superseded.contains(unit) {
                continue;
            }
            let view = unit.view;
            if unit.entity.is_some() {
                explicit_entities
                    .entry(view)
                    .or_default()
                    .push(unit.clone());
                continue;
            }
            let outcome = Self::with_taken_region(state, view, |state, region| {
                Self::retract_unit(state, region, provider, unit, revision, changed_nodes, true);
            });
            debug_assert!(outcome.is_some(), "a View that owns a unit has its region");
        }

        // The two batches differ only in authority: a stale record is only
        // released, an explicit retraction may take a sibling site's unit over.
        for (batches, force) in [(stale_entities, false), (explicit_entities, true)] {
            for (view, units) in batches {
                let outcome = Self::with_taken_region(state, view, |state, region| {
                    let mut dirty = retracted_dirty.remove(&view).unwrap_or_default();
                    Self::retract_entities(
                        state,
                        region,
                        provider,
                        &units,
                        force,
                        revision,
                        changed_nodes,
                        &mut dirty,
                    );
                    retracted_dirty.insert(view, dirty);
                });
                debug_assert!(outcome.is_some(), "a validated View has its region");
            }
        }
        let mut by_view: FxHashMap<ViewId, Vec<DefinitionDraft>> = FxHashMap::default();
        for draft in drafts {
            by_view.entry(draft.unit.view).or_default().push(draft);
        }
        let mut installed: Vec<DefinitionRecord> = old
            .into_iter()
            .filter(|record| {
                !restaged.contains(&record.unit)
                    && !released.contains(&owner_key(&record.unit))
                    && !superseded.contains(&record.unit)
                    && !restate_all
            })
            .collect();
        for (view, drafts) in by_view {
            let applied = Self::with_taken_region(state, view, |state, region| {
                Self::apply_definitions(
                    state,
                    region,
                    provider,
                    &drafts,
                    &withdrawn,
                    revision,
                    decisions,
                    changed_nodes,
                    &mut installed,
                    retracted_dirty.remove(&view).unwrap_or_default(),
                );
            });
            debug_assert!(applied.is_some(), "a validated View has its region");
        }
        // A View whose retractions moved a field but which installs nothing this
        // run never reaches `apply_definitions`, so its projection is refreshed
        // here — still once per field.
        for (view, fields) in retracted_dirty {
            if fields.is_empty() {
                continue;
            }
            let outcome = Self::with_taken_region(state, view, |state, region| {
                for field in fields {
                    Self::refresh_unowned_structure(state, region, field, revision, changed_nodes);
                }
            });
            debug_assert!(outcome.is_some(), "a validated View has its region");
        }
        // Retained definitions are still this provider's output for the
        // revision: certify them so readers and reuse checks see them as final.
        // Only restaged units are written this run.
        for record in &installed {
            if restaged.contains(&record.unit) {
                continue;
            }
            if let Some(node) = current_unit_node(state, &record.unit) {
                let node_record = state.node_mut(node);
                if node_record.value.is_some() && node_record.verified_at < revision {
                    node_record.certify(revision);
                }
            }
        }
        state.node_mut(provider).definitions = installed;
    }

    /// Moves one field's keyed/whole protocol and its certificate together.
    ///
    /// The bit and the cell must land in the same revision, in the same commit:
    /// a reader that validates between them would take the branch of the old
    /// mode against the new content. The certificate is written only when the
    /// bit actually moves, so a keyed publication that restages `Value` then
    /// `Structure` on every run does not churn the readers it wakes.
    fn set_field_protocol(
        state: &mut State,
        view: ViewId,
        field: FieldId,
        keyed: bool,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let moved = state.keyed_fields.contains(&(view, field)) != keyed;
        if keyed {
            state.keyed_fields.insert((view, field));
        } else {
            state.keyed_fields.remove(&(view, field));
        }
        if !moved {
            return;
        }
        // A field is keyed only after this ran with `keyed`, so a missing
        // certificate means the field has never been keyed: the new mode is a
        // transition no reader can have observed yet.
        let key = UnitKey::field(view, field, UnitKind::Membership);
        let node = match state.units.get(&key).copied() {
            Some(node) => node,
            None => {
                let node = state.alloc(NodeRecord::unit("field protocol", key.clone()));
                state.units.insert(Arc::new(key), node);
                node
            }
        };
        let value_version = state.alloc_value_version();
        let record = state.node_mut(node);
        record.install_value(Some(value_of(keyed)), value_version, revision);
        record.routing_at = revision;
        changed_nodes.push(node);
    }

    #[allow(clippy::too_many_arguments)]
    /// Recomputes one field's structure unit from the committed content.
    fn refresh_structure(
        state: &mut State,
        region: &mut Region,
        field: FieldId,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        state
            .stats
            .structure_refreshes
            .fetch_add(1, Ordering::Relaxed);
        let descriptor = region.descriptor;
        let Some(structure) = (descriptor.content.structure)(&region.content, field) else {
            return;
        };
        let key = UnitKey::field(descriptor.id, field, UnitKind::Structure);
        let node = match region.units.get(&key) {
            Some(slot) => slot.node,
            None => {
                let node = Self::ensure_unit_node(state, key.clone(), None);
                region.units.insert(
                    any_key(key.clone()),
                    UnitSlot {
                        node,
                        present: true,
                    },
                );
                node
            }
        };
        let changed = {
            let record = state.node(node);
            record
                .value
                .as_ref()
                .is_none_or(|old| !(structure.eq)(old, &structure.value))
        };
        let value_version = changed.then(|| state.alloc_value_version());
        let record = state.node_mut(node);
        if changed {
            record.install_value(
                Some(structure.value),
                value_version.expect("changed"),
                revision,
            );
            changed_nodes.push(node);
        } else {
            record.certify(revision);
            record.suspect_at = 0;
            record.state = NodeState::Ready;
            record.failure = None;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_definitions(
        state: &mut State,
        region: &mut Region,
        provider: NodeId,
        drafts: &[DefinitionDraft],
        withdrawn: &FxHashSet<UnitKey>,
        revision: Revision,
        decisions: &[RouteDecision],
        changed_nodes: &mut Vec<NodeId>,
        installed: &mut Vec<DefinitionRecord>,
        retracted_dirty: Vec<FieldId>,
    ) {
        let descriptor = region.descriptor;
        let mut content_drafts: Vec<(UnitKey, Value)> = Vec::new();
        let mut content_changed = false;
        // Fields the group's own retractions moved before the installs ran. They
        // are seeded here so this View's single flush republishes every moved
        // projection once, from the group's final content.
        let mut dirty_fields: Vec<FieldId> = retracted_dirty;
        // Fields the group's presence decisions move. They are flushed under
        // their own rule, because a presence decision answers after the drafts.
        let mut presence_fields: Vec<FieldId> = Vec::new();
        let mut protocols: Vec<(FieldId, bool)> = Vec::new();
        for draft in drafts {
            // The group's presence decision owns this entity's cells: a
            // presence draft is not an install, and a payload staged beside it
            // states a value the decision marks absent.
            if draft_withdrawn(draft, withdrawn) {
                continue;
            }
            // The protocol a field ends the commit with is the one its last
            // declaring draft names; `define_keyed` stages the whole-field
            // value first and the structure after it.
            if let Some(keyed) = draft_protocol(&draft.unit, descriptor.fields) {
                match protocols
                    .iter_mut()
                    .find(|(field, _)| *field == draft.unit.field)
                {
                    Some(entry) => entry.1 = keyed,
                    None => protocols.push((draft.unit.field, keyed)),
                }
            }
            let owner = owner_key(&draft.unit);
            let previous_owner = region.owners.get(&owner).map(|existing| existing.node);
            if let Some(existing) = region.owners.get(&owner) {
                if existing.node != provider {
                    // A route selector is host code, so the choice was made
                    // before the lock was taken; it applies only while the
                    // collision it was made for is still the current one.
                    match route_winner(decisions, &draft.unit, existing.node, provider) {
                        Some(winner) if winner == existing.node => continue,
                        Some(_) => {
                            // The selected provider keeps the unit and the other
                            // claim is rejected or transferred now.
                            let previous = existing.node;
                            state.node_mut(previous).definitions.retain(|definition| {
                                definition.unit != draft.unit || definition.provider != previous
                            });
                            force_recompute(state, &[previous], revision);
                        }
                        None => {
                            // Resolve at the end of the revision: if the earlier
                            // owner is still reachable then both providers are
                            // live and the revision fails; if it retired during
                            // the revision, this was a handover.
                            state.pending_conflicts.push(crate::state::PendingConflict {
                                unit: draft.unit.clone(),
                                first: existing.node,
                                second: provider,
                                second_location: draft.location,
                                first_owner: existing.clone(),
                            });
                        }
                    }
                }
            }
            // Whether this is the cell's first appearance. A definition that
            // only restates the committed content is that appearance, not a
            // generation of its own.
            let fresh_cell = state.units.get(&draft.unit).is_none();
            let value_node = match region.units.get(&draft.unit) {
                Some(slot) => slot.node,
                None => {
                    let node = Self::ensure_unit_node(state, draft.unit.clone(), None);
                    region.units.insert(
                        any_key(draft.unit.clone()),
                        UnitSlot {
                            node,
                            present: false,
                        },
                    );
                    node
                }
            };
            let born_at = fresh_cell
                .then(|| Self::content_answer(state, region, &draft.unit, &draft.value, draft.eq))
                .flatten();
            let changed = born_at.is_none()
                && state
                    .node(value_node)
                    .value
                    .as_ref()
                    .is_none_or(|old| !(draft.eq)(old, &draft.value));
            let write = changed || born_at.is_some();
            let value_version = write.then(|| state.alloc_value_version());
            {
                let record = state.node_mut(value_node);
                if let Some(value_version) = value_version {
                    // A cell born from the content keeps the revision that gave
                    // it the answer, so no subscription reads the definition as
                    // a change. A real change is stamped with this revision.
                    let changed_at = born_at.unwrap_or(revision);
                    record.install_value(Some(Arc::clone(&draft.value)), value_version, changed_at);
                    if changed {
                        changed_nodes.push(value_node);
                    }
                } else {
                    record.certify(revision);
                    record.suspect_at = 0;
                    record.state = NodeState::Ready;
                    record.failure = None;
                }
                if previous_owner != Some(provider) {
                    record.routing_at = revision;
                }
            }
            if draft.unit.entity.is_some() {
                let membership = membership_key(&draft.unit);
                let member_node = match region.units.get(&membership) {
                    Some(slot) => slot.node,
                    None => {
                        let node = Self::ensure_unit_node(state, membership.clone(), None);
                        region.units.insert(
                            any_key(membership.clone()),
                            UnitSlot {
                                node,
                                present: false,
                            },
                        );
                        node
                    }
                };
                // A membership draft states presence itself; every other
                // entity unit installs the entity, so its membership is true. A
                // false presence never reaches here: the group's retraction
                // decided it before any cell installed.
                let present = draft.unit.kind != UnitKind::Membership
                    || draft.value.downcast_ref::<bool>().copied().unwrap_or(true);
                // Presence is one question with two spellings: the cell, or the
                // committed content while the cell holds no answer. A payload
                // draft whose entity the content already holds materializes the
                // presence at the content's revision — the answer is not new,
                // only its cell is, and a subscription seeded from that content
                // must not read the definition as a change.
                let recorded = membership_of(state, member_node);
                let present_in_content = recorded.is_none()
                    && (descriptor.content.contains)(&region.content, &membership);
                if draft.unit.kind != UnitKind::Membership && recorded != Some(true) {
                    let value_version = state.alloc_value_version();
                    let changed_at = if present_in_content {
                        state.node(region.content_node).changed_at
                    } else {
                        revision
                    };
                    let dependents = state.node(member_node).dependents.clone();
                    let record = state.node_mut(member_node);
                    record.install_value(Some(value_of(true)), value_version, changed_at);
                    record.routing_at = revision;
                    if present_in_content {
                        // The revision is the content's, not this one, but the
                        // payload is new to the graph: a reader that observed
                        // the vacant cell — the documented negative read — holds
                        // an absence this presence contradicts, and being
                        // certified for the revision would keep it. Mark those
                        // readers like `republish_cell` does; the cell itself is
                        // not seeded, because a subscription must still read
                        // this materialisation as no change.
                        if !dependents.is_empty() {
                            invalidate(state, dependents, revision);
                        }
                    } else {
                        changed_nodes.push(member_node);
                    }
                }
                if let Some(slot) = region.units.get_mut(&membership) {
                    slot.present = present;
                }
                if changed && !dirty_fields.contains(&draft.unit.field) {
                    dirty_fields.push(draft.unit.field);
                }
            }
            // An equal restatement is already represented by the committed
            // content; applying it again is pure whole-content work. Only a
            // unit whose value actually moved can move the content, and a
            // cell born from the content is already in it.
            if changed && (descriptor.content.moves_content)(&draft.unit) {
                state
                    .stats
                    .content_applies
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                content_changed = true;
                content_drafts.push((draft.unit.clone(), Arc::clone(&draft.value)));
                // A per-entity payload update cannot move keys/order/topology.
                // A whole-field payload or membership/structure unit can.
                if (draft.unit.kind != UnitKind::Value || draft.unit.entity.is_none())
                    && !dirty_fields.contains(&draft.unit.field)
                {
                    dirty_fields.push(draft.unit.field);
                }
            } else if changed {
                state
                    .stats
                    .content_projections
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else {
                state
                    .stats
                    .content_restatements
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            if previous_owner != Some(provider) {
                region.routing_version = region.routing_version.wrapping_add(1);
            }
            region.owners.insert(
                Arc::new(owner),
                ProviderRef {
                    node: provider,
                    label: draft.label,
                    location: draft.location,
                },
            );
            installed.push(DefinitionRecord {
                unit: draft.unit.clone(),
                provider,
                value: Arc::clone(&draft.value),
            });
        }
        if content_changed {
            let content = (descriptor.content.define_many)(&region.content, &content_drafts);
            let units: Vec<UnitKey> = content_drafts
                .iter()
                .map(|(unit, _)| unit.clone())
                .collect();
            let equal = (descriptor.content.changed_units_equal)(&region.content, &content, &units);
            if equal != Some(true) {
                state
                    .stats
                    .content_batches
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Self::commit_content_changed(state, region, content, revision, changed_nodes);
            }
        }
        for (field, keyed) in protocols {
            Self::set_field_protocol(state, descriptor.id, field, keyed, revision, changed_nodes);
        }
        // The group's presence decisions land after its content rewrite: a
        // `Membership = false` draft states that the entity is absent, and
        // absence implies the payload is absent. Both cells go through the one
        // retraction primitive, so no draft of the same group can leave a
        // payload certified behind an absence it stated itself.
        let withdrawn_units: Vec<UnitKey> = withdrawn
            .iter()
            .filter(|owner| owner.view == descriptor.id)
            .map(payload_key)
            .collect();
        if !withdrawn_units.is_empty() {
            Self::retract_entities(
                state,
                region,
                provider,
                &withdrawn_units,
                true,
                revision,
                changed_nodes,
                &mut presence_fields,
            );
        }
        // One flush for the whole group, and the projection each moved field
        // gets is decided by who answered last:
        //
        // * A field the group *publishes* a structure for has already been
        //   answered by that draft, so refreshing it again would write the cell
        //   a second time and could only produce the value it already holds.
        // * A field the group's presence decisions moved has no such answer:
        //   those drafts were computed from the content *before* the retraction,
        //   so the retraction is the group's last word and the projection must
        //   follow it.
        let published: FxHashSet<UnitKey> = drafts
            .iter()
            .filter(|draft| draft.unit.kind == UnitKind::Structure)
            .map(|draft| draft.unit.clone())
            .collect();
        for field in dirty_fields {
            let structure = UnitKey::field(descriptor.id, field, UnitKind::Structure);
            if published.contains(&structure) {
                continue;
            }
            Self::refresh_structure(state, region, field, revision, changed_nodes);
        }
        for field in presence_fields {
            Self::refresh_structure(state, region, field, revision, changed_nodes);
        }
    }

    /// Publishes one View's content, advancing the content unit's version.
    ///
    /// The definition pipeline — install, retraction, conflict restore —
    /// computes the content field by field and refreshes the fields it moved,
    /// so it takes this primitive. A whole-region replacement takes
    /// [`Self::publish_region_content`] instead.
    ///
    /// Returns whether the content moved: a caller that derives cells from it
    /// (the retraction that refreshes a field's structure) checks the answer
    /// instead of re-deriving the cutoff.
    fn commit_content(
        state: &mut State,
        region: &mut Region,
        content: Arc<dyn Any + Send + Sync>,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) -> bool {
        let eq = region.descriptor.content.eq;
        Self::commit_content_with_eq(state, region, content, eq, revision, changed_nodes)
    }

    /// [`commit_content`] with the caller's equality for the cutoff.
    ///
    /// The host content path stages the transaction's own `eq`, so it passes it
    /// through instead of re-deciding the cutoff with the descriptor's.
    fn commit_content_with_eq(
        state: &mut State,
        region: &mut Region,
        content: Arc<dyn Any + Send + Sync>,
        eq: fn(&Value, &Value) -> bool,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) -> bool {
        state.stats.content_commits.fetch_add(1, Ordering::Relaxed);
        let changed = state
            .node(region.content_node)
            .value
            .as_ref()
            .is_none_or(|old| !eq(old, &content));
        if changed {
            let value_version = state.alloc_value_version();
            let record = state.node_mut(region.content_node);
            record.install_value(Some(content.clone()), value_version, revision);
            changed_nodes.push(region.content_node);
        }
        region.content = content;
        changed
    }

    /// Publishes content already known to have moved.
    ///
    /// The definition pipeline applies a unit only when that unit's value moved
    /// and the field says the unit moves content. In that case the rebuilt
    /// content is a new value by construction; comparing it with the previous
    /// whole content would repeat the work the unit comparison already did.
    fn commit_content_changed(
        state: &mut State,
        region: &mut Region,
        content: Arc<dyn Any + Send + Sync>,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        state.stats.content_commits.fetch_add(1, Ordering::Relaxed);
        let value_version = state.alloc_value_version();
        let record = state.node_mut(region.content_node);
        record.install_value(Some(content.clone()), value_version, revision);
        changed_nodes.push(region.content_node);
        region.content = content;
    }

    /// Publishes a whole-region content replacement and the structure it derives.
    ///
    /// A host `set_content` and a general component's are the same transition:
    /// both replace every field's content at once. A structure unit is computed
    /// from content but its readers observed the unit, not the content, so
    /// leaving it to lazily re-initialize would keep serving the old projection
    /// for good.
    ///
    /// Only materialized units are refreshed: a unit nobody read has nothing to
    /// correct, and a provider-owned structure is the provider's to publish.
    /// Returns whether the View is registered, so the caller can take the raw
    /// path for a namespace that has no region yet.
    pub(crate) fn publish_region_content(
        state: &mut State,
        view: ViewId,
        content: Arc<dyn Any + Send + Sync>,
        eq: fn(&Value, &Value) -> bool,
        revision: Revision,
        delta: CellDelta<'_>,
        changed_nodes: &mut Vec<NodeId>,
    ) -> bool {
        Self::with_taken_region(state, view, |state, region| {
            let descriptor = region.descriptor;
            let normalization_safe = descriptor.fields.iter().all(|field| {
                matches!(
                    field.kind,
                    irrigation_core::descriptor::FieldKind::Scalar
                        | irrigation_core::descriptor::FieldKind::Map
                        | irrigation_core::descriptor::FieldKind::List
                        | irrigation_core::descriptor::FieldKind::Set
                )
            });
            // One changed content-moving unit is a proof that this transaction
            // moved that unit. With several units, an add/remove pair can end at
            // the committed content, so the equality remains the normalization
            // point. Graph/tree/reduced families keep equality: their topology
            // and edit scripts can cancel across units.
            let units: Vec<UnitKey> = delta.installed.iter().cloned().collect();
            let unit_equality =
                (descriptor.content.changed_units_equal)(&region.content, &content, &units);
            let content_moved = match unit_equality {
                Some(true) => false,
                Some(false) => true,
                None => {
                    normalization_safe
                        && units.len() == 1
                        && units
                            .iter()
                            .any(|unit| (descriptor.content.moves_content)(unit))
                }
            };
            let changed = if content_moved {
                state
                    .stats
                    .source_content_applies
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Self::commit_content_changed(state, region, content, revision, changed_nodes);
                true
            } else if matches!(unit_equality, Some(true)) {
                // The changed units project equal content; no whole equality
                // is needed either.
                false
            } else {
                state
                    .stats
                    .source_equality_checks
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Self::commit_content_with_eq(state, region, content, eq, revision, changed_nodes)
            };
            if changed {
                if delta.installed.is_empty() {
                    Self::refresh_structures(state, region, revision, changed_nodes);
                } else {
                    let fields: Vec<FieldId> = delta
                        .installed
                        .iter()
                        .filter(|unit| unit.kind != UnitKind::Value || unit.entity.is_none())
                        .map(|unit| unit.field)
                        .collect();
                    if !fields.is_empty() {
                        Self::refresh_structures_of(
                            state,
                            region,
                            Some(&fields),
                            revision,
                            changed_nodes,
                        );
                    }
                }
            }
            if changed {
                Self::invalidate_cells(state, region, delta, revision, changed_nodes);
            }
        })
        .is_some()
    }

    /// Republishes every materialized structure projection from the region's
    /// committed content.
    ///
    /// A structure unit is derived from content but its readers observed the
    /// unit, not the content, so a content replacement that skipped this would
    /// leave them serving a projection the content no longer supports. Only
    /// materialized units are refreshed — a unit nobody read initializes from
    /// the new content — and a provider-owned structure is the provider's to
    /// publish.
    fn refresh_structures(
        state: &mut State,
        region: &mut Region,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        Self::refresh_structures_of(state, region, None, revision, changed_nodes);
    }

    fn refresh_structures_of(
        state: &mut State,
        region: &mut Region,
        fields: Option<&[FieldId]>,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let descriptor = region.descriptor;
        let indices: Vec<usize> = match fields {
            Some(fields) => {
                let mut indices: Vec<usize> = fields.iter().map(|field| field.0 as usize).collect();
                indices.sort_unstable();
                indices.dedup();
                indices
            }
            None => (0..descriptor.fields.len()).collect(),
        };
        for index in indices {
            let field = FieldId(index as u16);
            let structure = UnitKey::field(descriptor.id, field, UnitKind::Structure);
            let materialized =
                region.units.get(&structure).is_some() || state.units.get(&structure).is_some();
            if !materialized || Self::structure_is_published(region, field) {
                continue;
            }
            Self::refresh_structure(state, region, field, revision, changed_nodes);
        }
    }

    /// The materialized cells one whole-region replacement owns.
    ///
    /// A replacement with no cell write beside it names no delta: it replaced
    /// the content every cell is a projection of, and the runtime has no
    /// type-erased equality to tell which cell's payload survived. So the cell
    /// is opened for a new generation — its readers re-execute, and the next
    /// read re-derives it from the committed content, where a projection the
    /// content vtable cannot spell (a rank or adjacency index) is re-derived
    /// through the accessor that materialized it.
    ///
    /// Reading one of these cells in the replacing group is read-your-own-write
    /// for the same reason, and [`InstalledPorts`] answers that question with
    /// this one predicate.
    ///
    /// Ownership is answered from `region`, not from `state.regions`: the
    /// publisher holds the region taken out of the map, so asking the map would
    /// find no View and report every cell unowned.
    ///
    /// A structure unit is the field's own projection and was just republished
    /// with the content's equality, so it is not stale; a provider-owned cell is
    /// the provider's to publish.
    fn replacement_owns_cell(state: &State, region: &Region, unit: &UnitKey, node: NodeId) -> bool {
        // An entity's cells project the content, and so does a whole-field
        // value cell. An entity-less membership unit is the field's keyed/whole
        // protocol — a certificate the definition pipeline publishes, never a
        // projection — and a structure unit was just republished above.
        (unit.entity.is_some() || unit.kind == UnitKind::Value)
            && region.owners.get(&owner_key(unit)).is_none()
            && node_is_live(state, node)
    }

    /// Whether the replacement content already projects this cell's payload.
    ///
    /// The runtime has no type-erased equality for a cell payload, so it asks
    /// the one equality it does have: installing the payload into the new
    /// content must leave that content unchanged. Presence is answered directly
    /// by `contains`, because a membership payload is a bool and a family's
    /// `define` reads a membership as a value definition. A family whose
    /// `define` cannot realize a unit — a tree, which is likewise replaced whole
    /// — answers "unchanged", the same trust the stated delta itself carries for
    /// that family.
    fn cell_matches_content(state: &State, region: &Region, unit: &UnitKey, node: NodeId) -> bool {
        if unit.kind == UnitKind::Membership {
            let present = (region.descriptor.content.contains)(&region.content, unit);
            return membership_of(state, node) == Some(present);
        }
        let Some(payload) = state.node(node).value.clone() else {
            return false;
        };
        let next = (region.descriptor.content.define)(&region.content, unit, payload);
        (region.descriptor.content.eq)(&region.content, &next)
    }

    /// Opens the generation of every cell a transaction installed whose payload
    /// the content that transaction committed does not project.
    ///
    /// The publish decision for each staged write is measured once, against the
    /// state the transaction found. Two editors in one transaction each clone
    /// that state, and only the last replacement reaches the content, so a
    /// write can still publish a value the committed content contradicts. The
    /// content is the revision's answer: such a cell is opened for a new
    /// generation — its readers re-execute and the next read re-derives it —
    /// instead of serving the staged value for good.
    ///
    /// The pass runs after the transaction's whole publish loop, because a cell
    /// write staged after the content replacement is installed after the
    /// replacement too. Only payload and presence cells are checked, and a
    /// retraction's cells are never in `written`: it already owns their
    /// generation, and reopening one would let a read certify an absence the
    /// retraction must leave uncertified.
    pub(crate) fn reconcile_written_cells(
        state: &mut State,
        view: ViewId,
        written: &FxHashSet<UnitKey>,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let _ = Self::with_taken_region(state, view, |state, region| {
            for unit in written {
                if !matches!(unit.kind, UnitKind::Value | UnitKind::Membership) {
                    continue;
                }
                let Some(node) = current_unit_node(state, unit) else {
                    continue;
                };
                if Self::cell_matches_content(state, region, unit, node) {
                    continue;
                }
                let value_version = state.alloc_value_version();
                state
                    .node_mut(node)
                    .begin_generation(value_version, revision);
                changed_nodes.push(node);
            }
        });
    }

    /// Opens a new generation of every materialized cell a replacement owns.
    ///
    /// `delta` says what the same commit writes itself. A commit that stages
    /// cells beside the replacement claims their sum is the whole content delta,
    /// so each remaining cell is checked: one that still projects its payload is
    /// left alone, and one that does not is the replacement's. A commit that
    /// stages no cell names no delta, so the replacement owns every cell it did
    /// not install.
    fn invalidate_cells(
        state: &mut State,
        region: &Region,
        delta: CellDelta<'_>,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let view = region.descriptor.id;
        let Some(cells) = state.cells.get(&view) else {
            return;
        };
        let stale: Vec<(UnitKey, NodeId)> = cells
            .iter()
            .filter_map(|(key, node)| {
                let unit = key.as_any().downcast_ref::<UnitKey>()?;
                let owns = Self::replacement_owns_cell(state, region, unit, *node)
                    && !delta.installed.contains(unit);
                owns.then(|| (unit.clone(), *node))
            })
            .collect();
        for (unit, node) in stale {
            // A stated delta owns the value cells its transaction did not write —
            // one staged cell cannot stand in for a content that moves another.
            // Presence is the exception: every editor stages the memberships it
            // changes, and a family answers presence in its own way (a tree's
            // node set is not its membership field), so a presence cell the
            // transaction did not write did not move.
            let kept = if delta.stated {
                unit.kind == UnitKind::Membership
                    || Self::cell_matches_content(state, region, &unit, node)
            } else {
                unit.kind == UnitKind::Membership
                    && Self::cell_matches_content(state, region, &unit, node)
            };
            if kept {
                continue;
            }
            // The cell's payload belongs to this revision now, even though it
            // is installed by the next read: a change-version observer of the
            // cell must be told about the replacement that moved it, at the
            // revision that moved it.
            let value_version = state.alloc_value_version();
            state
                .node_mut(node)
                .begin_generation(value_version, revision);
            changed_nodes.push(node);
        }
    }

    /// Writes one unit's value as a committed change at `revision`.
    fn commit_unit_value(
        state: &mut State,
        node: NodeId,
        value: Value,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let value_version = state.alloc_value_version();
        let record = state.node_mut(node);
        record.install_value(Some(value), value_version, revision);
        record.routing_at = revision;
        changed_nodes.push(node);
    }

    /// Retires one cell's payload as a change of its own generation.
    ///
    /// Retraction is the inverse of installation, so the payload, the version,
    /// and the stamp move together like any other change. The cell is left
    /// uncertified on purpose — a retracted unit is not a value the runtime
    /// serves, and an observation of it must fail validation instead of pinning
    /// an absence.
    ///
    /// The cell itself is the seed, never its readers: the pass this feeds has
    /// to see those readers as the results it is about, and exempting them
    /// would let a demand carry out a value this retraction contradicts.
    fn retire_cell(
        state: &mut State,
        node: NodeId,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let value_version = state.alloc_value_version();
        let validity = state.alloc_validity();
        let record = state.node_mut(node);
        record.retire_value(value_version, revision);
        record.routing_at = revision;
        record.verified_at = 0;
        record.verified_validity = 0;
        record.suspect_at = revision;
        record.suspect_validity = validity;
        changed_nodes.push(node);
    }

    /// Restores one unit's committed value and its membership cell.
    ///
    /// Conflict resolution has to undo a later provider's install without
    /// running the definition pipeline; the unit node and the entity's
    /// membership both belong to the restored value.
    fn restore_definition(
        state: &mut State,
        region: &mut Region,
        unit: &UnitKey,
        value: Value,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        if unit.entity.is_some() {
            let membership = membership_key(unit);
            let present = if unit.kind == UnitKind::Membership {
                value.downcast_ref::<bool>().copied().unwrap_or(true)
            } else {
                true
            };
            if let Some(slot) = region.units.get_mut(&membership) {
                slot.present = present;
                let node = slot.node;
                Self::commit_unit_value(state, node, value_of(present), revision, changed_nodes);
            }
        }
        if unit.kind != UnitKind::Membership
            && let Some(slot) = region.units.get(unit)
        {
            let node = slot.node;
            Self::commit_unit_value(state, node, value, revision, changed_nodes);
        }
    }

    /// Retracts one entity: presence false and payload absent.
    ///
    /// A member's cells answer one question, so every retraction lands here —
    /// a provider releasing a definition, and an editor staging
    /// `Membership = false` without a payload write beside it. The cells
    /// resolve through the unit table as well as the region, because a
    /// host-installed entity has no region slot and its retraction must still
    /// clear both.
    ///
    /// The entity is named by either of its cells: a retraction that stages the
    /// presence and one that stages the payload describe the same removal, so
    /// the key is normalised to the payload before anything is cleared. Retraction
    /// is the inverse of installation, which means the payload is *gone* and not
    /// the removed value left behind a cleared certification — the next install
    /// of the entity must be able to take it over, and a direct read of the unit
    /// must answer absent.
    ///
    /// Returns whether the entity was present.
    pub(crate) fn retract_entity(
        state: &mut State,
        unit: &UnitKey,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) -> bool {
        let unit = &payload_key(unit);
        let membership = membership_key(unit);
        let removed = if let Some(node) = current_unit_node(state, &membership)
            && membership_of(state, node) == Some(true)
        {
            let value_version = state.alloc_value_version();
            let record = state.node_mut(node);
            record.install_value(Some(value_of(false)), value_version, revision);
            changed_nodes.push(node);
            true
        } else {
            false
        };
        // The payload belongs to the same generation as the presence: both cells
        // are this entity's, one retraction clears both, and the version and the
        // stamp move with the payload so no consumer keeps an observation of a
        // value the entity no longer has.
        //
        // The generation closes even when the cell holds no payload: a
        // replacement opened it with a version and a stamp of its own, and the
        // removal is what that generation changed for. Leaving it would keep the
        // payload's change version naming the replacement, so a payload
        // subscriber would never hear the removal. A cell that never held an
        // answer has no generation to close, and a cell another retraction in
        // this commit already retired keeps its cleared state.
        if let Some(node) = current_unit_node(state, unit) {
            let record = state.node(node);
            let closes_generation = record.value.is_some() || record.value_version != 0;
            let already_retired =
                record.value.is_none() && record.changed_at == revision && !record.content_stale;
            if closes_generation && !already_retired {
                Self::retire_cell(state, node, revision, changed_nodes);
            }
        }
        removed
    }

    fn retract_unit(
        state: &mut State,
        region: &mut Region,
        provider: NodeId,
        unit: &UnitKey,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
        force: bool,
    ) {
        let descriptor = region.descriptor;
        let owner = owner_key(unit);

        if let Some(existing) = region.owners.get(&owner) {
            // Retirement only releases ownership this provider still holds. An
            // explicit retraction may take a stale unit over, or a sibling
            // definition site of its own component; every other live provider's
            // unit was rejected by validation before the install.
            let may_take_over = force && retraction_may_take_over(state, provider, existing, unit);
            if existing.node != provider && !may_take_over {
                return;
            }
        }
        // A branch site can retire after another site replaced the field in the
        // same revision. The cells it defined are still its own to clear, but
        // the content now belongs to the replacement.
        let owns_content = Self::retraction_owns_content(state, region, provider, unit);
        if region.owners.get(&owner).is_some() && owns_content {
            region.routing_version = region.routing_version.wrapping_add(1);
        }
        region.owners.remove(&owner);
        let mut content = Arc::clone(&region.content);
        if owns_content {
            if unit.entity.is_none() && unit.kind == UnitKind::Structure {
                // Retraction mirrors installation: a retracted structure is a
                // field that is no longer keyed, and its readers must hear about
                // it. A whole-field value retracts into the whole protocol the
                // value installed, so its bit already reads false and nothing
                // moves.
                Self::set_field_protocol(
                    state,
                    unit.view,
                    unit.field,
                    false,
                    revision,
                    changed_nodes,
                );
            }
            content = (descriptor.content.retract)(&content, unit);
            if unit.entity.is_none() {
                // A whole-field definition was retracted: the field reverts to
                // its empty value, and the unit node takes that value so readers
                // and snapshots agree. The two whole-field units retract from
                // different spellings: a structure unit is the family's
                // projection of the field, while a value unit is the storage
                // the definition installed.
                let value = match unit.kind {
                    UnitKind::Structure => (descriptor.content.structure)(&content, unit.field)
                        .map(|structure| structure.value),
                    UnitKind::Value => (descriptor.content.field_storage)(&content, unit.field),
                    _ => None,
                };
                if let Some(slot) = region.units.get(unit) {
                    let node = slot.node;
                    let value_version = state.alloc_value_version();
                    let record = state.node_mut(node);
                    record.install_value(value, value_version, revision);
                    record.routing_at = revision;
                    changed_nodes.push(node);
                }
            }
        } else if unit.entity.is_none() && unit.kind == UnitKind::Value {
            // The replacement site publishes this field's protocol now, but no
            // provider refreshes a whole-field payload: the value this site
            // installed is nobody's answer any more, and the content vtable
            // cannot re-derive it. The structure half is *not* retired — the
            // incoming publisher refreshed that cell as its own, and it is the
            // field's projection under the protocol it just published.
            if let Some(node) = current_unit_node(state, unit)
                && state.node(node).value.is_some()
            {
                Self::retire_cell(state, node, revision, changed_nodes);
            }
        }
        let removed_membership = if unit.entity.is_some() {
            let membership = membership_key(unit);
            if let Some(slot) = region.units.get_mut(&membership) {
                slot.present = false;
            }
            Self::retract_entity(state, unit, revision, changed_nodes)
        } else {
            false
        };
        if !owns_content {
            return;
        }
        let content_moved = Self::commit_content(state, region, content, revision, changed_nodes);
        // The structure follows the content, not the presence cell: a
        // definition-path retraction can remove an entity the content spells
        // while no membership cell ever held `true` (a content-only key), and
        // the key set then moves without `removed_membership` saying so.
        if removed_membership
            || content_moved
            || (unit.entity.is_none()
                && (descriptor.content.structure)(&region.content, unit.field).is_some())
        {
            Self::refresh_unowned_structure(state, region, unit.field, revision, changed_nodes);
        }
    }

    /// Retracts a batch of one View's entity units with a single publish.
    ///
    /// [`Self::retract_unit`] is the single-unit primitive and publishes the
    /// field's content itself. A write group that retracts a whole field — a
    /// provider whose definition set shrank, a presence decision covering
    /// thousands of entities, a region deleted from a document — calls that
    /// primitive once per unit, and each call rebuilds and republishes the
    /// field's content: the group becomes quadratic in what it retracts.
    ///
    /// The per-unit half is unchanged and still the single owner of each
    /// entity's cells, ownership record and routing bump. Only the content
    /// publish is hoisted out of the loop, and the fields it moved are returned
    /// so the caller can refresh each projection exactly once, from the group's
    /// final content, instead of once per retracted unit.
    ///
    /// Entity units only. A whole-field unit retracts the field's *protocol* and
    /// reinstalls the unit's own storage, which is one unit by definition and
    /// stays on the single-unit path.
    #[allow(clippy::too_many_arguments)]
    fn retract_entities(
        state: &mut State,
        region: &mut Region,
        provider: NodeId,
        units: &[UnitKey],
        force: bool,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
        dirty_fields: &mut Vec<FieldId>,
    ) {
        let descriptor = region.descriptor;
        let mut content = Arc::clone(&region.content);
        let mut moved = false;
        for unit in units {
            let unit = &payload_key(unit);
            let owner = owner_key(unit);
            if let Some(existing) = region.owners.get(&owner) {
                // The same authority the single-unit primitive applies: this
                // provider's own unit, a sibling site of its own component, or a
                // stale record it may take over.
                let may_take_over =
                    force && retraction_may_take_over(state, provider, existing, unit);
                if existing.node != provider && !may_take_over {
                    continue;
                }
            }
            let owns_content = Self::retraction_owns_content(state, region, provider, unit);
            if owns_content {
                if region.owners.get(&owner).is_some() {
                    region.routing_version = region.routing_version.wrapping_add(1);
                }
                content = (descriptor.content.retract)(&content, unit);
                moved = true;
                if !dirty_fields.contains(&unit.field) {
                    dirty_fields.push(unit.field);
                }
            }
            region.owners.remove(&owner);
            let membership = membership_key(unit);
            if let Some(slot) = region.units.get_mut(&membership) {
                slot.present = false;
            }
            Self::retract_entity(state, unit, revision, changed_nodes);
        }
        if moved {
            Self::commit_content_changed(state, region, content, revision, changed_nodes);
        }
    }

    /// Refreshes one field's structure projection unless a provider publishes
    /// that unit.
    ///
    /// A field's structure is the projection of its content, so every path that
    /// moves content has to republish a materialized structure cell — the
    /// replacement path and the retraction path alike. The cell is not always
    /// the runtime's to write: a keyed publisher stages the structure unit
    /// itself, and a provider-owned cell is the provider's to publish on every
    /// path. Both callers ask [`Self::structure_is_published`], because a rule
    /// answered twice drifts.
    fn refresh_unowned_structure(
        state: &mut State,
        region: &mut Region,
        field: FieldId,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        if Self::structure_is_published(region, field) {
            return;
        }
        Self::refresh_structure(state, region, field, revision, changed_nodes);
    }

    /// Whether a field's structure unit is a provider's output.
    fn structure_is_published(region: &Region, field: FieldId) -> bool {
        let structure = UnitKey::field(region.descriptor.id, field, UnitKind::Structure);
        region.owners.get(&owner_key(&structure)).is_some()
    }

    /// Whether retracting `unit` still describes its field's committed content.
    ///
    /// A field's content belongs to whichever protocol unit a site published: a
    /// `Structure` install publishes the keyed one, an entity-less `Value`
    /// install the whole one, and entity cells belong to the keyed one. A branch
    /// site can retire after another site replaced the field in the same
    /// revision, and the obsolete protocol's cells then retire as records, not
    /// as content.
    fn retraction_owns_content(
        state: &State,
        region: &Region,
        provider: NodeId,
        unit: &UnitKey,
    ) -> bool {
        if unit.entity.is_some() {
            // Entity cells are keyed content; an entity-less value published in
            // the same field already replaced them.
            return state.keyed_fields.contains(&(unit.view, unit.field))
                || region
                    .owners
                    .get(&UnitKey::field(unit.view, unit.field, UnitKind::Value))
                    .is_none();
        }
        let other = match unit.kind {
            UnitKind::Structure => UnitKind::Value,
            UnitKind::Value => UnitKind::Structure,
            _ => return true,
        };
        let key = UnitKey::field(unit.view, unit.field, other);
        region
            .owners
            .get(&key)
            .is_none_or(|owner| owner.node == provider)
    }

    /// Records a wait on `node`, or parks when the task cannot yield.
    ///
    /// A pooled task registers itself and returns [`ComputeError::Suspended`]
    /// so its attempt unwinds and the worker is free. An inline task has no
    /// worker to yield and blocks until the node leaves `Running`.
    fn wait_for(&self, node: NodeId, me: TaskId) -> Result<()> {
        let mut state = self.lock();
        let owner = match state.node(node).state {
            NodeState::Running { owner, .. } => owner,
            _ => return Ok(()),
        };
        // A task already executing on this stack cannot wait for itself.
        if task::stack_contains(owner) {
            return Err(ComputeError::Cycle {
                path: vec![format!(
                    "re-entrant demand of {} ({node:?})",
                    state.node(node).label
                )],
            });
        }
        if Self::would_cycle(&state, owner, me) {
            return Err(ComputeError::Cycle {
                path: cycle_path(&state, me, node),
            });
        }
        if frame::current().is_some_and(|frame| frame.cancel().is_cancelled()) {
            return Err(ComputeError::Cancelled);
        }
        state.wait_graph.insert(me, node);
        if state.tasks.get(&me).map(|slot| slot.mode) == Some(TaskMode::Pooled) {
            // Yield the worker: record the wait and let the attempt unwind. The
            // claim set is unioned, not replaced: a node claimed for ordered
            // validation is owned even though its frame never opened.
            let held = task::active_snapshot();
            if let Some(slot) = state.tasks.get_mut(&me) {
                slot.status = TaskStatus::Waiting(node);
            }
            merge_held(&mut state, me, &held);
            state.stats.suspensions.fetch_add(1, Ordering::Relaxed);
            state.node_mut(node).waiters.push(me);
            drop(state);
            #[cfg(feature = "observe")]
            self.emit(crate::observe::RuntimeEvent::TaskWaiting { task: me, node });
            return Err(ComputeError::Suspended);
        }
        // Inline tasks have no worker to yield; park the calling thread.
        loop {
            let running = match state.node(node).state {
                NodeState::Running { owner: current, .. } => Some(current),
                _ => None,
            };
            match running {
                Some(current) if current == owner => {
                    drop(state);
                    let helped = self.pool.try_run_one();
                    state = self.lock();
                    if helped {
                        continue;
                    }
                    self.wake.wait_for(&mut state, self.config.wait_slice);
                }
                _ => break,
            }
        }
        state.wait_graph.remove(&me);
        Ok(())
    }

    /// Queues the tasks suspended on `node`. Called under the state lock; the
    /// caller enqueues the returned tasks and reports the wake events once the
    /// lock is released.
    pub(crate) fn wake_waiters(state: &mut State, node: NodeId) -> Waiters {
        let waiters = std::mem::take(&mut state.node_mut(node).waiters);
        let wakers = std::mem::take(&mut state.node_mut(node).wakers);
        let mut tasks = Vec::new();
        #[cfg(feature = "observe")]
        let mut woken = Vec::new();
        for waiter in waiters {
            let pooled = match state.tasks.get_mut(&waiter) {
                Some(slot) if slot.status == TaskStatus::Waiting(node) => {
                    slot.status = TaskStatus::Queued;
                    state.wait_graph.remove(&waiter);
                    slot.mode == TaskMode::Pooled
                }
                _ => false,
            };
            if pooled {
                tasks.push(waiter);
            }
            #[cfg(feature = "observe")]
            woken.push((waiter, node));
        }
        Waiters {
            tasks,
            wakers,
            #[cfg(feature = "observe")]
            woken,
            #[cfg(not(feature = "observe"))]
            woken: Vec::new(),
        }
    }

    /// Returns the node cycles in the current wait graph.
    #[must_use]
    pub fn wait_cycles(&self) -> Vec<Vec<NodeId>> {
        wait_cycles(&self.lock())
    }

    fn would_cycle(state: &State, from: TaskId, target: TaskId) -> bool {
        let mut current = from;
        let mut visited = HashSet::new();
        while visited.insert(current) {
            let Some(node) = state.wait_graph.get(&current) else {
                return false;
            };
            match state.node(*node).state {
                NodeState::Running { owner, .. } => {
                    if owner == target {
                        return true;
                    }
                    current = owner;
                }
                _ => return false,
            }
        }
        false
    }

    /// Allocates a fresh entity identity.
    pub fn alloc_entity(&self) -> u64 {
        self.lock().alloc_entity()
    }

    /// Makes a dropped deferred write group fail its owner for the revision.
    ///
    /// The group was taken out of the pending set, so its definitions will never
    /// publish. Its owner already committed and certified its value, so without
    /// this the next demand at the same revision would reuse that value and
    /// report success for a write that never landed.
    fn fail_dropped_writes(&self, owners: &[NodeId], error: &ComputeError, revision: Revision) {
        let mut failed: Vec<NodeId> = Vec::with_capacity(owners.len());
        for &owner in owners {
            if failed.contains(&owner) {
                continue;
            }
            failed.push(owner);
            let _ = self.fail::<()>(owner, error.clone(), revision);
        }
        // The owner committed the value that fed its observers before the group
        // was deferred, so they are not reusable either: invalidating them makes
        // a later demand reach the failed owner and surface the same error
        // instead of replaying a result whose write never landed.
        let mut state = self.lock();
        invalidate(&mut state, failed, revision);
    }

    /// Reopens the owner of a deferred group its demand never drained.
    ///
    /// The group will never install, but this path has not re-checked its
    /// requirement: a from-scratch evaluation at the revision may well install
    /// it, so failing the owner here would invent a failure the drain has not
    /// proved. Releasing the claims and dropping the owner's certification
    /// instead makes the next demand re-execute it, which stages a fresh group;
    /// that demand's drain then installs or fails it exactly as a fresh run
    /// would. The nodes that observed the owner are only marked suspect, so a
    /// replay that reproduces the same output keeps their change versions.
    pub(crate) fn discard_pending_write(state: &mut State, write: &crate::state::PendingWrite) {
        // A re-executing owner already took the group's scopes and value back
        // under its own task, so this teardown has nothing left to undo.
        if state.node(write.owner).is_running() {
            return;
        }
        state.release_claims(write.owner);
        // Order matters: `invalidate` stops at a seed it already considers
        // suspect, so the owner is marked while it still looks clean.
        let _ = invalidate(state, std::iter::once(write.owner), write.revision);
        uncertify(state, write.owner, write.revision);
    }

    /// Resolves write groups deferred to the end of a demand.
    ///
    /// A group whose trace is stale is not installed: its owner is forced to
    /// re-execute against the now-current state and the demand retries. This is
    /// the common path when a producer satisfied the group's requirement during
    /// the same demand.
    pub(crate) fn drain_pending_writes(&self, task: TaskId) -> Result<DrainOutcome> {
        let selector = self.route_selector.lock().clone();
        // Route collisions are collected under the lock and decided without it:
        // the selector is host code and may call back into the runtime.
        let (mut pending, revision, collisions) = {
            let mut state = self.lock();
            let mut pending: Vec<crate::state::PendingWrite> = Vec::new();
            let mut remaining = Vec::new();
            for write in state.pending_writes.drain(..) {
                if write.task == task {
                    pending.push(write);
                } else {
                    remaining.push(write);
                }
            }
            state.pending_writes = remaining;
            if pending.is_empty() {
                return Ok(DrainOutcome::Done);
            }
            let revision = state.revision;
            let mut collisions = Vec::new();
            for write in &pending {
                for collision in route_collisions(&state, write.owner, &write.drafts) {
                    if !collisions.iter().any(|(unit, _, _)| *unit == collision.0) {
                        collisions.push(collision);
                    }
                }
            }
            (pending, revision, collisions)
        };
        let decisions = decide_routes(selector.as_ref(), &collisions);
        let mut state = self.lock();
        let mut overtaken = false;
        loop {
            let mut installed_any = false;
            let mut stale = None;
            let mut index = 0;
            while index < pending.len() {
                let group = WriteGroup {
                    provider: pending[index].owner,
                    drafts: &pending[index].drafts,
                    retractions: &pending[index].retractions,
                    restate_all: pending[index].restate_all,
                    // A deferred group never carries a content replacement: a
                    // group that replaces content cannot be deferred.
                    content_views: &[],
                };
                let holds = pending[index]
                    .requirements
                    .iter()
                    .all(|requirement| requirement_holds(&state, requirement, group));
                if !holds {
                    index += 1;
                    continue;
                }
                let current = pending[index].trace.iter().all(|dep| {
                    let record = state.node(dep.node);
                    record.value.is_some()
                        && record.has_valid_trace()
                        && record.value_version == dep.version
                        && record.routing_at == dep.routing
                });
                if !current {
                    stale = Some(index);
                    break;
                }
                let write = pending.remove(index);
                let mut changed = Vec::new();
                if let Err(error) = Self::validate_definitions(
                    &state,
                    write.owner,
                    &write.drafts,
                    &write.retractions,
                    write.enforce_views,
                ) {
                    // A rejected group takes the rest of the demand's groups
                    // with it: nothing may publish, and every claim they hold
                    // has to be released or the next demand sees a phantom
                    // writer. The owners keep their committed value only until
                    // the failure below marks them, so the loss is not silent.
                    let mut dropped: Vec<NodeId> =
                        pending.iter().map(|pending| pending.owner).collect();
                    dropped.push(write.owner);
                    for &owner in &dropped {
                        state.release_claims(owner);
                    }
                    drop(state);
                    self.fail_dropped_writes(&dropped, &error, revision);
                    return Err(error);
                }
                Self::install_definitions(
                    &mut state,
                    write.owner,
                    write.drafts,
                    (write.retractions, write.restate_all),
                    revision,
                    write.enforce_views,
                    &decisions,
                    &mut changed,
                );
                if !changed.is_empty() {
                    // An install here happens after the demand's last commit,
                    // so a dependent this revision already finalized is a
                    // result that now contradicts the state the drain leaves.
                    // No commit follows to revalidate it: the demand has to
                    // replay instead of carrying that value out.
                    let (_, finalized) =
                        invalidate_tracked(&mut state, changed.iter().copied(), revision);
                    overtaken |= finalized;
                }
                state.release_claims(write.owner);
                installed_any = true;
            }
            if let Some(index) = stale {
                // The stale group is dropped on purpose: its owner re-executes
                // and stages the group again. The groups staged after it are
                // still this demand's work, and the retry keeps the same task,
                // so they stay pending for the retried demand's drain instead
                // of vanishing with the attempt that found them.
                let write = pending.remove(index);
                state.pending_writes.extend(pending);
                state.release_claims(write.owner);
                force_recompute(&mut state, &[write.owner], revision);
                drop(state);
                return Ok(DrainOutcome::Retry);
            }
            if !installed_any {
                let write = pending.first().expect("pending is non-empty");
                let group = WriteGroup {
                    provider: write.owner,
                    drafts: &write.drafts,
                    retractions: &write.retractions,
                    restate_all: write.restate_all,
                    content_views: &[],
                };
                let requirement = write
                    .requirements
                    .iter()
                    .find(|requirement| !requirement_holds(&state, requirement, group))
                    .expect("a pending group with no satisfied requirement");
                let error = precondition_error(requirement);
                let dropped: Vec<NodeId> = pending.iter().map(|pending| pending.owner).collect();
                for &owner in &dropped {
                    state.release_claims(owner);
                }
                drop(state);
                self.fail_dropped_writes(&dropped, &error, revision);
                return Err(error);
            }
            if pending.is_empty() {
                // The pool is finished before the retry is reported, so every
                // group this demand could resolve has installed and the retry
                // starts from the state the drain published.
                return Ok(if overtaken {
                    DrainOutcome::Overtaken
                } else {
                    DrainOutcome::Done
                });
            }
        }
    }

    /// Ends one demand: drains the write groups it deferred, then resolves the
    /// definition conflicts its commits staged.
    ///
    /// Deferral is part of the demand protocol rather than of one kind of root,
    /// so every demand surface finishes here — a general root and a host `call`
    /// defer exactly like a lowered one. Only the attempt that owns the revision
    /// gate may drain: a nested call is part of an enclosing attempt, and its
    /// groups are handed to that attempt when its task closes.
    pub(crate) fn finish_demand(&self, revision: Revision, roots: &[NodeId]) -> Result<()> {
        let me = task::current_or_detached();
        let owns_gate = self.lock().tasks.get(&me).is_some_and(|slot| slot.gate);
        if !owns_gate {
            return Ok(());
        }
        match self.drain_pending_writes(me)? {
            // Both outcomes owe a retry: a stale group means its owner has to
            // re-execute against the state the drain published, and an
            // overtaken value means a root committed before an install moved
            // its port. The retry is what revalidates either result.
            DrainOutcome::Retry | DrainOutcome::Overtaken => {
                return self.replay_demand(roots, revision);
            }
            DrainOutcome::Done => {}
        }
        match self.resolve_conflicts(revision)? {
            // A restore is a post-commit install too, so it reports the same
            // overtaken decision the drain does.
            ConflictOutcome::Overtaken => self.replay_demand(roots, revision),
            ConflictOutcome::Done => Ok(()),
        }
    }

    /// Replays a demand whose post-commit work disturbed a result it had
    /// already finalized.
    ///
    /// Forcing the roots makes the retried attempt evaluate them against the
    /// state the install left instead of carrying the contradicted value out.
    fn replay_demand(&self, roots: &[NodeId], revision: Revision) -> Result<()> {
        force_recompute(&mut self.lock(), roots, revision);
        Err(ComputeError::Retry)
    }

    /// Resolves the definition conflicts the demand's commits staged.
    ///
    /// The conflicts were staged after the demand's last commit, so a restore
    /// here can invalidate a node this revision already finalized. That is the
    /// drain's situation exactly, and it is reported the same way: the caller
    /// must replay instead of carrying the result out.
    pub(crate) fn resolve_conflicts(&self, revision: Revision) -> Result<ConflictOutcome> {
        let selector = self.route_selector.lock().clone();
        let (conflicts, collisions) = {
            let mut state = self.lock();
            let conflicts = std::mem::take(&mut state.pending_conflicts);
            let collisions: Vec<(UnitKey, NodeId, NodeId)> = conflicts
                .iter()
                .map(|conflict| (conflict.unit.clone(), conflict.first, conflict.second))
                .collect();
            (conflicts, collisions)
        };
        if conflicts.is_empty() {
            return Ok(ConflictOutcome::Done);
        }
        // The selector is host code and runs before the lock is taken; a
        // decision is used only while the conflict it was made for is current.
        let decisions = decide_routes(selector.as_ref(), &collisions);
        let mut state = self.lock();
        let mut overtaken = false;
        for conflict in conflicts {
            // The earlier provider is still live when it still claims this
            // entity. A provider that retired during the revision released its
            // claim, which makes this a handover rather than a conflict. This
            // test works for root demands and for call-only demands alike, and
            // it asks about the entity: a provider that staged the member's
            // presence and the late one that staged its payload own the same
            // entity, so the kinds must compare by `owner_key`.
            let owner_of_unit = owner_key(&conflict.unit);
            let first_live = state
                .node(conflict.first)
                .definitions
                .iter()
                .any(|definition| owner_key(&definition.unit) == owner_of_unit);
            if !first_live {
                continue;
            }
            // A provider that was not certified for this revision has not run
            // in the current graph. A new branch site may take the unit over;
            // an authoritative provider that ran this revision still wins and
            // produces the duplicate diagnostic.
            let winner = route_winner(&decisions, &conflict.unit, conflict.first, conflict.second);
            if let Some(winner) = winner {
                if winner == conflict.second {
                    // The later provider keeps the unit; drop the earlier
                    // provider's stale definition record without touching the
                    // committed content.
                    //
                    let first = state.node_mut(conflict.first);
                    first.definitions.retain(|definition| {
                        owner_key(&definition.unit) != owner_of_unit
                            || definition.provider != conflict.first
                    });
                    // The unit already serves the later provider's value, so
                    // this arm restores nothing — but retiring the earlier
                    // provider re-runs both, and a value this revision had
                    // already published from one of them is a result the demand
                    // would carry out after discarding it. No commit follows to
                    // revalidate it, so it reports the overtake the same way the
                    // restore arm's own invalidation does.
                    overtaken |= force_recompute_tracked(
                        &mut state,
                        &[conflict.first, conflict.second],
                        revision,
                    );
                    continue;
                }
                // The earlier provider keeps the route; retract the late one
                // and restore the selected provider's committed post-state.
                overtaken |= Self::restore_conflict(&mut state, &conflict, revision);
                force_recompute(&mut state, &[conflict.first, conflict.second], revision);
                continue;
            }
            let unit = format!("{:?}", conflict.unit);
            let first =
                describe_provider(conflict.first_owner.label, conflict.first_owner.location);
            let second =
                describe_provider(state.node(conflict.second).label, conflict.second_location);
            // The revision is rejected without losing the committed
            // definition: the late provider's install is undone, so the View
            // keeps the earlier provider's value and every node that serves it.
            // The duplicate is the outcome the host needs, so it wins over the
            // overtaken decision a replay would otherwise report.
            let _ = Self::restore_conflict(&mut state, &conflict, revision);
            return Err(ComputeError::DuplicateDefinition {
                unit,
                first,
                second,
            });
        }
        Ok(if overtaken {
            ConflictOutcome::Overtaken
        } else {
            ConflictOutcome::Done
        })
    }

    /// Undoes a later provider's install of a contested unit.
    ///
    /// The earlier provider's committed value, content, owner entry, and nodes
    /// are restored through the same primitives as a commit. The restore is an
    /// install after the demand's last commit, so its invalidation is tracked:
    /// the returned flag is `invalidate_tracked`'s, and it says a node this
    /// revision had already finalized was marked.
    fn restore_conflict(
        state: &mut State,
        conflict: &crate::state::PendingConflict,
        revision: Revision,
    ) -> bool {
        // The earlier provider's record is the one that owns this entity, and
        // it may spell the unit with the other kind: a set publishes a member's
        // presence while a map publishes its payload. The restore therefore
        // replays that record's own unit and value rather than synthesising the
        // late provider's spelling as a payload of the earlier owner.
        let owner_of_unit = owner_key(&conflict.unit);
        let restored = state
            .node(conflict.first)
            .definitions
            .iter()
            .find(|definition| owner_key(&definition.unit) == owner_of_unit)
            .map(|definition| (definition.unit.clone(), definition.value.clone()));
        let seeds = Self::with_taken_region(state, conflict.unit.view, |state, region| {
            let mut changed = Vec::new();
            Self::retract_unit(
                state,
                region,
                conflict.second,
                &conflict.unit,
                revision,
                &mut changed,
                false,
            );
            state
                .node_mut(conflict.second)
                .definitions
                .retain(|definition| {
                    owner_key(&definition.unit) != owner_of_unit
                        || definition.provider != conflict.second
                });
            if let Some((unit, value)) = restored {
                let content =
                    (region.descriptor.content.define)(&region.content, &unit, Arc::clone(&value));
                Self::commit_content(state, region, content, revision, &mut changed);
                region
                    .owners
                    .insert(Arc::new(owner_key(&unit)), conflict.first_owner.clone());
                Self::restore_definition(state, region, &unit, value, revision, &mut changed);
            }
            // Restoring the unit restores the field's structure too: the
            // retraction above published the post-retraction topology, and a
            // restored collection must not keep serving it.
            Self::refresh_structure(state, region, conflict.unit.field, revision, &mut changed);
            changed
        })
        .unwrap_or_default();
        if seeds.is_empty() {
            return false;
        }
        invalidate_tracked(state, seeds, revision).1
    }

    /// Demands the installed root at the current revision.
    /// Demands one root output projection without evaluating its siblings.
    pub fn demand_root_projection(&self, cancel: &CancelToken, index: usize) -> Result<Value> {
        let revision = self.current_revision();
        let (root, general_root) = {
            let state = self.lock();
            (state.root.clone(), state.general_root)
        };
        if let Some(root) = root {
            let Some(node) = root.outputs.get(index).copied() else {
                return Err(ComputeError::internal("projection index out of range"));
            };
            let runtime: Arc<dyn RuntimeOps> = self.arc();
            // Frames are thread-local execution state; see the note in `eval`.
            #[allow(clippy::arc_with_non_send_sync)]
            let scope_frame = Arc::new(Frame::scoped(
                Arc::clone(&runtime),
                NodeId::NONE,
                ScopeId::ROOT,
                0,
                cancel.clone(),
            ));
            let _guard = FrameGuard::push(scope_frame);
            let projection_eq = root.projection_eq;
            let value = catch_engine(|| {
                let body_value = crate::component::eval_value(
                    &runtime,
                    root.body,
                    || (root.run)(),
                    &root.body_eq,
                );
                crate::component::eval_value(
                    &runtime,
                    node,
                    || {
                        runtime
                            .observe(root.body)
                            .unwrap_or_else(|error| irrigation_core::ops::panic_engine(error));
                        (root.project)(&body_value, index)
                    },
                    &|left, right| projection_eq(index, left, right),
                )
            })?;
            self.finish_demand(revision, &[root.body])?;
            return Ok(value);
        }
        let Some(general_node) = general_root else {
            return Err(ComputeError::internal(
                "no root component installed; call `runtime.install` first",
            ));
        };
        if index != 0 {
            return Err(ComputeError::internal("projection index out of range"));
        }
        let runtime: Arc<dyn RuntimeOps> = self.arc();
        #[allow(clippy::arc_with_non_send_sync)]
        let scope_frame = Arc::new(Frame::scoped(
            Arc::clone(&runtime),
            NodeId::NONE,
            ScopeId::ROOT,
            0,
            cancel.clone(),
        ));
        let _guard = FrameGuard::push(scope_frame);
        let mut compute = || -> Result<Value> {
            Err(ComputeError::internal(
                "a general component does not use a compute closure",
            ))
        };
        let eq = {
            let state = self.lock();
            let general = state
                .node(general_node)
                .general
                .map(|index| Arc::clone(&state.generals[index]));
            move |left: &Value, right: &Value| {
                general
                    .as_ref()
                    .is_some_and(|general| general.output_eq(left, right))
            }
        };
        let value = self.eval(general_node, &mut compute, &eq)?;
        self.finish_demand(revision, &[general_node])?;
        Ok(value)
    }

    /// Demands the installed root and evaluates every output projection.
    pub fn demand_root(&self, cancel: &CancelToken) -> Result<()> {
        let revision = self.current_revision();
        let root = {
            let state = self.lock();
            state.root.clone()
        };
        let Some(root) = root else {
            let general_root = {
                let state = self.lock();
                state.general_root
            };
            let Some(general_node) = general_root else {
                return Err(ComputeError::internal(
                    "no root component installed; call `runtime.install` first",
                ));
            };
            let runtime: Arc<dyn RuntimeOps> = self.arc();
            // Frames are thread-local execution state; see the note in `eval`.
            #[allow(clippy::arc_with_non_send_sync)]
            let scope_frame = Arc::new(Frame::scoped(
                Arc::clone(&runtime),
                NodeId::NONE,
                ScopeId::ROOT,
                0,
                cancel.clone(),
            ));
            let _guard = FrameGuard::push(scope_frame);
            let mut compute = || -> Result<Value> {
                Err(ComputeError::internal(
                    "a general component does not use a compute closure",
                ))
            };
            let eq = {
                let state = self.lock();
                let general = state
                    .node(general_node)
                    .general
                    .map(|index| Arc::clone(&state.generals[index]));
                move |left: &Value, right: &Value| {
                    general
                        .as_ref()
                        .is_some_and(|general| general.output_eq(left, right))
                }
            };
            self.eval(general_node, &mut compute, &eq)?;
            return self.finish_demand(revision, &[general_node]);
        };
        let runtime: Arc<dyn RuntimeOps> = self.arc();
        // Frames are thread-local execution state; see the note in `eval`.
        #[allow(clippy::arc_with_non_send_sync)]
        let scope_frame = Arc::new(Frame::scoped(
            Arc::clone(&runtime),
            NodeId::NONE,
            ScopeId::ROOT,
            0,
            cancel.clone(),
        ));
        let _guard = FrameGuard::push(scope_frame);
        catch_engine(|| {
            let body_value =
                crate::component::eval_value(&runtime, root.body, || (root.run)(), &root.body_eq);
            for (index, node) in root.outputs.iter().enumerate() {
                let projection_eq = root.projection_eq;
                crate::component::eval_value(
                    &runtime,
                    *node,
                    || {
                        runtime
                            .observe(root.body)
                            .unwrap_or_else(|error| irrigation_core::ops::panic_engine(error));
                        (root.project)(&body_value, index)
                    },
                    &|left, right| projection_eq(index, left, right),
                );
            }
        })?;
        // A child commit can retire an old branch and invalidate the root after
        // the root itself published. The demand is not allowed to carry that
        // stale value out; force the root to re-execute against the new state.
        let root_dirty = {
            let state = self.lock();
            !state.node(root.body).has_valid_trace()
        };
        if root_dirty {
            force_recompute(&mut self.lock(), &[root.body], revision);
            return Err(ComputeError::Retry);
        }
        self.finish_demand(revision, &[root.body])
    }

    /// Captures every registered View's content at the current revision.
    #[must_use]
    pub fn capture_snapshot(&self) -> crate::snapshot::Snapshot {
        let state = self.lock();
        Self::snapshot_of(&state)
    }

    /// Captures the committed content and runs `stage` against the same state.
    ///
    /// One locked pass reads both halves of a consumer's read pair — the View
    /// content and the unit versions that decide whether it moved — so a
    /// concurrent publish cannot land between them. The caller still owns the
    /// revision pin: only the gate keeps an edit from committing between the
    /// demand that produced the content and this capture. The snapshot is
    /// shared with `stage` because every delivery carries it.
    pub(crate) fn capture_snapshot_with<T>(
        &self,
        stage: impl FnOnce(&mut State, &Arc<crate::snapshot::Snapshot>) -> T,
    ) -> T {
        let mut state = self.lock();
        let snapshot = Arc::new(Self::snapshot_of(&state));
        stage(&mut state, &snapshot)
    }

    fn snapshot_of(state: &State) -> crate::snapshot::Snapshot {
        let revision = state.revision;
        let mut views: FxHashMap<ViewId, Arc<dyn Any + Send + Sync>> = FxHashMap::default();
        let mut versions: FxHashMap<ViewId, Version> = FxHashMap::default();
        let mut routing: FxHashMap<ViewId, u64> = FxHashMap::default();
        for (id, region) in &state.regions {
            views.insert(*id, Arc::clone(&region.content));
            versions.insert(*id, state.node(region.content_node).changed_at);
            routing.insert(*id, region.routing_version);
        }
        crate::snapshot::Snapshot::new(revision, views, versions, routing)
    }

    /// Number of worker threads.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.pool.workers()
    }

    /// Emits one observation event when an observer is installed.
    #[cfg(feature = "observe")]
    pub(crate) fn emit(&self, event: crate::observe::RuntimeEvent) {
        if let Some(observer) = self.observer.get() {
            observer.record(event);
        }
    }

    /// Emits the wake events collected while the state lock was held.
    ///
    /// This runs only after the batch reached the scheduler: an observer is
    /// host code and can panic, and that panic must not drop a wake or the gate
    /// permit the converting task still holds.
    pub(crate) fn emit_woken(&self, woken: Vec<(TaskId, NodeId)>) {
        #[cfg(feature = "observe")]
        for (task, node) in woken {
            self.emit(crate::observe::RuntimeEvent::TaskWoken { task, node });
        }
        #[cfg(not(feature = "observe"))]
        let _ = woken;
    }

    /// Emits an invalidation event for one reverse-dependency walk.
    #[cfg(feature = "observe")]
    pub(crate) fn emit_invalidated(&self, revision: Revision, seeds: &[NodeId], count: u64) {
        let seeds: smallvec::SmallVec<[NodeId; 4]> = seeds.iter().copied().take(4).collect();
        self.emit(crate::observe::RuntimeEvent::Invalidated {
            revision,
            seeds,
            count,
        });
    }

    /// Emits the commit event for one executed node.
    #[cfg(feature = "observe")]
    pub(crate) fn note_commit(
        &self,
        node: NodeId,
        version: Version,
        revision: Revision,
        started: std::time::Instant,
    ) {
        let (views, equal_views) = {
            let state = self.lock();
            let mut views: smallvec::SmallVec<[irrigation_core::ids::ViewId; 4]> =
                smallvec::SmallVec::new();
            let mut equal_views: smallvec::SmallVec<[irrigation_core::ids::ViewId; 4]> =
                smallvec::SmallVec::new();
            let Some(record) = state.nodes.get(node.index()) else {
                return;
            };
            for definition in &record.definitions {
                let target = if state
                    .units
                    .get(&definition.unit)
                    .is_some_and(|value_node| state.node(*value_node).changed_at == revision)
                {
                    &mut views
                } else {
                    &mut equal_views
                };
                if target.len() < 4 && !target.contains(&definition.unit.view) {
                    target.push(definition.unit.view);
                }
            }
            (views, equal_views)
        };
        self.emit(crate::observe::RuntimeEvent::NodeCommitted {
            node,
            views,
            equal_views,
            changed: version == revision,
            invalidated: 0,
            definitions: 0,
            duration_ns: started.elapsed().as_nanos() as u64,
        });
    }

    /// Records an observer-refresh failure without failing the transaction.
    pub(crate) fn set_notification_error(&self, error: ComputeError) {
        self.lock().notification_error = Some(error);
    }

    /// Clears the observer-refresh failure after a successful refresh.
    pub(crate) fn clear_notification_error(&self) {
        self.lock().notification_error = None;
    }

    /// Returns `true` when a root component (ordinary or general) is installed.
    /// Marks the most recently started demand as cancelled, if there is one.
    pub fn cancel_active(&self) {
        let task = *self.active_task.lock();
        let Some(task) = task else {
            return;
        };
        let queued = {
            let mut state = self.lock();
            let Some(slot) = state.tasks.get_mut(&task) else {
                return;
            };
            slot.cancel.cancel();
            let wake = matches!(slot.status, TaskStatus::Waiting(_));
            let pooled = slot.mode == TaskMode::Pooled;
            if wake {
                slot.status = TaskStatus::Queued;
            }
            if wake {
                state.wait_graph.remove(&task);
            }
            (wake && pooled).then_some(task)
        };
        self.wake.notify_all();
        if let Some(task) = queued {
            self.enqueue(vec![task]);
        }
    }

    /// Records the demand that a later [`Inner::cancel_active`] targets.
    pub(crate) fn set_active_task(&self, task: Option<TaskId>) {
        let mut active = self.active_task.lock();
        *active = task;
    }
}

fn frame_cancelled(parent: &Option<Arc<Frame>>) -> bool {
    parent
        .as_ref()
        .is_some_and(|frame| frame.cancel().is_cancelled())
}

/// Returns the key used for unique-definition enforcement.
pub(crate) fn owner_key(unit: &UnitKey) -> UnitKey {
    if unit.entity.is_some() {
        UnitKey {
            kind: UnitKind::Membership,
            ..unit.clone()
        }
    } else {
        unit.clone()
    }
}

/// Returns the membership unit key for an entity-scoped value unit.
fn membership_key(unit: &UnitKey) -> UnitKey {
    UnitKey {
        kind: UnitKind::Membership,
        ..unit.clone()
    }
}

/// Returns the payload unit key for an entity-scoped unit.
///
/// An entity is one record spelled three ways — its presence, its payload, and
/// the definition that owns both — so a primitive that acts on the entity takes
/// either cell and normalises through here or through [`owner_key`].
pub(crate) fn payload_key(unit: &UnitKey) -> UnitKey {
    if unit.entity.is_some() {
        UnitKey {
            kind: UnitKind::Value,
            ..unit.clone()
        }
    } else {
        unit.clone()
    }
}

/// Whether one draft states an entity's retraction rather than a cell write.
///
/// A membership draft is the presence half of an entity's definition, so
/// `false` says the entity is absent — the same statement as retracting it, and
/// the group's decision is normalised through [`payload_key`] like every other
/// retraction rather than installed as a presence cell.
fn draft_states_absence(draft: &DefinitionDraft) -> bool {
    draft.unit.entity.is_some()
        && draft.unit.kind == UnitKind::Membership
        && draft.value.downcast_ref::<bool>() == Some(&false)
}

/// The owner keys of the entities one write group's presence drafts retract.
fn withdrawn_entities(drafts: &[DefinitionDraft]) -> FxHashSet<UnitKey> {
    drafts
        .iter()
        .filter(|draft| draft_states_absence(draft))
        .map(|draft| owner_key(&draft.unit))
        .collect()
}

/// Whether one draft belongs to the group's presence decision instead of its
/// install: the absence statement itself, or the presence and payload cells it
/// marks absent. Other cells of the entity are the entity's, not its presence.
fn draft_withdrawn(draft: &DefinitionDraft, withdrawn: &FxHashSet<UnitKey>) -> bool {
    draft.unit.entity.is_some()
        && matches!(draft.unit.kind, UnitKind::Membership | UnitKind::Value)
        && withdrawn.contains(&owner_key(&draft.unit))
}

/// One provider choice made outside the metadata lock.
///
/// A [`crate::runtime::RouteSelector`] is host code: it may call any runtime
/// accessor, so it can never run while the state lock is held. Collisions are
/// collected under the lock, decided after it is released, and matched again
/// when the write group is applied; a decision whose collision is no longer
/// current is dropped and the conflict takes the unselected path.
struct RouteDecision {
    unit: UnitKey,
    first: NodeId,
    second: NodeId,
    winner: NodeId,
}

/// Collects the units a write group would contest with their current owners.
///
/// Each entry is `(unit, current owner, claiming provider)`.
fn route_collisions(
    state: &State,
    provider: NodeId,
    drafts: &[DefinitionDraft],
) -> Vec<(UnitKey, NodeId, NodeId)> {
    let mut collisions: Vec<(UnitKey, NodeId, NodeId)> = Vec::new();
    for draft in drafts {
        let Some(region) = state.regions.get(&draft.unit.view) else {
            continue;
        };
        let Some(existing) = region.owners.get(&owner_key(&draft.unit)) else {
            continue;
        };
        if existing.node != provider && !collisions.iter().any(|(unit, _, _)| *unit == draft.unit) {
            collisions.push((draft.unit.clone(), existing.node, provider));
        }
    }
    collisions
}

/// Resolves route collisions with the installed selector, without the lock.
fn decide_routes(
    selector: Option<&Arc<dyn crate::runtime::RouteSelector>>,
    collisions: &[(UnitKey, NodeId, NodeId)],
) -> Vec<RouteDecision> {
    let Some(selector) = selector else {
        return Vec::new();
    };
    collisions
        .iter()
        .map(|(unit, first, second)| RouteDecision {
            unit: unit.clone(),
            first: *first,
            second: *second,
            winner: selector.select(unit, *first, *second),
        })
        .collect()
}

/// Returns the resolved winner for one collision, if it still describes it.
fn route_winner(
    decisions: &[RouteDecision],
    unit: &UnitKey,
    first: NodeId,
    second: NodeId,
) -> Option<NodeId> {
    decisions
        .iter()
        .find(|decision| {
            decision.unit == *unit && decision.first == first && decision.second == second
        })
        .map(|decision| decision.winner)
}

/// Returns the component that owns a scope, walking member scopes to the body.
fn port_declared(state: &State, port: &UnitKey) -> bool {
    state.route_plans.keys().any(|planned| {
        planned.view == port.view
            && planned.field == port.field
            && match (&planned.entity, &port.entity) {
                (None, None) => true,
                (Some(left), Some(right)) => left.key_eq(right.as_ref()),
                _ => false,
            }
    })
}

fn planned_for_component(
    state: &State,
    scope: &WriteScope,
    component: irrigation_core::ids::ComponentId,
) -> bool {
    state.route_plans.iter().any(|(unit, planned)| {
        *planned == component
            && match scope {
                WriteScope::Entity(target) => {
                    unit.view == target.view
                        && unit.field == target.field
                        && match (&unit.entity, &target.entity) {
                            (None, None) => true,
                            (Some(left), Some(right)) => left.key_eq(right.as_ref()),
                            _ => false,
                        }
                }
                WriteScope::Field(view, field) => unit.view == *view && unit.field == *field,
                WriteScope::View(view) => unit.view == *view,
            }
    })
}

fn component_of(state: &State, scope: ScopeId) -> Option<irrigation_core::ids::ComponentId> {
    let mut current = scope;
    for _ in 0..64 {
        if let Some(component) = state.scope_components.get(&current) {
            return Some(*component);
        }
        current = state.scopes.parent(current)?;
    }
    None
}

/// Returns the component that owns a node's write group.
///
/// A node the runtime tagged with its component (an invocation body, a custom
/// component) answers directly; a per-expression site is attributed through the
/// scope that encloses it, the same walk a write claim uses.
fn node_component(state: &State, node: NodeId) -> Option<ComponentId> {
    let record = state.node(node);
    record
        .component
        .or_else(|| component_of(state, record.scope))
}

/// Whether a retraction may take a unit over from its current owner.
///
/// Retraction mirrors installation: another provider's committed unit is not
/// silently removable, exactly as a second definition of it is a conflict. This
/// provider's own unit and a sibling definition site of the same component (an
/// old branch site) may be taken; so may a unit whose owner no longer claims it,
/// because that owner retired or handed the unit over during this revision.
fn retraction_may_take_over(
    state: &State,
    provider: NodeId,
    owner: &ProviderRef,
    unit: &UnitKey,
) -> bool {
    if owner.node == provider {
        return true;
    }
    let provider_component = node_component(state, provider);
    if provider_component.is_some() && provider_component == node_component(state, owner.node) {
        return true;
    }
    // Ownership is recorded per entity, so the claim is asked in the same
    // space: an owner that staged a member's presence and one that staged its
    // payload own the same entity, and the later one must not look released
    // just because the other spelled the unit with a different kind.
    let owner_of_unit = owner_key(unit);
    !state
        .node(owner.node)
        .definitions
        .iter()
        .any(|definition| owner_key(&definition.unit) == owner_of_unit)
}

/// Builds a readable cycle path from a task waiting for `node`.
fn cycle_path(state: &State, me: TaskId, node: NodeId) -> Vec<String> {
    let mut path = Vec::new();
    let mut current = Some(me);
    let mut waited = Some(node);
    let mut seen = std::collections::HashSet::new();
    while let (Some(task), Some(node)) = (current, waited) {
        if !seen.insert(task) {
            break;
        }
        path.push(format!(
            "task {task} waits for {} ({node:?})",
            state.node(node).label
        ));
        current = match state.node(node).state {
            NodeState::Running { owner, .. } => Some(owner),
            _ => None,
        };
        waited = current.and_then(|task| state.wait_graph.get(&task).copied());
    }
    path
}

/// The write group a requirement is evaluated against.
///
/// A group can replace a unit it published in an earlier revision, and a
/// requirement must never be re-checked against that publication: the check
/// runs before the install, so the group's own earlier output is still in the
/// region. The exemption is scoped to the group's provider node and to units the
/// group restates or retracts, so another provider's unit still fails it.
#[derive(Clone, Copy)]
struct WriteGroup<'a> {
    provider: NodeId,
    drafts: &'a [DefinitionDraft],
    retractions: &'a [UnitKey],
    restate_all: bool,
    /// Views whose whole content this group replaces.
    ///
    /// A content write is a write group of its own kind: it installs the View's
    /// content cell and every materialized cell projected from it, and those
    /// are ports like any other.
    content_views: &'a [ViewId],
}

impl WriteGroup<'_> {
    /// Whether this group replaces its own committed owner pair for `unit`.
    fn replaces(&self, state: &State, unit: &UnitKey) -> bool {
        let owner = owner_key(unit);
        let owned = state
            .regions
            .get(&unit.view)
            .and_then(|region| region.owners.get(&owner))
            .is_some_and(|owner| owner.node == self.provider);
        owned
            && (self.restate_all
                || self.retractions.iter().any(|unit| owner_key(unit) == owner)
                || self
                    .drafts
                    .iter()
                    .any(|draft| owner_key(&draft.unit) == owner))
    }
}

/// The ports one write group installs at commit.
///
/// A group installs the units it drafts or retracts, the definitions it owned
/// in the previous revision and no longer restates, and one entity's ownership
/// pair as a whole: a claim on a value also owns its membership. Every install
/// into a field additionally recomputes that field's structure, which is the
/// cell a collection read projects.
struct InstalledPorts {
    /// One entity's ownership pair, which a payload draft also installs.
    units: FxHashSet<UnitKey>,
    /// Content cells a replacement installs.
    contents: FxHashSet<UnitKey>,
    /// Materialized cells a replacement re-derives.
    cells: FxHashSet<UnitKey>,
    /// Fields whose structure a group installs.
    fields: FxHashSet<(ViewId, FieldId)>,
}

impl InstalledPorts {
    /// Collects the ports one group installs.
    fn collect(state: &State, group: WriteGroup<'_>) -> Self {
        let mut ports = Self {
            units: FxHashSet::default(),
            contents: FxHashSet::default(),
            cells: FxHashSet::default(),
            fields: FxHashSet::default(),
        };
        for view in group.content_views {
            // The replacement installs the View's content cell, every field
            // structure the publish refreshes, and every materialized cell it
            // owns — exactly the set the publish moves, so a body that read one
            // of them is read-your-own-write by the same predicate.
            let Some(region) = state.regions.get(view) else {
                continue;
            };
            ports
                .contents
                .insert(UnitKey::field(*view, FieldId(0), UnitKind::Content));
            for (index, _) in region.descriptor.fields.iter().enumerate() {
                let field = FieldId(index as u16);
                let structure = UnitKey::field(*view, field, UnitKind::Structure);
                let materialized =
                    region.units.get(&structure).is_some() || state.units.get(&structure).is_some();
                // A structure is never drafted: the publish derives it from the
                // new content, so it answers to the field rather than to one
                // entity — which is why it has no cell entry to collect.
                if materialized && region.owners.get(&owner_key(&structure)).is_none() {
                    ports.fields.insert((*view, field));
                }
            }
            if let Some(cells) = state.cells.get(view) {
                for (key, node) in cells.iter() {
                    let Some(unit) = key.as_any().downcast_ref::<UnitKey>() else {
                        continue;
                    };
                    if Inner::replacement_owns_cell(state, region, unit, *node) {
                        ports.cells.insert(unit.clone());
                    }
                }
            }
        }
        let restaged: FxHashSet<UnitKey> = group
            .drafts
            .iter()
            .map(|draft| owner_key(&draft.unit))
            .collect();
        for draft in group.drafts {
            ports.insert(&draft.unit);
        }
        for unit in group.retractions {
            ports.insert(unit);
        }
        // A lowered body restates its whole output, so a definition it no
        // longer states is retracted by the same install; a general component
        // retracts only the units it names. A whole-field value replaces the
        // entity units of that field, exactly as the install does.
        let replaced: FxHashSet<(ViewId, FieldId)> = group
            .drafts
            .iter()
            .filter(|draft| draft.unit.entity.is_none() && draft.unit.kind == UnitKind::Value)
            .map(|draft| (draft.unit.view, draft.unit.field))
            .collect();
        for record in &state.node(group.provider).definitions {
            let unit = &record.unit;
            if restaged.contains(&owner_key(unit)) {
                continue;
            }
            let superseded = unit.entity.is_some() && replaced.contains(&(unit.view, unit.field));
            let retracted = group
                .retractions
                .iter()
                .any(|retracted| owner_key(retracted) == owner_key(unit));
            if group.restate_all || superseded || retracted {
                ports.insert(unit);
            }
        }
        ports
    }

    /// Whether this group installs `unit`.
    fn installs(&self, unit: &UnitKey) -> bool {
        // A presence cell is the one observation a conditional write may be
        // based on: the plan's `Skip` outcome reads the key and stages the write
        // only when it is present, so the group never installs a presence it
        // observed absent — and the write it does stage installs the payload,
        // while the pair itself does not move. Reading a payload the group
        // publishes stays read-your-own-write.
        if unit.kind == UnitKind::Membership {
            return false;
        }
        if self.units.contains(&owner_key(unit))
            || self.contents.contains(unit)
            || self.cells.contains(unit)
        {
            return true;
        }
        // A structure cell is never drafted: the install derives it from the
        // new content, so it answers to the field rather than to one entity.
        unit.entity.is_none()
            && unit.kind == UnitKind::Structure
            && self.fields.contains(&(unit.view, unit.field))
    }

    /// Records one installed unit and the field it belongs to.
    fn insert(&mut self, unit: &UnitKey) {
        self.units.insert(owner_key(unit));
        self.fields.insert((unit.view, unit.field));
    }
}

/// Returns the first port this attempt read that its own write group installs.
///
/// A read of a port the group writes is not a fixed point: the install moves
/// the cell the write was computed from, so retrying re-reads the published
/// value and either applies the write twice or never settles. The read and the
/// write may sit in different site nodes of one attempt — `let base =
/// map.to_vec(); map.insert(base.len(), ..)` reads in the `to_vec` site and
/// writes in the call's — so the walk follows the site traces this attempt
/// recorded. It never descends into another computation: a component output is
/// a value this attempt observed, not a read it performed.
fn self_dependent_port(
    state: &State,
    provider: NodeId,
    deps: &[Dep],
    group: WriteGroup<'_>,
) -> Option<UnitKey> {
    // A group installs nothing when it stages nothing and keeps every
    // definition it owned: only a restating group retracts the units it no
    // longer states. A content replacement always installs its View's cells.
    if group.drafts.is_empty()
        && group.retractions.is_empty()
        && group.content_views.is_empty()
        && (!group.restate_all || state.node(provider).definitions.is_empty())
    {
        return None;
    }
    let ports = InstalledPorts::collect(state, group);
    let mut seen: FxHashSet<NodeId> = FxHashSet::default();
    let mut stack: SmallVec<[NodeId; 8]> = deps.iter().map(|dep| dep.node).collect();
    while let Some(node) = stack.pop() {
        if node == provider || !seen.insert(node) {
            continue;
        }
        let record = state.node(node);
        if let Some(unit) = &record.unit {
            if ports.installs(unit) {
                return Some(unit.clone());
            }
            continue;
        }
        if matches!(record.kind, NodeKind::Site | NodeKind::Operator) {
            stack.extend(record.trace.iter().map(|dep| dep.node));
        }
    }
    None
}

/// Whether one unit carries a value at the revision the check runs in.
///
/// A cell answers when it has a payload. A membership cell a whole-region
/// replacement never filled has none, yet the committed content does state its
/// entity's presence — and a tracked read of the same unit projects exactly
/// that content (`MapRef::contains_key`). Presence therefore has one answer:
/// the cell while it carries one, the committed content while it does not.
/// Nothing is materialized to satisfy a requirement; the entity-less
/// membership unit is the field's keyed/whole protocol certificate, which no
/// content vtable spells.
fn unit_present(state: &State, unit: &UnitKey) -> bool {
    if let Some(node) = current_unit_node(state, unit) {
        let record = state.node(node);
        match unit.kind {
            UnitKind::Membership => {
                if let Some(present) = record
                    .value
                    .as_ref()
                    .and_then(|value| value.downcast_ref::<bool>())
                {
                    return *present;
                }
            }
            _ => return record.value.is_some(),
        }
    }
    if unit.entity.is_none() || unit.kind != UnitKind::Membership {
        return false;
    }
    state
        .regions
        .get(&unit.view)
        .and_then(|region| content_projection(region, unit))
        .and_then(|value| value.downcast_ref::<bool>().copied())
        .unwrap_or(false)
}

/// Returns `true` when a structural requirement holds at commit.
///
/// A requirement must name the ports it constrains: one that names none can
/// never be re-evaluated, so it never holds instead of being vacuously true.
fn requirement_holds(state: &State, requirement: &Requirement, group: WriteGroup<'_>) -> bool {
    if requirement.ports.is_empty() {
        return false;
    }
    requirement.ports.iter().all(|unit| {
        let present = unit_present(state, unit);
        // An absence requirement names a unit the group is about to install. A
        // group that re-runs against its own previous publication would
        // otherwise fail on the unit it published itself, which is not the
        // from-scratch answer for this revision.
        present == requirement.expected || (!requirement.expected && group.replaces(state, unit))
    })
}

/// Builds the structured failure for an unsatisfied requirement.
fn precondition_error(requirement: &Requirement) -> ComputeError {
    ComputeError::PreconditionUnmet {
        condition: requirement.description.to_string(),
        ports: requirement
            .ports
            .iter()
            .map(|unit| format!("{unit:?}"))
            .collect(),
        reason: irrigation_core::error::PreconditionReason::NoProducer,
    }
}

/// Clears a node's published value and certification at `revision`.
///
/// Clearing the value rather than only marking the node suspect is what makes
/// every replay path re-execute it: the cached-value fast path, ordered
/// validation, and a stale trace all key on a present, certified value.
fn uncertify(state: &mut State, node: NodeId, revision: Revision) {
    let validity = state.alloc_validity();
    let record = state.node_mut(node);
    record.value = None;
    record.verified_at = 0;
    record.verified_validity = 0;
    record.suspect_validity = validity;
    record.suspect_at = revision;
    record.value_version = 0;
}

/// Forces providers and their dependents to re-execute on the next demand.
///
/// Clearing the value rather than only marking suspect makes the replay paths
/// (including the revision-zero fast path) re-execute the whole cone.
fn force_recompute(state: &mut State, nodes: &[NodeId], revision: Revision) {
    force_recompute_tracked(state, nodes, revision);
}

/// [`force_recompute`], also reporting whether it cleared a value `revision`
/// had already published.
///
/// A post-commit pass that clears a certified result leaves the demand holding
/// a value of its own that no longer exists in the state, so it has to replay.
/// The seeds are excluded: they are the nodes the pass is about, and a provider
/// that ran this revision is the resolution's own input, not a result it
/// discarded.
fn force_recompute_tracked(state: &mut State, nodes: &[NodeId], revision: Revision) -> bool {
    let mut stack: SmallVec<[NodeId; 8]> = nodes.iter().copied().collect();
    let roots: FxHashSet<NodeId> = stack.iter().copied().collect();
    let mut seen: FxHashSet<NodeId> = FxHashSet::default();
    let mut cleared = false;
    while let Some(node) = stack.pop() {
        if !seen.insert(node) || state.node(node).is_running() {
            continue;
        }
        let (published, dependents) = {
            let record = state.node(node);
            (
                record.value.is_some() && record.verified_at == revision,
                record.dependents.clone(),
            )
        };
        if published && !roots.contains(&node) {
            cleared = true;
        }
        uncertify(state, node, revision);
        stack.extend(dependents);
    }
    cleared
}

/// Formats a provider description with its source location.
fn describe_provider(
    label: &str,
    location: Option<&'static std::panic::Location<'static>>,
) -> String {
    match location {
        Some(location) => format!(
            "{label} at {}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        ),
        None => label.to_string(),
    }
}

/// Returns the node cycles in a wait graph.
pub(crate) fn wait_cycles(state: &State) -> Vec<Vec<NodeId>> {
    let mut cycles = Vec::new();
    let mut visited: std::collections::HashSet<TaskId> = std::collections::HashSet::new();
    for &start in state.wait_graph.keys() {
        if visited.contains(&start) {
            continue;
        }
        let mut path_tasks: Vec<TaskId> = Vec::new();
        let mut path_nodes: Vec<NodeId> = Vec::new();
        let mut current = Some(start);
        while let Some(task) = current {
            if let Some(index) = path_tasks.iter().position(|existing| *existing == task) {
                cycles.push(path_nodes[index..].to_vec());
                break;
            }
            if visited.contains(&task) {
                break;
            }
            path_tasks.push(task);
            let Some(&node) = state.wait_graph.get(&task) else {
                break;
            };
            path_nodes.push(node);
            current = match state.node(node).state {
                NodeState::Running { owner, .. } => Some(owner),
                _ => None,
            };
        }
        visited.extend(path_tasks);
    }
    cycles
}

/// Tasks and future wakers released when a node completes.
pub(crate) struct Waiters {
    /// Pooled tasks to enqueue.
    pub tasks: Vec<TaskId>,
    /// Future wakers to call after the state lock is dropped.
    pub wakers: Vec<std::task::Waker>,
    /// Wake events to report after the batch is handed to the scheduler.
    pub woken: Vec<(TaskId, NodeId)>,
}

impl Waiters {
    /// An empty batch.
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            wakers: Vec::new(),
            woken: Vec::new(),
        }
    }

    /// Wakes every registered future waker.
    pub fn wake(&mut self) {
        for waker in std::mem::take(&mut self.wakers) {
            waker.wake();
        }
    }

    /// Merges another batch into this one.
    pub fn merge(&mut self, other: Waiters) {
        self.tasks.extend(other.tasks);
        self.wakers.extend(other.wakers);
        self.woken.extend(other.woken);
    }
}

/// The result of resolving the demand's deferred write groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrainOutcome {
    /// Every group either installed or there was nothing to do.
    Done,
    /// A group's owner must re-execute; the demand should retry.
    Retry,
    /// A group installed over a value this revision had already finalized; the
    /// demand must replay its roots and retry.
    Overtaken,
}

/// The result of resolving the demand's pending definition conflicts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConflictOutcome {
    /// Every conflict was resolved without disturbing a result this revision
    /// had already finalized.
    Done,
    /// A restore installed over a value this revision had already finalized;
    /// the demand must replay its roots and retry.
    Overtaken,
}

/// One step of the unit-read protocol.
///
/// The blocking read and the receipt path share this decision; they differ only
/// in how the reader participates in a wait.
enum ReadStep {
    /// The value is final; the caller records the observation and returns it.
    Ready(Observation),
    /// The unit has no value at this revision; the caller records the negative
    /// observation and reports [`ComputeError::Absent`].
    Absent(Observation),
    /// `node` must leave `Running` before the read can decide again.
    Wait(NodeId),
    /// The unit's View has a producer that is not current: demand it first.
    Demand(NodeId),
    /// The attempt read a state that changed; decide again.
    Retry,
    /// The read fails with this error.
    Fail(ComputeError),
}

/// One certified unit read, with the View's current provider.
struct Observation {
    value: Option<Value>,
    version: Version,
    routing: Version,
    validity: u64,
    provider: Option<NodeId>,
}

enum Action {
    Return(Value, Version, Version, u64),
    Fail(ComputeError),
    Wait,
    /// Claim the node with a fresh generation.
    Claim(u64),
    /// The task already owns this node and still has to finish it.
    Resume(u64),
}

/// What [`Inner::begin_eval`] decided for one demand.
enum Begin {
    /// The cached value is current; the observation was already recorded.
    Done(Value),
    /// A custom component runs its own algorithm.
    General { index: usize, claim: Claim },
    /// The caller-supplied closure must run.
    Run(Claim),
}

/// One claimed demand: the revision it was claimed for and the identity of the
/// execution that now owns it.
#[derive(Clone, Copy)]
struct Claim {
    revision: Revision,
    scope: ScopeId,
    generation: u64,
    owner: TaskId,
}

/// The execution frame a claimed node owns while its caller-supplied body runs.
struct ClaimedFrame {
    frame: Arc<Frame>,
    guard: FrameGuard,
    cancel: CancelToken,
    started: std::time::Instant,
}

/// Releases the claims of a slot-less evaluation that did not publish.
///
/// A parallel region evaluates members on scoped threads that belong to no
/// demand task. Their claims are recorded in the thread-local detached set
/// instead of a task slot, so the task teardown can never reach them: the thread
/// is joined and gone, and a node left `Running` under that identity would park
/// every later demand on it. The guard bounds one [`Inner::eval`], which covers
/// the claim, ordered validation, the commit revalidation, and the body.
struct DetachedClaims<'a> {
    inner: &'a Inner,
    owner: TaskId,
    mark: u64,
}

impl<'a> DetachedClaims<'a> {
    fn new(inner: &'a Inner) -> Self {
        Self {
            inner,
            owner: task::current_or_detached(),
            mark: task::detached_claim_mark(),
        }
    }
}

impl Drop for DetachedClaims<'_> {
    fn drop(&mut self) {
        let held = task::take_detached_claims_since(self.mark);
        if held.is_empty() {
            return;
        }
        let mut waiters = Waiters::new();
        {
            let mut state = self.inner.lock();
            for node in held {
                if matches!(
                    state.node(node).state,
                    NodeState::Running { owner, .. } if owner == self.owner
                ) {
                    state.node_mut(node).state = NodeState::Vacant;
                    state.release_claims(node);
                    waiters.merge(Inner::wake_waiters(&mut state, node));
                }
            }
        }
        self.inner.wake.notify_all();
        self.inner.enqueue(std::mem::take(&mut waiters.tasks));
        waiters.wake();
        self.inner.emit_woken(std::mem::take(&mut waiters.woken));
    }
}

/// Certifies a View's producer, after certifying the producers it observes.
///
/// Producers are certified depth-first so that, by the time a consumer's body
/// is evaluated, every unit it observed is final for this revision. Without
/// this ordering a consumer could be evaluated against a producer that has
/// not run yet, which makes an unchanged observation indistinguishable from a
/// stale one.
fn ensure_view_current(
    inner: &Inner,
    view: ViewId,
    seen: &mut irrigation_core::hash::FxHashSet<ViewId>,
    depth: usize,
) -> Result<()> {
    if depth > 16 || !seen.insert(view) {
        return Ok(());
    }
    // Producer demands made here are housekeeping, not observations of the
    // caller. A scratch frame keeps them out of the caller's trace; otherwise a
    // trace alternates between runs and the producer gets retired as unreachable.
    let runtime: Arc<dyn RuntimeOps> = inner.arc();
    let cancel = frame::current().map_or_else(CancelToken::default, |frame| frame.cancel().clone());
    #[allow(clippy::arc_with_non_send_sync)]
    let scratch = Arc::new(Frame::scratch(
        runtime,
        NodeId::NONE,
        irrigation_core::ids::ScopeId::ROOT,
        0,
        cancel,
    ));
    let _guard = FrameGuard::push(scratch);
    ensure_view_current_inner(inner, view, seen, depth)
}

/// The recursion behind [`ensure_view_current`]. `seen` and `depth` are the
/// guard the wrapper installs; this function only threads them down, which is why
/// it looks like it never reads them.
#[allow(clippy::only_used_in_recursion)]
fn ensure_view_current_inner(
    inner: &Inner,
    view: ViewId,
    seen: &mut irrigation_core::hash::FxHashSet<ViewId>,
    depth: usize,
) -> Result<()> {
    let (node, deps, stale, self_computing) = {
        let state = inner.lock();
        let Some(node) = state.producers.get(&view).copied() else {
            return Ok(());
        };
        let record = state.node(node);
        let stale = record.verified_at != state.revision
            || !record.has_valid_trace()
            || record.value.is_none();
        if !stale || record.is_running() {
            return Ok(());
        }
        let deps: Vec<ViewId> = record
            .trace
            .iter()
            .filter_map(|dep| state.node(dep.node).unit.as_ref().map(|unit| unit.view))
            .collect();
        (node, deps, stale, record.general.is_some())
    };
    if !stale {
        return Ok(());
    }
    // Only a node that carries its own algorithm can be demanded without a
    // caller-supplied body. A lowered node's body belongs to its call site, so
    // that path certifies it; its consumers stay conservative.
    if !self_computing {
        return Ok(());
    }
    for dep in deps {
        ensure_view_current_inner(inner, dep, seen, depth + 1)?;
    }
    let ready = {
        let state = inner.lock();
        let record = state.node(node);
        record.verified_at == state.revision && record.has_valid_trace() && record.value.is_some()
    };
    if ready {
        return Ok(());
    }
    let eq = {
        let state = inner.lock();
        let general = state
            .node(node)
            .general
            .map(|index| Arc::clone(&state.generals[index]));
        move |left: &Value, right: &Value| {
            general
                .as_ref()
                .is_some_and(|general| general.output_eq(left, right))
        }
    };
    let mut compute = || {
        Err(ComputeError::internal(
            "a general component producer does not use a compute closure",
        ))
    };
    inner.eval(node, &mut compute, &eq)?;
    Ok(())
}

impl RuntimeOps for Inner {
    fn revision(&self) -> Revision {
        self.current_revision()
    }

    fn eval(
        &self,
        node: NodeId,
        compute: &mut dyn FnMut() -> Result<Value>,
        eq: &dyn Fn(&Value, &Value) -> bool,
    ) -> Result<Value> {
        Inner::eval(self, node, compute, eq)
    }

    fn site_node(&self, scope: ScopeId, site: SiteId) -> NodeId {
        Inner::site_node(self, scope, site)
    }

    fn unit_node(&self, view: ViewId, key: Arc<dyn AnyKey>) -> NodeId {
        Inner::cell_node(self, view, key)
    }

    fn unit_node_init(
        &self,
        view: ViewId,
        key: Arc<dyn AnyKey>,
        init: &mut dyn FnMut(&dyn Any) -> Option<Value>,
    ) -> NodeId {
        Inner::unit_node_init(self, view, key, init)
    }

    fn read_unit(&self, node: NodeId) -> Result<Value> {
        Inner::read_unit(self, node)
    }

    fn claim_write(
        &self,
        scope: WriteScope,
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        Inner::claim_write(self, scope, label, location)
    }

    fn claim_writes(
        &self,
        scopes: &[WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        Inner::claim_writes(self, scopes, label, location)
    }

    fn plan_writes(
        &self,
        scopes: &[WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        Inner::plan_writes(self, scopes, label, location)
    }

    fn unplan_writes(&self, scopes: &[WriteScope]) -> Result<()> {
        Inner::unplan_writes(self, scopes)
    }

    fn require_condition(
        &self,
        condition: bool,
        ports: &[UnitKey],
        description: &'static str,
    ) -> Result<()> {
        Inner::require_condition(self, condition, ports, description)
    }

    fn provide_ports(&self, ports: &[UnitKey]) -> Result<bool> {
        Inner::provide_ports(self, ports)
    }

    fn wait_for_port(&self, ports: &[UnitKey]) -> Result<bool> {
        Inner::wait_for_port(self, ports)
    }

    fn ensure_unit(&self, unit: &UnitKey) -> Result<()> {
        let producer = {
            let state = self.lock();
            state.producers.get(&unit.view).copied()
        };
        if let Some(node) = producer {
            let ready = {
                let state = self.lock();
                let record = state.node(node);
                record.verified_at == state.revision
                    && record.has_valid_trace()
                    && record.value.is_some()
            };
            if !ready {
                self.demand_producer(node)?;
            }
        }
        let mut state = self.lock();
        if state.units.get(unit).is_some() {
            return Ok(());
        }
        // A cell has one creator. The runtime materializes the units the
        // committed content spells; a unit only its accessor can project — an
        // adjacency index, a whole-field payload, a field's protocol
        // certificate — stays without a cell until that accessor reads it,
        // because installing a vacant one would state an answer this call
        // cannot derive.
        let initial = state
            .regions
            .get(&unit.view)
            .and_then(|region| content_projection(region, unit));
        if initial.is_some() {
            Self::ensure_unit_node(&mut state, unit.clone(), initial);
        }
        Ok(())
    }

    fn child_scope(&self, parent: ScopeId, discriminant: u64, label: &'static str) -> ScopeId {
        Inner::child_scope(self, parent, discriminant, label)
    }

    fn field_is_keyed(&self, view: ViewId, field: FieldId) -> bool {
        Inner::field_is_keyed(self, view, field)
    }

    fn child_scope_keyed(
        &self,
        parent: ScopeId,
        key: Arc<dyn AnyKey>,
        label: &'static str,
    ) -> ScopeId {
        Inner::child_scope_keyed(self, parent, key, label)
    }

    fn check_cancelled(&self) -> Result<()> {
        match frame::current() {
            Some(frame) if frame.cancel().is_cancelled() => Err(ComputeError::Cancelled),
            _ => Ok(()),
        }
    }

    fn max_depth(&self) -> u32 {
        self.config.max_depth
    }

    fn view_content(&self, view: ViewId) -> Option<Arc<dyn Any + Send + Sync>> {
        Inner::view_content(self, view)
    }

    fn register_view(&self, descriptor: &'static ViewDescriptor) {
        Inner::register_view(self, descriptor);
    }

    fn workers(&self) -> usize {
        Inner::workers(self)
    }

    fn changed_at(&self, node: NodeId) -> Version {
        Inner::changed_at(self, node)
    }

    fn validity_at(&self, node: NodeId) -> u64 {
        Inner::validity_at(self, node)
    }

    fn value_version_at(&self, node: NodeId) -> u64 {
        Inner::value_version_at(self, node)
    }

    fn suspend_request(&self, node: NodeId) -> bool {
        let me = task::current_or_detached();
        let mut state = self.lock();
        let pooled = state.tasks.get(&me).map(|slot| slot.mode) == Some(TaskMode::Pooled);
        if pooled {
            state.wait_graph.insert(me, node);
            if let Some(slot) = state.tasks.get_mut(&me) {
                slot.status = TaskStatus::Waiting(node);
            }
            merge_held(&mut state, me, &task::active_snapshot());
            state.node_mut(node).waiters.push(me);
        }
        pooled
    }

    fn declare_view(&self, scope: ScopeId, view: ViewId) {
        self.lock()
            .declared_views
            .entry(scope)
            .or_default()
            .insert(view);
    }

    fn try_read_unit(
        &self,
        node: NodeId,
        waker: &std::task::Waker,
    ) -> Result<std::task::Poll<Value>> {
        let me = task::current_or_detached();
        loop {
            match self.read_unit_step(node) {
                ReadStep::Ready(observation) => {
                    self.record_read(node, &observation);
                    return Ok(std::task::Poll::Ready(
                        observation.value.expect("a ready read carries a value"),
                    ));
                }
                ReadStep::Absent(observation) => {
                    self.record_read(node, &observation);
                    return Err(ComputeError::Absent { node });
                }
                ReadStep::Fail(error) => return Err(error),
                ReadStep::Retry => continue,
                ReadStep::Demand(producer) => {
                    self.demand_producer(producer)?;
                }
                ReadStep::Wait(target) => {
                    // A demand task waits like any other read: a pooled task
                    // yields its worker and an inline task parks. Only a
                    // detached receipt has to stay pending on a waker.
                    if self.lock().tasks.contains_key(&me) {
                        self.wait_for(target, me)?;
                    } else {
                        self.lock().node_mut(target).wakers.push(waker.clone());
                        return Ok(std::task::Poll::Pending);
                    }
                }
            }
        }
    }

    fn observe(&self, node: NodeId) -> Result<()> {
        let state = self.lock();
        let record = state.node(node);
        if record.value.is_none() {
            return Err(ComputeError::internal(format!(
                "observation of {node:?} before it produced a value"
            )));
        }
        // A dirty node still has its last value. Recording that value with the
        // certification stamp makes the enclosing commit fail validation and
        // retry; the retry re-evaluates the node. Refusing here would turn an
        // ordinary same-revision invalidation into a hard error.
        frame::record_dep(Dep {
            node,
            version: record.value_version,
            routing: record.routing_at,
            validity: record.verified_validity,
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn invocation_node(
        &self,
        component: ComponentId,
        name: &'static str,
        args_hash: u64,
        args: Arc<dyn Any + Send + Sync>,
        scope: ScopeId,
        arity: usize,
        args_eq: &dyn Fn(&dyn Any, &dyn Any) -> bool,
    ) -> (NodeId, Vec<NodeId>) {
        let mut state = self.lock();
        let revision = state.revision;
        if let Some(entries) = state.invocations.get_mut(&(component, args_hash)) {
            for invocation in entries.iter_mut() {
                if args_eq(invocation.args.as_ref(), args.as_ref()) {
                    invocation.last_used = revision;
                    return (invocation.body, invocation.outputs.clone());
                }
            }
        }
        self.evict_invocations(&mut state);
        let invocation_id = state.next_invocation + 1;
        state.next_invocation = invocation_id;
        let invocation_scope = state.scopes.child(scope, invocation_id);
        state.scope_components.insert(invocation_scope, component);
        state.component_names.entry(component).or_insert(name);
        let mut body_record = NodeRecord::empty(name, NodeKind::InvocationBody);
        body_record.scope = invocation_scope;
        // Tag the call with its own component: the observation projection
        // attributes a link to the component that made it, not to the caller.
        body_record.component = Some(component);
        let body = state.alloc(body_record);
        state.scope_bodies.insert(invocation_scope, body);
        let mut outputs = Vec::with_capacity(arity);
        for index in 0..arity {
            let mut record = NodeRecord::empty("invocation output", NodeKind::InvocationOutput);
            record.scope = invocation_scope;
            record.site = SiteId(index as u32);
            record.component = Some(component);
            outputs.push(state.alloc(record));
        }
        state
            .invocations
            .entry((component, args_hash))
            .or_default()
            .push(Invocation {
                args,
                scope,
                body,
                outputs: outputs.clone(),
                last_used: revision,
            });
        (body, outputs)
    }
}

impl Inner {
    /// Evicts the least recently used unreferenced invocations above the cap.
    fn evict_invocations(&self, state: &mut State) {
        let Some(cap) = self.config.max_invocations else {
            return;
        };
        loop {
            let total: usize = state.invocations.values().map(Vec::len).sum();
            if total <= cap {
                return;
            }
            let mut candidate: Option<((ComponentId, u64), usize, u64)> = None;
            for (&key, entries) in &state.invocations {
                for (index, invocation) in entries.iter().enumerate() {
                    if !state.node(invocation.body).dependents.is_empty() {
                        continue;
                    }
                    let used = invocation.last_used;
                    if candidate.is_none_or(|(_, _, best)| used < best) {
                        candidate = Some((key, index, used));
                    }
                }
            }
            let Some((key, index, _)) = candidate else {
                return;
            };
            if let Some(entries) = state.invocations.get_mut(&key) {
                if index < entries.len() {
                    entries.remove(index);
                }
                if entries.is_empty() {
                    state.invocations.remove(&key);
                }
            }
            state
                .stats
                .invocations_evicted
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Counters from one reclamation pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReclaimStats {
    /// Nodes allocated before the pass.
    pub nodes_before: usize,
    /// Nodes still allocated after the pass.
    pub nodes_after: usize,
    /// Nodes whose record was cleared.
    pub nodes_reclaimed: usize,
    /// Absence-predicate unit cells dropped with their consumers.
    pub units_reclaimed: usize,
    /// Invocation records dropped with their bodies.
    pub invocations_reclaimed: usize,
    /// Interned scopes dropped with their nodes.
    pub scopes_reclaimed: usize,
}

/// A point-in-time memory report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryReport {
    /// Allocated node records.
    pub nodes: usize,
    /// Bytes held by committed values, as reported by their concrete types.
    pub value_bytes: usize,
    /// Recorded dependency edges.
    pub trace_edges: usize,
    /// Reverse dependency edges.
    pub dependent_edges: usize,
    /// Expression-site records.
    pub sites: usize,
    /// Logical units.
    pub units: usize,
    /// Component invocations.
    pub invocations: usize,
    /// Registered View regions.
    pub regions: usize,
    /// Interned scopes.
    pub scopes: usize,
    /// Demand task records.
    pub tasks: usize,
    /// Structure units (collection identity projections).
    pub structure_units: usize,
}

impl Inner {
    /// Whether a unit node is a false membership cell (a queried absence).
    fn is_absent_membership(record: &NodeRecord) -> bool {
        record
            .unit
            .as_ref()
            .is_some_and(|unit| unit.kind == UnitKind::Membership)
            && record
                .value
                .as_ref()
                .and_then(|value| value.downcast_ref::<bool>())
                .is_some_and(|present| !*present)
    }

    /// Reclaims nodes that no root, subscription, View unit, or trace can reach.
    ///
    /// Reclamation only loses reuse: a later demand reconstructs the graph.
    /// Nodes that still define derived units are never reclaimed, because their
    /// definitions would otherwise disappear from the region.
    /// Runs `collect` when the configured revision interval has elapsed.
    pub fn maybe_collect(&self) -> Option<ReclaimStats> {
        let interval = self.config.auto_collect_every?;
        if interval == 0 {
            return None;
        }
        let revision = {
            let state = self.lock();
            if state.revision.saturating_sub(state.last_collect) < interval {
                return None;
            }
            state.revision
        };
        let stats = self.reclaim()?;
        self.lock().last_collect = revision;
        self.lock()
            .stats
            .auto_collects
            .fetch_add(1, Ordering::Relaxed);
        Some(stats)
    }

    /// Walks `stack` to a fixpoint, marking every node its traces reach.
    fn seed_traces(
        state: &State,
        stack: &mut Vec<NodeId>,
        reachable: &mut std::collections::HashSet<NodeId>,
    ) {
        while let Some(node) = stack.pop() {
            if !reachable.insert(node) {
                continue;
            }
            for dep in &state.node(node).trace {
                stack.push(dep.node);
            }
        }
    }

    /// Reclaims unreachable nodes, their units, scopes, and invocation records.
    ///
    /// The pass is serialized with demands, so a call made inside one reclaims
    /// nothing.
    pub fn collect(&self) -> ReclaimStats {
        self.reclaim().unwrap_or_default()
    }

    /// Runs one reclamation pass when this thread can exclude demands.
    ///
    /// A demand holds a read permit for its whole task and reuses the nodes it
    /// interned, so the pass takes the write side and waits for every in-flight
    /// demand to finish. A caller that is already inside a demand cannot wait
    /// for the demands it belongs to without waiting for itself, so it skips the
    /// pass and reports none.
    fn reclaim(&self) -> Option<ReclaimStats> {
        if task::current().is_some() {
            return None;
        }
        let _permit = self.gate.write();
        let mut state = self.lock();
        let mut reachable: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        let mut stack: Vec<NodeId> = Vec::new();
        if let Some(root) = &state.root {
            stack.push(root.body);
            stack.extend(root.outputs.iter().copied());
        }
        // A general component is demanded through its own slot, not through a
        // trace: the root node and the producers it registers are live the
        // moment they are installed.
        stack.extend(state.general_root);
        stack.extend(state.producers.values().copied());
        for region in state.regions.values() {
            stack.push(region.content_node);
            for (_, slot) in region.units.iter() {
                stack.push(slot.node);
            }
        }
        // A subscription records the version of every unit it watches, so the
        // cell that answers for one is part of what it observes. Dropping that
        // cell would leave the recorded version naming a state the unit table no
        // longer answers, and the next staging pass would report a change this
        // pass invented — reclamation only loses reuse.
        for subscription in &state.subscriptions {
            for unit in &subscription.units {
                if let Some(node) = current_unit_node(&state, unit) {
                    stack.push(node);
                }
            }
        }
        for (_, node) in state.units.iter() {
            // An absence-predicate cell is kept alive only by a live trace, so
            // `collect` can drop it with the consumer that queried it.
            if Self::is_absent_membership(state.node(*node)) {
                continue;
            }
            stack.push(*node);
        }
        // A node that still owns definitions is retained whether or not anything
        // reaches it, and so is the trace it will be validated against: dropping
        // those observations would leave `validate_trace` reading a reclaimed
        // identity on the next demand.
        for (index, record) in state.nodes.iter().enumerate() {
            if record.freed || record.definitions.is_empty() {
                continue;
            }
            stack.push(NodeId::new(index as u32, record.generation));
        }
        Self::seed_traces(&state, &mut stack, &mut reachable);
        // An invocation the runtime still serves keeps its projections alive too:
        // they are handed out from the invocation record, so a reclaimed one
        // would be demanded again by an identity that names a freed slot.
        for entries in state.invocations.values() {
            for invocation in entries {
                if !reachable.contains(&invocation.body) {
                    continue;
                }
                stack.push(invocation.body);
                stack.extend(
                    invocation
                        .outputs
                        .iter()
                        .copied()
                        .filter(|node| node_is_live(&state, *node)),
                );
            }
        }
        Self::seed_traces(&state, &mut stack, &mut reachable);

        let nodes_before = state.nodes.len();
        let free: std::collections::HashSet<u32> = state.free_nodes.iter().copied().collect();
        let mut doomed: Vec<NodeId> = Vec::new();
        for (index, record) in state.nodes.iter().enumerate() {
            if free.contains(&(index as u32)) {
                continue;
            }
            let id = NodeId::new(index as u32, record.generation);
            if reachable.contains(&id) {
                continue;
            }
            let absent_membership =
                record.kind == NodeKind::Unit && Self::is_absent_membership(record);
            if matches!(record.kind, NodeKind::Unit | NodeKind::RegionContent) && !absent_membership
            {
                continue;
            }
            if !record.definitions.is_empty() {
                continue;
            }
            if record.is_running() {
                continue;
            }
            doomed.push(id);
        }
        let doomed_set: std::collections::HashSet<NodeId> = doomed.iter().copied().collect();
        for node in &doomed {
            let trace = state.node(*node).trace.clone();
            for dep in trace {
                state.remove_dependent(dep.node, *node);
            }
        }
        for node in &doomed {
            state.release_claims(*node);
            let record = state.node_mut(*node);
            record.value = None;
            record.trace.clear();
            record.state = NodeState::Vacant;
            record.verified_at = 0;
            record.verified_validity = 0;
            record.suspect_validity = 0;
            record.suspect_at = 0;
            record.changed_at = 0;
            record.value_version = 0;
            record.failure = None;
            record.dependents.clear();
            record.freed = true;
        }
        let live_scopes: FxHashSet<ScopeId> = reachable
            .iter()
            .map(|node| state.node(*node).scope)
            .collect();
        state
            .declared_views
            .retain(|scope, _| live_scopes.contains(scope));
        let scopes_reclaimed = state.scopes.prune(&live_scopes);
        // Reclaimed slots stay allocated but become reusable; a later allocation
        // advances the slot generation so stale ids are detectable.
        state
            .free_nodes
            .extend(doomed.iter().map(|node| node.index() as u32));
        let doomed_keys: Vec<Arc<dyn AnyKey>> = state
            .units
            .iter()
            .filter(|(_, node)| doomed_set.contains(node))
            .map(|(key, _)| Arc::clone(key))
            .collect();
        let units_reclaimed = doomed_keys.len();
        for key in doomed_keys {
            if let Some(unit) = key.as_any().downcast_ref::<UnitKey>() {
                state.forget_cell(unit);
            }
            state.units.remove(key.as_ref());
        }
        state.sites.retain(|_, node| !doomed_set.contains(node));
        let mut invocations_reclaimed = 0;
        for entries in state.invocations.values_mut() {
            let before = entries.len();
            entries.retain(|invocation| !doomed_set.contains(&invocation.body));
            invocations_reclaimed += before - entries.len();
        }
        state.invocations.retain(|_, entries| !entries.is_empty());
        Some(ReclaimStats {
            nodes_before,
            nodes_after: state.nodes.len(),
            nodes_reclaimed: doomed.len(),
            units_reclaimed,
            invocations_reclaimed,
            scopes_reclaimed,
        })
    }

    /// Reports memory held by runtime metadata and committed values.
    #[must_use]
    pub fn memory_report(&self) -> MemoryReport {
        let state = self.lock();
        let mut value_bytes = 0;
        let mut trace_edges = 0;
        let mut dependent_edges = 0;
        let mut structure_units = 0;
        for node in &state.nodes {
            if let Some(value) = &node.value {
                value_bytes += std::mem::size_of_val(value.as_ref());
            }
            trace_edges += node.trace.len();
            dependent_edges += node.dependents.len();
            if node
                .unit
                .as_ref()
                .is_some_and(|unit| unit.kind == UnitKind::Structure)
            {
                structure_units += 1;
            }
        }
        MemoryReport {
            nodes: state.nodes.len(),
            value_bytes,
            trace_edges,
            dependent_edges,
            sites: state.sites.len(),
            units: state.units.len(),
            invocations: state.invocations.values().map(Vec::len).sum(),
            regions: state.regions.len(),
            scopes: state.scopes.len(),
            tasks: state.tasks.len(),
            structure_units,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_cycles_reports_a_task_cycle() {
        let inner = Inner::new(RuntimeConfig {
            workers: 1,
            stack_size: 4 * 1024 * 1024,
            ..RuntimeConfig::default()
        });
        let (first, second, task_a, task_b) = {
            let mut state = inner.lock();
            let task_a = state.alloc_task(None, TaskMode::Pooled, false, CancelToken::default());
            let task_b = state.alloc_task(None, TaskMode::Pooled, false, CancelToken::default());
            let first = state.alloc(NodeRecord::empty("first", NodeKind::Unit));
            let second = state.alloc(NodeRecord::empty("second", NodeKind::Unit));
            state.node_mut(first).state = NodeState::Running {
                owner: task_b,
                generation: 1,
            };
            state.node_mut(second).state = NodeState::Running {
                owner: task_a,
                generation: 1,
            };
            state.wait_graph.insert(task_a, first);
            state.wait_graph.insert(task_b, second);
            (first, second, task_a, task_b)
        };
        let _ = (task_a, task_b);
        let cycles = inner.wait_cycles();
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].len(), 2);
        assert!(cycles[0].contains(&first));
        assert!(cycles[0].contains(&second));
    }

    #[test]
    fn a_pooled_request_suspends_the_whole_task() {
        let inner = Inner::new(RuntimeConfig {
            workers: 1,
            stack_size: 4 * 1024 * 1024,
            ..RuntimeConfig::default()
        });
        let (node, task) = {
            let mut state = inner.lock();
            let node = state.alloc(NodeRecord::empty("unit", NodeKind::Unit));
            state.node_mut(node).state = NodeState::Running {
                owner: 7,
                generation: 1,
            };
            let task = state.alloc_task(None, TaskMode::Pooled, false, CancelToken::default());
            (node, task)
        };
        let _enter = task::enter(task);
        assert!(inner.suspend_request(node));
        {
            let state = inner.lock();
            assert!(matches!(
                state.tasks[&task].status,
                TaskStatus::Waiting(waited) if waited == node
            ));
            assert!(state.node(node).waiters.contains(&task));
            assert_eq!(state.wait_graph.get(&task), Some(&node));
        }
        let mut waiters = Inner::wake_waiters(&mut inner.lock(), node);
        assert!(waiters.tasks.contains(&task));
        waiters.wake();
    }

    #[test]
    fn a_pending_request_registers_and_wakes_its_waker() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Flag(Arc<AtomicBool>);
        impl std::task::Wake for Flag {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let inner = Inner::new(RuntimeConfig {
            workers: 1,
            stack_size: 4 * 1024 * 1024,
            ..RuntimeConfig::default()
        });
        let node = {
            let mut state = inner.lock();
            let node = state.alloc(NodeRecord::empty("unit", NodeKind::Unit));
            state.node_mut(node).state = NodeState::Running {
                owner: 99,
                generation: 1,
            };
            node
        };
        let flag = Arc::new(AtomicBool::new(false));
        let waker = std::task::Waker::from(Arc::new(Flag(Arc::clone(&flag))));
        let poll = inner.try_read_unit(node, &waker).expect("poll");
        assert!(matches!(poll, std::task::Poll::Pending));
        let mut waiters = Inner::wake_waiters(&mut inner.lock(), node);
        waiters.wake();
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn trace_validation_rejects_a_routing_change_with_an_equal_value() {
        let inner = Inner::new(RuntimeConfig {
            workers: 1,
            stack_size: 4 * 1024 * 1024,
            ..RuntimeConfig::default()
        });
        let node = {
            let mut state = inner.lock();
            let node = state.alloc(NodeRecord::empty("unit", NodeKind::Unit));
            let record = state.node_mut(node);
            record.value = Some(value_of(42u32));
            record.changed_at = 1;
            record.value_version = 1;
            record.verified_at = 1;
            record.routing_at = 1;
            record.state = NodeState::Ready;
            node
        };
        let trace = vec![Dep {
            node,
            version: 1,
            routing: 1,
            validity: 0,
        }];
        assert!(inner.validate_trace(&trace, 1));
        inner.lock().node_mut(node).routing_at = 2;
        assert!(
            !inner.validate_trace(&trace, 1),
            "an equal value with a new provider must invalidate the observation"
        );
    }

    #[test]
    fn commit_rejects_a_stale_generation() {
        let inner = Inner::new(RuntimeConfig {
            workers: 1,
            stack_size: 4 * 1024 * 1024,
            ..RuntimeConfig::default()
        });
        let node = {
            let mut state = inner.lock();
            let node = state.alloc(NodeRecord::empty("claimed", NodeKind::Operator));
            state.node_mut(node).state = NodeState::Running {
                owner: 1,
                generation: 7,
            };
            node
        };
        let eq = |_: &Value, _: &Value| false;
        let cancel = CancelToken::default();
        let stale = inner.commit(
            node,
            value_of(1u32),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            (Vec::new(), false),
            true,
            true,
            None,
            FxHashMap::default(),
            FxHashSet::default(),
            &eq,
            0,
            1,
            6,
            &cancel,
        );
        assert!(matches!(stale, Err(ComputeError::Internal { .. })));
        let state = inner.lock();
        assert!(
            matches!(
                state.node(node).state,
                NodeState::Running {
                    owner: 1,
                    generation: 7
                }
            ),
            "a stale commit must not disturb the live claim"
        );
        assert!(state.node(node).value.is_none());
    }

    #[test]
    fn commit_checks_cancellation_under_the_lock() {
        let inner = Inner::new(RuntimeConfig {
            workers: 1,
            stack_size: 4 * 1024 * 1024,
            ..RuntimeConfig::default()
        });
        let node = {
            let mut state = inner.lock();
            let node = state.alloc(NodeRecord::empty("claimed", NodeKind::Operator));
            state.node_mut(node).state = NodeState::Running {
                owner: 1,
                generation: 1,
            };
            node
        };
        let eq = |_: &Value, _: &Value| false;
        let cancel = CancelToken::default();
        cancel.cancel();
        let result = inner.commit(
            node,
            value_of(1u32),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            (Vec::new(), false),
            true,
            true,
            None,
            FxHashMap::default(),
            FxHashSet::default(),
            &eq,
            0,
            1,
            1,
            &cancel,
        );
        assert!(matches!(result, Err(ComputeError::Cancelled)));
        let state = inner.lock();
        assert!(
            matches!(state.node(node).state, NodeState::Vacant),
            "a cancelled commit releases its claim"
        );
    }

    #[test]
    fn retirement_leaves_a_running_node_alone() {
        let inner = Inner::new(RuntimeConfig {
            workers: 1,
            stack_size: 4 * 1024 * 1024,
            ..RuntimeConfig::default()
        });
        let unit = UnitKey::field(ViewId(1), FieldId(0), UnitKind::Value);
        let mut state = inner.lock();
        let node = state.alloc(NodeRecord::empty("running", NodeKind::Operator));
        {
            let record = state.node_mut(node);
            record.state = NodeState::Running {
                owner: 7,
                generation: 1,
            };
            record.value = Some(value_of(7u32));
            record.verified_at = 1;
        }
        state.register_claim(node, 7, WriteScope::entity(&unit), "running", None);
        let mut changed = Vec::new();
        inner.retire_unreachable(&mut state, node, 1, &mut changed);
        let record = state.node(node);
        assert!(record.is_running(), "a live claim must not be retired");
        assert!(record.value.is_some(), "a live claim keeps its value");
        assert!(changed.is_empty());
        // The attempt can still publish, so retirement must leave its claim in
        // place: another writer of the port has to meet this claim, not race it.
        assert!(
            state
                .claim_covering(&unit)
                .is_some_and(|claim| claim.owner == node),
            "retirement released the claim of a task that is still executing"
        );
        assert!(record.claims.contains(&WriteScope::entity(&unit)));
    }
}
