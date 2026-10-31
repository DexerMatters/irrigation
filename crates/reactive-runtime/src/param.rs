//! Tracked parameters for collection operators.
//!
//! A collection operator runs a user closure once per element. The element's
//! value is not a compile-time constant of that closure: it must become a
//! dependency of every primitive that reads it. [`Param`] carries the element's
//! parameter node and records that observation on each dereference, so a
//! changed element repairs exactly the closure primitives that consumed it.

use std::ops::Deref;

use reactive_core::frame::current_or_engine_panic;
use reactive_core::ids::NodeId;
use reactive_core::ops::panic_engine;

/// A tracked element parameter.
///
/// `Deref` records an observation of the parameter node in the enclosing
/// computation, which is what makes `|n| *n * 2` incremental per element.
pub struct Param<T> {
    node: NodeId,
    value: T,
}

impl<T> std::fmt::Debug for Param<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Param").field("node", &self.node).finish_non_exhaustive()
    }
}

impl<T> Param<T> {
    /// Builds a parameter from a certified node and its value.
    #[must_use]
    pub fn new(node: NodeId, value: T) -> Self {
        Self { node, value }
    }

    /// The node carrying this parameter.
    #[must_use]
    pub fn node(&self) -> NodeId {
        self.node
    }

    /// Returns the value without recording an observation.
    ///
    /// Only used by operators that already recorded the parameter.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.value
    }
}

impl<T> Deref for Param<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        let frame = current_or_engine_panic("parameter read");
        frame
            .runtime()
            .observe(self.node)
            .unwrap_or_else(|error| panic_engine(error));
        &self.value
    }
}
