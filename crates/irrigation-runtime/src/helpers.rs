//! Typed read and define helpers used by generated View code.
//!
//! Generated accessors are ordinary-looking methods; these helpers are the
//! boundary where a method call becomes a tracked observation. They keep the
//! macro output small and make the read protocol testable on its own.

use std::any::Any;
use std::sync::Arc;

use irrigation_core::descriptor::{UnitKey, UnitKind, WriteScope};
use irrigation_core::error::ComputeError;
use irrigation_core::frame::{Requirement, current_or_engine_panic};
use irrigation_core::ids::{FieldId, ViewId};
use irrigation_core::ops::panic_engine;
use irrigation_core::value::{Value, value_of};

use crate::inner::DefinitionDraft;

/// Whether a field currently uses the keyed derived protocol.
///
/// This is protocol *state*, not an observation: a read verb that branches on it
/// must use [`field_protocol`], or a mode flip the reader never observed leaves
/// it on the wrong branch for good.
#[must_use]
pub fn field_is_keyed(view: ViewId, field: FieldId) -> bool {
    irrigation_core::frame::current()
        .is_some_and(|frame| frame.runtime().field_is_keyed(view, field))
}

/// The unit carrying one field's keyed/whole protocol certificate.
///
/// The entity-less `Membership` unit is not a member cell — no content vtable
/// answers it and no definition installs it — so it exists only to move the
/// protocol bit through the trace graph.
#[must_use]
pub fn field_protocol_unit(view: ViewId, field: FieldId) -> UnitKey {
    UnitKey::field(view, field, UnitKind::Membership)
}

/// Whether a field uses the keyed derived protocol, as a tracked observation.
///
/// A collection read verb dispatches on this bit, so it must be read through the
/// unit graph: the install that flips the mode writes the certificate in the
/// same commit, and that write invalidates every reader that observed it.
pub fn field_protocol(view: ViewId, field: FieldId) -> bool {
    // Read the committed bit first: `unit_node_init` runs its initializer under
    // the runtime lock, and `field_is_keyed` takes that same lock.
    let keyed = field_is_keyed(view, field);
    read_unit_value(field_protocol_unit(view, field), |_| Some(keyed))
}

/// Reads one logical unit, creating it with `init` when first observed.
///
/// # Panics
///
/// Panics with an engine payload outside a computation or on engine failure.
pub fn read_unit_value<T, F>(key: UnitKey, init: F) -> T
where
    T: Clone + PartialEq + Send + Sync + 'static,
    F: FnOnce(&dyn Any) -> Option<T>,
{
    let frame = current_or_engine_panic("view read");
    let runtime = Arc::clone(frame.runtime());
    let view = key.view;
    let mut init = {
        let mut init = Some(init);
        move |content: &dyn Any| -> Option<Value> {
            init.take().and_then(|init| init(content)).map(value_of)
        }
    };
    let node = runtime.unit_node_init(view, Arc::new(key), &mut init);
    let value = runtime
        .read_unit(node)
        .unwrap_or_else(|error| panic_engine(error));
    match value.downcast_ref::<T>() {
        Some(value) => value.clone(),
        None => panic_engine(ComputeError::TypeMismatch {
            expected: std::any::type_name::<T>(),
        }),
    }
}

/// Reads a unit whose absence is meaningful, returning `None` instead of
/// panicking when the unit was never defined.
///
/// Absence is the only outcome that becomes `None`, and it is recorded as a
/// negative observation by the runtime. Every other failure — a suspension, a
/// cancellation, a cycle, a panicking producer, a type mismatch — would publish
/// a value derived from a false absence, so it travels to the caller as an
/// engine panic exactly like [`read_unit_value`].
pub fn try_read_unit_value<T, F>(key: UnitKey, init: F) -> Option<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
    F: FnOnce(&dyn Any) -> Option<T>,
{
    let frame = current_or_engine_panic("view read");
    let runtime = Arc::clone(frame.runtime());
    let view = key.view;
    let mut init = {
        let mut init = Some(init);
        move |content: &dyn Any| -> Option<Value> {
            init.take().and_then(|init| init(content)).map(value_of)
        }
    };
    let node = runtime.unit_node_init(view, Arc::new(key), &mut init);
    match runtime.read_unit(node) {
        Ok(value) => match value.downcast_ref::<T>() {
            Some(value) => Some(value.clone()),
            None => panic_engine(ComputeError::TypeMismatch {
                expected: std::any::type_name::<T>(),
            }),
        },
        Err(ComputeError::Absent { .. }) => None,
        Err(error) => panic_engine(error),
    }
}

/// Defines one derived View unit from the currently executing computation.
///
/// The definition becomes visible only when the computation commits its value,
/// trace, and routing change together.
///
/// # Panics
///
/// Panics with an engine payload outside a computation.
#[track_caller]
pub fn define_unit<T>(unit: UnitKey, value: T, label: &'static str)
where
    T: PartialEq + Send + Sync + 'static,
{
    let frame = current_or_engine_panic("definition");
    let location = std::panic::Location::caller();
    frame
        .runtime()
        .claim_write(WriteScope::entity(&unit), label, Some(location))
        .unwrap_or_else(|error| panic_engine(error));
    irrigation_core::stage(DefinitionDraft {
        unit,
        value: value_of(value),
        eq: values_equal::<T>,
        label,
        location: Some(location),
    });
}

/// Declares a View for the write group that will commit the current work.
///
/// Used by collection handles that carry a `ViewId` but not the descriptor.
/// The View must already be registered on the runtime. The declaration lands
/// on the committing frame, not the innermost scope frame, so a definition
/// staged inside a per-iteration scope declares the View that its commit will
/// be checked against.
pub fn ensure_view_id(view: ViewId) {
    if let Some(frame) = irrigation_core::frame::current() {
        if let Some(scope) = irrigation_core::frame::commit_scope() {
            frame.runtime().declare_view(scope, view);
        }
    }
}

/// Claims a whole field for the current computation.
///
/// A whole-value writer uses this so a live per-entity writer conflicts with it
/// and so its own entity references do not conflict with themselves. Collection
/// write verbs take the claim as part of [`write_group`], which also projects
/// the committed base and stages the presence requirement; call this directly
/// only for a group the helper does not express.
///
/// # Panics
///
/// Panics with an engine payload outside a computation or on a conflict.
#[track_caller]
pub fn claim_field(view: ViewId, field: FieldId, label: &'static str) {
    let frame = current_or_engine_panic("field write claim");
    let location = std::panic::Location::caller();
    frame
        .runtime()
        .claim_write(WriteScope::Field(view, field), label, Some(location))
        .unwrap_or_else(|error| panic_engine(error));
}

/// Announces several ports the current computation intends to write.
///
/// Readers of a planned port wait for the running provider; a plan is not a
/// claim and installs nothing.
///
/// # Panics
///
/// Panics with an engine payload outside a computation.
#[track_caller]
pub fn plan_ports(scopes: &[irrigation_core::descriptor::WriteScope], label: &'static str) {
    let frame = current_or_engine_panic("port plan");
    let location = std::panic::Location::caller();
    frame
        .runtime()
        .plan_writes(scopes, label, Some(location))
        .unwrap_or_else(|error| panic_engine(error));
}

/// Withdraws planned ports for the current computation.
///
/// # Panics
///
/// Panics with an engine payload outside a computation.
#[track_caller]
pub fn unplan_ports(scopes: &[irrigation_core::descriptor::WriteScope]) {
    let frame = current_or_engine_panic("port plan withdrawal");
    frame
        .runtime()
        .unplan_writes(scopes)
        .unwrap_or_else(|error| panic_engine(error));
}

/// A group of write scopes that can be planned and claimed together.
///
/// The group is the user-facing shape behind [`plan_ports`] and
/// [`claim_ports`]; both operations are all-or-none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteGroup {
    scopes: Vec<irrigation_core::descriptor::WriteScope>,
}

impl WriteGroup {
    /// An empty group.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a group from scopes.
    #[must_use]
    pub fn from_scopes(
        scopes: impl IntoIterator<Item = irrigation_core::descriptor::WriteScope>,
    ) -> Self {
        Self {
            scopes: scopes.into_iter().collect(),
        }
    }

    /// Adds one scope to the group.
    pub fn add(&mut self, scope: irrigation_core::descriptor::WriteScope) {
        self.scopes.push(scope);
    }

    /// Adds several scopes.
    pub fn extend(
        &mut self,
        scopes: impl IntoIterator<Item = irrigation_core::descriptor::WriteScope>,
    ) {
        self.scopes.extend(scopes);
    }

    /// Announces every port in the group.
    pub fn plan(&self, label: &'static str) {
        plan_ports(&self.scopes, label);
    }

    /// Withdraws every planned port in the group.
    pub fn unplan(&self) {
        unplan_ports(&self.scopes);
    }

    /// Claims every port in the group atomically.
    pub fn claim(&self, label: &'static str) {
        claim_ports(&self.scopes, label);
    }

    /// The scopes in declaration order.
    #[must_use]
    pub fn scopes(&self) -> &[irrigation_core::descriptor::WriteScope] {
        &self.scopes
    }
}

/// Claims several ports atomically for the current computation.
///
/// A conflict on any port leaves the whole group unclaimed and panics with the
/// engine's structured conflict, exactly like a single write claim.
///
/// # Panics
///
/// Panics with an engine payload outside a computation or on a conflict.
#[track_caller]
pub fn claim_ports(scopes: &[irrigation_core::descriptor::WriteScope], label: &'static str) {
    let frame = current_or_engine_panic("port group claim");
    let location = std::panic::Location::caller();
    frame
        .runtime()
        .claim_writes(scopes, label, Some(location))
        .unwrap_or_else(|error| panic_engine(error));
}

/// Claims one unit's port for the current computation without defining it.
///
/// A read-modify-write verb claims the port before it projects the current
/// value, so a live writer of the same port conflicts instead of being
/// overwritten from a stale base. The projection is [`committed_content`],
/// never a tracked read of the port. [`write_group`] composes both steps for a
/// collection verb.
///
/// # Panics
///
/// Panics with an engine payload outside a computation or on a conflict.
#[track_caller]
pub fn claim_unit(unit: &UnitKey, label: &'static str) {
    let frame = current_or_engine_panic("unit write claim");
    let location = std::panic::Location::caller();
    frame
        .runtime()
        .claim_write(WriteScope::entity(unit), label, Some(location))
        .unwrap_or_else(|error| panic_engine(error));
}

/// Retracts one derived unit from the currently executing computation.
///
/// The retraction is part of the write group: it conflicts with a live writer of
/// the same scope and commits atomically with the rest of the node's
/// definitions.
///
/// # Panics
///
/// Panics with an engine payload outside a computation or on a claim conflict.
#[track_caller]
pub fn retract_unit(unit: UnitKey, label: &'static str) {
    let frame = current_or_engine_panic("retraction");
    let location = std::panic::Location::caller();
    frame
        .runtime()
        .claim_write(WriteScope::entity(&unit), label, Some(location))
        .unwrap_or_else(|error| panic_engine(error));
    // Staged on the frame that will commit, like a definition: inside a
    // per-iteration scope the retraction still belongs to the enclosing
    // computation.
    irrigation_core::frame::retract(unit);
}

/// One presence precondition a collection write verb stages at commit.
///
/// The precondition is staged as a [`irrigation_core::frame::Requirement`] on the
/// ports the verb's group installs, never evaluated against a commit that moves
/// them. [`write_group`] is the only producer of these, so a verb that follows
/// it cannot track a read of its own ports instead.
#[derive(Clone, Copy, Debug)]
pub struct PortPrecondition<'a> {
    ports: &'a [UnitKey],
    present: bool,
    description: &'static str,
}

impl<'a> PortPrecondition<'a> {
    /// Requires every port to be present when the group commits.
    #[must_use]
    pub fn present(ports: &'a [UnitKey], description: &'static str) -> Self {
        Self {
            ports,
            present: true,
            description,
        }
    }

    /// Requires every port to be absent when the group commits.
    #[must_use]
    pub fn absent(ports: &'a [UnitKey], description: &'static str) -> Self {
        Self {
            ports,
            present: false,
            description,
        }
    }
}

/// Runs one collection write verb's group from claim to definition.
///
/// The claim is taken before the base is projected, the projection is
/// [`committed_content`] (untracked), and the preconditions are staged as
/// commit-time requirements on the ports the group installs. The base is
/// therefore not a tracked read, and the precondition is re-checked at commit.
///
/// This helper cannot police the `write` closure. A verb that reads a port it
/// installs inside `write`, or in code that runs before this call, still
/// records the observation, and the install then moves the cell its own value
/// was computed from. That case is caught by the kernel rather than here:
/// `Inner::commit` runs `self_dependent_port` over the attempt's site traces and
/// fails the commit with `PreconditionUnmet { SelfDependent }`. The rule is
/// enforced there once; this helper only makes the correct spelling the easy
/// one.
///
/// `project` receives the View's committed content. A verb that overwrites
/// without projecting the base ignores it. `write` stages the definitions and
/// retractions; it must not read them back.
///
/// # Panics
///
/// Panics with an engine payload outside a computation, on a conflict with a
/// live writer, or when the View is not registered.
#[track_caller]
pub fn write_group<B, R>(
    scope: WriteScope,
    label: &'static str,
    preconditions: &[PortPrecondition<'_>],
    project: impl FnOnce(&dyn Any) -> B,
    write: impl FnOnce(B) -> R,
) -> R {
    let frame = current_or_engine_panic("write claim");
    let location = std::panic::Location::caller();
    frame
        .runtime()
        .claim_write(scope.clone(), label, Some(location))
        .unwrap_or_else(|error| panic_engine(error));
    let view = match &scope {
        WriteScope::Entity(unit) => unit.view,
        WriteScope::Field(view, _) | WriteScope::View(view) => *view,
    };
    ensure_view_id(view);
    let base = project(committed_content(view).as_ref());
    // Staged after the projection so a verb that reads its own ports is a
    // trace check failure at commit, never a requirement success.
    for precondition in preconditions {
        if precondition.present {
            require_present(precondition.ports, precondition.description);
        } else {
            require_absent(precondition.ports, precondition.description);
        }
    }
    write(base)
}

/// Waits for a live writer claiming one of `ports`.
///
/// Returns `true` when it waited, `false` when no live claim covers them.
///
/// # Panics
///
/// Panics with an engine payload outside a computation or when the wait fails.
pub fn wait_for_port(ports: &[UnitKey]) -> bool {
    let frame = current_or_engine_panic("port wait");
    frame
        .runtime()
        .wait_for_port(ports)
        .unwrap_or_else(|error| panic_engine(error))
}

/// Requires a value predicate, waiting for a live writer when it is false.
///
/// The predicate is re-evaluated after the wait. When no live writer claims a
/// port, the condition is a hard failure: value conditions are never deferred.
///
/// # Panics
///
/// Panics with `PreconditionUnmet { ValueMismatch }` when the condition cannot
/// become true.
pub fn require_value_await(
    mut condition: impl FnMut() -> bool,
    ports: &[UnitKey],
    description: &'static str,
) {
    for _ in 0..32 {
        if condition() {
            return;
        }
        if !wait_for_port(ports) {
            break;
        }
    }
    if condition() {
        return;
    }
    panic_engine(ComputeError::PreconditionUnmet {
        condition: description.to_string(),
        ports: ports.iter().map(|unit| format!("{unit:?}")).collect(),
        reason: irrigation_core::error::PreconditionReason::ValueMismatch,
    });
}

/// Requires a value predicate to hold before the write commits.
///
/// Value conditions are evaluated eagerly because the body already used the
/// value to compute the write. A false condition fails; it is never deferred.
/// The tracked reads the caller made still make the commit retry if a port
/// changes before publication.
///
/// # Panics
///
/// Panics with `PreconditionUnmet { ValueMismatch }` when `condition` is false.
pub fn require_value(condition: bool, ports: &[UnitKey], description: &'static str) {
    if condition {
        return;
    }
    panic_engine(ComputeError::PreconditionUnmet {
        condition: description.to_string(),
        ports: ports.iter().map(|unit| format!("{unit:?}")).collect(),
        reason: irrigation_core::error::PreconditionReason::ValueMismatch,
    });
}

/// Runs host-installed providers for missing ports.
///
/// Returns `true` when at least one provider ran; the caller re-reads the
/// condition afterwards.
///
/// # Panics
///
/// Panics with an engine payload outside a computation or when a provider fails.
pub fn provide_ports(ports: &[UnitKey]) -> bool {
    let frame = current_or_engine_panic("port provider");
    frame
        .runtime()
        .provide_ports(ports)
        .unwrap_or_else(|error| panic_engine(error))
}

/// Records a structural precondition for the current write group.
///
/// The caller evaluates the condition with tracked reads first; this helper
/// only publishes the requirement to the runtime.
///
/// `observed` is that evaluation, and it is a hint rather than a demand: when
/// `ports` is non-empty the runtime re-reads them at commit and decides then, so
/// passing `false` does not refuse — it is what a caller passes for a port a
/// provider will supply in the same revision. With no ports the observation is
/// the only answer and `false` fails immediately.
///
/// # Panics
///
/// Panics with an engine payload outside a computation.
pub fn require_condition(observed: bool, ports: &[UnitKey], description: &'static str) {
    let frame = current_or_engine_panic("write requirement");
    frame
        .runtime()
        .require_condition(observed, ports, description)
        .unwrap_or_else(|error| panic_engine(error));
}

/// Records that the given ports must be absent when the current write group
/// commits.
///
/// The caller must not read the ports instead: a unit this group installs is
/// read by the same commit that installs it, so the commit invalidates its own
/// trace, and the demand's retry re-checks the absence against the write it
/// has just published, where it can never hold again.
///
/// # Panics
///
/// Panics with an engine payload outside a computation, or with
/// `PreconditionUnmet { Contradiction }` when no port is named: a requirement
/// that names no port can never be re-evaluated at commit.
pub fn require_absent(ports: &[UnitKey], description: &'static str) {
    let _frame = current_or_engine_panic("write requirement");
    if ports.is_empty() {
        panic_engine(ComputeError::PreconditionUnmet {
            condition: description.to_string(),
            ports: Vec::new(),
            reason: irrigation_core::error::PreconditionReason::Contradiction,
        });
    }
    irrigation_core::frame::require(Requirement::absent(ports.to_vec(), description));
}

/// Records that the given ports must be present when the current write group
/// commits.
///
/// The counterpart of [`require_absent`] for a verb that installs or updates a
/// port: the requirement is re-evaluated at commit instead of the verb reading
/// a unit its own commit moves. A false requirement defers the whole group to
/// the end of the demand, where a provider or a later writer can still satisfy
/// it.
///
/// # Panics
///
/// Panics with an engine payload outside a computation, or with
/// `PreconditionUnmet { Contradiction }` when no port is named.
pub fn require_present(ports: &[UnitKey], description: &'static str) {
    // The runtime owns the empty-port policy and the Strict scope check, so the
    // presence form goes through the same entry. The boolean is not a
    // condition: for named ports the runtime stages presence, which is the
    // point — the verb does not evaluate it against a commit it moves.
    require_condition(true, ports, description);
}

/// Returns a registered View's committed content without tracing the read.
///
/// This is how [`write_group`] projects a collection write verb's base. The
/// projection must
/// not be a tracked read: the commit replaces the units a read would observe,
/// which invalidates the attempt's own trace, so the demand retries and applies
/// the edit to the state the first attempt published. For the same reason a
/// verb stages the precondition of a port its group installs with
/// [`require_absent`] / [`require_present`] instead of reading that port.
///
/// # Panics
///
/// Panics outside a computation or when the View is not registered.
#[must_use]
pub fn committed_content(view: ViewId) -> Arc<dyn Any + Send + Sync> {
    let frame = current_or_engine_panic("write base");
    frame.runtime().view_content(view).unwrap_or_else(|| {
        panic_engine(ComputeError::internal(format!(
            "view {view:?} is not registered"
        )))
    })
}

/// Returns the immutable content of a registered View.
///
/// # Panics
///
/// Panics outside a computation or when the View is not registered.
#[must_use]
pub fn view_content<C: Any + Send + Sync>(view: ViewId) -> Arc<C> {
    committed_content(view).downcast::<C>().unwrap_or_else(|_| {
        panic_engine(ComputeError::internal(format!(
            "view {view:?} content is not `{}`",
            std::any::type_name::<C>()
        )))
    })
}

/// Compares two type-erased values of the same concrete type.
pub fn values_equal<T: PartialEq + Any>(left: &Value, right: &Value) -> bool {
    match (left.downcast_ref::<T>(), right.downcast_ref::<T>()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Registers a View namespace and declares it for the committing write group.
///
/// Generated definition methods call this so a derived View used only inside a
/// component body still has a region to define units in. The declaration is
/// checked at commit against the provider's scope, which is the committing
/// frame's scope rather than the innermost scope frame's.
pub fn ensure_view(descriptor: &'static irrigation_core::descriptor::ViewDescriptor) {
    if let Some(frame) = irrigation_core::frame::current() {
        if let Some(scope) = irrigation_core::frame::commit_scope() {
            frame.runtime().declare_view(scope, descriptor.id);
        }
        frame.runtime().register_view(descriptor);
    }
}

/// Returns the node backing one expression site in the current scope.
///
/// # Panics
///
/// Panics with an engine payload outside a computation.
#[must_use]
pub fn node_of(site: irrigation_core::ids::SiteId) -> irrigation_core::ids::NodeId {
    let frame = current_or_engine_panic("node_of");
    frame
        .runtime()
        .site_node_in(Some(frame.node()), frame.scope(), site)
}

/// Records an observation of a certified node.
///
/// # Panics
///
/// Panics with an engine payload when the node is not certified for the current
/// revision.
pub fn observe_local(node: irrigation_core::ids::NodeId) {
    let frame = current_or_engine_panic("observe_local");
    frame
        .runtime()
        .observe(node)
        .unwrap_or_else(|error| panic_engine(error));
}

/// Observes a tracked local's node and returns a borrow of its value.
///
/// Used where the value is only borrowed (for example as the receiver of an
/// iterator adapter), so no temporary clone is created.
pub fn observe_ref<T>(node: irrigation_core::ids::NodeId, value: &T) -> &T {
    observe_local(node);
    value
}

/// Observes a tracked local's node and returns a clone of its value.
///
/// Cloning keeps the local available for later uses; handles are cheap to
/// clone, and larger values are usually `Arc`-backed by the View API.
pub fn observe_clone<T: Clone>(node: irrigation_core::ids::NodeId, value: &T) -> T {
    observe_local(node);
    value.clone()
}
