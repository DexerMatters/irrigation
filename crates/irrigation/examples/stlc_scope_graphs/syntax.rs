//! The typed syntax tree, and the readers over it.
//!
//! The tree is a `#[tree_view]` declaration; the grammar's `@tree` annotations
//! say which production builds which variant, and the parser publishes it from
//! the same records as the raw AST. Nothing here classifies a node: a stage takes
//! a [`TermRef`] and matches its [`TermKind`].

use crate::types::Type;

/// The declared syntax tree.
///
/// The host never constructs these values: the parser builds the tree from the
/// parse, so the generated construction helpers and the owned types are unused
/// here. The refs, kinds, and accessors are the whole surface this example uses.
#[allow(dead_code)]
#[irrigation::tree_view(kind = derived, root = Document, view = Syntax)]
pub mod ast {
    /// A parsed program.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Document {
        /// The program and its body.
        Program { term: Box<Term> },
    }

    /// One term.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Term {
        /// A variable reference.
        Var { name: String },
        /// A lambda abstraction; `body` is in the binder's scope.
        Lam {
            name: String,
            ty: Ty,
            body: Box<Term>,
        },
        /// A `let`; `value` is in the enclosing scope, `body` in the new one.
        Let {
            name: String,
            value: Box<Term>,
            body: Box<Term>,
        },
        /// A conditional.
        If {
            cond: Box<Term>,
            yes: Box<Term>,
            no: Box<Term>,
        },
        /// An application.
        App { func: Box<Term>, arg: Box<Term> },
        /// An integer literal.
        Int { int: i64 },
        /// A boolean literal.
        Bool { flag: bool },
    }

    /// A type annotation.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Ty {
        /// `Int`.
        Int,
        /// `Bool`.
        Bool,
        /// A function type.
        Arrow { domain: Box<Ty>, codomain: Box<Ty> },
    }
}

pub use ast::{TermKind, TermRef, TyKind};

/// The program's root term, when the parse reached one.
///
/// Reads the typed tree's root unit, so a component that calls this observes one
/// unit and re-runs when the program changes.
#[must_use]
pub fn root() -> Option<TermRef> {
    ast::Syntax.root()?.term()
}

/// The type a lambda's parameter annotation denotes.
#[must_use]
pub fn annotation(term: &TermRef) -> Type {
    match term.ty() {
        Some(ty) => type_of(&ty),
        None => Type::Error,
    }
}

fn type_of(ty: &ast::TyRef) -> Type {
    match ty.kind() {
        Some(TyKind::Int) => Type::Int,
        Some(TyKind::Bool) => Type::Bool,
        Some(TyKind::Arrow) => {
            let (Some(domain), Some(codomain)) = (ty.domain(), ty.codomain()) else {
                return Type::Error;
            };
            let domain = type_of(&domain);
            let codomain = type_of(&codomain);
            if domain == Type::Error || codomain == Type::Error {
                Type::Error
            } else {
                Type::Fun(Box::new(domain), Box::new(codomain))
            }
        }
        None => Type::Error,
    }
}
