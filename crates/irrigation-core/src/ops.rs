//! The narrow interface between generated code and the runtime.
//!
//! Everything a component body does goes through [`eval`], [`read_unit`], and
//! the output transaction's `define`. The runtime decides whether an evaluation
//! may be reused, which observations were consumed, and when staged definitions
//! commit.

use std::any::Any;
use std::sync::Arc;

use crate::error::{ComputeError, Result};
use crate::frame::{self, Frame, current_or_engine_panic};
use crate::ids::{ComponentId, NodeId, Revision, ScopeId, SiteId, Version, ViewId};
use crate::key::AnyKey;

use crate::value::{Value, value_of};

/// Operations the runtime exposes to code executing inside a computation.
///
/// The trait is object-safe so a frame can carry `Arc<dyn RuntimeOps>` without
/// knowing the concrete runtime type. Only the runtime crate implements it.
pub trait RuntimeOps: Send + Sync + 'static {
    /// The revision currently being computed.
    fn revision(&self) -> Revision;

    /// Demands `node`, executing `compute` only if the cached result cannot be
    /// reused. `eq` compares two concrete payloads for semantic equality.
    fn eval(
        &self,
        node: NodeId,
        compute: &mut dyn FnMut() -> Result<Value>,
        eq: &dyn Fn(&Value, &Value) -> bool,
    ) -> Result<Value>;

    /// Returns (creating on first use) the node for one expression site in a
    /// scope. Identity is `(scope, site)`; it never depends on evaluation order.
    fn site_node(&self, scope: ScopeId, site: SiteId) -> NodeId;

    /// Returns one expression site, attributing it to the executing node.
    ///
    /// The default ignores the node, which keeps existing implementors working.
    fn site_node_in(&self, _node: Option<NodeId>, scope: ScopeId, site: SiteId) -> NodeId {
        self.site_node(scope, site)
    }

    /// Returns (creating on first use) the node for a logical View unit.
    fn unit_node(&self, view: ViewId, key: Arc<dyn AnyKey>) -> NodeId;

    /// Returns (creating on first use) a unit node, initialized from the View's
    /// current content when it does not exist yet.
    fn unit_node_init(
        &self,
        view: ViewId,
        key: Arc<dyn AnyKey>,
        init: &mut dyn FnMut(&dyn Any) -> Option<Value>,
    ) -> NodeId;

    /// Reads a source or derived unit at the current revision, recording the
    /// observation in the current frame.
    fn read_unit(&self, node: NodeId) -> Result<Value>;

    /// Claims a writable scope for the current computation.
    ///
    /// Fails with [`ComputeError::WriteConflict`] when another live computation
    /// claims an overlapping scope.
    fn claim_write(
        &self,
        scope: crate::descriptor::WriteScope,
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()>;

    /// Claims several writable scopes atomically for the current computation.
    ///
    /// A conflict on any scope leaves the whole group unclaimed.
    fn claim_writes(
        &self,
        scopes: &[crate::descriptor::WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()>;

    /// Withdraws planned ports for the current computation.
    fn unplan_writes(&self, scopes: &[crate::descriptor::WriteScope]) -> Result<()>;

    /// Announces several ports the current computation intends to write.
    ///
    /// A plan is not a claim; it lets readers wait for the provider before the
    /// write verb runs.
    fn plan_writes(
        &self,
        scopes: &[crate::descriptor::WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()>;

    /// A stable identity for this runtime, unique among live runtimes.
    ///
    /// `NodeId` is only unique inside one state arena, so anything keyed on a
    /// thread-global registry has to match on this too: two runtimes built on one
    /// thread allocate identical ids for identical shapes.
    fn runtime_id(&self) -> u64;

    /// Records a structural precondition on the current write group.
    ///
    /// The condition was already evaluated by the caller with tracked reads.
    /// A false condition is resolved by wait, defer, or failure at commit.
    fn require_condition(
        &self,
        condition: bool,
        ports: &[crate::descriptor::UnitKey],
        description: &'static str,
    ) -> Result<()>;

    /// Records that `ports` must be absent when the current write group commits.
    ///
    /// The counterpart of [`Self::require_condition`], and the only owner of that
    /// protocol: a caller that builds the requirement itself would skip the
    /// write-arbitration check this method performs for both directions.
    fn require_absence(
        &self,
        ports: &[crate::descriptor::UnitKey],
        description: &'static str,
    ) -> Result<()>;

    /// Runs any host-installed provider for the given ports.
    ///
    /// Returns `true` when at least one provider ran. The caller re-evaluates
    /// its condition afterwards.
    fn provide_ports(&self, ports: &[crate::descriptor::UnitKey]) -> Result<bool>;

    /// Waits for a live writer that claims any of `ports`.
    ///
    /// Returns `true` when it waited, `false` when no live claim covers them.
    fn wait_for_port(&self, ports: &[crate::descriptor::UnitKey]) -> Result<bool>;

    /// Ensures a unit node exists, initializing a source unit from its View's
    /// content when it has never been written.
    fn ensure_unit(&self, unit: &crate::descriptor::UnitKey) -> Result<()>;

    /// Derives a stable child scope from a parent scope and a discriminant.
    fn child_scope(&self, parent: ScopeId, discriminant: u64, label: &'static str) -> ScopeId;

    /// Whether a field currently uses the keyed derived protocol.
    fn field_is_keyed(&self, view: crate::ids::ViewId, field: crate::ids::FieldId) -> bool;

    /// Derives a stable child scope from a typed key, comparing keys by
    /// equality instead of trusting a hash.
    fn child_scope_keyed(
        &self,
        parent: ScopeId,
        key: Arc<dyn AnyKey>,
        label: &'static str,
    ) -> ScopeId;

    /// Fails with [`ComputeError::Cancelled`] if the owning demand was
    /// cancelled.
    fn check_cancelled(&self) -> Result<()>;

    /// Maximum nesting depth the runtime accepts.
    fn max_depth(&self) -> u32;

    /// Returns the immutable content of a registered View.
    fn view_content(&self, view: ViewId) -> Option<Arc<dyn Any + Send + Sync>>;

    /// Returns the revision at which a node's value last changed.
    fn changed_at(&self, node: NodeId) -> Version;

    /// Returns the invalidation stamp of the node's last certification.
    fn validity_at(&self, node: NodeId) -> u64;

    /// Returns the monotonic version of the node's value.
    fn value_version_at(&self, node: NodeId) -> u64;

    /// Records an observation of an already-certified node.
    ///
    /// Used by projection nodes that consume a value produced earlier in the
    /// same demand; it never executes the observed node.
    fn observe(&self, node: NodeId) -> Result<()>;

    /// Records that a computation in `scope` declared `view` for definitions.
    fn declare_view(&self, scope: ScopeId, view: ViewId);

    /// Non-blocking unit read for a future receipt.
    ///
    /// Returns `Ready` when the unit can be served (certifying a clean value),
    /// or `Pending` after registering `waker` when another task is computing it.
    fn try_read_unit(
        &self,
        node: NodeId,
        waker: &std::task::Waker,
    ) -> Result<std::task::Poll<Value>>;

    /// Suspends the current demand on `node` when it is a pooled task.
    ///
    /// Returns `false` for an inline demand, which must park instead. A pooled
    /// task is marked waiting and queued when the node completes.
    fn suspend_request(&self, node: NodeId) -> bool;

    /// Registers a View namespace if it is not registered yet.
    fn register_view(&self, descriptor: &'static crate::descriptor::ViewDescriptor);

    /// Number of worker threads available for parallel regions.
    fn workers(&self) -> usize;

    /// Returns (creating on first use) the body and output nodes of a component
    /// invocation. `args_eq` compares two type-erased argument tuples.
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
    ) -> (NodeId, Vec<NodeId>);
}

/// Panic payload used to carry an engine error through ordinary user code.
///
/// User code in a component is shaped like ordinary Rust and does not thread a
/// `Result` through every expression. Engine failures (cycles, cancellation,
/// panics) abort the current execution scope instead, and the runtime converts
/// the payload back into a [`ComputeError`] at the node boundary.
#[derive(Debug)]
pub struct EnginePanic(pub ComputeError);

/// Aborts the current computation with an engine error.
pub fn panic_engine(error: ComputeError) -> ! {
    std::panic::panic_any(EnginePanic(error))
}

/// Keeps engine payloads out of the process panic hook.
///
/// Engine failures travel as panic payloads so ordinary user code does not have
/// to thread a `Result` through every expression. They are control flow, not
/// crashes: a demand that is cancelled or suspends would otherwise print a
/// backtrace. This installs a hook once that ignores [`EnginePanic`] payloads
/// and delegates every other payload to the hook that was installed before.
///
/// The runtime calls this when it is created. A hook installed afterwards
/// replaces this one, so a host that wants the filter must install it first or
/// preserve it with [`std::panic::take_hook`].
pub fn quiet_engine_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !is_engine_payload(info.payload()) {
                previous(info);
            }
        }));
    });
}

fn is_engine_payload(payload: &(dyn Any + Send)) -> bool {
    payload.downcast_ref::<EnginePanic>().is_some()
}

/// Runs `f`, converting an engine panic into an error value.
///
/// Ordinary panics are reported as [`ComputeError::Panicked`]. The error
/// conversion is out of line: this frame stays live for every nested demand,
/// so the payload/error locals must not be part of it.
pub fn catch_engine<T>(f: impl FnOnce() -> T) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => Ok(value),
        Err(payload) => Err(engine_failure(payload)),
    }
}

/// Converts one caught panic payload into the engine error it carries.
#[cold]
#[inline(never)]
fn engine_failure(payload: Box<dyn Any + Send>) -> ComputeError {
    match payload.downcast::<EnginePanic>() {
        Ok(engine) => engine.0,
        Err(payload) => ComputeError::Panicked {
            message: panic_message(payload.as_ref()),
        },
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        String::from("non-string panic payload")
    }
}

/// Evaluates one lowered expression site in the current scope.
///
/// The value type must support exact semantic equality: an equal result keeps
/// its change version and stops downstream work. Values are stored behind an
/// `Arc`, so reuse does not copy the payload.
///
/// # Panics
///
/// Panics with an engine payload when no computation is running or when the
/// runtime reports a cycle, cancellation, or another engine failure. The
/// runtime catches the payload at the node boundary.
pub fn eval<T, F>(site: SiteId, f: F) -> T
where
    T: PartialEq + Clone + Send + Sync + 'static,
    F: FnOnce() -> T,
{
    let frame = current_or_engine_panic("eval");
    let node = frame.runtime().site_node(frame.scope(), site);
    eval_node_in(&frame, node, f)
}

/// Evaluates a known node. Used by operators that already computed identity.
pub fn eval_node<T, F>(node: NodeId, f: F) -> T
where
    T: PartialEq + Clone + Send + Sync + 'static,
    F: FnOnce() -> T,
{
    let frame = current_or_engine_panic("eval_node");
    eval_node_in(&frame, node, f)
}

/// Evaluates a node with an explicit runtime handle.
///
/// Used by parallel regions: the spawned work has no ambient frame of its own,
/// but the node it evaluates installs one for the closure body.
pub fn eval_node_with<T, F>(runtime: &Arc<dyn RuntimeOps>, node: NodeId, f: F) -> T
where
    T: PartialEq + Clone + Send + Sync + 'static,
    F: FnOnce() -> T,
{
    let mut compute = {
        let mut f = Some(f);
        move || {
            let f = f.take().unwrap_or_else(|| {
                panic_engine(ComputeError::internal(
                    "node compute closure was invoked more than once",
                ))
            });
            Ok(value_of(f()))
        }
    };
    let eq =
        |left: &Value, right: &Value| match (left.downcast_ref::<T>(), right.downcast_ref::<T>()) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        };
    match runtime.eval(node, &mut compute, &eq) {
        Ok(value) => match value.downcast_ref::<T>() {
            Some(value) => value.clone(),
            None => panic_engine(ComputeError::TypeMismatch {
                expected: std::any::type_name::<T>(),
            }),
        },
        Err(error) => panic_engine(error),
    }
}

/// Evaluates a node through the ambient frame's runtime.
///
/// Delegates: only the runtime handle differs from [`eval_node_with`], and the body it
/// used to copy decided three things -- that a compute closure runs once, how `T` is
/// compared for the cutoff, and how a result of the wrong type is reported. Two copies of
/// that were two places for a node to be evaluated under different rules.
fn eval_node_in<T, F>(frame: &Frame, node: NodeId, f: F) -> T
where
    T: PartialEq + Clone + Send + Sync + 'static,
    F: FnOnce() -> T,
{
    eval_node_with(frame.runtime(), node, f)
}

/// Reads a logical View unit and records the observation.
///
/// # Panics
///
/// Panics with an engine payload outside a computation or on engine failure.
pub fn read_unit<T>(node: NodeId) -> T
where
    T: Clone + Send + Sync + 'static,
{
    let frame = current_or_engine_panic("read_unit");
    let value = match frame.runtime().read_unit(node) {
        Ok(value) => value,
        Err(error) => panic_engine(error),
    };
    match value.downcast_ref::<T>() {
        Some(value) => value.clone(),
        None => panic_engine(ComputeError::TypeMismatch {
            expected: std::any::type_name::<T>(),
        }),
    }
}

/// Reads a unit through an already-created node, recording the observation.
pub fn read_unit_value(node: NodeId) -> Value {
    let frame = current_or_engine_panic("read_unit_value");
    match frame.runtime().read_unit(node) {
        Ok(value) => value,
        Err(error) => panic_engine(error),
    }
}

/// Stages an item to commit atomically with the currently executing node.
///
/// Definitions become visible only when the node commits its value, trace, and
/// routing change together. A per-iteration scope frame delegates to the
/// computation that owns the commit, so a definition made inside a nested
/// component body is not dropped.
pub fn stage<T: std::any::Any + Send + Sync>(item: T) {
    let _ = current_or_engine_panic("stage");
    frame::stage(item);
}

/// Runs `f` in a child identity scope, used for loops and per-iteration work.
pub fn scope<R>(discriminant: u64, label: &'static str, f: impl FnOnce() -> R) -> R {
    frame::in_scope(discriminant, label, f)
}

/// Returns the current revision, or zero outside a computation.
#[must_use]
pub fn revision() -> Revision {
    frame::current().map_or(0, |frame| frame.runtime().revision())
}

/// The identity of the entity one emission owns.
///
/// A component body's expressions are lowered to computation sites, and a site is
/// interned per `(invocation scope, expression site)`. That makes this value
/// stable across re-executions of the same invocation, unique among invocations,
/// and retired with the site: when the body stops reaching the expression, the
/// site leaves the trace and the units it wrote are retracted.
///
/// The value is opaque. Use it as an entity id in the field being emitted into,
/// never as a durable or cross-runtime key: like every identity the runtime
/// allocates, it is only meaningful inside the runtime that produced it.
///
/// # Panics
///
/// Panics with an engine payload outside a computation.
#[must_use]
pub fn emission_id() -> u64 {
    u64::from(frame::current_or_engine_panic("emission_id").node().0)
}

/// The shared site for `key`: one site node at the runtime root, interned by key
/// equality.
///
/// Every invocation that names the same key evaluates the same node, so a value
/// written there has one owner and one lifecycle. The node is a dependency of
/// every caller, which is what keeps it alive while they exist and retracts its
/// definitions once the last caller stops demanding it.
///
/// A caller's payload must be a function of the key: the first evaluation stores
/// the value, and later callers validate it instead of re-writing it.
///
/// # Panics
///
/// Panics with an engine payload outside a computation.
#[must_use]
pub fn shared_node<K>(key: K) -> NodeId
where
    K: std::any::Any + std::hash::Hash + Eq + Send + Sync + std::fmt::Debug + 'static,
{
    let frame = frame::current_or_engine_panic("shared_node");
    let scope =
        frame
            .runtime()
            .child_scope_keyed(ScopeId::ROOT, crate::key::any_key(key), "shared entity");
    frame.runtime().site_node(scope, SiteId(0))
}

#[cfg(test)]
#[path = "../tests/unit/ops/tests.rs"]
mod tests;
