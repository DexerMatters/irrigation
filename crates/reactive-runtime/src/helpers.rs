//! Typed read and define helpers used by generated View code.
//!
//! Generated accessors are ordinary-looking methods; these helpers are the
//! boundary where a method call becomes a tracked observation. They keep the
//! macro output small and make the read protocol testable on its own.

use std::any::Any;
use std::sync::Arc;

use reactive_core::error::ComputeError;
use reactive_core::frame::current_or_engine_panic;
use reactive_core::ids::{FieldId, ViewId};
use reactive_core::ops::panic_engine;
use reactive_core::value::{Value, value_of};
use reactive_core::descriptor::UnitKey;

use crate::inner::DefinitionDraft;

/// Whether a field currently uses the keyed derived protocol.
#[must_use]
pub fn field_is_keyed(view: ViewId, field: FieldId) -> bool {
    reactive_core::frame::current()
        .is_some_and(|frame| frame.runtime().field_is_keyed(view, field))
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
        Ok(value) => value.downcast_ref::<T>().cloned(),
        Err(_) => None,
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
    reactive_core::stage(DefinitionDraft {
        unit,
        value: value_of(value),
        eq: values_equal::<T>,
        label,
        location: Some(std::panic::Location::caller()),
    });
}

/// Returns the immutable content of a registered View.
///
/// # Panics
///
/// Panics outside a computation or when the View is not registered.
#[must_use]
pub fn view_content<C: Any + Send + Sync>(view: ViewId) -> Arc<C> {
    let frame = current_or_engine_panic("view content read");
    let runtime = Arc::clone(frame.runtime());
    let content = runtime
        .view_content(view)
        .unwrap_or_else(|| panic_engine(ComputeError::internal(format!("view {view:?} is not registered"))));
    content.downcast::<C>().unwrap_or_else(|_| {
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

/// Registers a View namespace if a computation is running.
///
/// Generated definition methods call this so a derived View used only inside a
/// component body still has a region to define units in.
pub fn ensure_view(descriptor: &'static reactive_core::descriptor::ViewDescriptor) {
    if let Some(frame) = reactive_core::frame::current() {
        frame.runtime().register_view(descriptor);
    }
}

/// Returns the node backing one expression site in the current scope.
///
/// # Panics
///
/// Panics with an engine payload outside a computation.
#[must_use]
pub fn node_of(site: reactive_core::ids::SiteId) -> reactive_core::ids::NodeId {
    let frame = current_or_engine_panic("node_of");
    frame.runtime().site_node(frame.scope(), site)
}

/// Records an observation of a certified node.
///
/// # Panics
///
/// Panics with an engine payload when the node is not certified for the current
/// revision.
pub fn observe_local(node: reactive_core::ids::NodeId) {
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
pub fn observe_ref<T>(node: reactive_core::ids::NodeId, value: &T) -> &T {
    observe_local(node);
    value
}

/// Observes a tracked local's node and returns a clone of its value.
///
/// Cloning keeps the local available for later uses; handles are cheap to
/// clone, and larger values are usually `Arc`-backed by the View API.
pub fn observe_clone<T: Clone>(node: reactive_core::ids::NodeId, value: &T) -> T {
    observe_local(node);
    value.clone()
}

/// Reads a node by identity, recording the observation.
pub fn read_node<T: Clone + Any>(node: reactive_core::ids::NodeId) -> T {
    let frame = current_or_engine_panic("node read");
    let value = frame
        .runtime()
        .read_unit(node)
        .unwrap_or_else(|error| panic_engine(error));
    match value.downcast_ref::<T>() {
        Some(value) => value.clone(),
        None => panic_engine(ComputeError::TypeMismatch {
            expected: std::any::type_name::<T>(),
        }),
    }
}
