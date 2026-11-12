//! The demo program the Observatory observes.
//!
//! A tiny assignment language: a source document, the framework's text, lexer,
//! and parser components, and two ordinary components over the published AST.
//! The lexer and parser are on the board like every other stage — the board has
//! no idea what they are — and only the source document is editable.

use std::collections::HashSet;

use irrigation::view;

/// The demo program, declared as a tree.
///
/// The grammar's `@tree` annotations say which production builds which variant,
/// so the parser publishes this tree from the same records as the raw AST and a
/// component walks `StatementRef`/`ExpressionRef` instead of matching rule names
/// and child indices.
#[allow(dead_code)]
#[irrigation::tree_view(kind = derived, root = Document, view = Syntax)]
pub mod ast {
    /// A parsed program.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Document {
        /// The program and its statements, in source order.
        Program { items: Vec<Statement> },
    }

    /// One statement.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Statement {
        /// A binding.
        Assign {
            /// The bound name.
            name: String,
            /// The bound expression.
            value: Expression,
        },
        /// A print, with its arguments.
        Print {
            /// The printed expressions.
            args: Vec<Expression>,
        },
    }

    /// One expression.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Expression {
        /// A call.
        Call {
            /// The callee's name.
            callee: String,
            /// The arguments.
            args: Vec<Expression>,
        },
        /// An integer literal.
        Number {
            /// The literal's payload.
            value: i64,
        },
        /// A name reference.
        Name {
            /// The spelled name.
            name: String,
        },
    }
}

/// The symbol table, as a whole-value list.
#[view(derived)]
pub struct Symbols(List<Symbol>);

/// One bound name.
#[derive(Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The bound name.
    pub name: String,
}

impl std::fmt::Debug for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// Unresolved references, as a whole-value list.
#[view(derived)]
pub struct Diagnostics(List<String>);

/// A second source, of a different kind: numbers the host owns.
#[view(source)]
pub struct Weights(List<u64>);

/// The sum reduction, so the weights have a downstream View.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sum;

impl irrigation::Reduction for Sum {
    type Item = u64;
    type Aggregate = u64;

    fn leaf(item: &Self::Item) -> Self::Aggregate {
        *item
    }

    fn combine(left: &Self::Aggregate, right: &Self::Aggregate) -> Self::Aggregate {
        left + right
    }

    fn identity() -> Self::Aggregate {
        0
    }
}

/// Weighted total, as a reduced View.
#[view(derived)]
pub struct Total(Reduced<Sum>);

/// Every bound name in source order.
///
/// The tree's accessors read one unit each: the name of a statement is one leaf,
/// and the list of statements is one child slot. There is no rule table and no
/// payload table to materialize, so this component is woken by the names it
/// reads rather than by every token in the document.
fn bound_names() -> Vec<String> {
    let Some(document) = ast::Syntax.root() else {
        return Vec::new();
    };
    document
        .items()
        .into_iter()
        .filter_map(|statement| statement.name())
        .collect()
}

/// Every unresolved reference, in source order.
fn unresolved_names() -> Vec<String> {
    let Some(document) = ast::Syntax.root() else {
        return Vec::new();
    };
    let defined: HashSet<String> = bound_names().into_iter().collect();
    let mut messages = Vec::new();
    for statement in document.items() {
        for expression in arguments(&statement) {
            unresolved_in(&expression, &defined, &mut messages);
        }
    }
    messages
}

/// The expressions one statement evaluates.
fn arguments(statement: &ast::StatementRef) -> Vec<ast::ExpressionRef> {
    match statement.kind() {
        Some(ast::StatementKind::Assign) => statement.value().into_iter().collect(),
        Some(ast::StatementKind::Print) => statement.args(),
        None => Vec::new(),
    }
}

/// Reports every `Name` in `expression` that `defined` does not bind.
fn unresolved_in(
    expression: &ast::ExpressionRef,
    defined: &HashSet<String>,
    out: &mut Vec<String>,
) {
    match expression.kind() {
        Some(ast::ExpressionKind::Name) => {
            if let Some(name) = expression.name() {
                if !defined.contains(&name) {
                    out.push(format!("unresolved name `{name}`"));
                }
            }
        }
        Some(ast::ExpressionKind::Call) => {
            for argument in expression.args() {
                unresolved_in(&argument, defined, out);
            }
        }
        Some(ast::ExpressionKind::Number) => {}
        None => {}
    }
}

#[irrigation::irrigation]
pub mod resolve {
    /// Symbols: one component, one View. An owned parameter makes this an
    /// invoked component, so the runtime keeps its identity on the board.
    #[component]
    pub fn symbols(version: u8) -> usize {
        let _ = version;
        let names = super::bound_names();
        super::Symbols::from(
            names
                .iter()
                .map(|name| super::Symbol { name: name.clone() })
                .collect::<Vec<_>>(),
        );
        names.len()
    }

    /// Diagnostics: a second component reading the same source.
    #[component]
    pub fn diagnostics(strict: bool) -> usize {
        let messages: Vec<String> = super::unresolved_names()
            .into_iter()
            .filter(|message| strict || message.len() > 4)
            .collect();
        let count = messages.len();
        super::Diagnostics::from(messages);
        count
    }

    /// The weights total: a third component, reading the other source.
    #[component]
    pub fn total(scale: u64) -> u64 {
        let values = super::Weights.field0().to_vec();
        super::Total::from(values.clone());
        values.iter().sum::<u64>() * scale
    }

    /// The root composes them; each call carries a constant, which is enough
    /// for the runtime to keep the component boundaries. Every stage reads the
    /// Views it needs, so the root's demand reaches the whole pipeline.
    #[component]
    pub fn resolve_program(source: &irrigation_syntax::SourceFile) -> u64 {
        let _ = source;
        let a = symbols(1);
        let b = diagnostics(true);
        let c = total(1);
        (a as u64) + (b as u64) + c
    }
}
