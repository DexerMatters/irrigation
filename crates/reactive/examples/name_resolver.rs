//! A minimal name resolver.
//!
//! The abstract syntax tree is a tree View: nodes are entities, and kinds and
//! fields are separate observations. `name_resolver` is one ordinary component.
//! It collects `let` definitions into a symbol list and checks every `Var`
//! reference against them, publishing unresolved names as diagnostics.
//!
//! Run with `cargo run --example name_resolver -p reactive`.

use reactive::{Runtime, reactive, tree_view, view};

#[tree_view(root = Document, view = Syntax)]
pub mod ast {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Document {
        Module { items: Vec<Item> },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Item {
        Let { name: String, value: Expr },
        Expr(Expr),
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Expr {
        Var(String),
        Int(i64),
        Call { callee: String, args: Vec<Expr> },
    }
}

/// One resolved binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
}

#[view(derived)]
pub struct Symbols(List<Symbol>);

#[view(derived)]
pub struct Diagnostics(List<String>);

#[reactive]
mod resolver {
    use super::ast;
    use super::{Diagnostics, Symbol, Symbols};

    /// Reports every `Var` in `expr` that `defined` does not bind.
    #[component]
    pub fn unresolved(expr: ast::ExprRef, defined: Vec<String>) -> Vec<String> {
        let kind = expr.kind();
        if kind == Some(ast::ExprKind::Var) {
            let name = expr.var().unwrap_or_default();
            if defined.contains(&name) {
                Vec::new()
            } else {
                vec![name]
            }
        } else if kind == Some(ast::ExprKind::Call) {
            let args = expr.args();
            args.iter()
                .flat_map(|arg| unresolved(arg.clone(), defined.clone()))
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Reports the unbound references of one item.
    #[component]
    pub fn unresolved_item(item: ast::ItemRef, defined: Vec<String>) -> Vec<String> {
        match item.value() {
            Some(value) => unresolved(value, defined),
            None => match item.expr() {
                Some(expr) => unresolved(expr, defined),
                None => Vec::new(),
            },
        }
    }

    /// Publishes the module's symbols and their unresolved references.
    #[component]
    pub fn name_resolver(syntax: &ast::Syntax) -> (Symbols, Diagnostics) {
        match syntax.root() {
            Some(document) => {
                let items = document.items();
                let defined: Vec<String> = items.iter().filter_map(|item| item.name()).collect();
                let symbols: Vec<Symbol> = defined
                    .iter()
                    .map(|name| Symbol { name: name.clone() })
                    .collect();
                let unresolved: Vec<String> = items
                    .iter()
                    .flat_map(|item| unresolved_item(item.clone(), defined.clone()))
                    .collect();
                let diagnostics: Vec<String> = unresolved
                    .iter()
                    .map(|name| format!("unresolved name `{name}`"))
                    .collect();
                (Symbols::from(symbols), Diagnostics::from(diagnostics))
            }
            None => (Symbols::from(Vec::new()), Diagnostics::from(Vec::new())),
        }
    }
}

fn document() -> ast::Document {
    ast::Document::Module {
        items: vec![
            ast::Item::Let {
                name: String::from("x"),
                value: ast::Expr::Int(1),
            },
            ast::Item::Expr(ast::Expr::Call {
                callee: String::from("print"),
                args: vec![
                    ast::Expr::Var(String::from("x")),
                    ast::Expr::Var(String::from("y")),
                ],
            }),
        ],
    }
}

fn report(runtime: &Runtime) -> (Vec<String>, Vec<String>) {
    let _ = runtime
        .read_outputs::<resolver::NameResolver>()
        .expect("resolve");
    let snapshot = runtime.read().expect("snapshot");
    let symbols: Vec<String> = snapshot
        .view::<Symbols>()
        .field0()
        .into_iter()
        .map(|symbol| symbol.name)
        .collect();
    let diagnostics = snapshot.view::<Diagnostics>().field0();
    println!("  symbols:     {symbols:?}");
    println!("  diagnostics: {diagnostics:?}");
    (symbols, diagnostics)
}

fn main() {
    let runtime = Runtime::new();
    runtime.install(resolver::NameResolver).expect("install");
    runtime
        .edit(|edit| {
            edit.view::<ast::Syntax>().set_root(&document());
        })
        .expect("seed");
    println!("before:");
    let (symbols, diagnostics) = report(&runtime);
    assert_eq!(symbols, vec![String::from("x")]);
    assert_eq!(diagnostics, vec![String::from("unresolved name `y`")]);

    // Rename the binding `x` to `y` in place. The tree keeps every other
    // node's identity, so only the resolver work that reads those names
    // re-executes.
    let before = runtime.stats().executions;
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<ast::Syntax>();
            let root = tree.root();
            let items = root.items();
            let replacement = ast::Item::Let {
                name: String::from("y"),
                value: ast::Expr::Int(2),
            };
            tree.replace_item(items[0].clone(), &replacement);
        })
        .expect("edit");
    println!("\nafter renaming the first binding:");
    let (symbols, diagnostics) = report(&runtime);
    assert_eq!(symbols, vec![String::from("y")]);
    assert_eq!(diagnostics, vec![String::from("unresolved name `x`")]);
    println!(
        "  node executions for the edit and the repair: {}",
        runtime.stats().executions - before
    );
}
