//! The evaluation kernel: claims, ordered validation, execution, and commit.
//!
//! The algorithm is the sequential specification from the plan, refined for
//! multiple workers:
//!
//! ```text
//! demand(node, revision):
//!     if verified at revision: return cached value
//!     if another worker owns it: wait for it (cycle-checked)
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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, RwLock, Weak};
use std::thread::{self, ThreadId};
use std::time::Duration;

use reactive_core::error::{ComputeError, Result};
use reactive_core::frame::{self, CancelToken, Dep, Frame, FrameGuard};
use reactive_core::hash::{FxHashMap, hash_one};
use reactive_core::ids::{ComponentId, NodeId, Revision, ScopeId, SiteId, Version, ViewId};
use reactive_core::key::{AnyKey, any_key};
use reactive_core::ops::{RuntimeOps, catch_engine};
use reactive_core::value::{Value, value_of};
use reactive_core::descriptor::{UnitKey, UnitKind, ViewDescriptor};

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
    /// How long a waiter sleeps before re-checking a node.
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
    pub gate: RwLock<()>,
    /// Cached revision for lock-free fast paths.
    pub(crate) revision: AtomicU64,
    /// Weak self-reference used when building frames.
    self_ref: OnceLock<Weak<Inner>>,
    /// Workers that execute derived computation.
    pub pool: crate::scheduler::Pool,
    /// Cancellation token of the demand currently running.
    active_cancel: Mutex<Option<CancelToken>>,
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
        let pool = crate::scheduler::Pool::new(config.workers, config.stack_size);
        let inner = Arc::new(Self {
            config,
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            gate: RwLock::new(()),
            revision: AtomicU64::new(0),
            self_ref: OnceLock::new(),
            pool,
            active_cancel: Mutex::new(None),
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
        if state.views.contains_key(&descriptor.id) {
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
        let me = thread::current().id();
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
        let me = thread::current().id();
        if frame::current()
            .is_some_and(|frame| frame.cancel().is_cancelled())
        {
            return Err(ComputeError::Cancelled);
        }
        loop {
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
                        Action::Claim
                    }
                } else if record.verified_at == revision && record.value.is_some() {
                    Action::Return(record.value.clone().expect("checked"))
                } else if record.is_running() {
                    Action::Wait
                } else if record.suspect_at <= record.verified_at && record.value.is_some() {
                    record.verified_at = revision;
                    Action::Return(record.value.clone().expect("checked"))
                } else {
                    record.state = NodeState::Running {
                        owner: me,
                        generation: 0,
                    };
                    Action::Claim
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
                Action::Claim => break,
            }
        }

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
            cancel,
        ));
        let guard = FrameGuard::push(Arc::clone(&frame));
        let result = catch_engine(compute);
        let definitions: Vec<DefinitionDraft> = frame.take_staged();
        let deps = frame.take_deps();
        drop(guard);

        if frame::current().is_some_and(|frame| frame.cancel().is_cancelled()) {
            return self.fail(node, ComputeError::Cancelled, revision);
        }
        match result {
            Ok(Ok(value)) => {
                let committed = self.commit(node, value, deps, definitions, eq, revision)?;
                let version = self.changed_at(node);
                frame::record_dep(Dep { node, version });
                Ok(committed)
            }
            Ok(Err(error)) | Err(error) => self.fail(node, error, revision),
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

    fn commit(
        &self,
        node: NodeId,
        value: Value,
        deps: Vec<Dep>,
        definitions: Vec<DefinitionDraft>,
        eq: &dyn Fn(&Value, &Value) -> bool,
        revision: Revision,
    ) -> Result<Value> {
        let mut state = self.lock();
        if state.revision != revision {
            return Err(ComputeError::internal(
                "input revision changed while a computation was running",
            ));
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
            if changed {
                record.changed_at = revision;
            }
        }
        state.stats.executions.fetch_add(1, Ordering::Relaxed);
        if changed {
            let dependents = state.node(node).dependents.clone();
            changed_nodes.extend(dependents);
        }
        if !changed_nodes.is_empty() {
            invalidate(&mut state, changed_nodes, revision);
        }
        drop(state);
        self.wake.notify_all();
        Ok(value)
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
        drop(state);
        self.wake.notify_all();
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
        let restaged: HashSet<UnitKey> = drafts.iter().map(|draft| draft.unit.clone()).collect();

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
            let outcome =
                self.retract_unit(state, &mut region, &record.unit, revision, changed_nodes);
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
        for draft in drafts {
            let owner = owner_key(&draft.unit);
            if let Some(owner) = region.owners.get(&owner) {
                if owner.node != provider {
                    return Err(ComputeError::DuplicateDefinition {
                        unit: format!("{}[{:?}]", descriptor.name, draft.unit),
                        first: owner.label.to_string(),
                        second: draft.label.to_string(),
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
            }
            content = (descriptor.content.define)(&content, &draft.unit, Arc::clone(&draft.value));
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
        Ok(())
    }

    fn retract_unit(
        &self,
        state: &mut State,
        region: &mut Region,
        unit: &UnitKey,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) -> Result<()> {
        let descriptor = region.descriptor;
        let owner = owner_key(unit);
        region.owners.remove(&owner);
        let mut content = Arc::clone(&region.content);
        content = (descriptor.content.retract)(&content, unit);
        if unit.entity.is_some() {
            let membership = membership_key(unit);
            if let Some(slot) = region.units.get(&membership) {
                let node = slot.node;
                let present = slot.present;
                if present {
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
        Ok(())
    }

    fn wait_for(&self, node: NodeId, me: ThreadId) -> Result<()> {
        let mut state = self.lock();
        let owner = match state.node(node).state {
            NodeState::Running { owner, .. } => owner,
            _ => return Ok(()),
        };
        if owner == me {
            return Err(ComputeError::Cycle {
                path: vec![format!("re-entrant demand of {node:?}")],
            });
        }
        if self.would_cycle(&state, owner, me) {
            return Err(ComputeError::Cycle {
                path: vec![format!("{me:?} -> {node:?} -> {owner:?}")],
            });
        }
        state.wait_graph.insert(me, node);
        loop {
            let running = match state.node(node).state {
                NodeState::Running { owner: current, .. } => Some(current),
                _ => None,
            };
            match running {
                Some(current) if current == owner => {
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

    fn would_cycle(&self, state: &State, from: ThreadId, target: ThreadId) -> bool {
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

    /// Demands the installed root at the current revision.
    pub fn demand_root(&self, cancel: &CancelToken) -> Result<()> {
        let root = {
            let state = self.lock();
            state.root.clone()
        };
        let Some(root) = root else {
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
        })
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

    /// Marks the running demand as cancelled, if there is one.
    pub fn cancel_active(&self) {
        let active = self
            .active_cancel
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(token) = active.as_ref() {
            token.cancel();
        }
    }

    /// Installs the token of the demand about to run.
    pub(crate) fn set_active_cancel(&self, token: Option<CancelToken>) {
        let mut active = self
            .active_cancel
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *active = token;
    }
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
    Claim,
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

    fn child_scope(&self, parent: ScopeId, discriminant: u64, label: &'static str) -> ScopeId {
        Inner::child_scope(self, parent, discriminant, label)
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
