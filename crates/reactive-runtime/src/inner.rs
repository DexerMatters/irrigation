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

use std::any::Any;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;

use reactive_core::error::{ComputeError, Result};
use reactive_core::frame::{self, CancelToken, Dep, Frame, FrameGuard};
use reactive_core::hash::{FxHashMap, hash_one};
use reactive_core::ids::{ComponentId, FieldId, NodeId, Revision, ScopeId, SiteId, Version, ViewId};
use reactive_core::key::{AnyKey, any_key};
use reactive_core::ops::{RuntimeOps, catch_engine};
use reactive_core::value::{Value, value_of};
use reactive_core::descriptor::{UnitKey, UnitKind, ViewDescriptor};

use crate::task::{self, TaskId, TaskMode, TaskStatus};
use crate::state::{
    DefinitionRecord, Invocation, NodeKind, NodeRecord, NodeState, ProviderRef, Region, State,
    UnitSlot, invalidate,
};

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
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            workers: 1,
            stack_size: 256 * 1024 * 1024,
            max_depth: 250_000,
            wait_slice: Duration::from_millis(20),
        }
    }
}

/// The shared runtime state and its synchronization.
pub struct Inner {
    /// Configuration.
    pub config: RuntimeConfig,
    /// Metadata and node arena.
    pub state: Mutex<State>,
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
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeInner")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
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
            wake: Condvar::new(),
            gate: crate::gate::Gate::new(),
            revision: AtomicU64::new(0),
            self_ref: OnceLock::new(),
            pool,
            active_task: Mutex::new(None),
        });
        let _ = inner.self_ref.set(Arc::downgrade(&inner));
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
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
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
            return *node;
        }
        let node = state.alloc(NodeRecord::site(scope, site));
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
        let content = state
            .regions
            .get(&view)
            .map(|region| Arc::clone(&region.content) as Arc<dyn Any + Send + Sync>)
            .unwrap_or_else(|| Arc::new(()));
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
        state.units.insert(Arc::new(key.clone()), node);
        node
    }

    /// Reads a unit at the current revision and records the observation.
    pub fn read_unit(&self, node: NodeId) -> Result<Value> {
        let me = task::current_or_detached();
        loop {
            let outcome = {
                let mut state = self.lock();
                let revision = state.revision;
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
                        Some(value) => Outcome::Return(value.clone(), record.changed_at),
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
                } else if record.suspect_at <= record.verified_at && record.value.is_some() {
                    record.verified_at = revision;
                    Outcome::Return(record.value.clone().expect("checked"), record.changed_at)
                } else {
                    Outcome::Fail(ComputeError::internal(format!(
                        "read of undefined unit {:?}",
                        record.unit
                    )))
                }
            };
            match outcome {
                Outcome::Return(value, version) => {
                    frame::record_dep(Dep { node, version });
                    return Ok(value);
                }
                Outcome::Fail(error) => return Err(error),
                Outcome::Retry => continue,
                Outcome::Wait => self.wait_for(node, me)?,
            }
        }
    }

    /// Demands a node, executing `compute` when reuse is impossible.
    pub fn eval(
        &self,
        node: NodeId,
        compute: &mut dyn FnMut() -> Result<Value>,
        eq: &dyn Fn(&Value, &Value) -> bool,
    ) -> Result<Value> {
        if frame::current()
            .is_some_and(|frame| frame.cancel().is_cancelled())
        {
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
                        state.node_mut(node).state = NodeState::Running { owner: me, generation };
                        Action::Claim(generation)
                    }
                } else if record.verified_at == revision && record.value.is_some() {
                    Action::Return(record.value.clone().expect("checked"))
                } else if record.is_running() {
                    match record.state {
                        NodeState::Running { owner, generation } if owner == me => {
                            if task::active_contains(node) {
                                Action::Fail(ComputeError::Cycle {
                                    path: vec![format!("re-entrant demand of {node:?}")],
                                })
                            } else {
                                Action::Resume(generation)
                            }
                        }
                        _ => Action::Wait,
                    }
                } else if record.suspect_at <= record.verified_at && record.value.is_some() {
                    record.verified_at = revision;
                    Action::Return(record.value.clone().expect("checked"))
                } else {
                    state.next_generation += 1;
                    let generation = state.next_generation;
                    state.node_mut(node).state = NodeState::Running { owner: me, generation };
                    Action::Claim(generation)
                }
            };

            match action {
                Action::Return(value) => {
                    let version = self.changed_at(node);
                    frame::record_dep(Dep { node, version });
                    return Ok(value);
                }
                Action::Fail(error) => return Err(error),
                Action::Wait => self.wait_for(node, me)?,
                Action::Claim(generation) | Action::Resume(generation) => break generation,
            }
        };

        // Ordered validation: stop at the first observation that cannot be
        // certified without executing its own computation.
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
        if has_value && self.validate_trace(&trace, revision) {
            let value = {
                let mut state = self.lock();
                let record = state.node_mut(node);
                record.state = NodeState::Ready;
                record.verified_at = revision;
                record.suspect_at = 0;
                record
                    .value
                    .clone()
                    .expect("validated node without a value")
            };
            let version = self.changed_at(node);
            frame::record_dep(Dep { node, version });
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
                let committed =
                    self.commit(node, value, deps, definitions, None, eq, revision, me, generation, &cancel)?;
                let version = self.changed_at(node);
                frame::record_dep(Dep { node, version });
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
        let (general, old_trace, old_state, scope) = {
            let state = self.lock();
            let record = state.node(node);
            let general = Arc::clone(&state.generals[index]);
            (
                general,
                record.trace.clone(),
                record.general_state.clone(),
                record.scope,
            )
        };
        let changes = {
            let state = self.lock();
            let mut changes = Vec::new();
            for dep in &old_trace {
                let record = state.node(dep.node);
                if record.changed_at != dep.version {
                    if let Some(unit) = &record.unit {
                        changes.push(crate::general::Change {
                            unit: unit.clone(),
                            previous: dep.version,
                            current: record.changed_at,
                        });
                    }
                }
            }
            crate::general::ChangeBatch::new(changes)
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
        let mut cx = crate::general::GeneralCx::new(self.arc(), revision);
        let use_update = general.policy() == crate::general::DependencyUpdates::Incremental
            && old_state.is_some();
        let result = catch_engine(|| match (&old_state, use_update) {
            (Some(state), true) => general.update(&mut cx, state, &changes),
            _ => general.initialize(&mut cx),
        });
        let definitions: Vec<DefinitionDraft> = frame.take_staged();
        let deps = frame.take_deps();
        drop(guard);
        if frame_cancelled(&parent) {
            return self.fail(node, ComputeError::Cancelled, revision);
        }
        match result {
            Ok(Ok((state, output))) => {
                let eq = |left: &Value, right: &Value| general.output_eq(left, right);
                let committed = self.commit(
                    node,
                    output,
                    deps,
                    definitions,
                    Some(state),
                    &eq,
                    revision,
                    owner,
                    generation,
                    &cancel,
                )?;
                let version = self.changed_at(node);
                frame::record_dep(Dep { node, version });
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
        for dep in trace {
            let (verified, clean, changed) = {
                let record = state.node(dep.node);
                (
                    record.verified_at == revision,
                    record.suspect_at <= record.verified_at && record.value.is_some(),
                    record.changed_at,
                )
            };
            if !verified {
                if !clean {
                    return false;
                }
                state.node_mut(dep.node).verified_at = revision;
            }
            if changed != dep.version {
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
        retained_state: Option<crate::general::ErasedState>,
        eq: &dyn Fn(&Value, &Value) -> bool,
        revision: Revision,
        owner: TaskId,
        generation: u64,
        cancel: &CancelToken,
    ) -> Result<Value> {
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
                ComputeError::internal(
                    "input revision changed while a computation was running",
                ),
                revision,
            );
        }
        let mut changed_nodes = Vec::new();
        if let Err(error) =
            self.install_definitions(&mut state, node, definitions, revision, &mut changed_nodes)
        {
            drop(state);
            return self.fail(node, error, revision);
        }

        let (changed, old_trace) = {
            let record = state.node(node);
            let changed = record
                .value
                .as_ref()
                .is_none_or(|old| !eq(old, &value));
            (changed, record.trace.clone())
        };
        for dep in &old_trace {
            state.remove_dependent(dep.node, node);
        }
        let new_dependencies: HashSet<NodeId> = deps.iter().map(|dep| dep.node).collect();
        for dep in &deps {
            state.add_dependent(dep.node, node);
        }
        {
            let record = state.node_mut(node);
            record.value = Some(value.clone());
            record.trace = deps;
            record.state = NodeState::Ready;
            record.verified_at = revision;
            record.suspect_at = 0;
            record.failure = None;
            if let Some(retained) = retained_state {
                record.general_state = Some(retained);
            }
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
        if !changed_nodes.is_empty() {
            invalidate(&mut state, changed_nodes, revision);
        }
        let queued = Self::wake_waiters(&mut state, node);
        drop(state);
        self.wake.notify_all();
        self.enqueue(queued);
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
                let _ = self.retract_unit(
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
        let queued = Self::wake_waiters(&mut state, node);
        drop(state);
        self.wake.notify_all();
        self.enqueue(queued);
        Err(error)
    }

    fn install_definitions(
        &self,
        state: &mut State,
        provider: NodeId,
        drafts: Vec<DefinitionDraft>,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) -> Result<()> {
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
        }
        let restaged: HashSet<UnitKey> = drafts.iter().map(|draft| draft.unit.clone()).collect();
        if restaged.len() != drafts.len() {
            let mut seen = HashSet::new();
            for draft in &drafts {
                if !seen.insert(draft.unit.clone()) {
                    return Err(ComputeError::DuplicateDefinition {
                        unit: format!("{:?}", draft.unit),
                        first: draft.label.to_string(),
                        second: draft.label.to_string(),
                    });
                }
            }
        }

        // Retractions first: a unit this provider defined before but no longer
        // defines must disappear atomically with the new value.
        for record in &old {
            if restaged.contains(&record.unit) {
                continue;
            }
            let view = record.unit.view;
            let Some(mut region) = state.regions.remove(&view) else {
                continue;
            };
            let outcome = self.retract_unit(
                state,
                &mut region,
                provider,
                &record.unit,
                revision,
                changed_nodes,
            );
            state.regions.insert(view, region);
            outcome?;
        }

        let mut by_view: FxHashMap<ViewId, Vec<DefinitionDraft>> = FxHashMap::default();
        for draft in drafts {
            by_view.entry(draft.unit.view).or_default().push(draft);
        }
        let mut installed: Vec<DefinitionRecord> = Vec::new();
        for (view, drafts) in by_view {
            let Some(mut region) = state.regions.remove(&view) else {
                return Err(ComputeError::DefinitionDenied {
                    unit: format!("view {view:?}"),
                    reason: String::from("view is not registered"),
                });
            };
            let outcome = self.apply_definitions(
                state,
                &mut region,
                provider,
                &drafts,
                revision,
                changed_nodes,
                &mut installed,
            );
            state.regions.insert(view, region);
            outcome?;
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
        &self,
        state: &mut State,
        region: &mut Region,
        provider: NodeId,
        drafts: &[DefinitionDraft],
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
        installed: &mut Vec<DefinitionRecord>,
    ) -> Result<()> {
        let descriptor = region.descriptor;
        let mut content = Arc::clone(&region.content);
        let mut dirty_fields: Vec<FieldId> = Vec::new();
        for draft in drafts {
            let owner = owner_key(&draft.unit);
            if let Some(existing) = region.owners.get(&owner) {
                if existing.node != provider {
                    // Resolve at the end of the revision: if the earlier owner
                    // is still reachable then both providers are live and the
                    // revision fails; if it retired during the revision, this
                    // was a handover.
                    state.pending_conflicts.push(crate::state::PendingConflict {
                        unit: draft.unit.clone(),
                        first: existing.node,
                        second: provider,
                    });
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
                        let node =
                            Self::ensure_unit_node(state, membership.clone(), Some(value_of(false)));
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
            region.owners.insert(
                Arc::new(owner),
                ProviderRef {
                    node: provider,
                    label: draft.label,
                },
            );
            installed.push(DefinitionRecord {
                unit: draft.unit.clone(),
                provider,
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
        Ok(())
    }

    fn retract_unit(
        &self,
        state: &mut State,
        region: &mut Region,
        provider: NodeId,
        unit: &UnitKey,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) -> Result<()> {
        let descriptor = region.descriptor;
        let owner = owner_key(unit);
        if let Some(existing) = region.owners.get(&owner) {
            if existing.node != provider {
                return Ok(());
            }
        }
        region.owners.remove(&owner);
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
            if let Some(slot) = region.units.get(unit) {
                let node = slot.node;
                let record = state.node_mut(node);
                record.suspect_at = revision;
                // A directly read retracted unit must not be served from the
                // old certified value; membership guards family handles, this
                // guards direct unit reads.
                record.verified_at = 0;
                let dependents = record.dependents.clone();
                changed_nodes.extend(dependents);
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
        Ok(())
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
                path: vec![format!("re-entrant demand of {node:?}")],
            });
        }
        if self.would_cycle(&state, owner, me) {
            return Err(ComputeError::Cycle {
                path: vec![format!("{me:?} -> {node:?} -> {owner:?}")],
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
            state.node_mut(node).waiters.push(me);
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
                    let (next, _timeout) = self
                        .wake
                        .wait_timeout(state, self.config.wait_slice)
                        .unwrap_or_else(|poison| poison.into_inner());
                    state = next;
                }
                _ => break,
            }
        }
        state.wait_graph.remove(&me);
        Ok(())
    }

    /// Queues the tasks suspended on `node`. Called under the state lock; the
    /// caller enqueues the returned tasks once the lock is released.
    pub(crate) fn wake_waiters(state: &mut State, node: NodeId) -> Vec<TaskId> {
        let waiters = std::mem::take(&mut state.node_mut(node).waiters);
        let mut queued = Vec::new();
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
                queued.push(waiter);
            }
        }
        queued
    }

    fn would_cycle(&self, state: &State, from: TaskId, target: TaskId) -> bool {
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
            let unit = format!("{:?}", conflict.unit);
            let first = state.node(conflict.first).label.to_string();
            let second = state.node(conflict.second).label.to_string();
            let view = conflict.unit.view;
            if let Some(mut region) = state.regions.remove(&view) {
                let mut changed = Vec::new();
                let _ = self.retract_unit(
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
    pub fn demand_root_projection(
        &self,
        cancel: &CancelToken,
        index: usize,
    ) -> Result<Value> {
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
                        runtime.observe(root.body).unwrap_or_else(|error| {
                            reactive_core::ops::panic_engine(error)
                        });
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
        let eq = |_: &Value, _: &Value| false;
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
            let eq = |_: &Value, _: &Value| false;
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
            let body_value = crate::component::eval_value(
                &runtime,
                root.body,
                || (root.run)(),
                &root.body_eq,
            );
            for (index, node) in root.outputs.iter().enumerate() {
                let projection_eq = root.projection_eq;
                crate::component::eval_value(
                    &runtime,
                    *node,
                    || {
                        runtime.observe(root.body).unwrap_or_else(|error| {
                            reactive_core::ops::panic_engine(error)
                        });
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
        for (id, region) in &state.regions {
            views.insert(*id, Arc::clone(&region.content));
            versions.insert(*id, state.node(region.content_node).changed_at);
        }
        crate::snapshot::Snapshot::new(revision, views, versions)
    }

    /// Number of worker threads.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.pool.workers()
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
    pub(crate) fn has_root(&self) -> bool {
        let state = self.lock();
        state.root.is_some() || state.general_root.is_some()
    }

    /// Marks the most recently started demand as cancelled, if there is one.
    pub fn cancel_active(&self) {
        let task = *self
            .active_task
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
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
        let mut active = self
            .active_task
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
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

/// Computes the argument hash used for invocation interning.
#[must_use]
pub fn arguments_hash<T: std::hash::Hash>(args: &T) -> u64 {
    hash_one(args)
}

enum Outcome {
    Return(Value, Version),
    Fail(ComputeError),
    Retry,
    Wait,
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
        let mut state = self.lock();
        if state.units.get(unit).is_some() {
            return Ok(());
        }
        let initial = state
            .regions
            .get(&unit.view)
            .and_then(|region| (region.descriptor.content.get)(&region.content, unit));
        Self::ensure_unit_node(&mut state, unit.clone(), initial);
        Ok(())
    }

    fn child_scope(&self, parent: ScopeId, discriminant: u64, label: &'static str) -> ScopeId {
        Inner::child_scope(self, parent, discriminant, label)
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
        });
        Ok(())
    }

    fn invocation_node(
        &self,
        component: ComponentId,
        args_hash: u64,
        args: Arc<dyn Any + Send + Sync>,
        scope: ScopeId,
        arity: usize,
        args_eq: &dyn Fn(&dyn Any, &dyn Any) -> bool,
    ) -> (NodeId, Vec<NodeId>) {
        let mut state = self.lock();
        if let Some(entries) = state.invocations.get(&(component, args_hash)) {
            for invocation in entries {
                if args_eq(invocation.args.as_ref(), args.as_ref()) {
                    return (invocation.body, invocation.outputs.clone());
                }
            }
        }
        let invocation_id = state.next_invocation + 1;
        state.next_invocation = invocation_id;
        let invocation_scope = state.scopes.child(scope, invocation_id);
        let mut body_record = NodeRecord::empty("invocation", NodeKind::InvocationBody);
        body_record.scope = invocation_scope;
        let body = state.alloc(body_record);
        let mut outputs = Vec::with_capacity(arity);
        for index in 0..arity {
            let mut record = NodeRecord::empty("invocation output", NodeKind::InvocationOutput);
            record.scope = invocation_scope;
            record.site = SiteId(index as u32);
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
            });
        (body, outputs)
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
        let mut doomed: Vec<NodeId> = Vec::new();
        for (index, record) in state.nodes.iter().enumerate() {
            let id = NodeId(index as u32);
            if reachable.contains(&id) {
                continue;
            }
            let absent_membership = record.kind == NodeKind::Unit && Self::is_absent_membership(record);
            if matches!(record.kind, NodeKind::Unit | NodeKind::RegionContent) && !absent_membership {
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
        }
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
        state
            .invocations
            .retain(|_, entries| !entries.is_empty());
        ReclaimStats {
            nodes_before,
            nodes_after: state.nodes.len(),
            nodes_reclaimed: doomed.len(),
            units_reclaimed,
            invocations_reclaimed,
        }
    }

    /// Reports memory held by runtime metadata and committed values.
    #[must_use]
    pub fn memory_report(&self) -> MemoryReport {
        let state = self.lock();
        let mut value_bytes = 0;
        let mut trace_edges = 0;
        let mut dependent_edges = 0;
        for node in &state.nodes {
            if let Some(value) = &node.value {
                value_bytes += std::mem::size_of_val(value.as_ref());
            }
            trace_edges += node.trace.len();
            dependent_edges += node.dependents.len();
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            None,
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
                NodeState::Running { owner: 1, generation: 7 }
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
            None,
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
