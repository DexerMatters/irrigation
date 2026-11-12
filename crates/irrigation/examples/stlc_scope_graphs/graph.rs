//! The scope graph's node and edge payloads.
//!
//! A scope graph is `(S, L, →, ↦)`: scopes, labelled edges, and declarations.
//! This subset has lexical `P` (parent), `D` (declaration), `R` (reference),
//! `child`, and `T` (type) edges. A binder's term node carries its binding kind;
//! a type scope is one shared node per rendered type.
//!
//! The payloads carry no identities: an entity's identity is the emission
//! expression that wrote it, so nothing here has to be hashed or numbered.

use std::fmt;

/// What a term node is.
///
/// The graph stores the syntax tree's own kind, so a node's payload and the node
/// it was built from are the same value: there is no translation table.
pub use crate::syntax::TermKind;

/// What introduced a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindingKind {
    /// A lambda parameter.
    Lambda,
    /// A `let` binder.
    Let,
}

/// One graph node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeNode {
    /// A lexical scope.
    Scope {
        /// A short label.
        label: String,
    },
    /// One semantic term. A binder carries its declaration kind here.
    Term {
        /// The term's kind.
        kind: TermKind,
        /// The declaration kind, for a binder.
        binding: Option<BindingKind>,
        /// A short label.
        label: String,
    },
    /// A canonical type scope, shared by every term of that type.
    Type {
        /// The rendered type.
        label: String,
    },
}

impl fmt::Display for ScopeNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scope { label } | Self::Term { label, .. } | Self::Type { label } => {
                f.write_str(label)
            }
        }
    }
}

/// One graph edge. Its `Debug` form is the label the graph draws.
#[derive(Clone, PartialEq, Eq)]
pub enum ScopeEdge {
    /// The lexical parent edge.
    Parent,
    /// A declaration: a scope declares a binding term.
    Declares {
        /// The declared name.
        name: String,
    },
    /// A reference: a scope contains a variable term.
    Occurs {
        /// The referenced name.
        name: String,
    },
    /// A term-to-subterm edge.
    Child {
        /// The child's field index.
        slot: u8,
    },
    /// A term-to-type-scope edge.
    TypeOf,
}

impl fmt::Debug for ScopeEdge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parent => f.write_str("P"),
            Self::Declares { name } => write!(f, "D {name}"),
            Self::Occurs { name } => write!(f, "R {name}"),
            Self::Child { slot } => write!(f, "child#{slot}"),
            Self::TypeOf => f.write_str("T"),
        }
    }
}
