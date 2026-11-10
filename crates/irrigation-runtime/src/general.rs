//! `GeneralComponent`: custom incremental algorithms under the runtime's
//! scheduling, dependency, snapshot, and publication rules.
//!
//! Ordinary components are lowered from ordinary-looking Rust. A
//! `GeneralComponent` is the deliberate escape hatch for algorithms that need
//! their own retained state: an incremental parser, a dynamic graph index, or a
//! deletion-aware solver.
//!
//! The contract:
//!
//! * `initialize` runs on first demand and returns a replacement state plus an
//!   output product;
//! * `update` runs when a retained dependency changes and receives a
//!   [`ChangeBatch`] describing exactly which observed units moved;
//! * the replacement state, output value, dependency trace, and any staged
//!   definitions commit atomically, so an observer never sees a half-updated
//!   algorithm;
//! * the default [`DependencyUpdates::Reset`] policy reinitializes instead of
//!   calling `update`, which is always sound;
//! * old state stays valid if evaluation fails or is cancelled.

use std::any::Any;
use std::sync::Arc;

use irrigation_core::hash::{FxHashMap, FxHashSet};
use std::hash::Hash;

use irrigation_core::descriptor::{UnitKey, UnitKind, WriteScope};
use irrigation_core::error::{ComputeError, Result};
use irrigation_core::frame;
use irrigation_core::ids::{FieldId, Revision, ViewId};
use irrigation_core::key::any_key;
use irrigation_core::ops::RuntimeOps;
use irrigation_core::value::{Value, value_of};

use crate::inner::DefinitionDraft;

/// How a component handles requested-dependency invalidation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencyUpdates {
    /// Reinitialize from scratch when an observed dependency changes.
    ///
    /// This is the default because it is always sound.
    Reset,
    /// Call `update` with the change batch instead.
    ///
    /// The implementation accepts the obligation to handle every delivered
    /// receipt and invalidated scope.
    Incremental,
}

/// One observed unit whose version moved.
#[derive(Clone, Debug)]
pub struct Change {
    /// The logical unit that changed.
    pub unit: UnitKey,
    /// The version the previous run recorded.
    pub previous: u64,
    /// The current version.
    pub current: u64,
    /// Whether the unit's provider changed while its value stayed equal.
    pub routing_changed: bool,
}

/// The changes delivered to one `update` call.
///
/// The batch is a handle, not a snapshot: the runtime records what the previous
/// run observed and materializes the changed subset the first time `changes()`
/// is called. By then the body has already read what it cares about, so the
/// batch describes the values this run is about to see instead of the values
/// the previous run saw. This is what makes delivery uniform across component
/// kinds: no runtime-side ordering walk is needed to keep it accurate.
#[derive(Clone, Default)]
pub struct ChangeBatch {
    changes: Arc<std::sync::OnceLock<Vec<Change>>>,
    source: Option<Arc<dyn Fn() -> Vec<Change> + Send + Sync>>,
}

impl std::fmt::Debug for ChangeBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangeBatch")
            .field("changes", &self.changes())
            .finish()
    }
}

impl ChangeBatch {
    /// Builds a batch from observed changes.
    #[must_use]
    pub fn new(changes: Vec<Change>) -> Self {
        Self {
            changes: Arc::new(std::sync::OnceLock::from(changes)),
            source: None,
        }
    }

    /// Builds a batch that computes its contents on first access.
    pub(crate) fn lazy(source: impl Fn() -> Vec<Change> + Send + Sync + 'static) -> Self {
        Self {
            changes: Arc::new(std::sync::OnceLock::new()),
            source: Some(Arc::new(source)),
        }
    }

    /// The changes, in dependency order.
    #[must_use]
    pub fn changes(&self) -> &[Change] {
        self.changes.get_or_init(|| {
            self.source
                .as_ref()
                .map_or_else(Vec::new, |source| source())
        })
    }

    /// Returns `true` when nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes().is_empty()
    }

    /// Number of changed units.
    #[must_use]
    pub fn len(&self) -> usize {
        self.changes().len()
    }
}

/// The context handed to `initialize` and `update`.
///
/// Reads record observations in the enclosing computation, so a `GeneralCx`
/// read is tracked exactly like a generated View read.
pub struct GeneralCx {
    runtime: Arc<dyn RuntimeOps>,
    revision: Revision,
    scope: Option<u64>,
    unit_scopes: FxHashMap<UnitKey, u64>,
    retained: FxHashSet<u64>,
    allowed_output_views: Option<FxHashSet<irrigation_core::ids::ViewId>>,
    definition_error: Option<String>,
    content_updates: Vec<(irrigation_core::ids::ViewId, Arc<dyn Any + Send + Sync>)>,
    retractions: Vec<UnitKey>,
    /// Units this run defined or retracted; claimed as one batch after the body.
    pending_claims: Vec<UnitKey>,
    restate_all: bool,
}

impl std::fmt::Debug for GeneralCx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneralCx")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl GeneralCx {
    pub(crate) fn new(runtime: Arc<dyn RuntimeOps>, revision: Revision) -> Self {
        Self {
            runtime,
            revision,
            scope: None,
            unit_scopes: FxHashMap::default(),
            retained: FxHashSet::default(),
            allowed_output_views: None,
            definition_error: None,
            content_updates: Vec::new(),
            retractions: Vec::new(),
            pending_claims: Vec::new(),
            restate_all: false,
        }
    }

    /// Restates every definition of this run.
    ///
    /// Units this run does not stage are retracted, which is the pre-write
    /// semantics. Prefer [`OutputTxn::retract`]: restating turns every run into
    /// a full publication.
    pub fn replace_all(&mut self) {
        self.restate_all = true;
    }

    pub(crate) fn take_pending_claims(&mut self) -> Vec<UnitKey> {
        std::mem::take(&mut self.pending_claims)
    }

    pub(crate) fn set_definition_error(&mut self, reason: String) {
        self.definition_error = Some(reason);
    }

    pub(crate) fn take_definition_mode(&mut self) -> (Vec<UnitKey>, bool) {
        (
            std::mem::take(&mut self.retractions),
            std::mem::replace(&mut self.restate_all, false),
        )
    }

    /// Stages a replacement of a View's immutable content.
    ///
    /// Content is a write like any definition, so the same `OUTPUT_VIEWS`
    /// authorization applies and an unregistered View is refused here instead of
    /// being dropped at commit. The replacement is applied with the rest of the
    /// write group, after the group can no longer be rejected.
    pub fn set_content(
        &mut self,
        view: irrigation_core::ids::ViewId,
        content: Arc<dyn Any + Send + Sync>,
    ) {
        if !self.allows_output_view(view) {
            self.definition_error = Some(format!(
                "content replacement outside output views: {view:?}"
            ));
            return;
        }
        if self.runtime.view_content(view).is_none() {
            self.definition_error = Some(format!(
                "content replacement for an unregistered view: {view:?}"
            ));
            return;
        }
        self.content_updates.push((view, content));
    }

    pub(crate) fn take_content_updates(
        &mut self,
    ) -> Vec<(irrigation_core::ids::ViewId, Arc<dyn Any + Send + Sync>)> {
        std::mem::take(&mut self.content_updates)
    }

    pub(crate) fn take_definition_error(&mut self) -> Option<String> {
        self.definition_error.take()
    }

    pub(crate) fn restrict_output_views(
        &mut self,
        views: impl IntoIterator<Item = irrigation_core::ids::ViewId>,
    ) {
        self.allowed_output_views = Some(views.into_iter().collect());
    }

    fn allows_output_view(&self, view: irrigation_core::ids::ViewId) -> bool {
        self.allowed_output_views
            .as_ref()
            .is_none_or(|views| views.contains(&view))
    }

    /// Starts a dependency scope; subsequent reads are attributed to it.
    pub fn begin_scope(&mut self, scope: u64) {
        self.scope = Some(scope);
    }

    /// Ends the current dependency scope.
    pub fn end_scope(&mut self) {
        self.scope = None;
    }

    /// Retains a scope's changes for the next update.
    ///
    /// Changes to units read under an unretained scope are filtered from the
    /// next [`ChangeBatch`].
    pub fn retain(&mut self, scope: u64) {
        self.retained.insert(scope);
    }

    /// Takes the scope attribution and retained set after a run.
    pub(crate) fn take_scopes(&mut self) -> (FxHashMap<UnitKey, u64>, FxHashSet<u64>) {
        (
            std::mem::take(&mut self.unit_scopes),
            std::mem::take(&mut self.retained),
        )
    }

    /// Starts a typed output write for a declared field.
    ///
    /// The writer is supplied by the field's family. It carries the View
    /// descriptor and the field position, so the definition path and
    /// `ensure_view` are the same as the hand-written one.
    pub fn write<F: FieldWrite>(&mut self, field: F) -> F::Writer<'_> {
        field.open(self)
    }

    /// Starts a typed output transaction committed with the new state.
    pub fn output(&mut self) -> OutputTxn<'_> {
        OutputTxn { cx: self }
    }

    /// The checked form of [`Self::define`]: it refuses a unit outside the
    /// component's declared output Views.
    ///
    /// `#[track_caller]` matters here as well as on `define`. Without it the
    /// location recorded for a definition is this wrapper, so a host mistake is
    /// reported against the runtime's own source line.
    #[track_caller]
    fn define_output<T: PartialEq + Send + Sync + 'static>(
        &mut self,
        unit: UnitKey,
        value: T,
        label: &'static str,
    ) {
        if !self.allows_output_view(unit.view) {
            self.definition_error = Some(format!("definition outside output views: {:?}", unit));
            return;
        }
        self.define(unit, value, label);
    }

    fn retract_output(&mut self, unit: UnitKey) {
        if !self.allows_output_view(unit.view) {
            self.definition_error = Some(format!("retraction outside output views: {:?}", unit));
            return;
        }
        self.pending_claims.push(unit.clone());
        self.retractions.push(unit);
    }

    /// Requests one unit as a future receipt.
    ///
    /// The future completes with the value when the unit is certified or
    /// cleanly replayable. When another task is computing it, the future
    /// registers its waker and returns `Pending`; the runtime wakes it when the
    /// unit completes.
    pub fn request(&mut self, unit: UnitKey) -> Request<'_> {
        if let Some(scope) = self.scope {
            self.unit_scopes.insert(unit.clone(), scope);
        }
        Request {
            cx: self,
            unit: Some(unit),
        }
    }

    /// Reads one unit as a future receipt with an expected type.
    pub fn request_typed<T: Clone + Send + Sync + 'static>(
        &mut self,
        unit: UnitKey,
    ) -> TypedRequest<'_, T> {
        TypedRequest {
            request: self.request(unit),
            marker: std::marker::PhantomData,
        }
    }

    /// The revision being computed.
    #[must_use]
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// Reads one View unit and records the observation.
    ///
    /// # Errors
    ///
    /// Fails when the unit was never defined.
    pub fn read(&mut self, unit: UnitKey) -> Result<Value> {
        if let Some(scope) = self.scope {
            self.unit_scopes.insert(unit.clone(), scope);
        }
        self.runtime.ensure_unit(&unit)?;
        let node = self.runtime.unit_node(unit.view, Arc::new(unit));
        self.runtime.read_unit(node)
    }

    /// Records an observation of one unit without reading its value.
    ///
    /// Custom components use this to retain dependencies their update path did
    /// not re-read, so later edits still invalidate them.
    ///
    /// # Errors
    ///
    /// Fails when the unit is missing or not certified for this revision.
    pub fn observe(&mut self, unit: UnitKey) -> Result<()> {
        self.read(unit).map(|_| ())
    }

    /// Reads one unit with an expected concrete type.
    ///
    /// # Errors
    ///
    /// Fails when the unit is missing or holds another type.
    pub fn read_typed<T: Clone + Send + Sync + 'static>(&mut self, unit: UnitKey) -> Result<T> {
        let value = self.read(unit)?;
        value
            .downcast_ref::<T>()
            .cloned()
            .ok_or_else(|| ComputeError::TypeMismatch {
                expected: std::any::type_name::<T>(),
            })
    }

    /// Claims a writable scope for this component's output group.
    ///
    /// A destructive tree patch can claim the whole View so a concurrent
    /// writer cannot add a member that the patch would miss.
    pub fn claim_scope(&mut self, scope: WriteScope, label: &'static str) {
        let Some(frame) = frame::current() else {
            self.definition_error = Some(String::from("output claim outside a computation"));
            return;
        };
        if let Err(error) = frame.runtime().claim_write(scope, label, None) {
            self.definition_error = Some(format!("{error}"));
        }
    }

    /// Stages one output definition, committed with the new state.
    ///
    /// This is the unchecked primitive: it stages whatever unit it is given,
    /// including one outside the component's declared output Views. Prefer
    /// [`Self::output`]'s `define`, which refuses that; this form exists for the
    /// runtime's own tests of the refusal.
    #[track_caller]
    pub fn define<T: PartialEq + Send + Sync + 'static>(
        &mut self,
        unit: UnitKey,
        value: T,
        label: &'static str,
    ) {
        if !self.allows_output_view(unit.view) {
            self.definition_error = Some(format!("definition outside output views: {:?}", unit));
            return;
        }
        self.pending_claims.push(unit.clone());
        irrigation_core::stage(DefinitionDraft {
            unit,
            value: value_of(value),
            eq: values_equal::<T>,
            label,
            location: Some(std::panic::Location::caller()),
        });
    }
}

/// A future receipt for one unit request.
pub struct Request<'a> {
    cx: &'a mut GeneralCx,
    unit: Option<UnitKey>,
}

impl std::fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Request")
    }
}

impl std::future::Future for Request<'_> {
    type Output = Result<Value>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        let Some(unit) = this.unit.take() else {
            return std::task::Poll::Ready(Err(ComputeError::internal(
                "request polled after completion",
            )));
        };
        let runtime = Arc::clone(&this.cx.runtime);
        // A receipt is a read: materialize the unit from the committed View
        // content first, exactly as `GeneralCx::read` does. Without it a source
        // unit the host has not written yet would be created empty, and the
        // node could never be initialized afterwards.
        if let Err(error) = runtime.ensure_unit(&unit) {
            return std::task::Poll::Ready(Err(error));
        }
        let node = runtime.unit_node(unit.view, Arc::new(unit.clone()));
        match runtime.try_read_unit(node, context.waker()) {
            Ok(std::task::Poll::Ready(value)) => std::task::Poll::Ready(Ok(value)),
            Ok(std::task::Poll::Pending) => {
                if runtime.suspend_request(node) {
                    // A pooled demand yields the worker; the node's completion
                    // queues the task and the demand restarts from its root.
                    std::task::Poll::Ready(Err(ComputeError::Suspended))
                } else {
                    this.unit = Some(unit);
                    std::task::Poll::Pending
                }
            }
            Err(error) => std::task::Poll::Ready(Err(error)),
        }
    }
}

/// A typed variant of [`Request`].
pub struct TypedRequest<'a, T> {
    request: Request<'a>,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T> std::fmt::Debug for TypedRequest<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TypedRequest")
    }
}

impl<T: Clone + Send + Sync + 'static> std::future::Future for TypedRequest<'_, T> {
    type Output = Result<T>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        match std::pin::Pin::new(&mut this.request).poll(context) {
            std::task::Poll::Ready(Ok(value)) => {
                std::task::Poll::Ready(value.downcast_ref::<T>().cloned().ok_or_else(|| {
                    ComputeError::TypeMismatch {
                        expected: std::any::type_name::<T>(),
                    }
                }))
            }
            std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(error)),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// A declared field that can open an output writer.
///
/// Implemented by `irrigation_collections::Field` for each family. The trait
/// lives in the runtime so `GeneralCx::write` can be an inherent method without
/// the runtime depending on the collection handles.
pub trait FieldWrite {
    /// The family writer.
    type Writer<'a>;

    /// Opens a writer against this context.
    fn open<'a>(self, cx: &'a mut GeneralCx) -> Self::Writer<'a>;
}

/// A typed transaction for publishing per-unit definitions.
pub struct OutputTxn<'a> {
    cx: &'a mut GeneralCx,
}

impl std::fmt::Debug for OutputTxn<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutputTxn")
    }
}

impl OutputTxn<'_> {
    /// Defines one logical unit.
    pub fn define<T: PartialEq + Send + Sync + 'static>(&mut self, unit: UnitKey, value: T) {
        self.cx.define_output(unit, value, "output");
    }

    /// Defines one scalar field of a View.
    pub fn field<T: PartialEq + Send + Sync + 'static>(
        &mut self,
        view: ViewId,
        field: FieldId,
        value: T,
    ) {
        self.define(UnitKey::field(view, field, UnitKind::Value), value);
    }

    /// Defines one map entry.
    pub fn entry<K, V>(&mut self, view: ViewId, field: FieldId, key: K, value: V)
    where
        K: Any + Hash + Eq + Send + Sync + std::fmt::Debug + 'static,
        V: PartialEq + Send + Sync + 'static,
    {
        self.define(
            UnitKey::entity(view, field, UnitKind::Value, any_key(key)),
            value,
        );
    }

    /// Sets one entity's membership.
    pub fn membership<K>(&mut self, view: ViewId, field: FieldId, key: K, present: bool)
    where
        K: Any + Hash + Eq + Send + Sync + std::fmt::Debug + 'static,
    {
        self.define(
            UnitKey::entity(view, field, UnitKind::Membership, any_key(key)),
            present,
        );
    }

    /// Retracts one unit this component previously defined.
    ///
    /// Absence is not a retraction: a unit keeps its committed value until it
    /// is either restaged or retracted here.
    pub fn retract(&mut self, unit: UnitKey) {
        self.cx.retract_output(unit);
    }

    /// Restates every definition: units this run does not stage are retracted.
    ///
    /// This is the pre-write semantics, kept for components whose output set is
    /// genuinely recomputed from scratch.
    pub fn replace_all(&mut self) {
        self.cx.restate_all = true;
    }
}

fn values_equal<T: PartialEq + Any>(left: &Value, right: &Value) -> bool {
    match (left.downcast_ref::<T>(), right.downcast_ref::<T>()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// A component with a user-chosen incremental algorithm.
pub trait GeneralComponent: Send + Sync + 'static {
    /// Retained algorithm state. It is replaced, never mutated in place.
    type State: Clone + Send + Sync + 'static;
    /// Published output product.
    type Output: Clone + PartialEq + Send + Sync + 'static;

    /// Stable component identity.
    const ID: irrigation_core::ComponentId;
    /// Fully qualified name for diagnostics.
    const NAME: &'static str;
    /// Views this component reads, registered at install time.
    const VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] = &[];

    /// Views this component owns and publishes.
    ///
    /// A non-root producer must declare at least one output View. An output
    /// View must be derived and may have only one producer.
    const OUTPUT_VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] = &[];

    /// How requested-dependency invalidation is handled.
    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Reset
    }

    /// The component identity used for installation and diagnostics.
    ///
    /// The default is the static identity. A generic component whose output
    /// Views are chosen at construction overrides this.
    fn component_id(&self) -> irrigation_core::ids::ComponentId {
        Self::ID
    }

    /// The Views this instance owns.
    ///
    /// The default is the static declaration. A join with a runtime-chosen
    /// target overrides this to include the target View.
    fn output_views(&self) -> Vec<&'static irrigation_core::descriptor::ViewDescriptor> {
        Self::OUTPUT_VIEWS.to_vec()
    }

    /// Runs on first demand.
    ///
    /// # Errors
    ///
    /// Any failure aborts the attempt; nothing is published.
    fn initialize(&self, cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)>;

    /// Runs when an observed dependency changed under
    /// [`DependencyUpdates::Incremental`].
    ///
    /// # Errors
    ///
    /// Any failure aborts the attempt; the previous state stays valid.
    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &ChangeBatch,
    ) -> Result<(Self::State, Self::Output)> {
        // Reinitializing is always sound under `Reset` and is a correct but
        // coarse fallback under `Incremental`. A component that wants to use a
        // change batch must override this method.
        self.initialize(cx)
    }
}

/// Typed inputs read before every `initialize`/`update`.
pub trait GeneralInputs: Clone + Send + Sync + 'static {
    /// Reads every declared input for the current revision.
    ///
    /// # Errors
    ///
    /// Fails when a declared unit is missing or mistyped.
    fn read(cx: &mut GeneralCx) -> Result<Self>;
}

/// A typed component interface layered over [`GeneralComponent`].
///
/// The adapter reads `Inputs` before each run, fixes `Params` at install time,
/// and publishes a typed `Returns` value. Use with
/// `Runtime::install_typed_general` and demand with
/// `read_outputs_general::<TypedAdapter<T>>()`.
pub trait TypedGeneralComponent: Send + Sync + 'static {
    /// Typed, re-read inputs.
    type Inputs: GeneralInputs;
    /// Parameters fixed at install time.
    type Params: Clone + Send + Sync + 'static;
    /// Retained algorithm state.
    type State: Clone + Send + Sync + 'static;
    /// Published return value.
    type Returns: Clone + PartialEq + Send + Sync + 'static;

    /// Stable component identity.
    const ID: irrigation_core::ComponentId;
    /// Fully qualified name for diagnostics.
    const NAME: &'static str;
    /// Views this component reads, registered at install time.
    const VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] = &[];

    /// Views this component owns and publishes.
    ///
    /// A non-root producer must declare at least one output View. An output
    /// View must be derived and may have only one producer.
    const OUTPUT_VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] = &[];

    /// The parameters fixed when the component is installed.
    fn params(&self) -> Self::Params;

    /// How requested-dependency invalidation is handled.
    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Reset
    }

    /// Runs on first demand with freshly read inputs.
    ///
    /// # Errors
    ///
    /// Any failure aborts the attempt; nothing is published.
    fn initialize(
        &self,
        cx: &mut GeneralCx,
        inputs: &Self::Inputs,
        params: &Self::Params,
    ) -> Result<(Self::State, Self::Returns)>;

    /// Runs when an observed dependency changed under
    /// [`DependencyUpdates::Incremental`].
    ///
    /// # Errors
    ///
    /// Any failure aborts the attempt; the previous state stays valid.
    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        inputs: &Self::Inputs,
        changes: &ChangeBatch,
        params: &Self::Params,
    ) -> Result<(Self::State, Self::Returns)>;
}

/// Adapts a [`TypedGeneralComponent`] to the erased [`GeneralComponent`] API.
pub struct TypedAdapter<T: TypedGeneralComponent> {
    component: T,
    params: T::Params,
}

impl<T: TypedGeneralComponent> TypedAdapter<T> {
    /// Wraps a typed component, capturing its parameters.
    #[must_use]
    pub fn new(component: T) -> Self {
        let params = component.params();
        Self { component, params }
    }
}

impl<T: TypedGeneralComponent> std::fmt::Debug for TypedAdapter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TypedAdapter")
    }
}

impl<T: TypedGeneralComponent> GeneralComponent for TypedAdapter<T> {
    type State = (T::State, T::Inputs);
    type Output = T::Returns;

    const ID: irrigation_core::ComponentId = T::ID;
    const NAME: &'static str = T::NAME;
    const VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] = T::VIEWS;
    const OUTPUT_VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] =
        T::OUTPUT_VIEWS;

    fn dependency_updates(&self) -> DependencyUpdates {
        self.component.dependency_updates()
    }

    fn initialize(&self, cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)> {
        let inputs = T::Inputs::read(cx)?;
        let (state, returns) = self.component.initialize(cx, &inputs, &self.params)?;
        Ok(((state, inputs), returns))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        changes: &ChangeBatch,
    ) -> Result<(Self::State, Self::Output)> {
        let inputs = T::Inputs::read(cx)?;
        let (next, returns) =
            self.component
                .update(cx, &state.0, &inputs, changes, &self.params)?;
        Ok(((next, inputs), returns))
    }
}

/// A general component whose retained state need not be `Send`.
///
/// The component and its state live on the thread that installs it; demands
/// must therefore use a workerless runtime and run on that thread. State is
/// replaced through the runtime's atomic commit, exactly like a
/// [`GeneralComponent`].
pub trait LocalGeneralComponent: 'static {
    /// Retained algorithm state; it may be thread-local (`Rc`, `RefCell`, ...).
    type State: Clone + 'static;
    /// Published output product; it crosses to the runtime, so it is `Send`.
    type Output: Clone + PartialEq + Send + Sync + 'static;

    /// Stable component identity.
    const ID: irrigation_core::ComponentId;
    /// Fully qualified name for diagnostics.
    const NAME: &'static str;
    /// Views this component reads, registered at install time.
    const VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] = &[];

    /// Views this component owns and publishes.
    ///
    /// A non-root producer must declare at least one output View. An output
    /// View must be derived and may have only one producer.
    const OUTPUT_VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] = &[];

    /// How requested-dependency invalidation is handled.
    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Reset
    }

    /// Runs on first demand.
    ///
    /// # Errors
    ///
    /// Any failure aborts the attempt; nothing is published.
    fn initialize(&self, cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)>;

    /// Runs when an observed dependency changed under
    /// [`DependencyUpdates::Incremental`].
    ///
    /// # Errors
    ///
    /// Any failure aborts the attempt; the previous state stays valid.
    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        changes: &ChangeBatch,
    ) -> Result<(Self::State, Self::Output)>;
}

/// Type-erased local slot stored on the owner thread.
///
/// The state is replaced the way a `GeneralComponent`'s is: a run stages its
/// successor, and the runtime's commit tail promotes it or drops it. A local
/// slot cannot be published through the node record, so the staging lives here
/// and is driven by [`commit_local_state`]/[`discard_local_state`].
trait LocalSlot {
    fn policy(&self) -> DependencyUpdates;
    fn initialize(&mut self, cx: &mut GeneralCx) -> Result<Value>;
    fn update(&mut self, cx: &mut GeneralCx, changes: &ChangeBatch) -> Result<Value>;
    /// The runtime and node whose run staged the pending state, if any.
    ///
    /// The runtime is part of the identity: the registry is thread-global, and a
    /// `NodeId` is only unique inside one arena.
    fn pending_node(&self) -> Option<(u64, irrigation_core::ids::NodeId)>;
    /// Promotes the staged state: the commit that staged it published.
    fn commit(&mut self);
    /// Drops the staged state; the retained state stays valid.
    fn discard(&mut self);
}

struct Slot<C: LocalGeneralComponent> {
    component: C,
    state: Option<C::State>,
    staged: Option<C::State>,
    node: Option<(u64, irrigation_core::ids::NodeId)>,
}

impl<C: LocalGeneralComponent> Slot<C> {
    /// Records the successor a run produced, tagged with the node that ran it.
    fn stage(&mut self, state: C::State) {
        self.node = frame::current().map(|frame| (frame.runtime().runtime_id(), frame.node()));
        self.staged = Some(state);
    }
}

impl<C: LocalGeneralComponent> LocalSlot for Slot<C> {
    fn policy(&self) -> DependencyUpdates {
        self.component.dependency_updates()
    }

    fn initialize(&mut self, cx: &mut GeneralCx) -> Result<Value> {
        self.staged = None;
        self.node = None;
        let (state, output) = self.component.initialize(cx)?;
        self.stage(state);
        Ok(value_of(output))
    }

    fn update(&mut self, cx: &mut GeneralCx, changes: &ChangeBatch) -> Result<Value> {
        self.staged = None;
        self.node = None;
        if self.state.is_none() {
            return Err(ComputeError::internal(
                "local component was updated before it was initialized",
            ));
        }
        let (next, output) = {
            let state = self.state.as_ref().expect("checked above");
            self.component.update(cx, state, changes)?
        };
        self.stage(next);
        Ok(value_of(output))
    }

    fn pending_node(&self) -> Option<(u64, irrigation_core::ids::NodeId)> {
        self.node
    }

    fn commit(&mut self) {
        if let Some(staged) = self.staged.take() {
            self.state = Some(staged);
        }
        self.node = None;
    }

    fn discard(&mut self) {
        self.staged = None;
        self.node = None;
    }
}

/// Promotes the state a local component staged for `node`.
///
/// Called from the commit tail whenever the group published, including an
/// attempt whose observations moved before its install: that retry reuses the
/// publication, so the state it staged belongs to it. A cancelled or rejected
/// attempt published nothing and is discarded instead.
pub(crate) fn commit_local_state(runtime: u64, node: irrigation_core::ids::NodeId) {
    promote_local_state(runtime, node, true);
}

/// Drops the state a local component staged for `node`.
pub(crate) fn discard_local_state(runtime: u64, node: irrigation_core::ids::NodeId) {
    promote_local_state(runtime, node, false);
}

fn promote_local_state(runtime: u64, node: irrigation_core::ids::NodeId, commit: bool) {
    let keys: Vec<u64> = LOCAL_SLOTS.with(|slots| {
        slots
            .borrow()
            .iter()
            // Both halves: the registry is thread-global and a `NodeId` is only
            // unique inside one runtime's arena, so matching the node alone let a
            // commit in one runtime promote the state another had staged.
            .filter(|(_, slot)| slot.pending_node() == Some((runtime, node)))
            .map(|(key, _)| *key)
            .collect()
    });
    for key in keys {
        let Some(slot) = LOCAL_SLOTS.with(|slots| slots.borrow_mut().remove(&key)) else {
            continue;
        };
        // Promotion replaces retained state, and its destructor is user code
        // that may touch the registry: it never runs under the borrow.
        let mut guard = SlotGuard::new(key, slot);
        if commit {
            guard.slot_mut().commit();
        } else {
            guard.slot_mut().discard();
        }
        guard.restore();
    }
}

thread_local! {
    static LOCAL_SLOTS: std::cell::RefCell<
        FxHashMap<u64, Box<dyn LocalSlot>>,
    > = std::cell::RefCell::new(FxHashMap::default());
}

static NEXT_LOCAL_SLOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Puts a taken slot back, on the normal path and while a panic unwinds.
///
/// A slot held by an activation is not in the registry, so dropping user code
/// under the borrow would let a destructor re-enter the registry; restoring
/// from a guard keeps the component available even when its body unwinds.
struct SlotGuard {
    key: u64,
    slot: Option<Box<dyn LocalSlot>>,
}

impl SlotGuard {
    fn new(key: u64, slot: Box<dyn LocalSlot>) -> Self {
        Self {
            key,
            slot: Some(slot),
        }
    }

    fn slot_mut(&mut self) -> &mut dyn LocalSlot {
        self.slot
            .as_mut()
            .expect("a slot guard holds its slot until it is restored")
            .as_mut()
    }

    fn put_back(&mut self) {
        if let Some(slot) = self.slot.take() {
            LOCAL_SLOTS.with(|slots| slots.borrow_mut().insert(self.key, slot));
        }
    }

    /// Puts the slot back and disarms the drop guard.
    fn restore(mut self) {
        self.put_back();
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.put_back();
    }
}

/// Adapter installing a [`LocalGeneralComponent`] as a runtime general.
///
/// The runtime must be workerless (`workers = 0`) and demands must run on the
/// installing thread; the component's non-`Send` state never leaves it.
pub struct LocalAdapter<C: LocalGeneralComponent> {
    key: u64,
    owner: std::thread::ThreadId,
    marker: std::marker::PhantomData<fn() -> C>,
}

impl<C: LocalGeneralComponent> std::fmt::Debug for LocalAdapter<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalAdapter")
    }
}

impl<C: LocalGeneralComponent> LocalAdapter<C> {
    /// Registers the component on the current thread and builds the adapter.
    #[must_use]
    pub fn new(component: C) -> Self {
        let key = NEXT_LOCAL_SLOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        LOCAL_SLOTS.with(|slots| {
            slots.borrow_mut().insert(
                key,
                Box::new(Slot {
                    component,
                    state: None,
                    staged: None,
                    node: None,
                }),
            );
        });
        Self {
            key,
            owner: std::thread::current().id(),
            marker: std::marker::PhantomData,
        }
    }

    fn check_owner(&self) -> Result<()> {
        if std::thread::current().id() != self.owner {
            return Err(ComputeError::internal(
                "a local component was demanded on a thread other than its owner",
            ));
        }
        Ok(())
    }

    fn with_slot<T>(&self, run: impl FnOnce(&mut dyn LocalSlot) -> Result<T>) -> Result<T> {
        self.check_owner()?;
        let Some(slot) = LOCAL_SLOTS.with(|slots| slots.borrow_mut().remove(&self.key)) else {
            // The adapter is live, so its slot can only be absent while an
            // enclosing activation of this same adapter holds it: a local
            // component that demands itself is a cycle, not a missing slot.
            return Err(ComputeError::Cycle {
                path: vec![format!(
                    "re-entrant demand of local component `{}`",
                    C::NAME
                )],
            });
        };
        // `run` executes the component body, which may demand another local
        // component: the registry stays unborrowed for the whole call.
        let mut guard = SlotGuard::new(self.key, slot);
        let result = run(guard.slot_mut());
        guard.restore();
        result
    }
}

impl<C: LocalGeneralComponent> Drop for LocalAdapter<C> {
    fn drop(&mut self) {
        if std::thread::current().id() == self.owner {
            // The removed slot owns the component and its state, so its
            // destructors are user code: they run after the borrow is released.
            let removed = LOCAL_SLOTS.with(|slots| slots.borrow_mut().remove(&self.key));
            drop(removed);
        }
    }
}

impl<C: LocalGeneralComponent> GeneralComponent for LocalAdapter<C> {
    type State = ();
    type Output = C::Output;

    const ID: irrigation_core::ComponentId = C::ID;
    const NAME: &'static str = C::NAME;
    const VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] = C::VIEWS;
    const OUTPUT_VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] =
        C::OUTPUT_VIEWS;

    fn dependency_updates(&self) -> DependencyUpdates {
        self.with_slot(|slot| Ok(slot.policy()))
            .unwrap_or(DependencyUpdates::Reset)
    }

    fn initialize(&self, cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)> {
        let value = self.with_slot(|slot| slot.initialize(cx))?;
        let output = value.downcast_ref::<C::Output>().cloned().ok_or_else(|| {
            ComputeError::TypeMismatch {
                expected: std::any::type_name::<C::Output>(),
            }
        })?;
        Ok(((), output))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        changes: &ChangeBatch,
    ) -> Result<(Self::State, Self::Output)> {
        let value = self.with_slot(|slot| slot.update(cx, changes))?;
        let output = value.downcast_ref::<C::Output>().cloned().ok_or_else(|| {
            ComputeError::TypeMismatch {
                expected: std::any::type_name::<C::Output>(),
            }
        })?;
        Ok(((), output))
    }
}

/// Type-erased form stored by the runtime.
pub(crate) trait ErasedGeneral: Send + Sync + 'static {
    fn id(&self) -> irrigation_core::ComponentId;
    fn name(&self) -> &'static str;
    fn policy(&self) -> DependencyUpdates;
    fn initialize(&self, cx: &mut GeneralCx) -> Result<(ErasedState, Value)>;
    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &ErasedState,
        changes: &ChangeBatch,
    ) -> Result<(ErasedState, Value)>;
    fn output_eq(&self, left: &Value, right: &Value) -> bool;
    fn output_views(&self) -> Vec<irrigation_core::ids::ViewId>;
}

/// Retained state, type-erased.
pub(crate) type ErasedState = Arc<dyn Any + Send + Sync>;

impl std::fmt::Debug for dyn ErasedGeneral {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErasedGeneral")
            .field("name", &self.name())
            .finish()
    }
}

impl<T: GeneralComponent> ErasedGeneral for T {
    fn id(&self) -> irrigation_core::ComponentId {
        T::ID
    }

    fn name(&self) -> &'static str {
        T::NAME
    }

    fn policy(&self) -> DependencyUpdates {
        self.dependency_updates()
    }

    fn initialize(&self, cx: &mut GeneralCx) -> Result<(ErasedState, Value)> {
        let (state, output) = GeneralComponent::initialize(self, cx)?;
        Ok((Arc::new(state), value_of(output)))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &ErasedState,
        changes: &ChangeBatch,
    ) -> Result<(ErasedState, Value)> {
        let state = state
            .downcast_ref::<T::State>()
            .ok_or_else(|| ComputeError::internal("general component state type mismatch"))?;
        let (state, output) = GeneralComponent::update(self, cx, state, changes)?;
        Ok((Arc::new(state), value_of(output)))
    }

    fn output_eq(&self, left: &Value, right: &Value) -> bool {
        values_equal::<T::Output>(left, right)
    }

    fn output_views(&self) -> Vec<irrigation_core::ids::ViewId> {
        self.output_views().iter().map(|view| view.id).collect()
    }
}

/// A registered `GeneralComponent` instance.
#[derive(Clone)]
pub struct GeneralHandle {
    pub(crate) index: usize,
    pub(crate) name: &'static str,
}

impl std::fmt::Debug for GeneralHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneralHandle")
            .field("index", &self.index)
            .field("name", &self.name)
            .finish()
    }
}

/// A component future.
///
/// The runtime drives futures to completion on the worker that owns the node.
/// Reads inside the future are synchronous, so an algorithm that only reads
/// Views completes on the first poll. A future that awaits external I/O parks
/// its worker but stays cancellable: [`drive`] observes the demand's
/// cancellation token while it waits.
pub type ComponentFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>>;

/// Drives a component future to completion on the demanding thread.
///
/// A pending future parks the thread until its waker unparks it, then polls
/// again. This keeps the atomic commit contract and supports futures woken by
/// another thread (a timer, an I/O completion). The demanding worker is held
/// while the future is pending; true worker suspension remains future work.
///
/// # Errors
///
/// Returns the future's own error, or [`ComputeError::Cancelled`] once the
/// owning demand is cancelled.
pub fn drive<T>(future: ComponentFuture<'_, T>) -> Result<T> {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct Unpark(std::thread::Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    // A parked driver is still running: the runtime cannot requeue it, and its
    // condvar is not the one `park` waits on. The demand's token is therefore
    // re-checked on a bounded interval, which is what bounds cancellation
    // latency for an async component.
    const PARK_SLICE: std::time::Duration = std::time::Duration::from_millis(10);

    let cancel = frame::current().map(|frame| frame.cancel().clone());
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = future;
    loop {
        if cancel
            .as_ref()
            .is_some_and(frame::CancelToken::is_cancelled)
        {
            return Err(ComputeError::Cancelled);
        }
        match future.as_mut().poll(&mut context) {
            Poll::Ready(result) => return result,
            Poll::Pending => match &cancel {
                Some(_) => std::thread::park_timeout(PARK_SLICE),
                None => std::thread::park(),
            },
        }
    }
}

/// An async form of [`GeneralComponent`].
///
/// The runtime polls the returned future to completion, parking the demanding
/// thread while it is pending. Synchronous algorithms can still return a ready
/// future, and the driver keeps the same atomic state/output commit as the
/// synchronous trait.
pub trait AsyncGeneralComponent: Send + Sync + 'static {
    /// Retained algorithm state.
    type State: Clone + Send + Sync + 'static;
    /// Published output product.
    type Output: Clone + PartialEq + Send + Sync + 'static;

    /// Stable component identity.
    const ID: irrigation_core::ComponentId;
    /// Fully qualified name for diagnostics.
    const NAME: &'static str;
    /// Views this component reads, registered at install time.
    const VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] = &[];

    /// Views this component owns and publishes.
    ///
    /// A non-root producer must declare at least one output View. An output
    /// View must be derived and may have only one producer.
    const OUTPUT_VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] = &[];

    /// How requested-dependency invalidation is handled.
    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Reset
    }

    /// Runs on first demand.
    fn initialize<'a>(
        &'a self,
        cx: &'a mut GeneralCx,
    ) -> ComponentFuture<'a, (Self::State, Self::Output)>;

    /// Runs when an observed dependency changed under `Incremental`.
    fn update<'a>(
        &'a self,
        cx: &'a mut GeneralCx,
        state: &'a Self::State,
        changes: &'a ChangeBatch,
    ) -> ComponentFuture<'a, (Self::State, Self::Output)>;
}

/// Adapter that drives an [`AsyncGeneralComponent`] as a [`GeneralComponent`].
pub struct AsyncAdapter<T>(T);

impl<T> AsyncAdapter<T> {
    /// Wraps an async component.
    #[must_use]
    pub fn new(component: T) -> Self {
        Self(component)
    }
}

impl<T: AsyncGeneralComponent> std::fmt::Debug for AsyncAdapter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AsyncAdapter")
    }
}

impl<T: AsyncGeneralComponent> GeneralComponent for AsyncAdapter<T> {
    type State = T::State;
    type Output = T::Output;

    const ID: irrigation_core::ComponentId = T::ID;
    const NAME: &'static str = T::NAME;
    const VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] = T::VIEWS;
    const OUTPUT_VIEWS: &'static [&'static irrigation_core::descriptor::ViewDescriptor] =
        T::OUTPUT_VIEWS;

    fn dependency_updates(&self) -> DependencyUpdates {
        self.0.dependency_updates()
    }

    fn initialize(&self, cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)> {
        drive(self.0.initialize(cx))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        changes: &ChangeBatch,
    ) -> Result<(Self::State, Self::Output)> {
        drive(self.0.update(cx, state, changes))
    }
}

#[cfg(test)]
mod local_registry_tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct Counter;

    impl LocalGeneralComponent for Counter {
        type State = u64;
        type Output = u64;

        const ID: irrigation_core::ComponentId = irrigation_core::ComponentId(0x00C0_00F1);
        const NAME: &'static str = "general::local_registry_tests::Counter";

        fn initialize(&self, _cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)> {
            Ok((0, 0))
        }

        fn update(
            &self,
            _cx: &mut GeneralCx,
            state: &Self::State,
            _changes: &ChangeBatch,
        ) -> Result<(Self::State, Self::Output)> {
            Ok((state + 1, state + 1))
        }
    }

    /// Stages one slot for `runtime` at `node`, returning its registry key.
    fn stage_slot(runtime: u64, node: irrigation_core::ids::NodeId) -> u64 {
        let key = NEXT_LOCAL_SLOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        LOCAL_SLOTS.with(|slots| {
            slots.borrow_mut().insert(
                key,
                Box::new(Slot {
                    component: Counter,
                    state: Some(0),
                    staged: Some(1),
                    node: Some((runtime, node)),
                }),
            );
        });
        key
    }

    /// Whether the slot still holds a staged, unpromoted state.
    ///
    /// The slot itself returns to the registry either way -- `SlotGuard` restores
    /// it after promoting -- so the observable is whether it still names a
    /// pending node.
    fn waiting(key: u64) -> Option<bool> {
        LOCAL_SLOTS.with(|slots| {
            let slots = slots.borrow();
            slots.get(&key).map(|slot| slot.pending_node().is_some())
        })
    }

    /// The registry is thread-global and a `NodeId` is only unique inside one
    /// arena: a commit in one runtime must not promote another's staged state.
    #[test]
    fn a_promotion_only_matches_its_own_runtime() {
        let node = irrigation_core::ids::NodeId::new(7, 1);
        let key = stage_slot(1, node);

        // Another runtime on this thread commits the same node id: nothing here.
        promote_local_state(2, node, true);
        assert_eq!(
            waiting(key),
            Some(true),
            "another runtime's commit must leave this staged state alone"
        );

        // Its own runtime commits: the slot is consumed.
        promote_local_state(1, node, true);
        assert_eq!(
            waiting(key),
            Some(false),
            "the owning runtime's commit promotes it"
        );
    }
}
