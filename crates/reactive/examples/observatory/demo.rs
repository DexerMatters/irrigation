//! The demo program the Observatory observes.
//!
//! A tiny assignment language: three statements, one derived symbol table, and
//! one derived diagnostics list. It is small on purpose — every module on the
//! page is a View a compiler would recognise, not a runtime construct.

use reactive::tree_view;

/// The syntax tree: a document, statements, expressions.
#[tree_view(root = Document, view = Syntax)]
pub mod ast {
    /// A whole program.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Document {
        /// A flat list of statements.
        Program {
            /// Statements in source order.
            statements: Vec<Statement>,
        },
    }

    /// One statement.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Statement {
        /// `name := value`
        Assign {
            /// Bound name.
            name: String,
            /// Bound expression.
            value: Expression,
        },
        /// `print(expression, ...)`
        Print {
            /// Printed expressions.
            values: Vec<Expression>,
        },
    }

    /// One expression.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Expression {
        /// A bare identifier.
        Name(String),
        /// An integer literal.
        Number(i64),
        /// A call: `callee(args...)`.
        Call {
            /// Callee name.
            callee: String,
            /// Arguments.
            args: Vec<Expression>,
        },
    }
}

use reactive::view;

/// The symbol table, as a whole-value list.
#[view(derived)]
pub struct Symbols(List<Symbol>);

/// One bound name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// The bound name.
    pub name: String,
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

impl reactive::Reduction for Sum {
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

#[reactive::reactive]
pub mod resolve {
    use super::ast;

    /// Every unbound name inside `expression`.
    #[component]
    pub fn unresolved(expression: ast::ExpressionRef, defined: Vec<String>) -> Vec<String> {
        let kind = expression.kind();
        if kind == Some(ast::ExpressionKind::Name) {
            let name = expression.name().unwrap_or_default();
            if defined.contains(&name) {
                Vec::new()
            } else {
                vec![format!("unresolved name `{name}`")]
            }
        } else if kind == Some(ast::ExpressionKind::Call) {
            let args = expression.args();
            args.iter()
                .flat_map(|arg| unresolved(arg.clone(), defined.clone()))
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Unbound names in one statement.
    #[component]
    pub fn unresolved_statement(statement: ast::StatementRef, defined: Vec<String>) -> Vec<String> {
        let kind = statement.kind();
        if kind == Some(ast::StatementKind::Print) {
            let values = statement.values();
            values
                .iter()
                .flat_map(|value| unresolved(value.clone(), defined.clone()))
                .collect()
        } else if kind == Some(ast::StatementKind::Assign) {
            match statement.value() {
                Some(value) => unresolved(value, defined),
                None => Vec::new(),
            }
        } else {
            Vec::new()
        }
    }

    /// Symbols: one component, one View. An owned parameter makes this an
    /// invoked component, so the runtime keeps its identity on the board.
    #[component]
    pub fn symbols(version: u8) -> usize {
        let _ = version;
        let names: Vec<String> = match super::ast::Syntax.root() {
            Some(document) => document
                .statements()
                .iter()
                .filter_map(|statement| statement.name())
                .collect(),
            None => Vec::new(),
        };
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
        let messages: Vec<String> = match super::ast::Syntax.root() {
            Some(document) => {
                let statements = document.statements();
                let defined: Vec<String> = statements
                    .iter()
                    .filter_map(|statement| statement.name())
                    .collect();
                let all: Vec<String> = statements
                    .iter()
                    .flat_map(|statement| unresolved_statement(statement.clone(), defined.clone()))
                    .collect();
                if strict {
                    all
                } else {
                    all.iter()
                        .filter(|message| message.len() > 4)
                        .cloned()
                        .collect::<Vec<String>>()
                }
            }
            None => Vec::new(),
        };
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
    /// for the runtime to keep the component boundaries.
    #[component]
    pub fn resolve_program(syntax: &super::ast::Syntax, weights: &super::Weights) -> u64 {
        let _ = syntax;
        let _ = weights;
        let a = symbols(1);
        let b = diagnostics(true);
        let c = total(1);
        (a as u64) + (b as u64) + c
    }
}

/// The seed program: `x := 1`, `y := 2`, `print(x, y)`.
#[must_use]
pub fn program() -> ast::Document {
    ast::Document::Program {
        statements: vec![
            ast::Statement::Assign {
                name: String::from("x"),
                value: ast::Expression::Number(1),
            },
            ast::Statement::Assign {
                name: String::from("y"),
                value: ast::Expression::Number(2),
            },
            ast::Statement::Print {
                values: vec![
                    ast::Expression::Call {
                        callee: String::from("show"),
                        args: vec![ast::Expression::Name(String::from("x"))],
                    },
                    ast::Expression::Name(String::from("y")),
                ],
            },
        ],
    }
}
