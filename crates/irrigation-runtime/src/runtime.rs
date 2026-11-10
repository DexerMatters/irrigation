//! The public runtime handle: installation, reads, edits, and observations.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use irrigation_core::descriptor::ViewDescriptor;
use irrigation_core::error::{ComputeError, Result};
use irrigation_core::frame::CancelToken;
use irrigation_core::ids::{ComponentId, Revision, Version, ViewId};
use irrigation_core::ops::{RuntimeOps, catch_engine};
use irrigation_core::value::Value;

use crate::component::{ComponentSpec, RootComponent};
use crate::edit::Edit;
use crate::inner::{Inner, RuntimeConfig};
use crate::snapshot::{EditableView, Snapshot, ViewSchema};
use crate::state::{NodeKind, NodeRecord, RootRecord, State, SubscriptionRecord};
use crate::task::DemandCause;

/// The public entry point to one isolated incremental runtime.
///
/// A runtime owns all identity: View namespaces, component definitions, and
/// every memoized node. Two runtimes are fully independent, which is what makes
/// tests and multi-project hosts possible without process-global state.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
    /// Serializes observer refreshes up to staging.
    ///
    /// `last_version` is compared and advanced under this guard, so refreshes
    /// stage their deliveries in version order. A refresh releases it before
    /// dialing callbacks — a callback that edits starts a refresh of its own —
    /// so dialing is owned by the delivery queue instead. It is distinct from
    /// the metadata lock: a refresh holds it across a demand.
    notifications: Arc<parking_lot::Mutex<()>>,
    /// Staged callback deliveries, and the one thread allowed to dial them.
    deliveries: Arc<DeliveryQueue>,
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
            notifications: Arc::new(parking_lot::Mutex::new(())),
            deliveries: Arc::new(DeliveryQueue::default()),
        }
    }

    /// Creates a runtime that reports events to `observer`.
    ///
    /// The observer is fixed for the runtime's lifetime. Without this
    /// constructor no observer exists and no event is built.
    #[must_use]
    #[cfg(feature = "observe")]
    pub fn with_observer(
        config: RuntimeConfig,
        observer: Arc<dyn crate::observe::RuntimeObserver>,
    ) -> Self {
        // The observer is wrapped so its level is enforced where events leave
        // the kernel, not only where a consumer happens to check it.
        let observer: Arc<dyn crate::observe::RuntimeObserver> =
            Arc::new(crate::observe::LevelFilter::new(observer));
        Self {
            inner: Inner::new_observed(config, observer),
            notifications: Arc::new(parking_lot::Mutex::new(())),
            deliveries: Arc::new(DeliveryQueue::default()),
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
    pub fn install<R: RootComponent>(&self, root: R) -> Result<()> {
        self.install_root(root).map(|_| ())
    }

    /// Installs a root component and returns a typed handle for its outputs.
    ///
    /// The handle saves repeating the root type at every read and makes a
    /// mismatched root a type error instead of a runtime one.
    ///
    /// # Errors
    ///
    /// Fails when a root is already installed or when a View is missing.
    pub fn install_root<R: RootComponent>(&self, _root: R) -> Result<Installed<R>> {
        let _gate = self.inner.gate.write();
        for descriptor in R::VIEWS {
            self.inner.register_view(descriptor);
        }
        let mut state = self.inner.lock();
        check_root_slot(&state)?;
        check_component_id(&state, R::ID)?;
        let mut body_record = NodeRecord::empty("root body", NodeKind::InvocationBody);
        body_record.label = "root body";
        let body = state.alloc(body_record);
        let mut outputs = Vec::with_capacity(R::ARITY);
        for index in 0..R::ARITY {
            let mut record = NodeRecord::empty("root output", NodeKind::InvocationOutput);
            record.site = irrigation_core::ids::SiteId(index as u32);
            outputs.push(state.alloc(record));
        }
        state
            .scope_components
            .insert(irrigation_core::ids::ScopeId::ROOT, R::ID);
        state.component_names.insert(R::ID, R::NAME);
        state.root = Some(RootRecord {
            component: R::ID,
            name: R::NAME,
            body,
            outputs,
            run: || irrigation_core::value::value_of(R::run()),
            project: root_project::<R>,
            projection_eq: R::projection_eq,
            body_eq: root_body_eq::<R>,
            reconstruct: |parts| irrigation_core::value::value_of(R::reconstruct(parts)),
        });
        drop(state);
        Ok(Installed {
            runtime: self.clone(),
            marker: std::marker::PhantomData,
        })
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
        self.install_general_root(component).map(|_| ())
    }

    /// Installs a general component as the root and returns its handle.
    ///
    /// # Errors
    ///
    /// Fails when a root is already installed.
    pub fn install_general_root<T: crate::general::GeneralComponent>(
        &self,
        component: T,
    ) -> Result<InstalledGeneral<T>> {
        let component_id = component.component_id();
        let output_views = component.output_views();
        let _gate = self.inner.gate.write();
        for descriptor in T::VIEWS.iter().copied().chain(output_views.iter().copied()) {
            self.inner.register_view(descriptor);
        }
        let mut state = self.inner.lock();
        check_root_slot(&state)?;
        check_component_id(&state, component_id)?;
        for descriptor in &output_views {
            if descriptor.kind != irrigation_core::descriptor::ViewKind::Derived {
                return Err(ComputeError::internal(format!(
                    "general output view `{}` must be derived",
                    descriptor.name
                )));
            }
            if state.producers.contains_key(&descriptor.id) {
                return Err(ComputeError::internal(format!(
                    "view `{}` already has a producer",
                    descriptor.name
                )));
            }
        }
        state.component_names.insert(component_id, T::NAME);
        // The root body runs in the root scope, so the root must own it: the
        // planned-provider check and a Strict write claim both attribute a
        // write through its scope's component, and `install<R>` records the
        // same edge for an ordinary root.
        state
            .scope_components
            .insert(irrigation_core::ids::ScopeId::ROOT, component_id);
        state.generals.push(Arc::new(component));
        let index = state.generals.len() - 1;
        let mut record = NodeRecord::empty("general component", NodeKind::InvocationBody);
        record.label = "general component";
        record.general = Some(index);
        // The component owns this node: the board draws it as a component, and
        // its reads and definitions are attributed to it.
        record.component = Some(component_id);
        record.scope = irrigation_core::ids::ScopeId::ROOT;
        let node = state.alloc(record);
        for descriptor in &output_views {
            state.producers.insert(descriptor.id, node);
        }
        state.general_root = Some(node);
        Ok(InstalledGeneral {
            runtime: self.clone(),
            marker: std::marker::PhantomData,
        })
    }

    /// Installs a general component that owns one or more output Views.
    ///
    /// This is the producer path for custom components: the component is a
    /// normal graph participant, evaluated lazily when a consumer reads one of
    /// its output units. It is not a root and does not block root installation.
    /// Output Views must be derived, and exactly one producer may own a View.
    pub fn install_general_component<T: crate::general::GeneralComponent>(
        &self,
        component: T,
    ) -> Result<()> {
        self.install_general_producer(component).map(|_| ())
    }

    /// Installs a general component producer and returns its handle.
    ///
    /// # Errors
    ///
    /// Fails when a View already has a producer or the component is invalid.
    pub fn install_general_producer<T: crate::general::GeneralComponent>(
        &self,
        component: T,
    ) -> Result<InstalledProducer<T>> {
        let component_id = component.component_id();
        let output_views = component.output_views();
        let _gate = self.inner.gate.write();
        for descriptor in T::VIEWS.iter().copied().chain(output_views.iter().copied()) {
            self.inner.register_view(descriptor);
        }
        if output_views.is_empty() {
            return Err(ComputeError::internal(
                "a non-root general component must declare output views",
            ));
        }
        let mut state = self.inner.lock();
        check_component_id(&state, component_id)?;
        for descriptor in &output_views {
            if descriptor.kind != irrigation_core::descriptor::ViewKind::Derived {
                return Err(ComputeError::internal(format!(
                    "general output view `{}` must be derived",
                    descriptor.name
                )));
            }
            if state.producers.contains_key(&descriptor.id) {
                return Err(ComputeError::internal(format!(
                    "view `{}` already has a producer",
                    descriptor.name
                )));
            }
        }
        state.component_names.insert(component_id, T::NAME);
        state.generals.push(Arc::new(component));
        let index = state.generals.len() - 1;
        // Every component owns a scope, so attribution never depends on which
        // component was installed last. The id is consumed here, not only
        // minted: an install must never share a scope with a later install or
        // with the first invocation interned in the root scope.
        let key = state.next_invocation + 1;
        state.next_invocation = key;
        let scope = state.scopes.child(irrigation_core::ids::ScopeId::ROOT, key);
        state.scope_components.insert(scope, component_id);
        let mut record = NodeRecord::empty("general component producer", NodeKind::InvocationBody);
        record.label = "general component producer";
        record.general = Some(index);
        record.component = Some(component_id);
        record.scope = scope;
        let node = state.alloc(record);
        for descriptor in &output_views {
            state.producers.insert(descriptor.id, node);
        }
        Ok(InstalledProducer {
            runtime: self.clone(),
            marker: std::marker::PhantomData,
        })
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
        inner.demand(cancel, DemandCause::HostRead, move || {
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
            .demand(
                CancelToken::default(),
                DemandCause::HostSnapshot,
                move || Ok(task.capture_snapshot()),
            )
            .expect("snapshot demand cannot fail")
    }

    /// Captures committed View content without creating a demand task.
    ///
    /// Unlike [`Runtime::snapshot`], this cannot evaluate a node, so a
    /// diagnostic page that calls it never manufactures the work it observes.
    #[must_use]
    pub fn inspect_snapshot(&self) -> Snapshot {
        self.inner.capture_snapshot()
    }

    /// A unit's change version, when it is installed on this runtime.
    ///
    /// Read-only, for validating a renderer's unit claims.
    #[must_use]
    #[cfg(feature = "observe")]
    pub fn unit_changed_at(
        &self,
        unit: &irrigation_core::descriptor::UnitKey,
    ) -> Option<irrigation_core::ids::Version> {
        // A host edit installs into the global table, a provider into the
        // region, and the committed content defines a unit that has no cell
        // yet; `unit_versions` answers all three through the one version
        // owner, so this question only has one owner.
        self.inner
            .unit_versions(std::slice::from_ref(unit))
            .into_iter()
            .next()
            .flatten()
    }

    /// Several units' change versions under one lock.
    #[must_use]
    #[cfg(feature = "observe")]
    pub fn unit_versions(
        &self,
        units: &[irrigation_core::descriptor::UnitKey],
    ) -> Vec<Option<irrigation_core::ids::Version>> {
        self.inner.unit_versions(units)
    }

    /// Every registered View's metadata at the current revision.
    ///
    /// Read-only: it never demands a View.
    #[must_use]
    #[cfg(feature = "observe")]
    pub fn view_facts(&self) -> Vec<crate::observe::ViewFacts> {
        self.inner.view_facts()
    }

    /// One field's entities in the View's own order.
    #[must_use]
    #[cfg(feature = "observe")]
    pub fn field_entities(
        &self,
        view: irrigation_core::ids::ViewId,
        field: irrigation_core::ids::FieldId,
    ) -> Vec<crate::observe::EntityFacts> {
        self.inner.field_entities(view, field)
    }

    /// Every installed unit of one View, with its change revision.
    #[must_use]
    #[cfg(feature = "observe")]
    pub fn view_units(
        &self,
        view: irrigation_core::ids::ViewId,
    ) -> Vec<(
        irrigation_core::descriptor::UnitKey,
        irrigation_core::ids::Version,
    )> {
        self.inner.view_units(view)
    }

    /// A View's descriptor.
    #[must_use]
    #[cfg(feature = "observe")]
    pub fn descriptor(
        &self,
        view: irrigation_core::ids::ViewId,
    ) -> Option<&'static irrigation_core::descriptor::ViewDescriptor> {
        self.inner.descriptor(view)
    }

    /// A View's content, untyped.
    #[must_use]
    #[cfg(feature = "observe")]
    pub fn raw_content(
        &self,
        view: irrigation_core::ids::ViewId,
    ) -> Option<irrigation_core::value::Value> {
        self.inner.raw_content(view)
    }

    /// A unit's type-erased value.
    #[must_use]
    #[cfg(feature = "observe")]
    pub fn unit_value(
        &self,
        unit: &irrigation_core::descriptor::UnitKey,
    ) -> Option<irrigation_core::value::Value> {
        self.inner.unit_value(unit)
    }

    /// Every component the runtime knows, with its read and written Views.
    ///
    /// Read-only. Registration is what makes a component appear: root, general,
    /// or invoked.
    #[must_use]
    #[cfg(feature = "observe")]
    pub fn component_facts(&self) -> Vec<crate::observe::ComponentFacts> {
        self.inner.component_facts()
    }

    /// Projects the node arena under a bounded, read-only query.
    #[must_use]
    #[cfg(feature = "observe")]
    pub fn probe(&self, query: &crate::observe::GraphQuery) -> crate::observe::GraphFacts {
        self.inner.probe(query)
    }

    /// Explains one node, or returns `None` when it was reclaimed.
    ///
    /// A node can outlive the nodes it recorded: reclamation retains a provider
    /// for the units it defines while freeing what its body read. Unlike
    /// [`Runtime::explain_node`] this never panics on a stale identity, whether
    /// it names the node or one of its observations.
    #[must_use]
    pub fn try_explain_node(&self, node: irrigation_core::ids::NodeId) -> Option<Explanation> {
        // The identity check, the explanation read, and every dependency read
        // share one state guard: a collection between them could reclaim a slot
        // and make the read below panic on an identity this method promises to
        // tolerate.
        explain_locked(&self.inner.lock(), node)
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
        self.read_as(cancel, DemandCause::HostRead)
    }

    /// Demands the root, recording `cause` for the observation stream.
    fn read_as(&self, cancel: CancelToken, cause: DemandCause) -> Result<Snapshot> {
        let inner = Arc::clone(&self.inner);
        let task = Arc::clone(&self.inner);
        let token = cancel.clone();
        inner.demand(cancel, cause, move || {
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
    pub fn call_projection<S: ComponentSpec>(&self, args: S::Args, index: usize) -> Result<Value> {
        let inner = Arc::clone(&self.inner);
        let task = Arc::clone(&self.inner);
        let cancel = CancelToken::default();
        let token = cancel.clone();
        inner.demand(cancel, DemandCause::HostCall, move || {
            let runtime: Arc<dyn irrigation_core::ops::RuntimeOps> = task.arc();
            #[allow(clippy::arc_with_non_send_sync)]
            let frame = Arc::new(irrigation_core::frame::Frame::scoped(
                runtime,
                irrigation_core::ids::NodeId::NONE,
                irrigation_core::ids::ScopeId::ROOT,
                0,
                token.clone(),
            ));
            let _guard = irrigation_core::frame::FrameGuard::push(frame);
            let result = catch_engine(|| {
                crate::component::call_component_projection::<S>(args.clone(), index)
            });
            match result {
                Ok(output) => {
                    let revision = task.current_revision();
                    task.finish_demand(revision, &[])?;
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
        inner.demand(cancel, DemandCause::HostRead, move || {
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
                            irrigation_core::ops::panic_engine(ComputeError::internal(
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
        inner.demand(cancel, DemandCause::HostRead, move || {
            task.demand_root_projection(&token, index)
        })
    }

    /// Applies one View editor in a transaction and returns its result.
    ///
    /// This is the common single-View case of [`Runtime::edit`]; the editor
    /// owns the transaction for the closure and commits when it returns.
    ///
    /// # Errors
    ///
    /// Fails when the transaction cannot be committed.
    pub fn edit_view<V: EditableView>(&self, f: impl FnOnce(&mut V::Editor<'_>)) -> Result<()> {
        self.edit_view_with::<V, ()>(|editor| f(editor))
    }

    /// Applies one View editor and returns the closure's result.
    ///
    /// # Errors
    ///
    /// Fails when the transaction cannot be committed.
    pub fn edit_view_with<V: EditableView, T>(
        &self,
        f: impl FnOnce(&mut V::Editor<'_>) -> T,
    ) -> Result<T> {
        self.edit(|edit| edit.with_view::<V, T>(f))
    }

    /// Applies a transaction and returns the closure's result.
    ///
    /// Only the inputs are mutated; derived values are repaired on demand or by
    /// active subscriptions.
    ///
    /// # Errors
    ///
    /// Fails when the transaction cannot be committed, including when it writes
    /// a unit a derived View's provider owns.
    pub fn edit<T>(&self, f: impl FnOnce(&mut Edit) -> T) -> Result<T> {
        // An edit from inside a computation would wait for a demand's read
        // permit that the same thread holds, and an edit nested in another edit
        // would wait for the write side it already holds. Neither can open the
        // gate, so both are refused.
        if irrigation_core::frame::current().is_some() || crate::gate::in_edit(self.inner.local_key)
        {
            return Err(ComputeError::ReentrantUse {
                operation: "edit",
                reason: "the calling thread is inside a component body, which holds a read permit for its whole demand",
            });
        }
        let (value, changed) = {
            let _gate = self.inner.gate.write();
            // Marked so a demand issued from the closure refuses instead of
            // waiting for this thread's own write side.
            let _edit = crate::gate::enter_edit(self.inner.local_key);
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

    /// Registers a callback that receives the typed snapshot handle of `V`.
    ///
    /// The View is registered on demand, so an observation-only host does not
    /// need a separate `register_view` call.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe_view<V: ViewSchema>(
        &self,
        callback: impl Fn(V::Snapshot<'_>) + Send + Sync + 'static,
    ) -> Result<Subscription> {
        self.inner.register_view(V::DESCRIPTOR);
        self.observe::<V>(move |snapshot| callback(snapshot.view::<V>()))
    }

    /// Like [`Runtime::observe_view`], with the changed View versions.
    ///
    /// # Errors
    ///
    /// Fails when the View cannot be registered.
    pub fn observe_view_with_changes<V: ViewSchema>(
        &self,
        callback: impl Fn(V::Snapshot<'_>, &crate::snapshot::ChangeSummary) + Send + Sync + 'static,
    ) -> Result<Subscription> {
        self.inner.register_view(V::DESCRIPTOR);
        self.observe_with_changes::<V>(move |snapshot, changes| {
            callback(snapshot.view::<V>(), changes);
        })
    }

    /// Observes a set of logical units and fires only when one of them moves.
    ///
    /// An empty set observes the whole View content. The callback receives the
    /// current snapshot; use the View's typed accessors to read the watched
    /// entries. A watched unit that exists in the committed content but has no
    /// cell yet is seeded with the revision that content changed at, which is
    /// the revision its first read will stamp. The delivery comparison asks the
    /// same question of the same state, so a unit that has not moved is never
    /// reported — including the edit that precedes its first read.
    ///
    /// # Errors
    ///
    /// Fails when the View is not registered.
    pub fn observe_units<V: ViewSchema>(
        &self,
        units: Vec<irrigation_core::descriptor::UnitKey>,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> Result<Subscription> {
        // An empty set is the whole-View subscription, so it shares `observe`'s
        // seeding of the View's content version. Seeding it at zero fired once
        // for a View that had not changed.
        if units.is_empty() {
            return self.observe::<V>(callback);
        }
        self.observe_units_of(V::DESCRIPTOR, units, callback)
    }

    /// Observes units of a View named by a descriptor.
    ///
    /// The typed field handles use this to avoid a generic View type at the
    /// call site. The View is registered if it is not known yet.
    ///
    /// # Errors
    ///
    /// Fails when the units belong to another View.
    pub fn observe_units_of(
        &self,
        descriptor: &'static ViewDescriptor,
        units: Vec<irrigation_core::descriptor::UnitKey>,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> Result<Subscription> {
        self.inner.register_view(descriptor);
        let mut state = self.inner.lock();
        let versions: Vec<irrigation_core::ids::Version> = units
            .iter()
            .map(|unit| crate::state::unit_version(&state, unit))
            .collect();
        let id = state.next_subscription;
        state.next_subscription += 1;
        let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        state.subscriptions.push(SubscriptionRecord {
            id,
            view: descriptor.id,
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
        field: irrigation_core::ids::FieldId,
        key: K,
        callback: impl Fn(&Snapshot) + Send + Sync + 'static,
    ) -> Result<Subscription>
    where
        K: std::any::Any + std::hash::Hash + Eq + Send + Sync + std::fmt::Debug + 'static,
    {
        let key = irrigation_core::key::any_key(key);
        let view = V::view_id();
        let units = vec![
            irrigation_core::descriptor::UnitKey::entity(
                view,
                field,
                irrigation_core::descriptor::UnitKind::Membership,
                std::sync::Arc::clone(&key),
            ),
            irrigation_core::descriptor::UnitKey::entity(
                view,
                field,
                irrigation_core::descriptor::UnitKind::Value,
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
        unit: irrigation_core::descriptor::UnitKey,
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

    /// Number of worker threads available to derived computation.
    ///
    /// The configured number, including zero: a workerless runtime is supported --
    /// every demand runs inline on the calling thread -- and reporting `1` for it
    /// told callers a pool existed. Parallel-region sizing already treats anything
    /// at or below one worker as sequential, so no caller needs the floor.
    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.inner.config.workers
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
        inner.demand(cancel, DemandCause::HostCall, move || {
            let runtime: Arc<dyn irrigation_core::ops::RuntimeOps> = task.arc();
            // Frames are thread-local execution state; see the note in `eval`.
            #[allow(clippy::arc_with_non_send_sync)]
            let frame = Arc::new(irrigation_core::frame::Frame::scoped(
                Arc::clone(&runtime),
                irrigation_core::ids::NodeId::NONE,
                irrigation_core::ids::ScopeId::ROOT,
                0,
                token.clone(),
            ));
            let _guard = irrigation_core::frame::FrameGuard::push(frame);
            let result = catch_engine(|| crate::component::call_component::<S>(args.clone()));
            match result {
                Ok(output) => {
                    // A call is a demand root: the groups it deferred are drained
                    // and pending definition conflicts are resolved before the
                    // result is handed back, so a called provider neither loses a
                    // deferred definition nor silently shares one unit.
                    let revision = task.current_revision();
                    task.finish_demand(revision, &[])?;
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
            validation_attempts: state.stats.validation_attempts.load(Ordering::Relaxed),
            validation_failures: state.stats.validation_failures.load(Ordering::Relaxed),
            execution_failures: state.stats.execution_failures.load(Ordering::Relaxed),
            cutoffs: state.stats.cutoffs.load(Ordering::Relaxed),
            suspensions: state.stats.suspensions.load(Ordering::Relaxed),
            demands: state.stats.demands.load(Ordering::Relaxed),
            notifications: state.stats.notifications.load(Ordering::Relaxed),
            no_change_runs: state.stats.no_change_runs.load(Ordering::Relaxed),
            write_claims: state.stats.write_claims.load(Ordering::Relaxed),
            write_conflicts: state.stats.write_conflicts.load(Ordering::Relaxed),
            deferred_writes: state.stats.deferred_writes.load(Ordering::Relaxed),
            demand_retries: state.stats.demand_retries.load(Ordering::Relaxed),
            reader_waits: state.stats.reader_waits.load(Ordering::Relaxed),
            reader_plan_waits: state.stats.reader_plan_waits.load(Ordering::Relaxed),
            planned_ports: state.stats.planned_ports.load(Ordering::Relaxed),
            unplanned_ports: state.stats.unplanned_ports.load(Ordering::Relaxed),
            content_applies: state.stats.content_applies.load(Ordering::Relaxed),
            content_restatements: state.stats.content_restatements.load(Ordering::Relaxed),
            content_projections: state.stats.content_projections.load(Ordering::Relaxed),
            content_batches: state.stats.content_batches.load(Ordering::Relaxed),
            content_commits: state.stats.content_commits.load(Ordering::Relaxed),
            observe_payloads_skipped: state.stats.observe_payloads_skipped.load(Ordering::Relaxed),
            source_content_applies: state.stats.source_content_applies.load(Ordering::Relaxed),
            source_equality_checks: state.stats.source_equality_checks.load(Ordering::Relaxed),
            structure_refreshes: state.stats.structure_refreshes.load(Ordering::Relaxed),
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
        let stats = self.inner.collect();
        #[cfg(feature = "observe")]
        self.inner.emit(crate::observe::RuntimeEvent::Collected {
            nodes: u32::try_from(stats.nodes_reclaimed).unwrap_or(u32::MAX),
            scopes: u32::try_from(stats.scopes_reclaimed).unwrap_or(u32::MAX),
        });
        stats
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
        Explain {
            revision: state.revision,
            nodes: state.nodes.len(),
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
        unit: &irrigation_core::descriptor::UnitKey,
    ) -> Option<irrigation_core::ids::NodeId> {
        self.inner.provider_of(unit)
    }

    /// Reports aggregate identity and reuse diagnostics.
    #[must_use]
    pub fn identity_report(&self) -> IdentityReport {
        let state = self.inner.lock();
        IdentityReport {
            site_creations: state.stats.site_creations.load(Ordering::Relaxed),
            site_reuses: state.stats.site_reuses.load(Ordering::Relaxed),
            scopes: state.scopes.len(),
            invocations: state.invocations.values().map(Vec::len).sum(),
            invocations_evicted: state.stats.invocations_evicted.load(Ordering::Relaxed),
            unit_nodes: state.units.len(),
            regions: state.regions.len(),
        }
    }

    /// Captures the current provider recipe for one View region.
    #[must_use]
    pub fn routing_recipe(&self, view: irrigation_core::ids::ViewId) -> Option<RoutingRecipe> {
        self.inner.routing_recipe(view)
    }

    /// Plans the provider component for units before any value exists.
    ///
    /// A definition of a planned unit from any other component is rejected.
    pub fn plan_route(
        &self,
        entries: impl IntoIterator<
            Item = (
                irrigation_core::descriptor::UnitKey,
                irrigation_core::ids::ComponentId,
            ),
        >,
    ) {
        let mut state = self.inner.lock();
        for (unit, component) in entries {
            let view = unit.view;
            state.route_plans.insert(unit, component);
            if let Some(region) = state.regions.get_mut(&view) {
                region.routing_version = region.routing_version.wrapping_add(1);
            }
        }
    }

    /// Installs the provider-selection policy for live definition conflicts.
    pub fn set_route_selector(&self, selector: Arc<dyn RouteSelector>) {
        // The replaced selector is dropped outside the guard. It is host code,
        // and host code is allowed to call back into the runtime -- a teardown
        // that re-installs itself is the natural shape -- so dropping it under a
        // non-reentrant mutex it may want is the same mistake `Subscription::drop`
        // already avoids.
        let previous = {
            let mut slot = self.inner.route_selector.lock();
            slot.replace(selector)
        };
        drop(previous);
    }

    /// Installs a demand recipe for missing ports of one View.
    ///
    /// The provider runs when a structural requirement names a port of this View
    /// and no live writer can satisfy it. It is host code: it decides how to
    /// invoke the producing component.
    pub fn install_port_provider(&self, view: ViewId, provider: Arc<dyn PortProvider>) {
        // As above: the replaced provider is a host object, so it is dropped
        // after the registry guard is released.
        let previous = {
            let mut providers = self.inner.port_providers.lock();
            providers.insert(view, provider)
        };
        drop(previous);
    }

    /// Captures a revisioned routing certificate for one View region.
    #[must_use]
    pub fn routing_stamp(&self, view: irrigation_core::ids::ViewId) -> Option<RoutingStamp> {
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
    pub fn routing_generation(&self, unit: &irrigation_core::descriptor::UnitKey) -> Version {
        self.inner.routing_generation(unit)
    }

    /// Whether a node identity still names a live arena slot.
    ///
    /// A reclaimed slot that has been reused fails this check even when its
    /// index matches, because the generation differs.
    #[must_use]
    pub fn is_live(&self, node: irrigation_core::ids::NodeId) -> bool {
        live_node(&self.inner.lock(), node).is_some()
    }

    /// Returns the node cycles currently present in the wait graph.
    #[must_use]
    pub fn wait_cycles(&self) -> Vec<Vec<irrigation_core::ids::NodeId>> {
        self.inner.wait_cycles()
    }

    /// Returns the current wait graph edges as `(task, awaited node)`.
    ///
    /// A task appears here while one of its attempts is suspended on another
    /// task's node; a cycle among these edges is the blocker inspector's cycle
    /// evidence.
    #[must_use]
    pub fn wait_edges(&self) -> Vec<(u64, irrigation_core::ids::NodeId)> {
        let state = self.inner.lock();
        state
            .wait_graph
            .iter()
            .map(|(task, node)| (*task, *node))
            .collect()
    }

    /// Explains why one node changed: which observations moved and which are
    /// still certified.
    ///
    /// # Panics
    ///
    /// Panics when the node identity does not name a live node, or when one of
    /// the observations it recorded was reclaimed.
    #[must_use]
    pub fn explain_node(&self, node: irrigation_core::ids::NodeId) -> Explanation {
        explain_locked(&self.inner.lock(), node).unwrap_or_else(|| {
            panic!("node {node:?} or one of its recorded observations was reclaimed")
        })
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
                id: irrigation_core::ids::NodeId::new(index as u32, node.generation),
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

    fn notify_subscribers(&self) -> Result<()> {
        let has_subscriptions = {
            let state = self.inner.lock();
            !state.subscriptions.is_empty()
        };
        if !has_subscriptions {
            return Ok(());
        }
        // One refresh stages at a time. The snapshot and every `last_version`
        // comparison and advance happen under this guard, so refreshes decide
        // what to deliver in version order. Callbacks run after it is released
        // — a callback that edits starts a refresh of its own — so dialing is
        // serialized by the delivery queue: this refresh may find that a newer
        // one already advanced a subscription, and the dial rechecks it.
        let deliveries = {
            let _refresh = self.notifications.lock();
            // Whole-View subscriptions and subscriptions on units a provider
            // publishes need their demand brought up to date: such a unit's
            // version only moves when its provider runs. A subscription that
            // only watches host-written units is already current: firing it
            // must not rematerialize the root. Ownership answers that question —
            // a provider may define a unit in a source View as well as a derived
            // one. The View kind stays as the conservative arm for a derived
            // View's not-yet-installed units, which have no owner record yet but
            // are still a producer's to publish.
            let needs_demand = {
                let state = self.inner.lock();
                state.subscriptions.iter().any(|subscription| {
                    subscription.units.is_empty()
                        || subscription.units.iter().any(|unit| {
                            state.unit_owned_by_live_provider(unit)
                                || state.views.get(&unit.view).is_none_or(|descriptor| {
                                    descriptor.kind
                                        == irrigation_core::descriptor::ViewKind::Derived
                                })
                        })
                })
            };
            if needs_demand {
                self.refresh_demanded()
            } else {
                // Nothing is demanded, so no task pins the revision. The read
                // side of the gate is the pin for this staging pass: it cannot
                // nest, because this branch runs no demand.
                let _staging = self.inner.gate.read();
                Ok(self.inner.capture_snapshot_with(stage_deliveries))
            }
        }?;
        self.queue_deliveries(deliveries);
        Ok(())
    }

    /// Brings every subscription current inside one demand.
    ///
    /// The attempt holds the revision gate's read side from before the demand
    /// until it closes, and the snapshot and the unit versions it is paired with
    /// are read in one locked pass inside it. An edit therefore cannot land
    /// between the halves of the pair — a delivery always describes one revision.
    ///
    /// What is demanded is the subscription's own subject, not only the root: a
    /// watched unit's producer, or the View's producer for a whole-View
    /// subscription, runs before the root does. Without that, a producer the
    /// root does not transitively read never runs and its subscription never
    /// fires.
    fn refresh_demanded(&self) -> Result<Vec<Delivery>> {
        let inner = Arc::clone(&self.inner);
        let task = Arc::clone(&self.inner);
        let token = CancelToken::default();
        let cancel = token.clone();
        inner.demand(cancel, DemandCause::ObservationRefresh, move || {
            let revision = task.current_revision();
            let (producers, units) = {
                let state = task.lock();
                refresh_targets(&state)
            };
            for producer in &producers {
                demand_producer(&task, *producer)?;
            }
            for unit in units {
                task.ensure_unit(&unit)?;
            }
            let has_root = {
                let state = task.lock();
                state.root.is_some() || state.general_root.is_some()
            };
            if has_root {
                task.demand_root(&token)?;
            } else {
                // A producer-only runtime has no `demand_root` to end the
                // demand, so the refresh drains what its producers deferred
                // here. They are the roots a replay has to force.
                task.finish_demand(revision, &producers)?;
            }
            Ok(task.capture_snapshot_with(|state, snapshot| {
                debug_assert_eq!(
                    snapshot.revision(),
                    revision,
                    "an edit landed inside the refresh demand's gate permit"
                );
                stage_deliveries(state, snapshot)
            }))
        })
    }

    /// Appends deliveries staged by one refresh and dials them unless another
    /// refresh already owns the dial.
    ///
    /// It never waits for the owner: a callback the owner is running may be
    /// waiting for this refresh's caller to return, which is how a host
    /// synchronizes two threads from its callbacks.
    fn queue_deliveries(&self, deliveries: Vec<Delivery>) {
        {
            let mut queue = self.deliveries.state.lock();
            queue.pending.extend(deliveries);
        }
        self.drain_deliveries();
    }

    /// Dials queued deliveries, or returns at once when another thread is
    /// already dialing.
    ///
    /// Ownership is the serialization point. While one thread owns the queue no
    /// other refresh can check a subscription and invoke its callback, so a
    /// delivery that a newer refresh superseded is rechecked here instead of
    /// being dialed out of order.
    fn drain_deliveries(&self) {
        {
            let mut queue = self.deliveries.state.lock();
            if queue.owner.is_some() || queue.pending.is_empty() {
                return;
            }
            queue.owner = Some(std::thread::current().id());
        }
        let _owner = DrainOwner {
            queue: &self.deliveries,
        };
        loop {
            let delivery = {
                let mut queue = self.deliveries.state.lock();
                let Some(delivery) = queue.pending.pop_front() else {
                    queue.owner = None;
                    return;
                };
                delivery
            };
            self.dial(delivery);
        }
    }

    /// Invokes one staged delivery unless a later refresh already moved past it.
    fn dial(&self, delivery: Delivery) {
        if !delivery.active.load(Ordering::Acquire) {
            return;
        }
        // Only the queue's owner reaches this check and the callback it guards,
        // so a refresh that staged a newer delivery cannot pass its own check
        // between them; it has only appended that delivery for this owner to
        // dial next.
        let superseded = {
            let state = self.inner.lock();
            let current = state
                .subscriptions
                .iter()
                .find(|subscription| subscription.id == delivery.subscription);
            let superseded = current.is_none_or(|subscription| {
                if delivery.units.is_empty() {
                    subscription.last_version != delivery.current
                } else {
                    subscription.last_versions != delivery.units
                }
            });
            if !superseded {
                state.stats.notifications.fetch_add(1, Ordering::Relaxed);
            }
            superseded
        };
        if superseded {
            return;
        }
        #[cfg(feature = "observe")]
        self.inner
            .emit(crate::observe::RuntimeEvent::SubscriptionNotified {
                view: delivery.view,
                previous: delivery.previous,
                current: delivery.current,
            });
        match delivery.callback {
            crate::state::Observer::Plain(callback) => callback(&delivery.snapshot),
            crate::state::Observer::WithChanges(callback) => {
                let summary = crate::snapshot::ChangeSummary {
                    view: delivery.view,
                    previous: delivery.previous,
                    current: delivery.current,
                };
                callback(&delivery.snapshot, &summary);
            }
        }
    }
}

/// One callback a refresh decided to deliver, captured while `last_version`
/// advanced.
///
/// It carries the snapshot it was staged against and what it advanced the
/// subscription to, so dialing can tell whether a later refresh already moved
/// past it.
struct Delivery {
    subscription: u64,
    view: ViewId,
    callback: crate::state::Observer,
    snapshot: Arc<Snapshot>,
    active: Arc<AtomicBool>,
    previous: Version,
    current: Version,
    units: Vec<Version>,
}

/// Decides what one captured snapshot owes each subscription and advances it.
///
/// `state` and `snapshot` come from the same locked pass: the content the
/// callback will read and the unit versions that decide whether it is owed a
/// callback describe one revision, so a concurrent publish cannot pair a
/// version from one state with content from another.
fn stage_deliveries(state: &mut State, snapshot: &Arc<Snapshot>) -> Vec<Delivery> {
    let mut deliveries: Vec<Delivery> = Vec::new();
    for index in 0..state.subscriptions.len() {
        let (id, view, callback, active) = {
            let subscription = &state.subscriptions[index];
            (
                subscription.id,
                subscription.view,
                subscription.callback.clone(),
                Arc::clone(&subscription.active),
            )
        };
        if !active.load(Ordering::Acquire) {
            continue;
        }
        let version = snapshot.version_of(view);
        let (moved, versions) = {
            let subscription = &state.subscriptions[index];
            if subscription.units.is_empty() {
                (version != subscription.last_version, Vec::new())
            } else {
                let versions: Vec<Version> = subscription
                    .units
                    .iter()
                    .map(|unit| {
                        if subscription.routing {
                            crate::state::routing_version(state, unit)
                        } else {
                            crate::state::unit_version(state, unit)
                        }
                    })
                    .collect();
                (versions != subscription.last_versions, versions)
            }
        };
        if !moved {
            continue;
        }
        let previous = {
            let subscription = &mut state.subscriptions[index];
            let previous = subscription.last_version;
            subscription.last_version = version;
            subscription.last_versions.clone_from(&versions);
            previous
        };
        deliveries.push(Delivery {
            subscription: id,
            view,
            callback,
            snapshot: Arc::clone(snapshot),
            active,
            previous,
            current: version,
            units: versions,
        });
    }
    deliveries
}

/// The producers and units one refresh must demand to bring its subscriptions
/// current.
///
/// A whole-View subscription watches the View's content, so its subject is the
/// View's producer; a host-written View has none, and its content is already
/// committed when the refresh runs. Every watched unit is listed as well: the
/// producer of its View is what can move it, and asking for the unit is the
/// same route a read takes. A View the refresh never finds a producer for is
/// brought current by the root, or is already current.
fn refresh_targets(
    state: &State,
) -> (
    Vec<irrigation_core::ids::NodeId>,
    Vec<irrigation_core::descriptor::UnitKey>,
) {
    let mut producers = Vec::new();
    let mut units = Vec::new();
    for subscription in &state.subscriptions {
        if subscription.units.is_empty() {
            push_producer(state, &mut producers, subscription.view);
            continue;
        }
        for unit in &subscription.units {
            push_producer(state, &mut producers, unit.view);
            if !units.contains(unit) {
                units.push(unit.clone());
            }
        }
    }
    (producers, units)
}

/// Adds a View's producer to `producers` once.
fn push_producer(state: &State, producers: &mut Vec<irrigation_core::ids::NodeId>, view: ViewId) {
    if let Some(producer) = state.producers.get(&view)
        && !producers.contains(producer)
    {
        producers.push(*producer);
    }
}

/// Demands one producer for a refresh that has no unit of its own to read.
///
/// A unit subscription reaches its producer through
/// [`crate::ops::RuntimeOps::ensure_unit`]; a whole-View subscription watches
/// content, so the refresh evaluates the producer body directly. The value is
/// discarded: staging compares the versions the run published.
fn demand_producer(task: &Inner, producer: irrigation_core::ids::NodeId) -> Result<()> {
    let eq = {
        let state = task.lock();
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
    let mut compute = || {
        Err(ComputeError::internal(
            "a general component producer does not use a compute closure",
        ))
    };
    task.eval(producer, &mut compute, &eq).map(|_| ())
}

/// Staged deliveries and the thread that owns dialing them.
#[derive(Default)]
struct DeliveryQueue {
    state: parking_lot::Mutex<DeliveryQueueState>,
}

#[derive(Default)]
struct DeliveryQueueState {
    pending: VecDeque<Delivery>,
    /// The thread dialing, if any. A refresh that arrives while it is set only
    /// appends.
    owner: Option<std::thread::ThreadId>,
}

/// Releases dial ownership when a host callback panics.
///
/// Without it the unwinding thread would stay the owner and every later refresh
/// could only append to the queue.
struct DrainOwner<'a> {
    queue: &'a DeliveryQueue,
}

impl Drop for DrainOwner<'_> {
    fn drop(&mut self) {
        let mut queue = self.queue.state.lock();
        if queue.owner == Some(std::thread::current().id()) {
            queue.owner = None;
        }
    }
}

/// A typed handle to a general component installed as the root.
pub struct InstalledGeneral<T: crate::general::GeneralComponent> {
    runtime: Runtime,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T: crate::general::GeneralComponent> Clone for InstalledGeneral<T> {
    fn clone(&self) -> Self {
        Self {
            runtime: self.runtime.clone(),
            marker: std::marker::PhantomData,
        }
    }
}

impl<T: crate::general::GeneralComponent> std::fmt::Debug for InstalledGeneral<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstalledGeneral")
            .field("component", &T::NAME)
            .finish()
    }
}

impl<T: crate::general::GeneralComponent> InstalledGeneral<T> {
    /// Demands the general root and returns its typed output.
    ///
    /// # Errors
    ///
    /// Fails when the component fails.
    pub fn read(&self) -> Result<T::Output> {
        self.runtime.read_outputs_general::<T>()
    }

    /// The runtime this component was installed on.
    #[must_use]
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// The current input revision.
    #[must_use]
    pub fn revision(&self) -> Revision {
        self.runtime.revision()
    }
}

/// A typed handle to an installed general component producer.
pub struct InstalledProducer<T: crate::general::GeneralComponent> {
    runtime: Runtime,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T: crate::general::GeneralComponent> Clone for InstalledProducer<T> {
    fn clone(&self) -> Self {
        Self {
            runtime: self.runtime.clone(),
            marker: std::marker::PhantomData,
        }
    }
}

impl<T: crate::general::GeneralComponent> std::fmt::Debug for InstalledProducer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstalledProducer")
            .field("component", &T::NAME)
            .finish()
    }
}

impl<T: crate::general::GeneralComponent> InstalledProducer<T> {
    /// The runtime this producer was installed on.
    #[must_use]
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// The current input revision.
    #[must_use]
    pub fn revision(&self) -> Revision {
        self.runtime.revision()
    }
}

/// A typed handle to an installed root component.
///
/// `Installed` carries the root type, so reads and projections need no
/// turbofish and cannot name a different root than the one installed.
pub struct Installed<R: RootComponent> {
    runtime: Runtime,
    marker: std::marker::PhantomData<fn() -> R>,
}

impl<R: RootComponent> Clone for Installed<R> {
    fn clone(&self) -> Self {
        Self {
            runtime: self.runtime.clone(),
            marker: std::marker::PhantomData,
        }
    }
}

impl<R: RootComponent> std::fmt::Debug for Installed<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Installed")
            .field("root", &R::NAME)
            .field("revision", &self.runtime.revision())
            .finish()
    }
}

impl<R: RootComponent> Installed<R> {
    /// Demands the root and returns its typed output product.
    ///
    /// # Errors
    ///
    /// Fails when a component fails.
    pub fn read(&self) -> Result<R::Output> {
        self.runtime.read_outputs::<R>()
    }

    /// Demands the root's typed output with a cancellation token.
    ///
    /// # Errors
    ///
    /// Returns [`ComputeError::Cancelled`] when the token was cancelled.
    pub fn read_with_cancel(&self, cancel: CancelToken) -> Result<R::Output> {
        self.runtime.read_outputs_with_cancel::<R>(cancel)
    }

    /// Demands one output projection.
    ///
    /// # Errors
    ///
    /// Fails when the index is out of range or the demand fails.
    pub fn projection(&self, index: usize) -> Result<Value> {
        self.runtime.read_projection::<R>(index)
    }

    /// The current input revision.
    #[must_use]
    pub fn revision(&self) -> Revision {
        self.runtime.revision()
    }

    /// The runtime this root was installed on.
    #[must_use]
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }
}

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
        // Removing the record drops the host callback, so it happens after the
        // guard: a destructor may take the metadata lock like any other host
        // code, and re-taking it here would deadlock. `remove` keeps the dial
        // order of the remaining subscriptions stable.
        let removed = {
            let mut state = self.inner.lock();
            state
                .subscriptions
                .iter()
                .position(|subscription| subscription.id == self.id)
                .map(|index| state.subscriptions.remove(index))
        };
        drop(removed);
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
    /// Trace validations attempted, successful or not.
    pub validation_attempts: u64,
    /// Trace validations rejected at a moved observation.
    pub validation_failures: u64,
    /// Closure attempts that ended in failure or cancellation.
    pub execution_failures: u64,
    /// Successful commits whose value was equal (cutoff).
    pub cutoffs: u64,
    /// Pooled attempts that suspended on another task's node.
    pub suspensions: u64,
    /// Demand tasks that can evaluate, started over the runtime's lifetime.
    pub demands: u64,
    /// Subscription callbacks that fired.
    pub notifications: u64,
    /// General-component runs elided because no observed unit moved.
    pub no_change_runs: u64,
    /// Write scopes claimed.
    pub write_claims: u64,
    /// Overlapping live claims rejected.
    pub write_conflicts: u64,
    /// Write groups deferred to the end of a demand.
    pub deferred_writes: u64,
    /// Demand attempts restarted by commit-time validation.
    pub demand_retries: u64,
    /// Reader waits on a live writer claim.
    pub reader_waits: u64,
    /// Reader waits on a planned provider.
    pub reader_plan_waits: u64,
    /// Planned ports registered.
    pub planned_ports: u64,
    /// Planned ports withdrawn before their claim.
    pub unplanned_ports: u64,
    /// Content values rebuilt because a definition unit actually moved.
    pub content_applies: u64,
    /// Equal definition restatements that skipped content application.
    pub content_restatements: u64,
    /// Changed units that move only a projection, not field content.
    pub content_projections: u64,
    /// Batched content applications performed by definition commits.
    pub content_batches: u64,
    /// Whole-content values published, one call per field commit.
    ///
    /// A write group that retracts or installs many units of one field must
    /// publish that field's content once; one commit per unit makes the group
    /// quadratic in the field's size. This is the counter that shows which one
    /// it is.
    pub content_commits: u64,
    /// Node-commit payloads not built because no observer wanted node detail.
    pub observe_payloads_skipped: u64,
    /// Source content installed from one changed content-moving unit.
    pub source_content_applies: u64,
    /// Source content replacements that needed whole-content equality.
    pub source_equality_checks: u64,
    /// Structure projections refreshed after a commit.
    pub structure_refreshes: u64,
}

/// A host-installed recipe that can produce a missing port on demand.
pub trait PortProvider: Send + Sync + 'static {
    /// Runs the producer for `port`, if it can.
    fn provide(&self, port: &irrigation_core::descriptor::UnitKey) -> Result<()>;
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
        unit: &irrigation_core::descriptor::UnitKey,
        first: irrigation_core::ids::NodeId,
        second: irrigation_core::ids::NodeId,
    ) -> irrigation_core::ids::NodeId;
}

/// Aggregate identity diagnostics for a runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdentityReport {
    /// Site nodes interned for the first time.
    pub site_creations: u64,
    /// Site-node lookups that reused an existing identity.
    pub site_reuses: u64,
    /// Live interned scopes.
    pub scopes: usize,
    /// Retained component invocations.
    pub invocations: usize,
    /// Invocations evicted by the cache cap.
    pub invocations_evicted: u64,
    /// Installed logical units.
    pub unit_nodes: usize,
    /// Registered View regions.
    pub regions: usize,
}

/// A captured provider recipe for one View region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutingRecipe {
    /// The routing revision this recipe was captured at.
    pub stamp: RoutingStamp,
    /// One provider per installed logical unit, unsorted.
    pub providers: Vec<(
        irrigation_core::descriptor::UnitKey,
        irrigation_core::ids::NodeId,
    )>,
    /// Planned provider components for units that do not have a value yet.
    pub planned: Vec<(
        irrigation_core::descriptor::UnitKey,
        irrigation_core::ids::ComponentId,
    )>,
}

/// A [`RouteSelector`] that prefers the providers captured in a recipe.
pub struct RecipeSelector {
    preferences: std::collections::HashMap<
        irrigation_core::descriptor::UnitKey,
        irrigation_core::ids::NodeId,
    >,
    fallback_second: bool,
}

impl RecipeSelector {
    /// Builds a selector from a captured recipe, preferring the later claim
    /// for units the recipe does not name.
    #[must_use]
    pub fn new(recipe: &RoutingRecipe) -> Self {
        Self {
            preferences: recipe.providers.iter().cloned().collect(),
            fallback_second: true,
        }
    }
}

impl std::fmt::Debug for RecipeSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecipeSelector")
            .field("preferences", &self.preferences.len())
            .finish_non_exhaustive()
    }
}

impl RouteSelector for RecipeSelector {
    fn select(
        &self,
        unit: &irrigation_core::descriptor::UnitKey,
        first: irrigation_core::ids::NodeId,
        second: irrigation_core::ids::NodeId,
    ) -> irrigation_core::ids::NodeId {
        match self.preferences.get(unit) {
            Some(&preferred) if preferred == first => first,
            Some(&preferred) if preferred == second => second,
            _ if self.fallback_second => second,
            _ => first,
        }
    }
}

/// A revisioned routing certificate for one View region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoutingStamp {
    /// The certified View.
    pub view: irrigation_core::ids::ViewId,
    /// The region's routing version at capture time.
    pub version: u64,
}

/// Why one node changed.
#[derive(Clone, Debug)]
pub struct Explanation {
    /// The explained node.
    pub node: irrigation_core::ids::NodeId,
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
    pub scope: irrigation_core::ids::ScopeId,
    /// Expression site within the scope.
    pub site: irrigation_core::ids::SiteId,
    /// Human-readable scope ancestry, outermost first.
    pub scope_path: Vec<String>,
    /// Recorded observations, in dependency order.
    pub dependencies: Vec<DependencyExplanation>,
}

/// One observation in an [`Explanation`].
#[derive(Clone, Debug)]
pub struct DependencyExplanation {
    /// Observed node.
    pub node: irrigation_core::ids::NodeId,
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
    pub id: irrigation_core::ids::NodeId,
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
    pub deps: Vec<irrigation_core::frame::Dep>,
    /// Reverse edges.
    pub dependents: Vec<irrigation_core::ids::NodeId>,
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

/// Builds an explanation from state already guarded by the metadata lock.
///
/// Both explanation entry points must read the node and its dependencies from
/// one guard; a re-lock would let a concurrent collection reclaim them. The
/// trace is resolved without the panicking accessor: a node that still defines
/// units is retained while the observations its body recorded are freed, so a
/// recorded dependency can already be stale when the explanation is built.
fn explain_locked(state: &State, node: irrigation_core::ids::NodeId) -> Option<Explanation> {
    let record = live_node(state, node)?;
    let dependencies = record
        .trace
        .iter()
        .map(|dep| {
            live_node(state, dep.node).map(|dependency| DependencyExplanation {
                node: dep.node,
                unit: dependency.unit.as_ref().map(|unit| format!("{unit:?}")),
                label: dependency.label,
                recorded: dep.version,
                current: dependency.value_version,
                changed: dependency.value_version != dep.version,
                suspect: !dependency.has_valid_trace(),
                recorded_routing: dep.routing,
                current_routing: dependency.routing_at,
                routing_changed: dependency.routing_at != dep.routing,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Explanation {
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
    })
}

/// The record for `id`, or `None` when its slot was reclaimed or reused.
fn live_node(state: &State, id: irrigation_core::ids::NodeId) -> Option<&NodeRecord> {
    let record = state.nodes.get(id.index())?;
    (record.generation == id.generation() && !record.freed).then_some(record)
}

/// Rejects a second root on the runtime's one root slot.
///
/// An ordinary root and a general root share that slot: only one of them is ever
/// demanded, so accepting both would leave one of them dead.
fn check_root_slot(state: &State) -> Result<()> {
    if state.root.is_some() || state.general_root.is_some() {
        return Err(ComputeError::internal(
            "a root component is already installed on this runtime",
        ));
    }
    Ok(())
}

/// Rejects a component identity that another general component already owns.
///
/// The table holds general roots and producers alike, so a root cannot be
/// installed under an identity a producer already answers to.
fn check_component_id(state: &State, id: ComponentId) -> Result<()> {
    // The root is a live component too: its identity is in
    // `scope_components[ROOT]`, `root.component` and `component_names`, and the
    // arbitration rules that read a component id -- plan authority, and the
    // sibling exception a forced retraction may take over by -- cannot tell two
    // components apart that share one. Scanning only the installed producers let
    // a general component adopt the root's id, overwrite its name, and inherit
    // both privileges over the root's units.
    let taken = state.generals.iter().any(|general| general.id() == id)
        || state.root.as_ref().is_some_and(|root| root.component == id);
    if taken {
        return Err(ComputeError::internal("component id is already installed"));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    #[cfg(feature = "observe")]
    use irrigation_core::descriptor::{UnitKey, UnitKind};
    use irrigation_core::frame::Dep;
    #[cfg(feature = "observe")]
    use irrigation_core::ids::{FieldId, ViewId};

    use crate::state::{NodeKind, NodeRecord};

    /// A body that still defines units is retained while the observations it
    /// recorded are reclaimed; explaining it must report the loss, not panic.
    #[test]
    fn explaining_tolerates_a_reclaimed_observation() {
        let runtime = crate::Runtime::new();
        let (body, dependency) = {
            let mut state = runtime.inner.lock();
            let dependency = state.alloc(NodeRecord::empty("site", NodeKind::Site));
            let mut stale = NodeRecord::empty("body", NodeKind::InvocationBody);
            stale.trace.push(Dep {
                node: dependency,
                version: 3,
                routing: 1,
                validity: 2,
            });
            let body = state.alloc(stale);
            // `collect` retains a provider for the definitions it installed but
            // frees the nodes its trace names, exactly as this does.
            state.node_mut(dependency).freed = true;
            (body, dependency)
        };
        assert!(
            runtime.try_explain_node(dependency).is_none(),
            "a reclaimed identity explains to nothing"
        );
        assert!(
            runtime.try_explain_node(body).is_none(),
            "a live node with a reclaimed observation explains to nothing"
        );
    }

    /// The same trace with its observation intact still explains.
    #[test]
    fn explaining_a_live_observation_still_explains() {
        let runtime = crate::Runtime::new();
        let (body, dependency) = {
            let mut state = runtime.inner.lock();
            let dependency = state.alloc(NodeRecord::empty("site", NodeKind::Site));
            let mut record = NodeRecord::empty("body", NodeKind::InvocationBody);
            record.trace.push(Dep {
                node: dependency,
                version: 3,
                routing: 1,
                validity: 2,
            });
            (state.alloc(record), dependency)
        };
        let explanation = runtime
            .try_explain_node(body)
            .expect("a live node with a live observation explains");
        assert_eq!(explanation.dependencies.len(), 1);
        assert_eq!(explanation.dependencies[0].node, dependency);
    }

    #[test]
    #[cfg(feature = "observe")]
    fn a_host_edited_unit_has_one_change_version() {
        let runtime = crate::Runtime::new();
        let unit = UnitKey::field(ViewId(0x5151), FieldId(0), UnitKind::Value);
        // A host edit installs the unit in the global table, not in its region.
        runtime
            .edit(|edit| edit.set(unit.clone(), 5u32))
            .expect("host edit");
        assert_eq!(
            runtime.unit_changed_at(&unit),
            Some(1),
            "the host edit installed the unit"
        );
        assert_eq!(
            runtime.unit_versions(std::slice::from_ref(&unit)),
            vec![Some(1)],
            "the two questions about one unit must agree"
        );
    }
}
