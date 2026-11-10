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

mod cell;
mod claims;
mod content;
mod definitions;
mod eval;
mod ops;
mod views;
mod waits;

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
