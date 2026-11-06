//! Component descriptors and invocation demand.
//!
//! A component function is compiled into a [`ComponentSpec`]: its arguments are
//! interned by value, its body runs as one node, and each output projection is
//! a separate node. Consumers of output 0 are therefore not invalidated when
//! only output 1 changes.

use std::any::Any;
use std::hash::Hash;
use std::sync::Arc;

use reactive_core::error::Result;
use reactive_core::frame::current_or_engine_panic;
use reactive_core::ids::ComponentId;
use reactive_core::ops::panic_engine;
use reactive_core::value::Value;

use crate::inner::arguments_hash;

/// A registered component generator.
///
/// One component may have many invocations, each identified by its argument
/// tuple. The body node computes the whole output product; projection nodes
/// provide independent cutoff.
pub trait ComponentSpec: Send + Sync + 'static {
    /// Interned argument tuple.
    type Args: Clone + Hash + PartialEq + Send + Sync + 'static;
    /// Output product.
    type Output: Clone + PartialEq + Send + Sync + 'static;

    /// Stable component identity.
    const ID: ComponentId;
    /// Fully qualified name for diagnostics.
    const NAME: &'static str;
    /// Number of output projections.
    const ARITY: usize;

    /// Executes the component body.
    fn run(args: &Self::Args) -> Self::Output;

    /// Extracts one output projection as a type-erased value.
    fn project(output: &Self::Output, index: usize) -> Value;

    /// Rebuilds the output from demanded projections.
    fn reconstruct(parts: &[Value]) -> Self::Output;

    /// Compares one projection with the previous value.
    fn projection_eq(index: usize, left: &Value, right: &Value) -> bool;
}

/// A root component installed on a runtime.
pub trait RootComponent: Send + Sync + 'static {
    /// Output product.
    type Output: Clone + PartialEq + Send + Sync + 'static;

    /// Stable component identity.
    const ID: ComponentId;
    /// Fully qualified name for diagnostics.
    const NAME: &'static str;
    /// Number of output projections.
    const ARITY: usize;
    /// Views this component reads, registered at install time.
    const VIEWS: &'static [&'static reactive_core::descriptor::ViewDescriptor];

    /// Executes the component body.
    fn run() -> Self::Output;

    /// Extracts one output projection as a type-erased value.
    fn project(output: &Self::Output, index: usize) -> Value;

    /// Compares one projection with the previous value.
    fn projection_eq(index: usize, left: &Value, right: &Value) -> bool;

    /// Rebuilds the output product from demanded projections.
    fn reconstruct(parts: &[Value]) -> Self::Output;
}

/// Demands one projection of a component invocation.
///
/// The body still runs, but only the requested projection node is evaluated and
/// versioned; the sibling projections stay unevaluated until demanded.
///
/// # Panics
///
/// Panics with an engine payload outside a computation or on engine failure.
pub fn call_component_projection<S: ComponentSpec>(args: S::Args, index: usize) -> Value {
    let frame = current_or_engine_panic("component call");
    let runtime = Arc::clone(frame.runtime());
    let hash = arguments_hash(&args);
    let args_eq = |left: &dyn Any, right: &dyn Any| match (
        left.downcast_ref::<S::Args>(),
        right.downcast_ref::<S::Args>(),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    };
    let (body, outputs) = runtime.invocation_node(
        S::ID,
        S::NAME,
        hash,
        Arc::new(args.clone()),
        frame.scope(),
        S::ARITY,
        &args_eq,
    );
    let Some(node) = outputs.get(index).copied() else {
        panic_engine(reactive_core::ComputeError::internal(
            "projection index out of range",
        ));
    };
    let body_eq = |left: &Value, right: &Value| match (
        left.downcast_ref::<S::Output>(),
        right.downcast_ref::<S::Output>(),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    };
    let body_value = eval_value(
        &runtime,
        body,
        || reactive_core::value::value_of(S::run(&args)),
        &body_eq,
    );
    let projection_eq = |left: &Value, right: &Value| S::projection_eq(index, left, right);
    eval_value(
        &runtime,
        node,
        || {
            runtime
                .observe(body)
                .unwrap_or_else(|error| panic_engine(error));
            let output = body_value.downcast_ref::<S::Output>().unwrap_or_else(|| {
                panic_engine(reactive_core::ComputeError::internal(
                    "component body produced an unexpected type",
                ))
            });
            S::project(output, index)
        },
        &projection_eq,
    )
}

/// Demands one component invocation and returns its output product.
///
/// # Panics
///
/// Panics with an engine payload outside a computation or on engine failure.
pub fn call_component<S: ComponentSpec>(args: S::Args) -> S::Output {
    let frame = current_or_engine_panic("component call");
    let runtime = Arc::clone(frame.runtime());
    let hash = arguments_hash(&args);
    let args_eq = |left: &dyn Any, right: &dyn Any| match (
        left.downcast_ref::<S::Args>(),
        right.downcast_ref::<S::Args>(),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    };
    let (body, outputs) = runtime.invocation_node(
        S::ID,
        S::NAME,
        hash,
        Arc::new(args.clone()),
        frame.scope(),
        S::ARITY,
        &args_eq,
    );

    let body_eq = |left: &Value, right: &Value| match (
        left.downcast_ref::<S::Output>(),
        right.downcast_ref::<S::Output>(),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    };
    let body_value = eval_value(
        &runtime,
        body,
        || reactive_core::value::value_of(S::run(&args)),
        &body_eq,
    );

    let mut parts = Vec::with_capacity(S::ARITY);
    for (index, node) in outputs.iter().enumerate() {
        let node = *node;
        let projection_eq = |left: &Value, right: &Value| S::projection_eq(index, left, right);
        // Reading the body inside the projection node records the dependency,
        // so a body change invalidates the projections even when the value was
        // produced by an earlier demand.
        let value = eval_value(
            &runtime,
            node,
            || {
                runtime
                    .observe(body)
                    .unwrap_or_else(|error| panic_engine(error));
                let output = body_value.downcast_ref::<S::Output>().unwrap_or_else(|| {
                    panic_engine(reactive_core::ComputeError::internal(
                        "component body produced an unexpected type",
                    ))
                });
                S::project(output, index)
            },
            &projection_eq,
        );
        parts.push(value);
    }
    S::reconstruct(&parts)
}

/// Evaluates one node with a value-producing closure, converting engine
/// failures into a panic payload that the enclosing node catches.
pub fn eval_value(
    runtime: &Arc<dyn reactive_core::ops::RuntimeOps>,
    node: reactive_core::ids::NodeId,
    compute: impl FnOnce() -> Value,
    eq: &dyn Fn(&Value, &Value) -> bool,
) -> Value {
    let mut compute = {
        let mut compute = Some(compute);
        move || -> Result<Value> {
            let compute = compute.take().unwrap_or_else(|| {
                panic_engine(reactive_core::ComputeError::internal(
                    "node compute invoked twice",
                ))
            });
            Ok(compute())
        }
    };
    match runtime.eval(node, &mut compute, eq) {
        Ok(value) => value,
        Err(error) => panic_engine(error),
    }
}

/// Demands an already-known node, recording the observation.
pub fn demand_node(node: reactive_core::ids::NodeId) -> Value {
    let frame = current_or_engine_panic("demand_node");
    let runtime = Arc::clone(frame.runtime());
    match runtime.read_unit(node) {
        Ok(value) => value,
        Err(error) => panic_engine(error),
    }
}
