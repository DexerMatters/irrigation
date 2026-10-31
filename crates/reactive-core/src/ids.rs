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

/// The revision at which a value last changed.
///
/// `changed_at == 0` means "never changed since creation".
pub type Version = u64;

/// Index of a memoized computation node in the runtime arena.
///
/// A `NodeId` is only meaningful inside the runtime that produced it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    /// A sentinel used where an optional node is convenient.
    pub const NONE: Self = Self(u32::MAX);

    /// Returns the raw arena index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Self::NONE {
            f.write_str("NodeId(NONE)")
        } else {
            write!(f, "NodeId({})", self.0)
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
pub struct ComponentId(pub u32);

impl fmt::Debug for ComponentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ComponentId({:#010x})", self.0)
    }
}

/// Registered identity of one View namespace.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewId(pub u32);

impl fmt::Debug for ViewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ViewId({:#010x})", self.0)
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
/// The macro uses this to derive stable [`SiteId`] and [`ComponentId`] values
/// from fully qualified names.
#[must_use]
pub const fn const_hash(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
        i += 1;
    }
    hash
}

/// Folds two hashes into one, used for composite identities.
#[must_use]
pub const fn hash_combine(a: u64, b: u64) -> u64 {
    a.rotate_left(5) ^ b.wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_hash_is_deterministic_and_distinguishes_names() {
        assert_eq!(const_hash("a::b"), const_hash("a::b"));
        assert_ne!(const_hash("a::b"), const_hash("a::c"));
        assert_ne!(const_hash(""), const_hash("x"));
    }

    #[test]
    fn node_id_none_is_distinct_from_real_indices() {
        assert_eq!(NodeId::NONE.index(), u32::MAX as usize);
        assert_eq!(NodeId(7).index(), 7);
    }
}
