//! Lowering vocabulary for generated components.
//!
//! The standard component macro lowers a Rust-shaped function into typed
//! builder calls rather than a separately interpreted IR arena. This crate
//! names the pieces of that contract so diagnostics, tooling, and future
//! backends can talk about them:
//!
//! * a [`Site`] is one lowered expression, identified by `(scope, site)`;
//! * a [`Projection`] is one independently versioned output of a component;
//! * a [`Generator`] describes the component a call targets.
//!
//! Keeping these names in one place is what makes the "fine logical nodes,
//! economical physical execution" rule reviewable: a site is a logical node,
//! while execution is batched by the runtime.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

use reactive_core::ids::{ComponentId, SiteId};

/// One lowered expression site.
///
/// Site identities are assigned by the macro and are stable for a built
/// binary. They are never source byte offsets and carry no cross-binary
/// meaning, which is why durable caching across binaries is a separate design.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Site {
    /// The expression's compile-time identity.
    pub id: SiteId,
    /// The lowering shape, used in diagnostics.
    pub shape: SiteShape,
}

impl Site {
    /// Builds a site descriptor.
    #[must_use]
    pub const fn new(id: SiteId, shape: SiteShape) -> Self {
        Self { id, shape }
    }
}

/// What a lowered site computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SiteShape {
    /// A pure primitive such as arithmetic, a field projection, or a call.
    Primitive,
    /// A read of a View unit.
    Read,
    /// A branch condition that selects a continuation.
    Control,
    /// A component request.
    Call,
    /// A collection operator over stable members.
    Members,
    /// A whole-collection structure observation.
    Structure,
}

/// One independently versioned output of a component.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Projection {
    /// Position in the output product.
    pub index: u16,
}

impl Projection {
    /// The first output projection.
    pub const FIRST: Self = Self { index: 0 };

    /// Builds a projection descriptor.
    #[must_use]
    pub const fn new(index: u16) -> Self {
        Self { index }
    }
}

/// A component generator as seen by a caller.
#[derive(Clone, Copy, Debug)]
pub struct Generator {
    /// Registered component identity.
    pub component: ComponentId,
    /// Fully qualified name.
    pub name: &'static str,
    /// Number of output projections.
    pub arity: usize,
}

impl Generator {
    /// Builds a generator descriptor.
    #[must_use]
    pub const fn new(component: ComponentId, name: &'static str, arity: usize) -> Self {
        Self {
            component,
            name,
            arity,
        }
    }

    /// The projection descriptor for one output.
    #[must_use]
    pub const fn projection(&self, index: u16) -> Projection {
        Projection::new(index)
    }
}

/// A read performed by a lowered site, for explainability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Observation {
    /// The site that performed the read.
    pub site: SiteId,
    /// The projection observed on the provider, when the provider is a
    /// component rather than a View unit.
    pub projection: Option<Projection>,
}

impl Observation {
    /// Builds an observation record.
    #[must_use]
    pub const fn new(site: SiteId, projection: Option<Projection>) -> Self {
        Self { site, projection }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_reports_its_arity() {
        let generator = Generator::new(ComponentId(7), "analysis::infer", 2);
        assert_eq!(generator.arity, 2);
        assert_eq!(generator.projection(1), Projection::new(1));
    }
}
