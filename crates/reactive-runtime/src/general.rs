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

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use reactive_core::descriptor::{UnitKey, UnitKind};
use reactive_core::error::{ComputeError, Result};
use reactive_core::ids::{FieldId, Revision, ViewId};
use reactive_core::key::any_key;
use reactive_core::ops::RuntimeOps;
use reactive_core::value::{Value, value_of};

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
#[derive(Clone, Debug, Default)]
pub struct ChangeBatch {
    changes: Vec<Change>,
}

impl ChangeBatch {
    /// Builds a batch from observed changes.
    #[must_use]
    pub fn new(changes: Vec<Change>) -> Self {
        Self { changes }
    }

    /// The changes, in dependency order.
    #[must_use]
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    /// Returns `true` when nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Number of changed units.
    #[must_use]
    pub fn len(&self) -> usize {
        self.changes.len()
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
    unit_scopes: HashMap<UnitKey, u64>,
    retained: HashSet<u64>,
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
            unit_scopes: HashMap::new(),
            retained: HashSet::new(),
        }
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
    pub(crate) fn take_scopes(&mut self) -> (HashMap<UnitKey, u64>, HashSet<u64>) {
        (
            std::mem::take(&mut self.unit_scopes),
            std::mem::take(&mut self.retained),
        )
    }

    /// Starts a typed output transaction committed with the new state.
    pub fn output(&mut self) -> OutputTxn<'_> {
        OutputTxn { cx: self }
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
        let node = self
            .runtime
            .unit_node(unit.view, Arc::new(unit.clone()));
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

    /// Stages one output definition, committed with the new state.
    pub fn define<T: PartialEq + Send + Sync + 'static>(
        &mut self,
        unit: UnitKey,
        value: T,
        label: &'static str,
    ) {
        reactive_core::stage(DefinitionDraft {
            unit,
            value: value_of(value),
            eq: values_equal::<T>,
            label,
        });
    }
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
        self.cx.define(unit, value, "output");
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
    const ID: reactive_core::ComponentId;
    /// Fully qualified name for diagnostics.
    const NAME: &'static str;
    /// Views this component reads, registered at install time.
    const VIEWS: &'static [&'static reactive_core::descriptor::ViewDescriptor] = &[];

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
    const ID: reactive_core::ComponentId;
    /// Fully qualified name for diagnostics.
    const NAME: &'static str;
    /// Views this component reads, registered at install time.
    const VIEWS: &'static [&'static reactive_core::descriptor::ViewDescriptor] = &[];

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

    const ID: reactive_core::ComponentId = T::ID;
    const NAME: &'static str = T::NAME;
    const VIEWS: &'static [&'static reactive_core::descriptor::ViewDescriptor] = T::VIEWS;

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
        let (next, returns) = self
            .component
            .update(cx, &state.0, &inputs, changes, &self.params)?;
        Ok(((next, inputs), returns))
    }
}

/// Type-erased form stored by the runtime.
pub(crate) trait ErasedGeneral: Send + Sync + 'static {
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
/// Views completes on the first poll. Awaiting external I/O has no executor
/// here and is reported as unsupported rather than silently blocking.
pub type ComponentFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>>;

/// Drives a component future to completion.
///
/// # Errors
///
/// Returns [`ComputeError::Unsupported`] when the future is still pending,
/// which means it awaited work this runtime does not provide.
pub fn drive<T>(future: ComponentFuture<'_, T>) -> Result<T> {
    use std::task::{Context, Poll};

    let waker = std::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = future;
    match future.as_mut().poll(&mut context) {
        Poll::Ready(result) => result,
        Poll::Pending => Err(ComputeError::unsupported(
            "component future is pending; awaiting external work needs a dedicated executor",
        )),
    }
}

/// An async form of [`GeneralComponent`].
///
/// The runtime polls the returned future to completion. Synchronous algorithms
/// can still return a ready future, and the driver keeps the same atomic
/// state/output commit as the synchronous trait.
pub trait AsyncGeneralComponent: Send + Sync + 'static {
    /// Retained algorithm state.
    type State: Clone + Send + Sync + 'static;
    /// Published output product.
    type Output: Clone + PartialEq + Send + Sync + 'static;

    /// Stable component identity.
    const ID: reactive_core::ComponentId;
    /// Fully qualified name for diagnostics.
    const NAME: &'static str;
    /// Views this component reads, registered at install time.
    const VIEWS: &'static [&'static reactive_core::descriptor::ViewDescriptor] = &[];

    /// How requested-dependency invalidation is handled.
    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Reset
    }

    /// Runs on first demand.
    fn initialize<'a>(&'a self, cx: &'a mut GeneralCx) -> ComponentFuture<'a, (Self::State, Self::Output)>;

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

    const ID: reactive_core::ComponentId = T::ID;
    const NAME: &'static str = T::NAME;
    const VIEWS: &'static [&'static reactive_core::descriptor::ViewDescriptor] = T::VIEWS;

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
