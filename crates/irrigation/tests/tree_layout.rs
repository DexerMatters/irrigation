//! The declared tree describes itself: names, slots, kinds, and cardinalities.
//!
//! The layout is generated from the same declaration the accessors are, so the
//! gate is that both address the same slot. It also proves the layout is enough
//! to walk a tree nobody wrote accessors for — the property a parser or a
//! lowering pass needs.

use std::collections::HashMap;
use std::fmt::Write as _;

use irrigation::{Cardinality, LeafKind, Runtime, TreeLayout, TreeSchema, TreeValue, tree_view};

#[tree_view(root = Document, view = Syntax)]
pub mod ast {
    /// One module.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Document {
        /// A module and its items.
        Module { items: Vec<Item> },
    }

    /// One item.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Item {
        /// A binding.
        Let {
            /// The bound name.
            name: String,
            /// The bound expression.
            value: Expr,
        },
        /// A bare expression.
        Expr(Expr),
    }

    /// One expression.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Expr {
        /// A literal.
        Int(i64),
        /// A reference.
        Var(String),
        /// A call.
        Call {
            /// The callee's name.
            callee: String,
            /// The arguments.
            args: Vec<Expr>,
        },
    }
}

/// The document the tests read.
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

fn runtime() -> Runtime {
    let runtime = Runtime::new();
    runtime
        .edit(|edit| edit.view::<ast::Syntax>().set_root(&document()))
        .expect("seed");
    runtime
}

/// The layout is a `'static` constant derived from the declaration alone.
const LAYOUT: &TreeLayout = <ast::Syntax as TreeSchema>::LAYOUT;

#[test]
fn the_layout_names_the_declared_types() {
    let names: Vec<&str> = LAYOUT.types.iter().map(|ty| ty.name).collect();
    assert_eq!(
        names,
        vec!["Document", "Item", "Expr"],
        "the root type is first, then declaration order"
    );
    assert_eq!(LAYOUT.root, 0);
    assert_eq!(LAYOUT.root_layout().map(|ty| ty.name), Some("Document"));
    assert_eq!(LAYOUT.type_index("Expr"), Some(2));
    assert_eq!(LAYOUT.type_index("Missing"), None);
}

#[test]
fn the_layout_names_the_declared_variants() {
    let (module, variant) = LAYOUT
        .variant(0, "Module")
        .expect("the root variant is declared");
    assert_eq!(module, 0);
    assert_eq!(variant.name, "Module");
    assert!(variant.leaves.is_empty());
    assert_eq!(variant.children.len(), 1);

    let (_, item_expr) = LAYOUT.variant(1, "Expr").expect("the tuple variant");
    assert_eq!(item_expr.name, "Expr");
    let child = item_expr.child("expr").expect("the tuple field's accessor");
    assert_eq!(child.cardinality, Cardinality::One);
    assert_eq!(child.slot, 0);
    assert_eq!(
        LAYOUT.type_layout(child.target).map(|ty| ty.name),
        Some("Expr"),
        "the child points at the declared type"
    );
}

#[test]
fn the_layout_reports_leaf_slots_and_kinds() {
    let (_, let_variant) = LAYOUT.variant(1, "Let").expect("the Let variant");
    let name = let_variant.leaf("name").expect("the name field");
    assert_eq!(name.slot, 0);
    assert_eq!(name.kind, LeafKind::Text);
    assert_eq!(let_variant.child("value").map(|c| c.slot), Some(0));

    let (_, int_variant) = LAYOUT.variant(2, "Int").expect("the Int variant");
    let int = int_variant.leaf("int").expect("the tuple field");
    assert_eq!(int.kind, LeafKind::Int);
    assert_eq!(int.slot, 0);

    let (_, call_variant) = LAYOUT.variant(2, "Call").expect("the Call variant");
    let callee = call_variant.leaf("callee").expect("the callee field");
    assert_eq!(callee.kind, LeafKind::Text);
    let args = call_variant.child("args").expect("the args field");
    assert_eq!(args.cardinality, Cardinality::Many);
    assert_eq!(
        LAYOUT.type_layout(args.target).map(|ty| ty.name),
        Some("Expr")
    );
}

#[test]
fn every_layout_slot_is_the_slot_the_accessor_reads() {
    let runtime = runtime();
    let snapshot = runtime.snapshot();
    let root = snapshot
        .view::<ast::Syntax>()
        .root()
        .expect("a published root");
    let (_, module) = LAYOUT.variant(0, "Module").expect("the root variant");
    let items_slot = module.child("items").expect("the items field").slot;
    assert_eq!(
        root.items().len(),
        root.tree().child_slot(items_slot).ids().len(),
        "the layout's child slot is the accessor's"
    );

    let items = root.items();
    let binding = &items[0];
    let (_, let_variant) = LAYOUT.variant(1, "Let").expect("the Let variant");
    let name_slot = let_variant.leaf("name").expect("the name field").slot;
    assert_eq!(
        binding.name().as_deref(),
        binding
            .tree()
            .field(name_slot)
            .and_then(|v| { v.as_text().map(str::to_owned) })
            .as_deref(),
        "a leaf's slot is the accessor's"
    );
    let value_slot = let_variant.child("value").expect("the value field").slot;
    assert_eq!(
        binding.value().map(|value| value.raw()),
        binding.tree().child_slot(value_slot).ids().first().copied(),
        "a child's slot is the accessor's"
    );

    let call = &items[1];
    let Some(call) = call.expr() else {
        panic!("the second item is a bare expression");
    };
    let (_, call_variant) = LAYOUT.variant(2, "Call").expect("the Call variant");
    let callee_slot = call_variant.leaf("callee").expect("the callee field").slot;
    assert_eq!(
        call.callee().as_deref(),
        call.tree()
            .field(callee_slot)
            .and_then(|v| v.as_text().map(str::to_owned))
            .as_deref()
    );
    let args_slot = call_variant.child("args").expect("the args field").slot;
    assert_eq!(
        call.args().len(),
        call.tree().child_slot(args_slot).ids().len()
    );
}

/// Renders a tree using only its layout and its raw refs.
fn render(layout: &TreeLayout, node: &irrigation::TreeRef, out: &mut String) {
    let Some((type_index, variant_index)) = node.tag() else {
        return;
    };
    let Some(ty) = layout.type_layout(type_index) else {
        return;
    };
    let Some(variant) = ty.variants.get(usize::from(variant_index)) else {
        return;
    };
    let _ = write!(out, "{}::{}(", ty.name, variant.name);
    for leaf in variant.leaves {
        let value = node.field(leaf.slot);
        let text = match value {
            Some(TreeValue::Text(value)) => value,
            Some(TreeValue::Int(value)) => value.to_string(),
            Some(TreeValue::UInt(value)) => value.to_string(),
            Some(TreeValue::Bool(value)) => value.to_string(),
            Some(TreeValue::Float(value)) => value.to_string(),
            Some(TreeValue::Unit) | None => String::new(),
        };
        let _ = write!(out, "{}={text};", leaf.name);
    }
    for child in variant.children {
        let _ = write!(out, "{}[", child.name);
        for id in node.child_slot(child.slot).ids() {
            render(layout, &node.child(id), out);
        }
        out.push(']');
    }
    out.push(')');
}

#[test]
fn the_layout_alone_walks_the_whole_tree() {
    let runtime = runtime();
    let snapshot = runtime.snapshot();
    let root = snapshot
        .view::<ast::Syntax>()
        .root()
        .expect("a published root");
    let mut rendered = String::new();
    render(LAYOUT, root.tree(), &mut rendered);
    assert_eq!(
        rendered,
        "Document::Module(items[Item::Let(name=x;value[Expr::Int(int=1;)])Item::Expr(\
expr[Expr::Call(callee=print;args[Expr::Var(var=x;)Expr::Var(var=y;)])])])",
        "a walker that knows only the layout sees the whole declaration"
    );
}

#[test]
fn the_layout_carries_no_runtime_state() {
    // The layout is a constant: a consumer can resolve names and slots without a
    // runtime, which is what makes it usable for a declaration-time check.
    let mut by_name: HashMap<&str, u16> = HashMap::new();
    for (index, ty) in LAYOUT.types.iter().enumerate() {
        by_name.insert(ty.name, u16::try_from(index).expect("a small index"));
    }
    assert_eq!(by_name.get("Document"), Some(&0));
    assert_eq!(by_name.len(), LAYOUT.types.len());
}
