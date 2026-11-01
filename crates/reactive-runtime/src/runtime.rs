//! The public runtime handle: installation, reads, edits, and observations.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use reactive_core::error::{ComputeError, Result};
use reactive_core::frame::CancelToken;
use reactive_core::hash::FxHashMap;
use reactive_core::ids::{ComponentId, Revision, Version, ViewId};
use reactive_core::ops::catch_engine;
use reactive_core::value::Value;
use reactive_core::descriptor::ViewDescriptor;

use crate::component::{ComponentSpec, RootComponent};
use crate::edit::Edit;
use crate::inner::{Inner, RuntimeConfig};
use crate::snapshot::{Snapshot, ViewSchema};
use crate::state::{NodeKind, NodeRecord, RootRecord, SubscriptionRecord};

/// The public entry point to one isolated incremental runtime.
///
/// A runtime owns all identity: View namespaces, component definitions, and
/// every memoized node. Two runtimes are fully independent, which is what makes
/// tests and multi-project hosts possible without process-global state.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("revision", &self.revision())
            .finish_non_exhaustive()
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    /// Creates a runtime with one worker.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(RuntimeConfig::default())
    }

    /// Creates a runtime with explicit configuration.
    #[must_use]
    pub fn with_config(config: RuntimeConfig) -> Self {
        Self {
            inner: Inner::new(config),
        }
    }

    /// The current input revision.
    #[must_use]
    pub fn revision(&self) -> Revision {
        self.inner.current_revision()
    }

    /// Registers a View without installing a root component.
    ///
    /// Useful for tests and for hosts that compose several roots.
    pub fn register_view(&self, descriptor: &'static ViewDescriptor) {
        self.inner.register_view(descriptor);
    }

    /// Installs a root component and registers every View it reads.
    ///
    /// # Errors
    ///
    /// Fails when a root is already installed or when a View is missing.
    pub fn install<R: RootComponent>(&self, _root: R) -> Result<()> {
        let _gate = self.inner.gate.write();
        for descriptor in R::VIEWS {
            self.inner.register_view(descriptor);
        }
        let mut state = self.inner.lock();
        if state.root.is_some() {
            return Err(ComputeError::internal(
                "a root component is already installed on this runtime",
            ));
        }
        let mut body_record = NodeRecord::empty("root body", NodeKind::InvocationBody);
        body_record.label = "root body";
        let body = state.alloc(body_record);
        let mut outputs = Vec::with_capacity(R::ARITY);
        for index in 0..R::ARITY {
            let mut record = NodeRecord::empty("root output", NodeKind::InvocationOutput);
            record.site = reactive_core::ids::SiteId(index as u32);
            outputs.push(state.alloc(record));
        }
        state.root = Some(RootRecord {
            component: R::ID,
            name: R::NAME,
            body,
            outputs,
            run: || reactive_core::value::value_of(R::run()),
            project: root_project::<R>,
            projection_eq: R::projection_eq,
            body_eq: root_body_eq::<R>,
            reconstruct: |parts| reactive_core::value::value_of(R::reconstruct(parts)),
        });
        Ok(())
    }

    /// Installs a typed custom algorithm as the runtime's root.
    ///
    /// # Errors
    ///
    /// Fails when a root is already installed.
    pub fn install_typed_general<T: crate::general::TypedGeneralComponent>(
        &self,
        component: T,
    ) -> Result<()> {
        self.install_general(crate::general::TypedAdapter::new(component))
    }

    /// Installs a local (non-`Send` state) algorithm as the runtime's root.
    ///
    /// # Errors
    ///
    /// Fails when the runtime has pooled workers (demands must run on the
    /// installing thread) or when a root is already installed.
    pub fn install_local_general<C: crate::general::LocalGeneralComponent>(
        &self,
        component: C,
    ) -> Result<()> {
        if self.inner.workers() != 0 {
            return Err(ComputeError::internal(
                "local components require a workerless runtime (`workers = 0`)",
            ));
        }
        self.install_general(crate::general::LocalAdapter::new(component))
    }

    /// Installs a custom incremental algorithm as the runtime's root.
    ///
    /// # Errors
    ///
    /// Fails when a root is already installed.
    pub fn install_general<T: crate::general::GeneralComponent>(&self, component: T) -> Result<()> {
        let _gate = self.inner.gate.write();
        for descriptor in T::VIEWS {
            self.inner.register_view(descriptor);
        }
        let mut state = self.inner.lock();
        if state.root.is_some() || state.general_root.is_some() {
            return Err(ComputeError::internal(
                "a root component is already installed on this runtime",
            ));
        }
        state.generals.push(Arc::new(component));
        let index = state.generals.len() - 1;
        let mut record = NodeRecord::empty("general component", NodeKind::InvocationBody);
        record.label = "general component";
        record.general = Some(index);
        let node = state.alloc(record);
        state.general_root = Some(node);
        Ok(())
    }

    /// Demands the installed `GeneralComponent` and returns its typed output.
    ///
    /// # Errors
    ///
    /// Fails when no general component is installed or when it fails.
    pub fn read_outputs_general<T: crate::general::GeneralComponent>(&self) -> Result<T::Output> {
        self.auto_collect();
        let inner = Arc::clone(&self.inner);
        let task = Arc::clone(&self.inner);
        let cancel = CancelToken::default();
        let token = cancel.clone();
        inner.demand(cancel, move || {
            task.demand_root(&token).and_then(|()| {
                let node = {
                    let state = task.lock();
                    state.general_root
                };
                let Some(node) = node else {
                    return Err(ComputeError::internal("no general component installed"));
                };
                let value = {
                    let state = task.lock();
                    state.node(node).value.clone()
                };
                let Some(value) = value else {
                    return Err(ComputeError::internal(
                        "general component produced no output",
                    ));
                };
                value
                    .downcast_ref::<T::Output>()
                    .cloned()
                    .ok_or(ComputeError::TypeMismatch {
                        expected: std::any::type_name::<T::Output>(),
                    })
            })
        })
    }

    /// Captures the current View contents without demanding a root.
    ///
    /// Useful for hosts that read only source Views, and for tests that do not
    /// install a component.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let inner = Arc::clone(&self.inner);
        let task = Arc::clone(&self.inner);
        inner
            .demand(CancelToken::default(), move || Ok(task.capture_snapshot()))
            .expect("snapshot demand cannot fail")
    }

    /// Demands the installed root and returns an immutable snapshot.
    ///
    /// # Errors
    ///
    /// Fails when no root is installed or when a component fails.
    pub fn read(&self) -> Result<Snapshot> {
        self.read_with_cancel(CancelToken::default())
    }

    /// Demands the installed root with an explicit cancellation token.
    ///
    /// A cancelled demand never publishes: work already staged by it is
    /// discarded and the nodes it touched stay vacant for the next demand.
    ///
    /// # Errors
    ///
    /// Returns [`ComputeError::Cancelled`] when the token was cancelled before
    /// or during the demand.
    pub fn read_with_cancel(&self, cancel: CancelToken) -> Result<Snapshot> {
        let inner = Arc::clone(&self.inner);
        let task = Arc::clone(&self.inner);
        let token = cancel.clone();
        inner.demand(cancel, move || {
            task.demand_root(&token).map(|()| task.capture_snapshot())
        })
    }

    /// Cancels the demand currently running, if any.
    ///
    /// Cancelled work cannot publish; a later demand recomputes normally.
    pub fn cancel(&self) {
        self.inner.cancel_active();
    }

    /// Calls a component and demands only one of its output projections.
    ///
    /// # Errors
    ///
    /// Fails when the index is out of range or the demand fails.
    pub fn call_projection<S: ComponentSpec>(
        &self,
        args: S::Args,
        index: usize,
    ) -> Result<Value> {
        let inner = Arc::clone(&self.inner);
        let task = Arc::clone(&self.inner);
        let cancel = CancelToken::default();
        let token = cancel.clone();
        inner.demand(cancel, move || {
            let runtime: Arc<dyn reactive_core::ops::RuntimeOps> = task.arc();
            #[allow(clippy::arc_with_non_send_sync)]
            let frame = Arc::new(reactive_core::frame::Frame::scoped(
                runtime,
                reactive_core::ids::NodeId::NONE,
                reactive_core::ids::ScopeId::ROOT,
                0,
                token.clone(),
            ));
            let _guard = reactive_core::frame::FrameGuard::push(frame);
            let result = catch_engine(|| {
                crate::component::call_component_projection::<S>(args.clone(), index)
            });
            match result {
                Ok(output) => {
                    let revision = task.current_revision();
                    task.resolve_conflicts(revision)?;
                    Ok(output)
                }
                Err(error) => Err(error),
            }
        })
    }

    /// Demands the installed root and returns its typed output product.
    ///
    /// # Errors
    ///
    /// Fails when no root is installed, when a component fails, or when `R`
    /// does not match the installed root.
    pub fn read_outputs<R: RootComponent>(&self) -> Result<R::Output> {
        self.auto_collect();
        self.read_outputs_with_cancel::<R>(CancelToken::default())
    }

    /// Demands the installed root's typed output with a cancellation token.
    ///
    /// # Errors
    ///
    /// Returns [`ComputeError::Cancelled`] when the token was cancelled.
    pub fn read_outputs_with_cancel<R: RootComponent>(
        &self,
        cancel: CancelToken,
    ) -> Result<R::Output> {
        let inner = Arc::clone(&self.inner);
        let task = Arc::clone(&self.inner);
        let token = cancel.clone();
        inner.demand(cancel, move || {
            task.demand_root(&token).and_then(|()| {
                let root = {
                    let state = task.lock();
                    state.root.clone()
                };
                let Some(root) = root else {
                    return Err(ComputeError::internal("no root component installed"));
                };
                let parts: Vec<Value> = {
                    let state = task.lock();
                    root.outputs
                        .iter()
                        .map(|node| {
                            state
                                .node(*node)
                                .value
                                .clone()
                                .expect("root output was demanded")
                        })
                        .collect()
                };
                catch_engine(|| {
                    (root.reconstruct)(&parts)
                        .downcast_ref::<R::Output>()
                        .cloned()
                        .unwrap_or_else(|| {
                            reactive_core::ops::panic_engine(ComputeError::internal(
                                "root output type does not match the installed root",
                            ))
                        })
                })
            })
        })
    }

    /// Demands one output projection of the installed root.
    ///
    /// The component body still runs, but only the requested projection node is
    /// evaluated and versioned; the other projections stay unevaluated until
    /// demanded.
    ///
    /// # Errors
    ///
    /// Fails when no root is installed, the index is out of range, or the
    /// demand fails.
    pub fn read_projection<R: RootComponent>(&self, index: usize) -> Result<Value> {
        self.auto_collect();
        self.read_projection_with_cancel::<R>(index, CancelToken::default())
    }

    /// Demands one output projection with a cancellation token.
    ///
    /// # Errors
    ///
    /// Returns [`ComputeError::Cancelled`] when the token was cancelled.
    pub fn read_projection_with_cancel<R: RootComponent>(
        &self,
        index: usize,
        cancel: CancelToken,
    ) -> Result<Value> {
        let inner = Arc::clone(&self.inner);
        let task = Arc::clone(&self.inner);
        let token = cancel.clone();
        inner.demand(cancel, move || {
            task.demand_root_projection(&token, index)
        })
    }

    /// Applies a transaction and returns the closure's result.
    ///
    /// Only the inputs are mutated; derived values are repaired on demand or by
    /// active subscriptions.
    ///
    /// # Errors
    ///
    /// Fails when the transaction cannot be committed.
    pub fn edit<T>(&self, f: impl FnOnce(&mut Edit) -> T) -> Result<T> {
        let (value, changed) = {
            let _gate = self.inner.gate.write();
            let mut edit = Edit::new(Arc::clone(&self.inner));
            let value = f(&mut edit);
            let changed = edit.commit()?;
            (value, changed)
        };
        if changed {
            // The transaction is already committed: a failed observer refresh
            // must not turn a successful edit into an error. It is reported
            // through `notification_error` instead.
            if let Err(error) = self.notify_subscribers() {
                self.inner.set_notification_error(error);
            } else {
                self.inner.clear_notification_error();
            }
        }
        Ok(value)
    }

    /// Returns the last observer-refresh failure, if any.
    ///
    /// [`Runtime::edit`] commits before refreshing observers. A failure to
    /// demand the root while refreshing them does not fail the edit; it is
    /// recorded here until the next successful refresh.
    #[must_use]
    pub fn notification_error(&self) -> Option<ComputeError> {
        self.inner.lock().notification_error.clone()
    }

    /// Registers a callback that runs after every change to `V`.
    ///
    /// The subscription keeps an observation root active: changes trigger a
    /// fresh result without a manual read.
    ///
    /// # Errors
    ///
    /// Fails when the View is not registered.
    pub fn observe<V: ViewSchema>(
        &self,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> Result<Subscription> {
        if self.inner.view_descriptor(V::view_id()).is_none() {
            return Err(ComputeError::internal(format!(
                "cannot observe unregistered view `{}`",
                V::DESCRIPTOR.name
            )));
        }
        let mut state = self.inner.lock();
        let id = state.next_subscription;
        state.next_subscription += 1;
        let active = Arc::new(AtomicBool::new(true));
        let version = state
            .regions
            .get(&V::view_id())
            .map_or(0, |region| state.node(region.content_node).changed_at);
        state.subscriptions.push(SubscriptionRecord {
            id,
            view: V::view_id(),
            callback: crate::state::Observer::Plain(Arc::new(callback)),
            last_version: version,
            units: Vec::new(),
            last_versions: Vec::new(),
            routing: false,
            active: Arc::clone(&active),
        });
        Ok(Subscription {
            inner: Arc::clone(&self.inner),
            id,
            active,
        })
    }

    /// Observes a set of logical units and fires only when one of them moves.
    ///
    /// An empty set observes the whole View content. The callback receives the
    /// current snapshot; use the View's typed accessors to read the watched
    /// entries.
    ///
    /// # Errors
    ///
    /// Fails when the View is not registered.
    pub fn observe_units<V: ViewSchema>(
        &self,
        units: Vec<reactive_core::descriptor::UnitKey>,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> Result<Subscription> {
        if self.inner.view_descriptor(V::view_id()).is_none() {
            return Err(ComputeError::internal(format!(
                "cannot observe unregistered view `{}`",
                V::DESCRIPTOR.name
            )));
        }
        let mut state = self.inner.lock();
        let versions: Vec<reactive_core::ids::Version> = units
            .iter()
            .map(|unit| crate::state::unit_version(&state, unit))
            .collect();
        let id = state.next_subscription;
        state.next_subscription += 1;
        let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        state.subscriptions.push(SubscriptionRecord {
            id,
            view: V::view_id(),
            callback: crate::state::Observer::Plain(Arc::new(callback)),
            last_version: 0,
            units,
            last_versions: versions,
            routing: false,
            active: Arc::clone(&active),
        });
        Ok(Subscription {
            inner: Arc::clone(&self.inner),
            id,
            active,
        })
    }

    /// Observes one map entry of a View: its membership and value units.
    ///
    /// # Errors
    ///
    /// Fails when the View is not registered.
    pub fn observe_entry<V: ViewSchema, K>(
        &self,
        field: reactive_core::ids::FieldId,
        key: K,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> Result<Subscription>
    where
        K: std::any::Any
            + std::hash::Hash
            + Eq
            + Send
            + Sync
            + std::fmt::Debug
            + 'static,
    {
        let key = reactive_core::key::any_key(key);
        let view = V::view_id();
        let units = vec![
            reactive_core::descriptor::UnitKey::entity(
                view,
                field,
                reactive_core::descriptor::UnitKind::Membership,
                std::sync::Arc::clone(&key),
            ),
            reactive_core::descriptor::UnitKey::entity(
                view,
                field,
                reactive_core::descriptor::UnitKind::Value,
                key,
            ),
        ];
        self.observe_units::<V>(units, callback)
    }

    /// Observes one unit's provider routing generation.
    ///
    /// Fires when the unit's provider changes, including a handover that keeps
    /// the value equal; value-only edits do not fire.
    ///
    /// # Errors
    ///
    /// Fails when the View is not registered.
    pub fn observe_routing<V: ViewSchema>(
        &self,
        unit: reactive_core::descriptor::UnitKey,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> Result<Subscription> {
        if self.inner.view_descriptor(V::view_id()).is_none() {
            return Err(ComputeError::internal(format!(
                "cannot observe unregistered view `{}`",
                V::DESCRIPTOR.name
            )));
        }
        let mut state = self.inner.lock();
        let initial = crate::state::routing_version(&state, &unit);
        let id = state.next_subscription;
        state.next_subscription += 1;
        let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        state.subscriptions.push(SubscriptionRecord {
            id,
            view: V::view_id(),
            callback: crate::state::Observer::Plain(Arc::new(callback)),
            last_version: 0,
            units: vec![unit],
            last_versions: vec![initial],
            routing: true,
            active: Arc::clone(&active),
        });
        Ok(Subscription {
            inner: Arc::clone(&self.inner),
            id,
            active,
        })
    }

    /// Registers a callback that also receives the changed View versions.
    ///
    /// # Errors
    ///
    /// Fails when the View is not registered.
    pub fn observe_with_changes<V: ViewSchema>(
        &self,
        callback: impl Fn(&Snapshot, &crate::snapshot::ChangeSummary) + Send + Sync + 'static,
    ) -> Result<Subscription> {
        if self.inner.view_descriptor(V::view_id()).is_none() {
            return Err(ComputeError::internal(format!(
                "cannot observe unregistered view `{}`",
                V::DESCRIPTOR.name
            )));
        }
        let mut state = self.inner.lock();
        let id = state.next_subscription;
        state.next_subscription += 1;
        let active = Arc::new(AtomicBool::new(true));
        let version = state
            .regions
            .get(&V::view_id())
            .map_or(0, |region| state.node(region.content_node).changed_at);
        state.subscriptions.push(SubscriptionRecord {
            id,
            view: V::view_id(),
            callback: crate::state::Observer::WithChanges(Arc::new(callback)),
            last_version: version,
            units: Vec::new(),
            last_versions: Vec::new(),
            routing: false,
            active: Arc::clone(&active),
        });
        Ok(Subscription {
            inner: Arc::clone(&self.inner),
            id,
            active,
        })
    }

    /// Demands a component by descriptor, returning its output product.
    ///
    /// # Errors
    ///
    /// Fails when called outside a demand context or when the component fails.
    pub fn call<S: ComponentSpec>(&self, args: S::Args) -> Result<S::Output> {
        self.auto_collect();
        let inner = Arc::clone(&self.inner);
        let task = Arc::clone(&self.inner);
        let cancel = CancelToken::default();
        let token = cancel.clone();
        inner.demand(cancel, move || {
            let runtime: Arc<dyn reactive_core::ops::RuntimeOps> = task.arc();
            // Frames are thread-local execution state; see the note in `eval`.
            #[allow(clippy::arc_with_non_send_sync)]
            let frame = Arc::new(reactive_core::frame::Frame::scoped(
                Arc::clone(&runtime),
                reactive_core::ids::NodeId::NONE,
                reactive_core::ids::ScopeId::ROOT,
                0,
                token.clone(),
            ));
            let _guard = reactive_core::frame::FrameGuard::push(frame);
            let result = catch_engine(|| crate::component::call_component::<S>(args.clone()));
            match result {
                Ok(output) => {
                    // A call is a demand root: pending definition conflicts are
                    // resolved before the result is handed back, so two called
                    // providers cannot silently share one unit.
                    let revision = task.current_revision();
                    task.resolve_conflicts(revision)?;
                    Ok(output)
                }
                Err(error) => Err(error),
            }
        })
    }

    /// Runtime counters, useful in tests and benchmarks.
    #[must_use]
    pub fn stats(&self) -> RuntimeStats {
        let state = self.inner.lock();
        let mut tasks = TaskCounts::default();
        for slot in state.tasks.values() {
            match slot.status {
                crate::task::TaskStatus::Queued => tasks.queued += 1,
                crate::task::TaskStatus::Running => tasks.running += 1,
                crate::task::TaskStatus::Waiting(_) => tasks.waiting += 1,
            }
        }
        RuntimeStats {
            nodes_created: state.stats.nodes_created.load(Ordering::Relaxed),
            executions: state.stats.executions.load(Ordering::Relaxed),
            validations: state.stats.validations.load(Ordering::Relaxed),
            revisions: state.stats.revisions.load(Ordering::Relaxed),
            invalidations: state.stats.invalidations.load(Ordering::Relaxed),
            auto_collects: state.stats.auto_collects.load(Ordering::Relaxed),
            site_creations: state.stats.site_creations.load(Ordering::Relaxed),
            site_reuses: state.stats.site_reuses.load(Ordering::Relaxed),
            invocations_evicted: state.stats.invocations_evicted.load(Ordering::Relaxed),
            invalidation_ns: state.stats.invalidation_ns.load(Ordering::Relaxed),
            nodes: state.nodes.len(),
            tasks,
        }
    }

    fn auto_collect(&self) {
        let _ = self.inner.maybe_collect();
    }

    /// Reclaims nodes unreachable from roots, subscriptions, or View units.
    ///
    /// Reclamation only loses reuse; correctness is unaffected because a later
    /// demand reconstructs what it needs.
    pub fn collect(&self) -> crate::inner::ReclaimStats {
        self.inner.collect()
    }

    /// Reports memory held by the runtime.
    #[must_use]
    pub fn memory_report(&self) -> crate::inner::MemoryReport {
        self.inner.memory_report()
    }

    /// A structured explanation of the current graph.
    #[must_use]
    pub fn explain(&self) -> Explain {
        let state = self.inner.lock();
        let mut by_kind: FxHashMap<&'static str, usize> = FxHashMap::default();
        let mut changed_nodes = 0;
        for node in &state.nodes {
            *by_kind.entry(node.label).or_default() += 1;
            if node.changed_at == state.revision {
                changed_nodes += 1;
            }
        }
        Explain {
            revision: state.revision,
            nodes: state.nodes.len(),
            changed_nodes,
            by_kind,
            root: state.root.as_ref().map(|root| root.name),
            views: state.views.len(),
            subscriptions: state.subscriptions.len(),
            waiting_tasks: state.wait_graph.len(),
            site_creations: state.stats.site_creations.load(Ordering::Relaxed),
            site_reuses: state.stats.site_reuses.load(Ordering::Relaxed),
            invocations_evicted: state.stats.invocations_evicted.load(Ordering::Relaxed),
            invalidation_ns: state.stats.invalidation_ns.load(Ordering::Relaxed),
        }
    }

    /// Returns the provider node that currently owns a logical unit.
    #[must_use]
    pub fn provider_of(
        &self,
        unit: &reactive_core::descriptor::UnitKey,
    ) -> Option<reactive_core::ids::NodeId> {
        self.inner.provider_of(unit)
    }

    /// Installs the provider-selection policy for live definition conflicts.
    pub fn set_route_selector(&self, selector: Arc<dyn RouteSelector>) {
        *self
            .inner
            .route_selector
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(selector);
    }

    /// Captures a revisioned routing certificate for one View region.
    #[must_use]
    pub fn routing_stamp(
        &self,
        view: reactive_core::ids::ViewId,
    ) -> Option<RoutingStamp> {
        let state = self.inner.lock();
        state.regions.get(&view).map(|region| RoutingStamp {
            view,
            version: region.routing_version,
        })
    }

    /// Validates that a routing certificate still describes the region.
    ///
    /// A handover, addition, or removal of a provider invalidates it; a
    /// value-only edit does not.
    #[must_use]
    pub fn validate_routing(&self, stamp: &RoutingStamp) -> bool {
        let state = self.inner.lock();
        state
            .regions
            .get(&stamp.view)
            .is_some_and(|region| region.routing_version == stamp.version)
    }

    /// Returns a unit's routing generation.
    ///
    /// It advances when the unit's provider changes, including a handover that
    /// leaves the value equal; consumers record it alongside the value version.
    #[must_use]
    pub fn routing_generation(&self, unit: &reactive_core::descriptor::UnitKey) -> Version {
        self.inner.routing_generation(unit)
    }

    /// Whether a node identity still names a live arena slot.
    ///
    /// A reclaimed slot that has been reused fails this check even when its
    /// index matches, because the generation differs.
    #[must_use]
    pub fn is_live(&self, node: reactive_core::ids::NodeId) -> bool {
        let state = self.inner.lock();
        state
            .nodes
            .get(node.index())
            .is_some_and(|record| record.generation == node.generation() && !record.freed)
    }

    /// Returns the node cycles currently present in the wait graph.
    #[must_use]
    pub fn wait_cycles(&self) -> Vec<Vec<reactive_core::ids::NodeId>> {
        self.inner.wait_cycles()
    }

    /// Returns the current wait graph edges as `(task, awaited node)`.
    ///
    /// A task appears here while one of its attempts is suspended on another
    /// task's node; a cycle among these edges is the blocker inspector's cycle
    /// evidence.
    #[must_use]
    pub fn wait_edges(&self) -> Vec<(u64, reactive_core::ids::NodeId)> {
        let state = self.inner.lock();
        state
            .wait_graph
            .iter()
            .map(|(task, node)| (*task, *node))
            .collect()
    }

    /// Number of installed units in a View region.
    #[must_use]
    pub fn region_unit_count(&self, view: ViewId) -> usize {
        self.inner
            .lock()
            .regions
            .get(&view)
            .map_or(0, |region| region.units.len())
    }

    /// Explains why one node changed: which observations moved and which are
    /// still certified.
    ///
    /// # Panics
    ///
    /// Panics when the node identity is out of range.
    #[must_use]
    pub fn explain_node(&self, node: reactive_core::ids::NodeId) -> Explanation {
        let state = self.inner.lock();
        let record = state.node(node);
        let dependencies = record
            .trace
            .iter()
            .map(|dep| {
                let dependency = state.node(dep.node);
                DependencyExplanation {
                    node: dep.node,
                    unit: dependency.unit.as_ref().map(|unit| format!("{unit:?}")),
                    label: dependency.label,
                    recorded: dep.version,
                    current: dependency.changed_at,
                    changed: dependency.changed_at != dep.version,
                    suspect: dependency.suspect_at > dependency.verified_at,
                    recorded_routing: dep.routing,
                    current_routing: dependency.routing_at,
                    routing_changed: dependency.routing_at != dep.routing,
                }
            })
            .collect();
        Explanation {
            node,
            label: record.label,
            value_changed_at: record.changed_at,
            verified_at: record.verified_at,
            suspect_at: record.suspect_at,
            executed_at: record.executed_at,
            validated_at: record.validated_at,
            state: format!("{:?}", record.state),
            waiters: record.waiters.len(),
            scope: record.scope,
            site: record.site,
            scope_path: state.scopes.path(record.scope),
            dependencies,
        }
    }

    /// Dumps every node with its dependency structure, for diagnostics and
    /// tests.
    #[must_use]
    pub fn dump(&self) -> Vec<NodeInfo> {
        let state = self.inner.lock();
        state
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| NodeInfo {
                id: reactive_core::ids::NodeId::new(index as u32, node.generation),
                label: node.label,
                kind: format!("{:?}", node.kind),
                changed_at: node.changed_at,
                verified_at: node.verified_at,
                suspect_at: node.suspect_at,
                deps: node.trace.clone(),
                dependents: node.dependents.clone(),
                unit: node.unit.as_ref().map(|unit| format!("{unit:?}")),
                value: node
                    .value
                    .as_ref()
                    .map(|value| format!("{} bytes", std::mem::size_of_val(value.as_ref()))),
            })
            .collect()
    }

    /// Cancels the demand currently running, if any.
    ///
    /// Cancelled work never publishes.
    pub fn cancel_current(&self) {
        // Demand-scoped cancellation is installed by the scheduler; this is the
        // cooperative hook used by observers and hosts.
        let state = self.inner.lock();
        drop(state);
    }

    fn notify_subscribers(&self) -> Result<()> {
        let has_subscriptions = {
            let state = self.inner.lock();
            !state.subscriptions.is_empty()
        };
        if !has_subscriptions {
            return Ok(());
        }
        // Whole-View subscriptions and subscriptions on derived units need the
        // root brought up to date. A subscription that only watches source
        // units is already current: firing it must not rematerialize the root.
        let (has_root, needs_demand) = {
            let state = self.inner.lock();
            let needs_demand = state.subscriptions.iter().any(|subscription| {
                subscription.units.is_empty()
                    || subscription.units.iter().any(|unit| {
                        state.views.get(&unit.view).is_none_or(|descriptor| {
                            descriptor.kind == reactive_core::descriptor::ViewKind::Derived
                        })
                    })
            });
            (
                state.root.is_some() || state.general_root.is_some(),
                needs_demand,
            )
        };
        let snapshot = if has_root && needs_demand {
            self.read()?
        } else {
            self.snapshot()
        };
        let pending: Vec<PendingSubscription> = {
            let state = self.inner.lock();
            state
                .subscriptions
                .iter()
                .map(|subscription| {
                    (
                        subscription.id,
                        subscription.view,
                        subscription.callback.clone(),
                        Arc::clone(&subscription.active),
                    )
                })
                .collect()
        };
        for (id, view, callback, active) in pending {
            if !active.load(Ordering::Acquire) {
                continue;
            }
            let version = snapshot.version_of(view);
            let (should_fire, previous) = {
                let mut state = self.inner.lock();
                let Some(index) = state
                    .subscriptions
                    .iter()
                    .position(|subscription| subscription.id == id)
                else {
                    continue;
                };
                if state.subscriptions[index].units.is_empty() {
                    if version != state.subscriptions[index].last_version {
                        let previous = state.subscriptions[index].last_version;
                        state.subscriptions[index].last_version = version;
                        (true, previous)
                    } else {
                        (false, state.subscriptions[index].last_version)
                    }
                } else {
                    let units = state.subscriptions[index].units.clone();
                    let routing = state.subscriptions[index].routing;
                    let versions: Vec<reactive_core::ids::Version> = units
                        .iter()
                        .map(|unit| {
                            if routing {
                                crate::state::routing_version(&state, unit)
                            } else {
                                crate::state::unit_version(&state, unit)
                            }
                        })
                        .collect();
                    if versions != state.subscriptions[index].last_versions {
                        let previous = state.subscriptions[index].last_version;
                        state.subscriptions[index].last_version = version;
                        state.subscriptions[index].last_versions = versions;
                        (true, previous)
                    } else {
                        (false, state.subscriptions[index].last_version)
                    }
                }
            };
            if should_fire {
                match callback {
                    crate::state::Observer::Plain(callback) => callback(&snapshot),
                    crate::state::Observer::WithChanges(callback) => {
                        let summary = crate::snapshot::ChangeSummary {
                            view,
                            previous,
                            current: version,
                        };
                        callback(&snapshot, &summary);
                    }
                }
            }
        }
        Ok(())
    }
}

type PendingSubscription = (u64, ViewId, crate::state::Observer, Arc<AtomicBool>);

/// Handle for an active observation. Dropping it removes the subscription.
pub struct Subscription {
    inner: Arc<Inner>,
    id: u64,
    active: Arc<AtomicBool>,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        let mut state = self.inner.lock();
        state.subscriptions.retain(|subscription| subscription.id != self.id);
    }
}

/// How many demand tasks are queued, running, or suspended.
///
/// A demand is complete only when it is in none of these sets. The counts are
/// the runtime's progress accounting: a task that waits stays in `waiting`
/// even when every worker is idle, and it leaves only by completing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskCounts {
    /// Tasks waiting for a worker.
    pub queued: usize,
    /// Tasks with an attempt executing.
    pub running: usize,
    /// Tasks suspended on another task's node.
    pub waiting: usize,
}

/// Snapshot of runtime counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeStats {
    /// Nodes allocated over the runtime's lifetime.
    pub nodes_created: u64,
    /// Node executions.
    pub executions: u64,
    /// Trace validations that succeeded.
    pub validations: u64,
    /// Published revisions.
    pub revisions: u64,
    /// Nodes marked suspect by invalidation.
    pub invalidations: u64,
    /// Nodes currently allocated.
    pub nodes: usize,
    /// In-flight demand tasks.
    pub tasks: TaskCounts,
    /// Automatic collections triggered on demand entry.
    pub auto_collects: u64,
    /// Site nodes created on first interning.
    pub site_creations: u64,
    /// Site-node lookups that reused an existing identity.
    pub site_reuses: u64,
    /// Invocation records evicted by the cache cap.
    pub invocations_evicted: u64,
    /// Nanoseconds spent marking invalidation cones.
    pub invalidation_ns: u64,
}

/// Chooses which provider owns a unit when two live providers claim it.
///
/// The selector is the explicit routing authority for live conflicts; without
/// one, a live duplicate remains a `DuplicateDefinition` error. A selector that
/// prefers the earlier provider retracts the later definition, leaving the
/// earlier provider to restore its value on its next demand.
pub trait RouteSelector: Send + Sync + 'static {
    /// Returns the winning provider (one of `first` or `second`).
    fn select(
        &self,
        unit: &reactive_core::descriptor::UnitKey,
        first: reactive_core::ids::NodeId,
        second: reactive_core::ids::NodeId,
    ) -> reactive_core::ids::NodeId;
}

/// A revisioned routing certificate for one View region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoutingStamp {
    /// The certified View.
    pub view: reactive_core::ids::ViewId,
    /// The region's routing version at capture time.
    pub version: u64,
}

/// Why one node changed.
#[derive(Clone, Debug)]
pub struct Explanation {
    /// The explained node.
    pub node: reactive_core::ids::NodeId,
    /// Human-readable node label.
    pub label: &'static str,
    /// Revision at which the node's value last changed.
    pub value_changed_at: Version,
    /// Revision for which the value is certified.
    pub verified_at: Revision,
    /// Revision of the last invalidation.
    pub suspect_at: Revision,
    /// Revision at which the node last executed its closure.
    pub executed_at: Revision,
    /// Revision at which the node's trace was last validated or replayed.
    pub validated_at: Revision,
    /// Scheduling state (`Vacant`, `Running`, `Ready`, `Failed`).
    pub state: String,
    /// Tasks currently suspended on this node.
    pub waiters: usize,
    /// Identity scope of the node.
    pub scope: reactive_core::ids::ScopeId,
    /// Expression site within the scope.
    pub site: reactive_core::ids::SiteId,
    /// Human-readable scope ancestry, outermost first.
    pub scope_path: Vec<String>,
    /// Recorded observations, in dependency order.
    pub dependencies: Vec<DependencyExplanation>,
}

/// One observation in an [`Explanation`].
#[derive(Clone, Debug)]
pub struct DependencyExplanation {
    /// Observed node.
    pub node: reactive_core::ids::NodeId,
    /// Logical unit, when the observed node is a View unit.
    pub unit: Option<String>,
    /// Observed node label.
    pub label: &'static str,
    /// Version recorded when the observation was made.
    pub recorded: Version,
    /// Current version of the observed node.
    pub current: Version,
    /// Whether this observation moved.
    pub changed: bool,
    /// Whether the observed node is itself awaiting validation.
    pub suspect: bool,
    /// Routing generation recorded when the observation was made.
    pub recorded_routing: Version,
    /// Current routing generation of the observed node.
    pub current_routing: Version,
    /// Whether the observation's routing changed while its value stayed equal.
    pub routing_changed: bool,
}

/// One node in a diagnostic dump.
#[derive(Clone, Debug)]
pub struct NodeInfo {
    /// Node identity.
    pub id: reactive_core::ids::NodeId,
    /// Human-readable label.
    pub label: &'static str,
    /// Node kind.
    pub kind: String,
    /// Revision at which the value last changed.
    pub changed_at: Version,
    /// Revision for which the value is certified.
    pub verified_at: Revision,
    /// Revision at which the node was invalidated.
    pub suspect_at: Revision,
    /// Recorded observations.
    pub deps: Vec<reactive_core::frame::Dep>,
    /// Reverse edges.
    pub dependents: Vec<reactive_core::ids::NodeId>,
    /// Logical unit, for unit nodes.
    pub unit: Option<String>,
    /// Size description of the value.
    pub value: Option<String>,
}

/// Structured explanation of the current graph.
#[derive(Clone, Debug)]
pub struct Explain {
    /// Current revision.
    pub revision: Revision,
    /// Allocated nodes.
    pub nodes: usize,
    /// Nodes whose value changed at the current revision.
    pub changed_nodes: usize,
    /// Node counts by label.
    pub by_kind: FxHashMap<&'static str, usize>,
    /// Installed root name.
    pub root: Option<&'static str>,
    /// Registered Views.
    pub views: usize,
    /// Active subscriptions.
    pub subscriptions: usize,
    /// Tasks currently suspended on a node.
    pub waiting_tasks: usize,
    /// Site nodes created on first interning.
    pub site_creations: u64,
    /// Site-node lookups that reused an existing identity.
    pub site_reuses: u64,
    /// Invocation records evicted by the cache cap.
    pub invocations_evicted: u64,
    /// Nanoseconds spent marking invalidation cones.
    pub invalidation_ns: u64,
}

fn root_project<R: RootComponent>(value: &Value, index: usize) -> Value {
    let output = value
        .downcast_ref::<R::Output>()
        .expect("root output type mismatch");
    R::project(output, index)
}

fn root_body_eq<R: RootComponent>(left: &Value, right: &Value) -> bool {
    match (
        left.downcast_ref::<R::Output>(),
        right.downcast_ref::<R::Output>(),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Component identity of a registered descriptor.
#[must_use]
pub fn component_id<S: ComponentSpec>() -> ComponentId {
    S::ID
}


