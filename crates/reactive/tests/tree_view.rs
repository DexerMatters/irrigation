//! Tree View family and `#[tree_view]` gates.
//!
//! The shape is the plan's example: a mutually recursive enum group where
//! nodes are entities, tags and fields are separate observations, and edits
//! preserve identity.

use reactive::{reactive, tree_view, Runtime};

#[tree_view(root = Document, view = Syntax)]
pub mod syntax {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Document {
        Module { items: Vec<Expr> },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Expr {
        Literal(Literal),
        Name(String),
        Call { callee: Box<Expr>, args: Vec<Expr> },
        Let { name: String, value: Box<Expr>, body: Box<Expr> },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Literal {
        Int(i64),
        Text(String),
    }
}

#[reactive]
mod analysis {
    use super::syntax;

    #[component]
    pub fn count(expr: syntax::ExprRef) -> i64 {
        let kind = expr.kind();
        if kind == Some(syntax::ExprKind::Literal) {
            let literal = expr.literal();
            match literal {
                Some(literal) => literal.int().unwrap_or(0),
                None => 0,
            }
        } else if kind == Some(syntax::ExprKind::Name) {
            1
        } else if kind == Some(syntax::ExprKind::Call) {
            let callee = expr.callee();
            let args = expr.args();
            let head = match callee {
                Some(callee) => count(callee),
                None => 0,
            };
            let tail = args.iter().map(|arg| count(arg.clone())).sum::<i64>();
            head + tail
        } else {
            0
        }
    }

    #[component]
    pub fn total(syntax: &syntax::Syntax) -> i64 {
        let root = syntax.root();
        match root {
            Some(document) => {
                let items = document.items();
                items.iter().map(|item| count(item.clone())).sum::<i64>()
            }
            None => 0,
        }
    }
}

fn document() -> syntax::Document {
    syntax::Document::Module {
        items: vec![
            syntax::Expr::Call {
                callee: Box::new(syntax::Expr::Name(String::from("f"))),
                args: vec![syntax::Expr::Literal(syntax::Literal::Int(1))],
            },
            syntax::Expr::Literal(syntax::Literal::Int(2)),
        ],
    }
}

fn runtime() -> Runtime {
    let runtime = Runtime::new();
    runtime.install(analysis::Total).expect("install");
    runtime
}

#[test]
fn tree_reads_walk_kinds_and_fields() {
    let runtime = runtime();
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            tree.set_root(&document());
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<analysis::Total>().expect("outputs"), 4);

    let snapshot = runtime.read().expect("read");
    let view = snapshot.view::<syntax::Syntax>();
    let root = view.root().expect("root");
    assert_eq!(root.kind(), Some(syntax::DocumentKind::Module));
    let items = root.items();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].kind(), Some(syntax::ExprKind::Call));
    assert_eq!(items[1].kind(), Some(syntax::ExprKind::Literal));
    let callee = items[0].callee().expect("callee");
    assert_eq!(callee.name(), Some(String::from("f")));
    let args = items[0].args();
    assert_eq!(args.len(), 1);
    let literal = args[0].literal().expect("literal");
    assert_eq!(literal.int(), Some(1));
    // Each `Literal` expression is an `Expr` node plus a `Literal` node.
    assert_eq!(view.node_count(), 7);
}

#[test]
fn a_replaced_node_keeps_its_identity_and_repairs_consumers() {
    let runtime = runtime();
    let mut literal_id = 0;
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            let root = tree.set_root(&document());
            let items = root.items();
            let literal = items[1].clone();
            literal_id = literal.raw();
            tree.replace_expr(literal, &syntax::Expr::Literal(syntax::Literal::Int(20)));
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<analysis::Total>().expect("outputs"), 22);

    // The replacement preserved the node identity.
    let snapshot = runtime.read().expect("read");
    let root = snapshot.view::<syntax::Syntax>().root().expect("root");
    let items = root.items();
    assert_eq!(items[1].raw(), literal_id);
    assert_eq!(items[1].literal().and_then(|literal| literal.int()), Some(20));
}

#[test]
fn moving_a_subtree_preserves_identities() {
    let runtime = runtime();
    let mut moved_id = 0;
    let mut callee_id = 0;
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            let root = tree.set_root(&document());
            let items = root.items();
            let call = items[0].clone();
            let moved = items[1].clone();
            callee_id = call.callee().expect("callee").raw();
            moved_id = moved.raw();
            // Move the second item under the call's argument slot.
            assert!(tree.move_subtree(moved.raw(), call.raw(), 1, 0));
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<analysis::Total>().expect("outputs"), 4);

    let snapshot = runtime.read().expect("read");
    let root = snapshot.view::<syntax::Syntax>().root().expect("root");
    let items = root.items();
    // Only the call remains at the top level.
    assert_eq!(items.len(), 1);
    let args = items[0].args();
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].raw(), moved_id);
    assert_eq!(items[0].callee().expect("callee").raw(), callee_id);
    assert_eq!(args[0].literal().and_then(|literal| literal.int()), Some(2));
}

#[test]
fn removing_a_subtree_retracts_its_nodes() {
    let runtime = runtime();
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            let root = tree.set_root(&document());
            let items = root.items();
            tree.remove_expr(items[0].clone());
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<analysis::Total>().expect("outputs"), 2);
    let snapshot = runtime.read().expect("read");
    let view = snapshot.view::<syntax::Syntax>();
    assert_eq!(view.node_count(), 3);
    assert_eq!(view.root().expect("root").items().len(), 1);
}

#[test]
fn leaf_field_edits_do_not_invalidate_other_projections() {
    let runtime = runtime();
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            tree.set_root(&document());
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<analysis::Total>().expect("outputs"), 4);

    let before = runtime.memory_report();
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            let root = tree.root();
            let items = root.items();
            let literal = items[1].clone();
            tree.replace_expr(literal, &syntax::Expr::Literal(syntax::Literal::Int(3)));
        })
        .expect("edit");
    assert_eq!(runtime.read_outputs::<analysis::Total>().expect("outputs"), 5);
    // Replacing one leaf does not allocate a whole new tree.
    let after = runtime.memory_report();
    assert!(
        after.nodes <= before.nodes + 8,
        "unexpected node growth: {before:?} -> {after:?}"
    );
}

#[test]
fn replacing_a_variant_reuses_unchanged_children() {
    let runtime = runtime();
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            tree.set_root(&document());
        })
        .expect("edit");

    let (root_id, call_id, callee_id, arg_id, literal_id, second_id) = {
        let snapshot = runtime.snapshot();
        let view = snapshot.view::<syntax::Syntax>();
        let root = view.root().expect("root");
        let items = root.items();
        let call = items[0].clone();
        let callee = call.callee().expect("callee");
        let args = call.args();
        let literal = args[0].literal().expect("literal");
        (
            root.raw(),
            call.raw(),
            callee.raw(),
            args[0].raw(),
            literal.raw(),
            items[1].raw(),
        )
    };

    // Replacing the root with an identical document changes no identity.
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            let replaced = tree.replace_root(tree.root(), &document());
            assert_eq!(replaced.raw(), root_id);
        })
        .expect("edit");
    let after_identical = {
        let snapshot = runtime.snapshot();
        let view = snapshot.view::<syntax::Syntax>();
        let root = view.root().expect("root");
        let items = root.items();
        let call = items[0].clone();
        let callee = call.callee().expect("callee");
        let args = call.args();
        let literal = args[0].literal().expect("literal");
        (
            root.raw(),
            call.raw(),
            callee.raw(),
            args[0].raw(),
            literal.raw(),
            items[1].raw(),
        )
    };
    assert_eq!(
        after_identical,
        (root_id, call_id, callee_id, arg_id, literal_id, second_id),
        "an identical replacement must not renumber descendants"
    );

    // A variant change reuses the replaced node and removes only its old child;
    // unrelated siblings keep their identities.
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            let items = tree.root().items();
            tree.replace_expr(
                items[1].clone(),
                &syntax::Expr::Name(String::from("x")),
            );
        })
        .expect("edit");
    let snapshot = runtime.snapshot();
    let view = snapshot.view::<syntax::Syntax>();
    let root = view.root().expect("root");
    let items = root.items();
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].raw(), second_id, "the replaced node keeps its id");
    assert_eq!(items[1].name(), Some(String::from("x")));
    assert_eq!(items[0].raw(), call_id);
    assert_eq!(items[0].callee().expect("callee").raw(), callee_id);
    assert_eq!(view.node_count(), 6, "the old literal child was retracted");
}
