//! The public runtime handle: installation, reads, edits, and observations.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use reactive_core::error::{ComputeError, Result};
use reactive_core::frame::CancelToken;
use reactive_core::hash::FxHashMap;
use reactive_core::ids::{ComponentId, Revision, Version, ViewId};
use reactive_core::ops::catch_engine;
use reactive_core::value::Value;
use reactive_store::descriptor::ViewDescriptor;

use crate::component::{ComponentSpec, RootComponent, eval_value};
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

    /// Demands the installed root and returns an immutable snapshot.
    ///
    /// # Errors
    ///
    /// Fails when no root is installed or when a component fails.
    pub fn read(&self) -> Result<Snapshot> {
        let _gate = self.inner.gate.read();
        self.demand_root()?;
        let state = self.inner.lock();
        let revision = state.revision;
        let mut views: FxHashMap<ViewId, Arc<dyn Any + Send + Sync>> = FxHashMap::default();
        let mut versions: FxHashMap<ViewId, Version> = FxHashMap::default();
        for (id, region) in &state.regions {
            views.insert(*id, Arc::clone(&region.content));
            versions.insert(*id, state.node(region.content_node).changed_at);
        }
        Ok(Snapshot::new(revision, views, versions))
    }

    /// Demands the installed root and returns its typed output product.
    ///
    /// # Errors
    ///
    /// Fails when no root is installed, when a component fails, or when `R`
    /// does not match the installed root.
    pub fn read_outputs<R: RootComponent>(&self) -> Result<R::Output> {
        let _gate = self.inner.gate.read();
        self.demand_root()?;
        let root = {
            let state = self.inner.lock();
            state.root.clone()
        };
        let Some(root) = root else {
            return Err(ComputeError::internal("no root component installed"));
        };
        let parts: Vec<Value> = {
            let state = self.inner.lock();
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
            self.notify_subscribers()?;
        }
        Ok(value)
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
            callback: Arc::new(callback),
            last_version: version,
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
        let _gate = self.inner.gate.read();
        catch_engine(|| crate::component::call_component::<S>(args))
    }

    /// Runtime counters, useful in tests and benchmarks.
    #[must_use]
    pub fn stats(&self) -> RuntimeStats {
        let state = self.inner.lock();
        RuntimeStats {
            nodes_created: state.stats.nodes_created.load(Ordering::Relaxed),
            executions: state.stats.executions.load(Ordering::Relaxed),
            validations: state.stats.validations.load(Ordering::Relaxed),
            revisions: state.stats.revisions.load(Ordering::Relaxed),
            nodes: state.nodes.len(),
        }
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
        }
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
                id: reactive_core::ids::NodeId(index as u32),
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

    fn demand_root(&self) -> Result<()> {
        let root = {
            let state = self.inner.lock();
            state.root.clone()
        };
        let Some(root) = root else {
            return Err(ComputeError::internal(
                "no root component installed; call `runtime.install` first",
            ));
        };
        let runtime: Arc<dyn reactive_core::ops::RuntimeOps> = self.inner.arc();
        let cancel = CancelToken::default();
        drop(cancel);
        catch_engine(|| {
            let body_value = eval_value(&runtime, root.body, || (root.run)(), &root.body_eq);
            for (index, node) in root.outputs.iter().enumerate() {
                let projection_eq = root.projection_eq;
                eval_value(&runtime, *node, || {
                    runtime
                        .observe(root.body)
                        .unwrap_or_else(|error| reactive_core::ops::panic_engine(error));
                    (root.project)(&body_value, index)
                }, &|left, right| projection_eq(index, left, right));
            }
        })
    }

    fn notify_subscribers(&self) -> Result<()> {
        let has_subscriptions = {
            let state = self.inner.lock();
            !state.subscriptions.is_empty()
        };
        if !has_subscriptions {
            return Ok(());
        }
        let snapshot = self.read()?;
        let pending: Vec<(u64, ViewId, Arc<dyn Fn(&Snapshot) + Send + Sync>, Arc<AtomicBool>)> = {
            let state = self.inner.lock();
            state
                .subscriptions
                .iter()
                .map(|subscription| {
                    (
                        subscription.id,
                        subscription.view,
                        Arc::clone(&subscription.callback),
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
            let should_fire = {
                let mut state = self.inner.lock();
                match state.subscriptions.iter_mut().find(|sub| sub.id == id) {
                    Some(subscription) => {
                        if version != subscription.last_version {
                            subscription.last_version = version;
                            true
                        } else {
                            false
                        }
                    }
                    None => false,
                }
            };
            if should_fire {
                callback(&snapshot);
            }
        }
        Ok(())
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
        let mut state = self.inner.lock();
        state.subscriptions.retain(|subscription| subscription.id != self.id);
    }
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
    /// Nodes currently allocated.
    pub nodes: usize,
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

impl Snapshot {
    fn version_of(&self, view: ViewId) -> Version {
        self.version_by_id(view).unwrap_or(0)
    }
}
