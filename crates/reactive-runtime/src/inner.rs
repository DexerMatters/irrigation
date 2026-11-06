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

use parking_lot::{Condvar, Mutex, MutexGuard};
use reactive_core::hash::FxHashSet;
use smallvec::SmallVec;
use std::any::Any;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use reactive_core::descriptor::{UnitKey, UnitKind, ViewDescriptor};
use reactive_core::error::{ComputeError, Result};
use reactive_core::frame::{self, CancelToken, Dep, Frame, FrameGuard};
use reactive_core::hash::FxHashMap;
use reactive_core::ids::{
    ComponentId, FieldId, NodeId, Revision, ScopeId, SiteId, Version, ViewId,
};
use reactive_core::key::{AnyKey, any_key};
use reactive_core::ops::{RuntimeOps, catch_engine};
use reactive_core::value::{Value, value_of};

use crate::state::{
    DefinitionRecord, Invocation, NodeKind, NodeRecord, NodeState, ProviderRef, Region, State,
    UnitSlot, invalidate,
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

/// Resolves the node that currently holds a unit.
fn current_unit_node(state: &State, unit: &UnitKey) -> Option<NodeId> {
    state.units.get(unit).copied().or_else(|| {
        state
            .regions
            .get(&unit.view)
            .and_then(|region| region.units.get(unit))
            .map(|slot| slot.node)
    })
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
        if record.changed_at == dep.version && record.routing_at == dep.routing {
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
            current: record.changed_at,
            routing_changed: record.changed_at == dep.version,
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
    if state.node(provider).verified_at != revision {
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
    (state.node(producer).verified_at != revision).then_some(producer)
}

impl Inner {
    /// Creates the shared runtime.
    #[must_use]
    pub fn new(config: RuntimeConfig) -> Arc<Self> {
        reactive_core::ops::quiet_engine_panics();
        let pool = crate::scheduler::Pool::new(config.workers, config.stack_size);
        let inner = Arc::new(Self {
            config,
            state: Mutex::new(State::default()),
            route_selector: Mutex::new(None),
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
            let record = state.node_mut(content_node);
            record.value = Some(content.clone());
            record.verified_at = revision;
            record.state = NodeState::Ready;
        }
        state.units.insert(
            Arc::new(UnitKey::field(
                descriptor.id,
                reactive_core::ids::FieldId(0),
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
                units: reactive_core::key::KeyTable::new(),
                owners: reactive_core::key::KeyTable::new(),
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
        if let Some(node) = state.units.get(key.as_ref()) {
            return *node;
        }
        let content = state.regions.get(&view).map_or_else(
            || Arc::new(()) as Arc<dyn Any + Send + Sync>,
            |region| Arc::clone(&region.content) as Arc<dyn Any + Send + Sync>,
        );
        let initial = init(content.as_ref());
        let typed = match key.as_any().downcast_ref::<UnitKey>() {
            Some(unit) => unit.clone(),
            None => UnitKey::field(view, reactive_core::ids::FieldId(0), UnitKind::Value),
        };
        Self::ensure_unit_node(&mut state, typed, initial)
    }

    fn ensure_unit_node(state: &mut State, key: UnitKey, initial: Option<Value>) -> NodeId {
        if let Some(node) = state.units.get(&key) {
            return *node;
        }
        let revision = state.revision;
        let mut record = NodeRecord::unit("unit", key.clone());
        if let Some(value) = initial {
            record.value = Some(value);
            record.verified_at = revision;
            record.state = NodeState::Ready;
        }
        let node = state.alloc(record);
        state.units.insert(Arc::new(key), node);
        node
    }

    /// Reads a unit at the current revision and records the observation.
    pub fn read_unit(&self, node: NodeId) -> Result<Value> {
        let me = task::current_or_detached();
        loop {
            let (pending_producer, definition_current, unit_provider) = {
                let state = self.lock();
                let revision = state.revision;
                (
                    bad_producer(&state, node, revision),
                    unit_definition_is_current(&state, node, revision),
                    state
                        .node(node)
                        .unit
                        .as_ref()
                        .and_then(|unit| state.producers.get(&unit.view).copied()),
                )
            };
            let outcome = {
                let mut state = self.lock();
                let revision = state.revision;
                if let Some(producer) = pending_producer {
                    Outcome::Demand(producer)
                } else {
                    let record = state.node_mut(node);
                    if let Some(failure) = &record.failure {
                        if record.failed_at == revision {
                            Outcome::Fail(failure.clone())
                        } else {
                            record.failure = None;
                            record.state = NodeState::Vacant;
                            Outcome::Retry
                        }
                    } else if record.verified_at == revision {
                        match &record.value {
                            Some(value) => {
                                Outcome::Return(value.clone(), record.changed_at, record.routing_at)
                            }
                            None => Outcome::Fail(ComputeError::internal(format!(
                                "unit {} is undefined",
                                record
                                    .unit
                                    .as_ref()
                                    .map_or_else(|| String::from("?"), |unit| format!("{unit:?}"))
                            ))),
                        }
                    } else if record.is_running() {
                        Outcome::Wait
                    } else if (record.suspect_at <= record.verified_at || definition_current)
                        && record.value.is_some()
                    {
                        record.verified_at = revision;
                        Outcome::Return(
                            record.value.clone().expect("checked"),
                            record.changed_at,
                            record.routing_at,
                        )
                    } else {
                        Outcome::Fail(ComputeError::internal(format!(
                            "read of undefined unit {:?}",
                            record.unit
                        )))
                    }
                }
            };
            match outcome {
                Outcome::Return(value, version, routing) => {
                    frame::record_dep(Dep {
                        node,
                        version,
                        routing,
                    });
                    // Observe the provider too, so a provider handover is
                    // visible. Once per run: the trace lists producers, not
                    // reads, so it cannot grow with the View's size.
                    if let Some(provider_node) = unit_provider {
                        let (version, routing) = {
                            let state = self.lock();
                            let record = state.node(provider_node);
                            (record.changed_at, record.routing_at)
                        };
                        frame::record_provider_dep(Dep {
                            node: provider_node,
                            version,
                            routing,
                        });
                    }
                    return Ok(value);
                }
                Outcome::Fail(error) => return Err(error),
                Outcome::Retry => continue,
                Outcome::Wait => self.wait_for(node, me)?,
                Outcome::Demand(producer) => {
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
                    self.eval(producer, &mut compute, &eq)?;
                }
            }
        }
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
                    (record.verified_at != revision).then_some(unit.view)
                })
                .collect()
        };
        if views.is_empty() {
            return Ok(());
        }
        let mut seen = reactive_core::hash::FxHashSet::default();
        for view in views {
            ensure_view_current(self, view, &mut seen, 0)?;
        }
        Ok(())
    }

    /// Demands a node, executing `compute` when reuse is impossible.
    pub fn eval(
        &self,
        node: NodeId,
        compute: &mut dyn FnMut() -> Result<Value>,
        eq: &dyn Fn(&Value, &Value) -> bool,
    ) -> Result<Value> {
        if frame::current().is_some_and(|frame| frame.cancel().is_cancelled()) {
            return Err(ComputeError::Cancelled);
        }
        let me = task::current_or_detached();
        let generation = loop {
            let action = {
                let mut state = self.lock();
                let revision = state.revision;
                let record = state.node_mut(node);
                if let Some(failure) = &record.failure {
                    if record.failed_at == revision {
                        Action::Fail(failure.clone())
                    } else {
                        record.failure = None;
                        record.state = NodeState::Vacant;
                        state.next_generation += 1;
                        let generation = state.next_generation;
                        state.node_mut(node).state = NodeState::Running {
                            owner: me,
                            generation,
                        };
                        Action::Claim(generation)
                    }
                } else if record.verified_at == revision && record.value.is_some() {
                    Action::Return(record.value.clone().expect("checked"))
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
                } else if record.suspect_at <= record.verified_at && record.value.is_some() {
                    record.verified_at = revision;
                    record.validated_at = revision;
                    Action::Return(record.value.clone().expect("checked"))
                } else {
                    state.next_generation += 1;
                    let generation = state.next_generation;
                    state.node_mut(node).state = NodeState::Running {
                        owner: me,
                        generation,
                    };
                    Action::Claim(generation)
                }
            };

            match action {
                Action::Return(value) => {
                    let version = self.changed_at(node);
                    let routing = self.routing_at(node);
                    frame::record_dep(Dep {
                        node,
                        version,
                        routing,
                    });
                    return Ok(value);
                }
                Action::Fail(error) => return Err(error),
                Action::Wait => self.wait_for(node, me)?,
                Action::Claim(generation) => {
                    #[cfg(feature = "observe")]
                    self.emit(crate::observe::RuntimeEvent::NodeClaimed { node, task: me });
                    break generation;
                }
                Action::Resume(generation) => break generation,
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
            let logged = {
                let mut state = self.lock();
                let general = state.node(node).general.is_some();
                if general {
                    state.stats.no_change_runs.fetch_add(1, Ordering::Relaxed);
                }
                let record = state.node_mut(node);
                record.state = NodeState::Ready;
                record.verified_at = revision;
                record.validated_at = revision;
                record.suspect_at = 0;
                record
                    .value
                    .clone()
                    .expect("validated node without a value")
            };
            let value = logged;
            let version = self.changed_at(node);
            let routing = self.routing_at(node);
            frame::record_dep(Dep {
                node,
                version,
                routing,
            });
            #[cfg(feature = "observe")]
            self.emit(crate::observe::RuntimeEvent::NodeValidated {
                node,
                deps: trace.len() as u32,
            });
            return Ok(value);
        }

        // Custom components run their own algorithm instead of the caller's
        // closure; their state and output commit atomically.
        let general_index = self.lock().node(node).general;
        if let Some(index) = general_index {
            task::active_push(node);
            let result = self.run_general(node, index, revision, me, generation);
            task::active_pop(node);
            return result;
        }

        // Execute with a fresh recorder.
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
        // Frames are thread-local execution state: `RefCell` is intentional and
        // the frame never crosses a thread boundary.
        #[allow(clippy::arc_with_non_send_sync)]
        let frame = Arc::new(Frame::computing(
            self.arc(),
            node,
            scope,
            depth + 1,
            cancel.clone(),
        ));
        task::active_push(node);
        let guard = FrameGuard::push(Arc::clone(&frame));
        let execute_started = std::time::Instant::now();
        let result = catch_engine(compute);
        let definitions: Vec<DefinitionDraft> = frame.take_staged();
        let deps = frame.take_deps();
        drop(guard);
        task::active_pop(node);

        if frame::current().is_some_and(|frame| frame.cancel().is_cancelled()) {
            return self.fail(node, ComputeError::Cancelled, revision);
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
                    Vec::new(),
                    (Vec::new(), true),
                    true,
                    None,
                    FxHashMap::default(),
                    FxHashSet::default(),
                    eq,
                    revision,
                    me,
                    generation,
                    &cancel,
                )?;
                let version = self.changed_at(node);
                let routing = self.routing_at(node);
                frame::record_dep(Dep {
                    node,
                    version,
                    routing,
                });
                #[cfg(feature = "observe")]
                self.note_commit(node, version, revision, execute_started);
                #[cfg(not(feature = "observe"))]
                let _ = (execute_started, version);
                Ok(committed)
            }
            Ok(Err(error)) | Err(error) => {
                // A suspension is scheduling, not failure: the claim is kept
                // and the task is queued again when the awaited node completes.
                if error.is_suspended() {
                    return Err(error);
                }
                self.fail(node, error, revision)
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
        let (unit_scopes, retained_scopes) = cx.take_scopes();
        let definition_error = cx.take_definition_error();
        let content_updates = cx.take_content_updates();
        let (retractions, restate_all) = cx.take_definition_mode();
        let definitions: Vec<DefinitionDraft> = frame.take_staged();
        let deps = frame.take_deps();
        drop(guard);
        if frame_cancelled(&parent) {
            return self.fail(node, ComputeError::Cancelled, revision);
        }
        if let Some(reason) = definition_error {
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
                    content_updates,
                    (retractions, restate_all),
                    false,
                    Some(state),
                    unit_scopes,
                    retained_scopes,
                    &eq,
                    revision,
                    owner,
                    generation,
                    &cancel,
                )?;
                let version = self.changed_at(node);
                let routing = self.routing_at(node);
                frame::record_dep(Dep {
                    node,
                    version,
                    routing,
                });
                #[cfg(feature = "observe")]
                self.note_commit(node, version, revision, started);
                Ok(committed)
            }
            Ok(Err(error)) | Err(error) => {
                if error.is_suspended() {
                    return Err(error);
                }
                self.fail(node, error, revision)
            }
        }
    }

    /// Every component the runtime knows, with the Views it reads and defines.
    ///
    /// Registration is what puts a component here: the root, every installed
    /// general component, and every component that has been invoked. Ownership
    /// is the scope's: a record names its component directly when it belongs to
    /// an invocation, and every node a run creates — its expression sites, the
    /// definitions installed on them — is attributed through the scope that
    /// holds it.
    ///
    /// Reads resolve the same way. A trace names producers, and a producer is
    /// usually an expression site rather than a unit, so the walk descends
    /// through nodes it does not own and collects the units it reaches. When it
    /// reaches another component's body the walk stops: what this component
    /// reads from a callee is the View the callee defines, not everything the
    /// callee happened to look at.
    ///
    /// Timestamps come from the records that run — a component's body and its
    /// output projections — never from the units and sites in its scope, whose
    /// own timestamps belong to whoever published them.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn component_facts(&self) -> Vec<crate::observe::ComponentFacts> {
        use crate::observe::{ComponentFacts, ComponentKind};
        use reactive_core::ids::ComponentId;
        use std::collections::HashMap;

        let state = self.lock();
        let mut components: HashMap<ComponentId, ComponentFacts> = HashMap::new();
        let mut entry = |id: ComponentId, name: &'static str, kind: ComponentKind| {
            components.entry(id).or_insert(ComponentFacts {
                id,
                name,
                kind,
                reads: Vec::new(),
                writes: Vec::new(),
                executed_at: 0,
                suspect_at: 0,
            });
        };
        if let Some(root) = &state.root {
            entry(root.component, root.name, ComponentKind::Root);
        }
        for general in &state.generals {
            entry(general.id(), general.name(), ComponentKind::General);
        }
        for (id, name) in &state.component_names {
            entry(*id, name, ComponentKind::Invoked);
        }

        // Who owns each node: the record says so for an invocation's body and
        // projections, and the scope says so for everything the run created.
        let mut owner: Vec<Option<ComponentId>> = Vec::with_capacity(state.nodes.len());
        for record in state.nodes.iter() {
            let id = if record.freed {
                None
            } else {
                record.component.or_else(|| {
                    record
                        .general
                        .and_then(|index| state.generals.get(index))
                        .map(|general| general.id())
                        .or_else(|| component_of(&state, record.scope))
                })
            };
            owner.push(id);
        }

        let root_body = state.root.as_ref().map(|root| root.body);
        let root_outputs: &[NodeId] = state
            .root
            .as_ref()
            .map_or(&[], |root| root.outputs.as_slice());
        let runs = |node: NodeId, record: &crate::state::NodeRecord| -> bool {
            record.component.is_some()
                || Some(node) == root_body
                || Some(node) == state.general_root
                || root_outputs.contains(&node)
        };

        // Pass one: what each component defines, and when it last ran.
        for (index, record) in state.nodes.iter().enumerate() {
            if record.freed {
                continue;
            }
            let Some(id) = owner[index] else {
                continue;
            };
            let Some(facts) = components.get_mut(&id) else {
                continue;
            };
            // A unit holds a value and a region content node holds the host's
            // content; neither is a component's definition.
            if !matches!(record.kind, NodeKind::Unit | NodeKind::RegionContent) {
                for view in record
                    .definitions
                    .iter()
                    .map(|definition| definition.unit.view)
                    .chain(record.content_writes.iter().copied())
                {
                    if !facts.writes.contains(&view) {
                        facts.writes.push(view);
                    }
                }
            }
            let node = NodeId::new(index as u32, record.generation);
            if runs(node, record) {
                facts.executed_at = facts.executed_at.max(record.executed_at);
                facts.suspect_at = facts.suspect_at.max(record.suspect_at);
            }
        }

        // Pass two: what each component reads. The walk needs the writes above,
        // because a callee boundary stands for the Views the callee defines.
        let mut seen = vec![0u32; state.nodes.len()];
        let mut stamp = 0u32;
        let mut reached: Vec<ViewId> = Vec::new();
        for (index, record) in state.nodes.iter().enumerate() {
            if record.freed {
                continue;
            }
            let Some(id) = owner[index] else {
                continue;
            };
            if record.trace.is_empty() {
                continue;
            }
            stamp = stamp.wrapping_add(1);
            reached.clear();
            let mut stack: Vec<NodeId> = record.trace.iter().map(|dep| dep.node).collect();
            while let Some(dep) = stack.pop() {
                let slot = dep.index();
                if slot >= seen.len() || seen[slot] == stamp {
                    continue;
                }
                seen[slot] = stamp;
                let other = state.node(dep);
                if other.freed {
                    continue;
                }
                if let Some(unit) = &other.unit {
                    reached.push(unit.view);
                    continue;
                }
                // Crossing into a callee is a boundary, not a read. What this
                // component reads from another one is the value it returns — the
                // output projection — so the Views that callee defines are what
                // the edge stands for. Entering through a callee's *body* is a
                // transitive dependency the board does not draw: it would give
                // the root an edge from every stage below it.
                let callee = other
                    .component
                    .or_else(|| component_of(&state, other.scope));
                match callee {
                    Some(callee) if callee != id => {
                        if matches!(other.kind, NodeKind::InvocationOutput)
                            && let Some(facts) = components.get(&callee)
                        {
                            reached.extend(facts.writes.iter().copied());
                        }
                    }
                    _ => stack.extend(other.trace.iter().map(|dep| dep.node)),
                }
            }
            let Some(facts) = components.get_mut(&id) else {
                continue;
            };
            for view in reached.drain(..) {
                // A component that publishes a View does not read it back.
                if !facts.writes.contains(&view) && !facts.reads.contains(&view) {
                    facts.reads.push(view);
                }
            }
        }

        let mut facts: Vec<ComponentFacts> = components.into_values().collect();
        for component in &mut facts {
            component.reads.sort_unstable_by_key(|view| view.0);
            component.writes.sort_unstable_by_key(|view| view.0);
        }
        facts.sort_by_key(|component| (component.name, component.id.0));
        facts
    }

    /// Returns a node's routing generation.    /// Returns a node's routing generation.
    #[must_use]
    pub fn routing_at(&self, node: NodeId) -> Version {
        self.lock().node(node).routing_at
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
                    record.suspect_at <= record.verified_at && record.value.is_some(),
                    record.changed_at,
                    record.routing_at,
                )
            };
            if !verified {
                if !clean {
                    state
                        .stats
                        .validation_failures
                        .fetch_add(1, Ordering::Relaxed);
                    return false;
                }
                state.node_mut(dep.node).verified_at = revision;
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

    #[allow(clippy::too_many_arguments)]
    fn commit(
        &self,
        node: NodeId,
        value: Value,
        deps: Vec<Dep>,
        definitions: Vec<DefinitionDraft>,
        content_updates: Vec<(reactive_core::ids::ViewId, Arc<dyn Any + Send + Sync>)>,
        definition_mode: (Vec<UnitKey>, bool),
        enforce_views: bool,
        retained_state: Option<crate::general::ErasedState>,
        unit_scopes: FxHashMap<UnitKey, u64>,
        retained_scopes: FxHashSet<u64>,
        eq: &dyn Fn(&Value, &Value) -> bool,
        revision: Revision,
        owner: TaskId,
        generation: u64,
        cancel: &CancelToken,
    ) -> Result<Value> {
        let route_selector = self.route_selector.lock().clone();
        let mut state = self.lock();
        // A commit from a claim that is no longer ours must not publish. This is
        // the generation check the plan requires; it also makes the retirement
        // race harmless if a guard is ever missed.
        let owns = matches!(
            state.node(node).state,
            NodeState::Running {
                owner: current_owner,
                generation: current_generation,
            } if current_owner == owner && current_generation == generation
        );
        if !owns {
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
        let mut content_views: Vec<reactive_core::ids::ViewId> = Vec::new();
        for (view, content) in content_updates {
            if let Some(region) = state.regions.get_mut(&view) {
                region.content = content;
                content_views.push(view);
            }
        }
        state.node_mut(node).content_writes = content_views;
        let mut changed_nodes = Vec::new();
        if let Err(error) = Self::install_definitions(
            &mut state,
            node,
            definitions,
            definition_mode,
            revision,
            enforce_views,
            route_selector.as_ref(),
            &mut changed_nodes,
        ) {
            drop(state);
            return self.fail(node, error, revision);
        }

        let (changed, old_trace) = {
            let record = state.node(node);
            let changed = record.value.as_ref().is_none_or(|old| !eq(old, &value));
            (changed, record.trace.clone())
        };
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
            record.verified_at = revision;
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
            }
        }
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
        let seeds = changed_nodes;
        let mut waiters = self.wake_waiters(&mut state, node);
        drop(state);
        #[cfg(feature = "observe")]
        self.emit_invalidated(revision, &seeds, invalidated);
        #[cfg(not(feature = "observe"))]
        let _ = (invalidated, seeds);
        self.wake.notify_all();
        self.enqueue(std::mem::take(&mut waiters.tasks));
        waiters.wake();
        Ok(value)
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
            return;
        }
        // A node owned by a live task, or with waiters, is not dead: clearing
        // it would hand a second task the same claim. Leave it; a later commit
        // or reclamation retires it once the claim is released.
        if state.node(node).is_running() || !state.node(node).waiters.is_empty() {
            return;
        }
        let definitions = state.node(node).definitions.clone();
        for definition in definitions {
            let view = definition.unit.view;
            if let Some(mut region) = state.regions.remove(&view) {
                Self::retract_unit(
                    state,
                    &mut region,
                    node,
                    &definition.unit,
                    revision,
                    changed_nodes,
                );
                state.regions.insert(view, region);
            }
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
            record.suspect_at = 0;
            record.changed_at = 0;
            record.failure = None;
        }
        for dep in trace {
            self.retire_unreachable(state, dep.node, revision, changed_nodes);
        }
    }

    fn fail(&self, node: NodeId, error: ComputeError, revision: Revision) -> Result<Value> {
        let mut state = self.lock();
        state
            .stats
            .execution_failures
            .fetch_add(1, Ordering::Relaxed);
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
        let mut waiters = self.wake_waiters(&mut state, node);
        drop(state);
        #[cfg(feature = "observe")]
        self.emit(crate::observe::RuntimeEvent::NodeFailed {
            node,
            kind: crate::observe::error_kind(&error),
        });
        self.wake.notify_all();
        self.enqueue(std::mem::take(&mut waiters.tasks));
        waiters.wake();
        Err(error)
    }

    #[allow(clippy::too_many_arguments)]
    fn install_definitions(
        state: &mut State,
        provider: NodeId,
        drafts: Vec<DefinitionDraft>,
        definition_mode: (Vec<UnitKey>, bool),
        revision: Revision,
        enforce_views: bool,
        selector: Option<&Arc<dyn crate::runtime::RouteSelector>>,
        changed_nodes: &mut Vec<NodeId>,
    ) -> Result<()> {
        let (retractions, restate_all) = definition_mode;
        let retracted: FxHashSet<UnitKey> = retractions.into_iter().collect();
        let old: Vec<DefinitionRecord> = state.node(provider).definitions.clone();
        // Two-phase commit: every target region must exist before anything is
        // retracted or installed, so a rejected commit leaves the regions
        // exactly as they were.
        for draft in &drafts {
            if !state.regions.contains_key(&draft.unit.view) {
                return Err(ComputeError::DefinitionDenied {
                    unit: format!("{:?}", draft.unit),
                    reason: String::from("view is not registered"),
                });
            }
            if enforce_views {
                let declared = state
                    .declared_views
                    .get(&state.node(provider).scope)
                    .is_some_and(|views| views.contains(&draft.unit.view));
                if !declared {
                    return Err(ComputeError::DefinitionDenied {
                        unit: format!("{:?}", draft.unit),
                        reason: String::from(
                            "the provider did not declare this View (call `ensure_view` first)",
                        ),
                    });
                }
            }
            if let Some(planned) = state.route_plans.get(&draft.unit) {
                let provider_component = component_of(state, state.node(provider).scope);
                if provider_component != Some(*planned) {
                    return Err(ComputeError::DefinitionDenied {
                        unit: format!("{:?}", draft.unit),
                        reason: String::from(
                            "the unit is planned for a different provider component",
                        ),
                    });
                }
            }
        }
        let restaged: FxHashSet<UnitKey> = drafts.iter().map(|draft| draft.unit.clone()).collect();
        if restaged.len() != drafts.len() {
            let mut seen: std::collections::HashMap<
                UnitKey,
                Option<&'static std::panic::Location<'static>>,
            > = std::collections::HashMap::new();
            for draft in &drafts {
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

        // Retractions first: a unit this provider no longer produces must
        // disappear atomically with the new value. A unit is removed only when
        // it is retracted explicitly or the run restates everything; absence
        // alone keeps the committed value.
        for record in &old {
            if restaged.contains(&record.unit) {
                continue;
            }
            if !restate_all && !retracted.contains(&record.unit) {
                continue;
            }
            let view = record.unit.view;
            let Some(mut region) = state.regions.remove(&view) else {
                continue;
            };
            Self::retract_unit(
                state,
                &mut region,
                provider,
                &record.unit,
                revision,
                changed_nodes,
            );
            state.regions.insert(view, region);
        }

        let mut by_view: FxHashMap<ViewId, Vec<DefinitionDraft>> = FxHashMap::default();
        for draft in drafts {
            by_view.entry(draft.unit.view).or_default().push(draft);
        }
        let mut installed: Vec<DefinitionRecord> = old
            .into_iter()
            .filter(|record| {
                !restaged.contains(&record.unit)
                    && !retracted.contains(&record.unit)
                    && !restate_all
            })
            .collect();
        for (view, drafts) in by_view {
            let Some(mut region) = state.regions.remove(&view) else {
                return Err(ComputeError::DefinitionDenied {
                    unit: format!("view {view:?}"),
                    reason: String::from("view is not registered"),
                });
            };
            Self::apply_definitions(
                state,
                &mut region,
                provider,
                &drafts,
                revision,
                selector,
                changed_nodes,
                &mut installed,
            );
            state.regions.insert(view, region);
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
                    node_record.verified_at = revision;
                }
            }
        }
        state.node_mut(provider).definitions = installed;
        Ok(())
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
        {
            let record = state.node_mut(node);
            if changed {
                record.value = Some(structure.value);
                record.changed_at = revision;
                changed_nodes.push(node);
            }
            record.verified_at = revision;
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
        revision: Revision,
        selector: Option<&Arc<dyn crate::runtime::RouteSelector>>,
        changed_nodes: &mut Vec<NodeId>,
        installed: &mut Vec<DefinitionRecord>,
    ) {
        let descriptor = region.descriptor;
        let mut content = Arc::clone(&region.content);
        let mut dirty_fields: Vec<FieldId> = Vec::new();
        for draft in drafts {
            if draft.unit.entity.is_none() {
                match draft.unit.kind {
                    UnitKind::Structure => {
                        state
                            .keyed_fields
                            .insert((draft.unit.view, draft.unit.field));
                    }
                    UnitKind::Value => {
                        state
                            .keyed_fields
                            .remove(&(draft.unit.view, draft.unit.field));
                    }
                    _ => {}
                }
            }
            let owner = owner_key(&draft.unit);
            let previous_owner = region.owners.get(&owner).map(|existing| existing.node);
            if let Some(existing) = region.owners.get(&owner) {
                if existing.node != provider {
                    if let Some(selector) = selector {
                        // An installed route selector is the authority at
                        // install time: the selected provider keeps the unit,
                        // the other claim is rejected or transferred now.
                        let winner = selector.select(&draft.unit, existing.node, provider);
                        if winner == existing.node {
                            continue;
                        }
                        let previous = existing.node;
                        state.node_mut(previous).definitions.retain(|definition| {
                            definition.unit != draft.unit || definition.provider != previous
                        });
                        force_recompute(state, &[previous], revision);
                    } else {
                        // Resolve at the end of the revision: if the earlier
                        // owner is still reachable then both providers are live
                        // and the revision fails; if it retired during the
                        // revision, this was a handover.
                        state.pending_conflicts.push(crate::state::PendingConflict {
                            unit: draft.unit.clone(),
                            first: existing.node,
                            second: provider,
                            second_location: draft.location,
                        });
                    }
                }
            }
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
            let changed = {
                let record = state.node(value_node);
                record
                    .value
                    .as_ref()
                    .is_none_or(|old| !(draft.eq)(old, &draft.value))
            };
            {
                let record = state.node_mut(value_node);
                if changed {
                    record.value = Some(Arc::clone(&draft.value));
                    record.changed_at = revision;
                    changed_nodes.push(value_node);
                }
                if previous_owner != Some(provider) {
                    record.routing_at = revision;
                }
                record.verified_at = revision;
                record.suspect_at = 0;
                record.state = NodeState::Ready;
                record.failure = None;
            }
            if draft.unit.entity.is_some() {
                let membership = membership_key(&draft.unit);
                let member_node = match region.units.get(&membership) {
                    Some(slot) => slot.node,
                    None => {
                        let node = Self::ensure_unit_node(
                            state,
                            membership.clone(),
                            Some(value_of(false)),
                        );
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
                let was_present = region
                    .units
                    .get(&membership)
                    .is_some_and(|slot| slot.present);
                if !was_present {
                    let record = state.node_mut(member_node);
                    record.value = Some(value_of(true));
                    record.changed_at = revision;
                    record.routing_at = revision;
                    record.verified_at = revision;
                    record.state = NodeState::Ready;
                    changed_nodes.push(member_node);
                }
                if let Some(slot) = region.units.get_mut(&membership) {
                    slot.present = true;
                }
                if !dirty_fields.contains(&draft.unit.field) {
                    dirty_fields.push(draft.unit.field);
                }
            }
            content = (descriptor.content.define)(&content, &draft.unit, Arc::clone(&draft.value));
            if draft.unit.entity.is_none()
                && (descriptor.content.structure)(&content, draft.unit.field).is_some()
                && !dirty_fields.contains(&draft.unit.field)
            {
                dirty_fields.push(draft.unit.field);
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
        let content_changed = {
            let record = state.node(region.content_node);
            record
                .value
                .as_ref()
                .is_none_or(|old| !(descriptor.content.eq)(old, &content))
        };
        if content_changed {
            let record = state.node_mut(region.content_node);
            record.value = Some(Arc::clone(&content) as Value);
            record.changed_at = revision;
            record.verified_at = revision;
            record.state = NodeState::Ready;
            changed_nodes.push(region.content_node);
        }
        region.content = content;
        for field in dirty_fields {
            Self::refresh_structure(state, region, field, revision, changed_nodes);
        }
    }

    fn retract_unit(
        state: &mut State,
        region: &mut Region,
        provider: NodeId,
        unit: &UnitKey,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let descriptor = region.descriptor;
        let owner = owner_key(unit);

        if let Some(existing) = region.owners.get(&owner) {
            if existing.node != provider {
                return;
            }
        }
        if region.owners.get(&owner).is_some() {
            region.routing_version = region.routing_version.wrapping_add(1);
        }
        region.owners.remove(&owner);
        if unit.entity.is_none() && unit.kind == UnitKind::Structure {
            state.keyed_fields.remove(&(unit.view, unit.field));
        }
        let mut content = Arc::clone(&region.content);
        content = (descriptor.content.retract)(&content, unit);
        if unit.entity.is_none() {
            // A whole-field definition was retracted: the field reverts to its
            // empty value, and the unit node takes that value so readers and
            // snapshots agree.
            let structure = UnitKey::field(unit.view, unit.field, UnitKind::Structure);
            let value = (descriptor.content.get)(&content, &structure);
            if let Some(slot) = region.units.get(unit) {
                let node = slot.node;
                let dependents = state.node(node).dependents.clone();
                {
                    let record = state.node_mut(node);
                    record.value = value;
                    record.changed_at = revision;
                    record.routing_at = revision;
                    record.verified_at = revision;
                    record.suspect_at = 0;
                    record.state = NodeState::Ready;
                    record.failure = None;
                }
                changed_nodes.push(node);
                changed_nodes.extend(dependents);
            }
        }
        let mut removed_membership = false;
        if unit.entity.is_some() {
            let membership = membership_key(unit);
            if let Some(slot) = region.units.get(&membership) {
                let node = slot.node;
                let present = slot.present;
                if present {
                    removed_membership = true;
                    let record = state.node_mut(node);
                    record.value = Some(value_of(false));
                    record.changed_at = revision;
                    record.verified_at = revision;
                    record.state = NodeState::Ready;
                    changed_nodes.push(node);
                }
            }
            if let Some(slot) = region.units.get_mut(&membership) {
                slot.present = false;
            }
            // A membership unit is its own membership record: its value was
            // just set to false and certified above. Only a distinct value unit
            // loses its certification.
            if unit.kind != UnitKind::Membership {
                if let Some(slot) = region.units.get(unit) {
                    let node = slot.node;
                    let record = state.node_mut(node);
                    record.suspect_at = revision;
                    record.routing_at = revision;
                    // A directly read retracted unit must not be served from
                    // the old certified value; membership guards family
                    // handles, this guards direct unit reads.
                    record.verified_at = 0;
                    let dependents = record.dependents.clone();
                    changed_nodes.extend(dependents);
                }
            }
        }
        let content_changed = {
            let record = state.node(region.content_node);
            record
                .value
                .as_ref()
                .is_none_or(|old| !(descriptor.content.eq)(old, &content))
        };
        if content_changed {
            let record = state.node_mut(region.content_node);
            record.value = Some(Arc::clone(&content) as Value);
            record.changed_at = revision;
            record.verified_at = revision;
            record.state = NodeState::Ready;
            changed_nodes.push(region.content_node);
        }
        region.content = content;
        if removed_membership
            || (unit.entity.is_none()
                && (descriptor.content.structure)(&region.content, unit.field).is_some())
        {
            Self::refresh_structure(state, region, unit.field, revision, changed_nodes);
        }
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
            // Yield the worker: record the wait and let the attempt unwind.
            let held = task::active_snapshot();
            if let Some(slot) = state.tasks.get_mut(&me) {
                slot.status = TaskStatus::Waiting(node);
                slot.held = held;
            }
            state.stats.suspensions.fetch_add(1, Ordering::Relaxed);
            state.node_mut(node).waiters.push(me);
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
    /// caller enqueues the returned tasks once the lock is released.
    pub(crate) fn wake_waiters(&self, state: &mut State, node: NodeId) -> Waiters {
        let waiters = std::mem::take(&mut state.node_mut(node).waiters);
        let wakers = std::mem::take(&mut state.node_mut(node).wakers);
        let mut tasks = Vec::new();
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
            self.emit(crate::observe::RuntimeEvent::TaskWoken { task: waiter, node });
        }
        Waiters { tasks, wakers }
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

    pub(crate) fn resolve_conflicts(&self, revision: Revision) -> Result<()> {
        let selector = self.route_selector.lock().clone();
        let mut state = self.lock();
        let conflicts = std::mem::take(&mut state.pending_conflicts);
        if conflicts.is_empty() {
            return Ok(());
        }
        for conflict in conflicts {
            // The earlier provider is still live when it still claims this
            // unit. A provider that retired during the revision released its
            // claim, which makes this a handover rather than a conflict. This
            // test works for root demands and for call-only demands alike.
            let first_live = state
                .node(conflict.first)
                .definitions
                .iter()
                .any(|definition| definition.unit == conflict.unit);
            if !first_live {
                continue;
            }
            if let Some(selector) = &selector {
                let winner = selector.select(&conflict.unit, conflict.first, conflict.second);
                let view = conflict.unit.view;
                if winner == conflict.second {
                    // The later provider keeps the unit; drop the earlier
                    // provider's stale definition record without touching the
                    // committed content.
                    let first = state.node_mut(conflict.first);
                    first.definitions.retain(|definition| {
                        definition.unit != conflict.unit || definition.provider != conflict.first
                    });
                    force_recompute(&mut state, &[conflict.first, conflict.second], revision);
                    continue;
                }
                // The earlier provider keeps the route; retract the late one
                // and restore the selected provider's committed post-state.
                let restored = state
                    .node(conflict.first)
                    .definitions
                    .iter()
                    .find(|definition| definition.unit == conflict.unit)
                    .map(|definition| definition.value.clone());
                let owner = owner_key(&conflict.unit);
                let provider_ref = state
                    .regions
                    .get(&view)
                    .and_then(|region| region.owners.get(&owner))
                    .cloned();
                if let Some(mut region) = state.regions.remove(&view) {
                    let mut changed = Vec::new();
                    Self::retract_unit(
                        &mut state,
                        &mut region,
                        conflict.second,
                        &conflict.unit,
                        revision,
                        &mut changed,
                    );
                    if let (Some(value), Some(provider_ref)) = (restored, provider_ref) {
                        region.content = (region.descriptor.content.define)(
                            &region.content,
                            &conflict.unit,
                            Arc::clone(&value),
                        );
                        region.owners.insert(Arc::new(owner), provider_ref);
                        if let Some(slot) = region.units.get(&conflict.unit) {
                            let node = slot.node;
                            let record = state.node_mut(node);
                            record.value = Some(value);
                            record.changed_at = revision;
                            record.verified_at = revision;
                            record.suspect_at = 0;
                            record.state = NodeState::Ready;
                        }
                        changed.push(
                            region
                                .units
                                .get(&conflict.unit)
                                .map_or(conflict.first, |slot| slot.node),
                        );
                    }
                    state.regions.insert(view, region);
                }
                force_recompute(&mut state, &[conflict.first, conflict.second], revision);
                continue;
            }
            let unit = format!("{:?}", conflict.unit);
            let first_location = state
                .regions
                .get(&conflict.unit.view)
                .and_then(|region| region.owners.get(&owner_key(&conflict.unit)))
                .and_then(|provider| provider.location);
            let first = describe_provider(state.node(conflict.first).label, first_location);
            let second =
                describe_provider(state.node(conflict.second).label, conflict.second_location);
            let view = conflict.unit.view;
            if let Some(mut region) = state.regions.remove(&view) {
                let mut changed = Vec::new();
                Self::retract_unit(
                    &mut state,
                    &mut region,
                    conflict.second,
                    &conflict.unit,
                    revision,
                    &mut changed,
                );
                state.regions.insert(view, region);
            }
            return Err(ComputeError::DuplicateDefinition {
                unit,
                first,
                second,
            });
        }
        Ok(())
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
                            .unwrap_or_else(|error| reactive_core::ops::panic_engine(error));
                        (root.project)(&body_value, index)
                    },
                    &|left, right| projection_eq(index, left, right),
                )
            })?;
            self.resolve_conflicts(revision)?;
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
        self.resolve_conflicts(revision)?;
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
            return self.resolve_conflicts(revision);
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
                            .unwrap_or_else(|error| reactive_core::ops::panic_engine(error));
                        (root.project)(&body_value, index)
                    },
                    &|left, right| projection_eq(index, left, right),
                );
            }
        })?;
        self.resolve_conflicts(revision)
    }

    /// Captures every registered View's content at the current revision.
    #[must_use]
    pub fn capture_snapshot(&self) -> crate::snapshot::Snapshot {
        let state = self.lock();
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

    /// Returns a unit's change version and presence, when it is installed.
    ///
    /// Read-only: it never demands the unit.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn unit_facts(&self, unit: &UnitKey) -> Option<(Version, u32)> {
        let state = self.lock();
        let region = state.regions.get(&unit.view)?;
        let slot = region.units.get(unit)?;
        Some((state.node(slot.node).changed_at, 0))
    }

    /// Returns several units' change versions under one lock.
    ///
    /// A unit installed by a host edit lives in the global table, one installed
    /// by a provider lives in the region; both are installed units, so both
    /// answer. Consulting only the region made a host-rendered row on an edited
    /// unit look removed.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn unit_versions(&self, units: &[UnitKey]) -> Vec<Option<Version>> {
        let state = self.lock();
        units
            .iter()
            .map(|unit| {
                if let Some(node) = state.units.get(unit) {
                    return Some(state.node(*node).changed_at);
                }
                let region = state.regions.get(&unit.view)?;
                let slot = region.units.get(unit)?;
                Some(state.node(slot.node).changed_at)
            })
            .collect()
    }

    /// One field's entities in the View's own order, for a generic drawing.
    ///
    /// A list follows its sequence, a set and a map follow their key table
    /// (sorted by the key's display text, so the board is stable across
    /// revisions), and a scalar field answers with its single value unit. A tree
    /// field answers empty: its nodes live in slot units, see [`Self::tree_rows`].
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn field_entities(
        &self,
        view: reactive_core::ids::ViewId,
        field: reactive_core::ids::FieldId,
    ) -> Vec<crate::observe::EntityFacts> {
        use reactive_core::descriptor::UnitKind;
        let state = self.lock();
        let revision = state.revision;
        let Some(region) = state.regions.get(&view) else {
            return Vec::new();
        };
        let declared = region.descriptor.fields.get(field.0 as usize);
        if declared.is_none() {
            return Vec::new();
        }
        // The structure projection already knows the field's order.
        let order: Option<Vec<u64>> = (region.descriptor.content.structure)(&region.content, field)
            .and_then(|structure| {
                structure
                    .value
                    .downcast_ref::<reactive_store::PSeq<u64>>()
                    .map(|sequence| sequence.iter().copied().collect())
            });
        let mut rows: Vec<crate::observe::EntityFacts> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut push = |unit: reactive_core::descriptor::UnitKey, node: NodeId| {
            let key = unit
                .entity
                .as_ref()
                .map(|entity| entity.key_display())
                .unwrap_or_default();
            if unit.entity.is_some() && seen.contains(&format!("{}:{}", unit.kind as u8, key)) {
                return;
            }
            if unit.entity.is_some() {
                seen.push(format!("{}:{}", unit.kind as u8, key));
            }
            rows.push(crate::observe::EntityFacts {
                unit,
                version: state.node(node).changed_at,
                changed: state.node(node).changed_at == revision,
            });
        };
        let edited = state
            .units
            .iter()
            .filter_map(|(key, node)| key.as_any().downcast_ref::<UnitKey>().map(|u| (u, *node)));
        let owned = region.units.iter().filter_map(|(key, slot)| {
            key.as_any()
                .downcast_ref::<UnitKey>()
                .map(|u| (u, slot.node))
        });
        for (unit, node) in edited.chain(owned) {
            if unit.view != view || unit.field != field {
                continue;
            }
            // The identity unit is what a row stands for; the value unit is
            // fetched by the projection when it needs the text.
            let wanted = match declared.map(|field| field.kind) {
                Some(reactive_core::descriptor::FieldKind::Scalar) => unit.kind == UnitKind::Value,
                _ => unit.kind == UnitKind::Membership,
            };
            if wanted {
                // A scalar field has one unit and no entity; a collection's
                // identity is per entity.
                push(unit.clone(), node);
            }
        }
        if let Some(order) = order {
            // A list answers in sequence order: the ids are the entities.
            rows.sort_by_key(|row| {
                let id = row
                    .unit
                    .entity
                    .as_ref()
                    .and_then(|entity| entity.as_any().downcast_ref::<u64>().copied());
                id.and_then(|id| order.iter().position(|existing| *existing == id))
                    .unwrap_or(usize::MAX)
            });
        } else if declared
            .is_some_and(|field| field.kind != reactive_core::descriptor::FieldKind::Scalar)
        {
            rows.sort_by_key(|row| {
                row.unit
                    .entity
                    .as_ref()
                    .map(|entity| entity.key_display())
                    .unwrap_or_default()
            });
        }
        rows
    }

    /// Every installed unit of one View, with the revision it last changed at.
    ///
    /// A generic drawing reads identities and versions from here — including a
    /// tree's slot units, which are not attached to a declared field.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn view_units(
        &self,
        view: reactive_core::ids::ViewId,
    ) -> Vec<(reactive_core::descriptor::UnitKey, Version)> {
        use reactive_core::descriptor::UnitKind;
        let state = self.lock();
        let Some(region) = state.regions.get(&view) else {
            return Vec::new();
        };
        let mut seen: Vec<String> = Vec::new();
        let mut rows: Vec<(UnitKey, Version)> = Vec::new();
        let edited = state
            .units
            .iter()
            .filter_map(|(key, node)| key.as_any().downcast_ref::<UnitKey>().map(|u| (u, *node)));
        let owned = region.units.iter().filter_map(|(key, slot)| {
            key.as_any()
                .downcast_ref::<UnitKey>()
                .map(|u| (u, slot.node))
        });
        for (unit, node) in edited.chain(owned) {
            if unit.view != view
                || unit.kind != UnitKind::Value && unit.kind != UnitKind::Membership
            {
                continue;
            }
            let token = format!(
                "{}:{}:{}",
                unit.field.0,
                unit.kind as u8,
                unit.entity
                    .as_ref()
                    .map(|entity| entity.key_display())
                    .unwrap_or_default()
            );
            if seen.contains(&token) {
                continue;
            }
            seen.push(token);
            rows.push((unit.clone(), state.node(node).changed_at));
        }
        rows
    }

    /// A View's descriptor, for a projection that draws it generically.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn descriptor(&self, view: reactive_core::ids::ViewId) -> Option<&'static ViewDescriptor> {
        self.lock()
            .regions
            .get(&view)
            .map(|region| region.descriptor)
    }

    /// A View's content, untyped.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn raw_content(
        &self,
        view: reactive_core::ids::ViewId,
    ) -> Option<reactive_core::value::Value> {
        self.lock()
            .regions
            .get(&view)
            .map(|region| Arc::clone(&region.content))
    }

    /// A unit's type-erased value, for a display hook with no typed snapshot.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn unit_value(
        &self,
        unit: &reactive_core::descriptor::UnitKey,
    ) -> Option<reactive_core::value::Value> {
        let state = self.lock();
        let region = state.regions.get(&unit.view)?;
        let node = region.units.get(unit)?.node;
        state.node(node).value.clone()
    }

    /// Projects every registered View's metadata at one revision.
    ///
    /// Values are never cloned; element counts come from the content vtable.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn view_facts(&self) -> Vec<crate::observe::ViewFacts> {
        let state = self.lock();
        let revision = state.revision;
        // One pass over the edited-unit table, grouped by the View and field it
        // names. The per-field projection used to walk every unit in the runtime
        // once per field, which made a keystroke cost a scan per plate.
        let mut edited: FxHashMap<(ViewId, FieldId), Vec<NodeId>> = FxHashMap::default();
        for (key, node) in state.units.iter() {
            let Some(unit) = key.as_any().downcast_ref::<UnitKey>() else {
                continue;
            };
            edited
                .entry((unit.view, unit.field))
                .or_default()
                .push(*node);
        }
        let mut views: Vec<&crate::state::Region> = state.regions.values().collect();
        views.sort_by_key(|region| region.descriptor.id.0);
        views
            .into_iter()
            .map(|region| {
                let descriptor = region.descriptor;
                let content_version = state.node(region.content_node).changed_at;
                let mut changed_units = 0;
                let fields = descriptor
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(index, field)| {
                        let id = reactive_core::ids::FieldId(index as u16);
                        // Edited units live in the global table, provided ones
                        // in the region; both count toward the field.
                        let mut units = 0;
                        let mut changed = 0;
                        let mut seen: std::collections::HashSet<reactive_core::ids::NodeId> =
                            std::collections::HashSet::new();
                        let edited_nodes = edited
                            .get(&(descriptor.id, id))
                            .map_or(&[][..], |nodes| nodes.as_slice());
                        let provided = region.units.iter().filter_map(|(key, slot)| {
                            key.as_any()
                                .downcast_ref::<UnitKey>()
                                .filter(|unit| unit.field == id)
                                .map(|_| slot.node)
                        });
                        // `units` is the number of *entities* a field holds, not
                        // the number of unit rows (an entity has several).
                        let mut entities: std::collections::HashSet<String> =
                            std::collections::HashSet::new();
                        for node in edited_nodes.iter().copied().chain(provided) {
                            if !seen.insert(node) {
                                continue;
                            }
                            let record = &state.nodes[node.index()];
                            match record.unit.as_ref() {
                                Some(unit) if unit.entity.is_some() => {
                                    let key = unit
                                        .entity
                                        .as_ref()
                                        .map(|entity| entity.key_display())
                                        .unwrap_or_default();
                                    entities.insert(key);
                                }
                                Some(unit)
                                    if unit.kind == reactive_core::descriptor::UnitKind::Value =>
                                {
                                    units += 1;
                                }
                                _ => {}
                            }
                            if record.changed_at == revision {
                                changed += 1;
                            }
                        }
                        if !entities.is_empty() {
                            units = entities.len();
                        }
                        changed_units += changed;
                        crate::observe::FieldFacts {
                            field: id,
                            name: field.name,
                            kind: field.kind,
                            granularity: granularity_of(&state, descriptor, id),
                            units,
                            elements: (descriptor.content.len)(&region.content, id),
                            content_version,
                            changed_units: changed,
                        }
                    })
                    .collect();
                crate::observe::ViewFacts {
                    view: descriptor.id,
                    name: descriptor.name,
                    kind: descriptor.kind,
                    fields,
                    // The producing *component*, not the node: the page names
                    // the component beside the View. An installed definition
                    // names its provider; a registered producer (a general
                    // component, the root) is the fallback.
                    producer: region
                        .owners
                        .iter()
                        .next()
                        .map(|(_, provider)| provider.node)
                        .or_else(|| state.producers.get(&descriptor.id).copied())
                        .and_then(|node| {
                            let record = state.node(node);
                            record
                                .component
                                .or_else(|| component_of(&state, record.scope))
                        }),
                    routing: region.routing_version,
                    content_version,
                    demanded: descriptor.kind == reactive_core::descriptor::ViewKind::Source
                        || !region.owners.is_empty(),
                    content_node: region.content_node,
                    provider_node: region
                        .owners
                        .iter()
                        .next()
                        .map(|(_, provider)| provider.node),
                    suspect: {
                        // A definition's dependents are invalidated, not the
                        // unit node itself. The provider of any installed unit
                        // stands in for "this View's data may be stale".
                        let owner = region
                            .owners
                            .iter()
                            .next()
                            .map_or(region.content_node, |(_, provider)| provider.node);
                        let record = state.node(owner);
                        record.suspect_at > record.verified_at
                    },
                    changed_units,
                }
            })
            .collect()
    }

    /// Projects the node arena with a cap.
    ///
    /// Holds the metadata lock for one pass and never clones a value.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn probe(&self, query: &crate::observe::GraphQuery) -> crate::observe::GraphFacts {
        use crate::observe::{GraphFacts, NodeFacts, UnitFacts};
        let state = self.lock();
        let revision = state.revision;
        let mut nodes = Vec::new();
        let mut matched = 0usize;
        let mut truncated = false;
        for (index, record) in state.nodes.iter().enumerate() {
            if record.freed {
                continue;
            }
            let id = NodeId::new(index as u32, record.generation);
            if !scope_matches(&state, query.scope, id, record) {
                continue;
            }
            if matches!(record.kind, NodeKind::Site) && !query.include_sites {
                continue;
            }
            matched += 1;
            if nodes.len() >= query.node_limit {
                truncated = true;
                continue;
            }
            nodes.push(NodeFacts {
                id,
                kind: kind_name(record.kind),
                label: record.label,
                scope: record.scope,
                component: record.component,
                unit: record.unit.as_ref().map(|unit| UnitFacts {
                    view: unit.view,
                    field: unit.field,
                    kind: unit.kind,
                    entity: unit.entity.as_ref().map(|entity| entity.key_display()),
                }),
                changed_at: record.changed_at,
                verified_at: record.verified_at,
                suspect_at: record.suspect_at,
                executed_at: record.executed_at,
                validated_at: record.validated_at,
                routing_at: record.routing_at,
                waiters: record.waiters.len() as u32,
                value_size: record
                    .value
                    .as_ref()
                    .map(|value| std::mem::size_of_val(value.as_ref())),
            });
        }
        let included: std::collections::HashSet<NodeId> =
            nodes.iter().map(|node| node.id).collect();
        let mut edges = Vec::new();
        'outer: for node in &nodes {
            let record = &state.nodes[node.id.index()];
            for dep in &record.trace {
                if !included.contains(&dep.node) {
                    continue;
                }
                if edges.len() >= query.edge_limit {
                    break 'outer;
                }
                edges.push(crate::observe::EdgeFacts {
                    from: node.id,
                    to: dep.node,
                    recorded: dep.version,
                    current: state.nodes[dep.node.index()].changed_at,
                });
            }
        }
        GraphFacts {
            revision,
            nodes,
            edges,
            total_nodes: matched,
            truncated,
        }
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
            let mut views: smallvec::SmallVec<[reactive_core::ids::ViewId; 4]> =
                smallvec::SmallVec::new();
            let mut equal_views: smallvec::SmallVec<[reactive_core::ids::ViewId; 4]> =
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
fn owner_key(unit: &UnitKey) -> UnitKey {
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

/// Returns the component that owns a scope, walking member scopes to the body.
fn component_of(state: &State, scope: ScopeId) -> Option<reactive_core::ids::ComponentId> {
    let mut current = scope;
    for _ in 0..64 {
        if let Some(component) = state.scope_components.get(&current) {
            return Some(*component);
        }
        current = state.scopes.parent(current)?;
    }
    None
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

/// Forces providers and their dependents to re-execute on the next demand.
///
/// Clearing the value rather than only marking suspect makes the replay paths
/// (including the revision-zero fast path) re-execute the whole cone.
fn force_recompute(state: &mut State, nodes: &[NodeId], revision: Revision) {
    let mut stack: SmallVec<[NodeId; 8]> = nodes.iter().copied().collect();
    let mut seen: FxHashSet<NodeId> = FxHashSet::default();
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        let dependents = {
            let record = state.node_mut(node);
            if record.is_running() {
                continue;
            }
            record.value = None;
            record.verified_at = 0;
            record.suspect_at = revision;
            record.dependents.clone()
        };
        stack.extend(dependents);
    }
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
}

impl Waiters {
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
    }
}

enum Outcome {
    Return(Value, Version, Version),
    Fail(ComputeError),
    Retry,
    Wait,
    /// The unit's View has a producer that is not current: demand it first.
    Demand(NodeId),
}

enum Action {
    Return(Value),
    Fail(ComputeError),
    Wait,
    /// Claim the node with a fresh generation.
    Claim(u64),
    /// The task already owns this node and still has to finish it.
    Resume(u64),
}

/// Certifies a View's producer, after certifying the producers it observes.
///
/// Producers are certified depth-first so that, by the time a consumer's body
/// is evaluated, every unit it observed is final for this revision. Without
/// this ordering a consumer could be evaluated against a producer that has
/// not run yet, which makes an unchanged observation indistinguishable from a
/// stale one.
/// Certifies a View's producer, after certifying the producers it observes.
///
/// Producers are certified depth-first so that, by the time a consumer's body is
/// evaluated, every unit it observed is final for this revision.
fn ensure_view_current(
    inner: &Inner,
    view: ViewId,
    seen: &mut reactive_core::hash::FxHashSet<ViewId>,
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
    let scratch = Arc::new(Frame::scoped(
        runtime,
        NodeId::NONE,
        reactive_core::ids::ScopeId::ROOT,
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
    seen: &mut reactive_core::hash::FxHashSet<ViewId>,
    depth: usize,
) -> Result<()> {
    let (node, deps, stale, self_computing) = {
        let state = inner.lock();
        let Some(node) = state.producers.get(&view).copied() else {
            return Ok(());
        };
        let record = state.node(node);
        let stale = record.verified_at != state.revision || record.value.is_none();
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
        record.verified_at == state.revision && record.value.is_some()
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
        self.unit_node_init(view, key, &mut |_| None)
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

    fn ensure_unit(&self, unit: &UnitKey) -> Result<()> {
        let producer = {
            let state = self.lock();
            state.producers.get(&unit.view).copied()
        };
        if let Some(node) = producer {
            let ready = {
                let state = self.lock();
                let record = state.node(node);
                record.verified_at == state.revision && record.value.is_some()
            };
            if !ready {
                let mut compute = || {
                    Err(ComputeError::internal(
                        "a general component producer does not use a compute closure",
                    ))
                };
                let eq = {
                    let state = self.lock();
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
                self.eval(node, &mut compute, &eq)?;
            }
        }
        let mut state = self.lock();
        if state.units.get(unit).is_some() {
            return Ok(());
        }
        let initial = state.regions.get(&unit.view).and_then(|region| {
            if unit.kind == UnitKind::Structure && unit.entity.is_none() {
                // An undefined structure unit initializes from the family's
                // structure projection, not the generic unit getter.
                (region.descriptor.content.structure)(&region.content, unit.field)
                    .map(|structure| structure.value)
            } else {
                (region.descriptor.content.get)(&region.content, unit)
            }
        });
        Self::ensure_unit_node(&mut state, unit.clone(), initial);
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

    fn suspend_request(&self, node: NodeId) -> bool {
        let me = task::current_or_detached();
        let mut state = self.lock();
        let pooled = state.tasks.get(&me).map(|slot| slot.mode) == Some(TaskMode::Pooled);
        if pooled {
            state.wait_graph.insert(me, node);
            if let Some(slot) = state.tasks.get_mut(&me) {
                slot.status = TaskStatus::Waiting(node);
                slot.held = task::active_snapshot();
            }
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
        let outcome = {
            let mut state = self.lock();
            let revision = state.revision;
            let record = state.node_mut(node);
            if let Some(failure) = &record.failure {
                if record.failed_at == revision {
                    return Err(failure.clone());
                }
                record.failure = None;
                record.state = NodeState::Vacant;
            }
            if record.is_running() {
                record.wakers.push(waker.clone());
                return Ok(std::task::Poll::Pending);
            }
            if record.verified_at == revision {
                match &record.value {
                    Some(value) => (value.clone(), record.changed_at, record.routing_at),
                    None => {
                        return Err(ComputeError::internal(format!(
                            "unit {} is undefined",
                            record
                                .unit
                                .as_ref()
                                .map_or_else(|| String::from("?"), |unit| format!("{unit:?}"))
                        )));
                    }
                }
            } else if record.suspect_at <= record.verified_at && record.value.is_some() {
                record.verified_at = revision;
                (
                    record.value.clone().expect("checked"),
                    record.changed_at,
                    record.routing_at,
                )
            } else {
                return Err(ComputeError::internal(format!(
                    "read of undefined unit {:?}",
                    record.unit
                )));
            }
        };
        let (value, version, routing) = outcome;
        frame::record_dep(Dep {
            node,
            version,
            routing,
        });
        Ok(std::task::Poll::Ready(value))
    }

    fn observe(&self, node: NodeId) -> Result<()> {
        let state = self.lock();
        let revision = state.revision;
        let record = state.node(node);
        if record.verified_at != revision {
            return Err(ComputeError::internal(format!(
                "observation of {node:?} before it was certified for revision {revision}"
            )));
        }
        frame::record_dep(Dep {
            node,
            version: record.changed_at,
            routing: record.routing_at,
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
        let stats = self.collect();
        self.lock().last_collect = revision;
        self.lock()
            .stats
            .auto_collects
            .fetch_add(1, Ordering::Relaxed);
        Some(stats)
    }

    /// Reclaims unreachable nodes, their units, scopes, and invocation records.
    pub fn collect(&self) -> ReclaimStats {
        // Reclamation must not race a demand that is midway through committing
        // a trace; wait for in-flight demands instead.
        let _permit = self.gate.read();
        let mut state = self.lock();
        let mut reachable: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        let mut stack: Vec<NodeId> = Vec::new();
        if let Some(root) = &state.root {
            stack.push(root.body);
            stack.extend(root.outputs.iter().copied());
        }
        for region in state.regions.values() {
            stack.push(region.content_node);
            for (_, slot) in region.units.iter() {
                stack.push(slot.node);
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
        while let Some(node) = stack.pop() {
            if !reachable.insert(node) {
                continue;
            }
            for dep in &state.node(node).trace {
                stack.push(dep.node);
            }
        }

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
            let record = state.node_mut(*node);
            record.value = None;
            record.trace.clear();
            record.state = NodeState::Vacant;
            record.verified_at = 0;
            record.suspect_at = 0;
            record.changed_at = 0;
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
        ReclaimStats {
            nodes_before,
            nodes_after: state.nodes.len(),
            nodes_reclaimed: doomed.len(),
            units_reclaimed,
            invocations_reclaimed,
            scopes_reclaimed,
        }
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

/// Element granularity for one field, derived from the region it lives in.
///
/// The field kind alone is not enough: a source tree stages one whole content
/// value, while a derived tree can stage a unit per node. Only the installed
/// units can answer honestly.
#[cfg(feature = "observe")]
fn granularity_of(
    state: &State,
    descriptor: &'static ViewDescriptor,
    field: reactive_core::ids::FieldId,
) -> crate::observe::Granularity {
    use crate::observe::Granularity;
    let region = match state.regions.get(&descriptor.id) {
        Some(region) => region,
        None => return Granularity::Unknown,
    };
    let mut entities = 0usize;
    let mut units = 0usize;
    // An edit stages a unit in the global table, while a provider stages it in
    // the region; both are installed units and both answer the question.
    let edited = state
        .units
        .iter()
        .filter_map(|(key, _)| key.as_any().downcast_ref::<UnitKey>())
        .filter(|unit| unit.view == descriptor.id && unit.field == field);
    let owned = region
        .units
        .iter()
        .filter_map(|(key, _)| key.as_any().downcast_ref::<UnitKey>())
        .filter(|unit| unit.field == field);
    for unit in edited.chain(owned) {
        units += 1;
        if unit.entity.is_some() {
            entities += 1;
        }
    }
    if entities > 0 {
        return Granularity::PerEntity;
    }
    // A tree keeps its node units in the tag/field/children slots, not in the
    // root field. If the region has entity units at all, rows can be marked.
    if descriptor
        .fields
        .get(field.0 as usize)
        .is_some_and(|field| field.kind == reactive_core::descriptor::FieldKind::Tree)
    {
        let tree_entities = region
            .units
            .iter()
            .filter_map(|(key, _)| key.as_any().downcast_ref::<UnitKey>())
            .filter(|unit| unit.entity.is_some())
            .count();
        if tree_entities > 0 {
            return Granularity::PerEntity;
        }
    }
    match descriptor
        .fields
        .get(field.0 as usize)
        .map(|field| field.kind)
    {
        Some(reactive_core::descriptor::FieldKind::Scalar) if units > 0 => Granularity::PerEntity,
        Some(
            reactive_core::descriptor::FieldKind::List
            | reactive_core::descriptor::FieldKind::Set
            | reactive_core::descriptor::FieldKind::Map
            | reactive_core::descriptor::FieldKind::Graph
            | reactive_core::descriptor::FieldKind::Tree,
        ) => Granularity::WholeValue,
        Some(_) => Granularity::Unknown,
        None => Granularity::Unknown,
    }
}

/// Whether a node belongs to the queried scope.
#[cfg(feature = "observe")]
fn scope_matches(
    state: &State,
    scope: crate::observe::GraphScope,
    id: NodeId,
    record: &NodeRecord,
) -> bool {
    use crate::observe::GraphScope;
    match scope {
        GraphScope::All => true,
        GraphScope::View(view) => {
            record.unit.as_ref().is_some_and(|unit| unit.view == view)
                || matches!(record.kind, NodeKind::RegionContent)
                    && state
                        .regions
                        .get(&view)
                        .is_some_and(|region| region.content_node == id)
        }
        GraphScope::Component(component) => {
            record.component == Some(component)
                || component_of(state, record.scope) == Some(component)
        }
        GraphScope::Node(node) => node == id,
    }
}

/// Stable kind names for the wire.
#[cfg(feature = "observe")]
fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Site => "site",
        NodeKind::Unit => "unit",
        NodeKind::InvocationBody => "body",
        NodeKind::InvocationOutput => "output",
        NodeKind::Operator => "operator",
        NodeKind::RegionContent => "content",
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
        let mut waiters = inner.wake_waiters(&mut inner.lock(), node);
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
        let mut waiters = inner.wake_waiters(&mut inner.lock(), node);
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
            record.verified_at = 1;
            record.routing_at = 1;
            record.state = NodeState::Ready;
            node
        };
        let trace = vec![Dep {
            node,
            version: 1,
            routing: 1,
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
            (Vec::new(), false),
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
            (Vec::new(), false),
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
        let mut changed = Vec::new();
        inner.retire_unreachable(&mut state, node, 1, &mut changed);
        let record = state.node(node);
        assert!(record.is_running(), "a live claim must not be retired");
        assert!(record.value.is_some(), "a live claim keeps its value");
        assert!(changed.is_empty());
    }
}
