//! Opaque identity types shared by the whole system.
//!
//! Users never name these. Generated code and the runtime use them to key
//! arenas, scopes, and views. All identifiers are stable within one running
//! runtime; they are not a durable cache format.

use core::fmt;

/// Monotonically increasing input revision.
///
/// Revision `0` is the initial state before any transaction.
pub type Revision = u64;

/// A monotonic stamp: a value version, a routing generation, or a change
/// revision.
///
/// `Version` is an alias, not a distinct type, and it carries three different
/// counters: the per-runtime monotonic value version a cell takes on every
/// install (`NodeRecord::value_version`), the routing generation a provider
/// handover advances, and the revision a value last changed at
/// (`NodeRecord::changed_at`).
///
/// The alias is why a version can be compared with a revision and still compile --
/// the compiler cannot tell them apart -- and it has cost this codebase real
/// defects, including the `GraphFacts` drift label that compared the two directly.
/// Treat any `Version` as "a number from the runtime's counters" and read the
/// field's own doc to learn which counter it is.
pub type Version = u64;

/// Index and generation of a memoized computation node in the runtime arena.
///
/// The low 20 bits are the arena slot and the high 12 bits the slot's
/// generation. A reclaimed slot is reused only after its generation advances,
/// so a stale `NodeId` can be detected instead of silently observing a new node.
/// A `NodeId` is only meaningful inside the runtime that produced it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    /// Number of low bits used for the arena index.
    pub const INDEX_BITS: u32 = 20;
    /// Mask of the arena index.
    pub const INDEX_MASK: u32 = (1 << Self::INDEX_BITS) - 1;
    /// Maximum generation before the slot is retired.
    pub const MAX_GENERATION: u32 = (1 << (32 - Self::INDEX_BITS)) - 1;

    /// A sentinel used where an optional node is convenient.
    pub const NONE: Self = Self(u32::MAX);

    /// Builds a node identity from a slot and generation.
    #[must_use]
    pub const fn new(index: u32, generation: u32) -> Self {
        Self(((generation & Self::MAX_GENERATION) << Self::INDEX_BITS) | (index & Self::INDEX_MASK))
    }

    /// Builds a node identity from a slot and generation, or returns `None`
    /// when either half does not fit the packed representation.
    ///
    /// Use this at boundaries that accept an identity from outside the runtime;
    /// [`NodeId::new`] deliberately masks for trusted internal callers.
    #[must_use]
    pub const fn try_new(index: u32, generation: u32) -> Option<Self> {
        if index > Self::INDEX_MASK || generation > Self::MAX_GENERATION {
            return None;
        }
        Some(Self((generation << Self::INDEX_BITS) | index))
    }

    /// Returns the arena slot.
    #[must_use]
    pub const fn index(self) -> usize {
        (self.0 & Self::INDEX_MASK) as usize
    }

    /// Returns the slot generation.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.0 >> Self::INDEX_BITS
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Self::NONE {
            f.write_str("NodeId(NONE)")
        } else if self.generation() == 0 {
            write!(f, "NodeId({})", self.index())
        } else {
            write!(f, "NodeId({}@{})", self.index(), self.generation())
        }
    }
}

/// Compile-time identity of one lowered expression site.
///
/// Site ids are assigned by the macro and are stable for a built binary.
/// They are not source byte offsets and carry no cross-binary meaning.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SiteId(pub u32);

impl fmt::Debug for SiteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SiteId({:#010x})", self.0)
    }
}

/// Registered identity of one component definition.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentId(pub u64);

impl fmt::Debug for ComponentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ComponentId({:#018x})", self.0)
    }
}

/// Registered identity of one View namespace.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewId(pub u64);

impl fmt::Debug for ViewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ViewId({:#018x})", self.0)
    }
}

/// Index of a field or projection inside a View.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldId(pub u16);

impl fmt::Debug for FieldId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FieldId({})", self.0)
    }
}

/// Identity of one dynamic computation scope.
///
/// A scope groups the nodes produced by one recursive invocation, one
/// collection member, or one loop iteration. Nodes are keyed by
/// `(scope, site)`, so scopes are what keep per-member work apart.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub u64);

impl ScopeId {
    /// The scope of top-level demand, outside any invocation.
    pub const ROOT: Self = Self(0);

    /// Returns the raw identifier.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for ScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ScopeId({:#018x})", self.0)
    }
}

/// FNV-1a hash of a string, usable in `const` contexts.
///
/// The macro derives [`ViewId`] and [`ComponentId`] values from fully qualified
/// names with the full 64-bit hash; registration rejects two names that share an
/// id. [`SiteId`]s are sequential per component, not hashed.
#[must_use]
pub const fn const_hash(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
        i += 1;
    }
    hash
}

#[cfg(test)]
#[path = "../tests/unit/ids/tests.rs"]
mod tests;
