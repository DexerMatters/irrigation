//! Tree View family and `#[tree_view]` gates.
//!
//! The shape is the plan's example: a mutually recursive enum group where
//! nodes are entities, tags and fields are separate observations, and edits
//! preserve identity.

use irrigation::{Runtime, irrigation, tree_view};

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
        Call {
            callee: Box<Expr>,
            args: Vec<Expr>,
        },
        Let {
            name: String,
            value: Box<Expr>,
            body: Box<Expr>,
        },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Literal {
        Int(i64),
        Text(String),
    }
}

#[irrigation]
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

fn count_executions(runtime: &Runtime) -> Vec<(irrigation::NodeId, u64)> {
    let ids: Vec<irrigation::NodeId> = runtime
        .dump()
        .into_iter()
        .filter(|node| node.kind == "InvocationBody" && node.label.ends_with("analysis::count"))
        .map(|node| node.id)
        .collect();
    ids.into_iter()
        .map(|id| (id, runtime.explain_node(id).executed_at))
        .collect()
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
    assert_eq!(
        runtime.read_outputs::<analysis::Total>().expect("outputs"),
        4
    );

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
    assert_eq!(
        runtime.read_outputs::<analysis::Total>().expect("outputs"),
        22
    );

    // The replacement preserved the node identity.
    let snapshot = runtime.read().expect("read");
    let root = snapshot.view::<syntax::Syntax>().root().expect("root");
    let items = root.items();
    assert_eq!(items[1].raw(), literal_id);
    assert_eq!(
        items[1].literal().and_then(|literal| literal.int()),
        Some(20)
    );
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
    assert_eq!(
        runtime.read_outputs::<analysis::Total>().expect("outputs"),
        4
    );

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
    assert_eq!(
        runtime.read_outputs::<analysis::Total>().expect("outputs"),
        2
    );
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
    assert_eq!(
        runtime.read_outputs::<analysis::Total>().expect("outputs"),
        4
    );

    let before = count_executions(&runtime);
    assert_eq!(
        before.len(),
        4,
        "the fixture has one `count` invocation per expression"
    );
    let memory_before = runtime.memory_report();

    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            let root = tree.root();
            let items = root.items();
            let literal = items[1].clone();
            tree.replace_expr(literal, &syntax::Expr::Literal(syntax::Literal::Int(3)));
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<analysis::Total>().expect("outputs"),
        5
    );

    // `executed_at` is the revision the invocation last ran its body in; a
    // validated-but-reused invocation keeps the older revision. Only the edited
    // literal's `count` may have run at the new revision: a whole-tree walk
    // re-runs all four.
    let after = count_executions(&runtime);
    assert_eq!(
        after.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        before.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        "the leaf replacement must not renumber the `count` invocations"
    );
    let revision = runtime.revision();
    let reexecuted: Vec<irrigation::NodeId> = after
        .iter()
        .filter(|(_, executed_at)| *executed_at == revision)
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        reexecuted.len(),
        1,
        "a leaf replacement may re-run only the edited literal's `count`; a \
         whole-tree walk would re-run all four: {after:?}"
    );
    let changed: Vec<irrigation::NodeId> = runtime
        .dump()
        .into_iter()
        .filter(|node| {
            node.kind == "InvocationBody"
                && node.label.ends_with("analysis::count")
                && node.changed_at == revision
        })
        .map(|node| node.id)
        .collect();
    assert_eq!(
        reexecuted, changed,
        "the re-executed `count` must be the invocation whose value changed"
    );

    // Replacing one leaf does not allocate a whole new tree.
    let after_memory = runtime.memory_report();
    assert!(
        after_memory.nodes <= memory_before.nodes + 8,
        "unexpected node growth: {memory_before:?} -> {after_memory:?}"
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
            tree.replace_expr(items[1].clone(), &syntax::Expr::Name(String::from("x")));
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

#[test]
fn checked_tree_operations_report_preconditions() {
    let runtime = runtime();
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            let root = tree.set_root(&document());
            let items = root.items();
            let call = items[0].clone();
            let moved = items[1].clone();
            assert!(
                tree.move_subtree_checked(moved.raw(), call.raw(), 1, 0)
                    .is_ok()
            );
            assert!(
                tree.move_subtree_checked(999_999, call.raw(), 1, 0)
                    .is_err(),
                "a missing node is a precondition failure"
            );
            assert!(
                tree.move_subtree_checked(call.raw(), moved.raw(), 1, 0)
                    .is_err(),
                "a descendant destination is rejected"
            );
        })
        .expect("edit");
}

// ---------------------------------------------------------------------------
// Patched trees and total field reads
// ---------------------------------------------------------------------------

/// Reads a slot no declared variant has.
fn absent_slot(node: &syntax::DocumentRef) -> Option<irrigation::TreeValue> {
    node.tree().field(9)
}

/// Reads the first leaf slot of one expression.
fn first_slot(node: &syntax::ExprRef) -> Option<irrigation::TreeValue> {
    node.tree().field(0)
}

/// One leaf read from a node the variant may not have.
#[irrigation]
mod probes {
    use super::syntax;

    #[component]
    pub fn missing_field(syntax: &syntax::Syntax) -> Option<irrigation::TreeValue> {
        match syntax.root() {
            Some(root) => super::absent_slot(&root),
            None => None,
        }
    }

    #[component]
    pub fn first_field(syntax: &syntax::Syntax) -> Option<irrigation::TreeValue> {
        match syntax.root() {
            Some(root) => {
                let items = root.items();
                match items.first() {
                    Some(item) => super::first_slot(item),
                    None => None,
                }
            }
            None => None,
        }
    }
}

fn named_document() -> syntax::Document {
    syntax::Document::Module {
        items: vec![syntax::Expr::Name(String::from("f"))],
    }
}

#[test]
fn reading_a_slot_the_node_lacks_is_none() {
    let runtime = Runtime::new();
    runtime.install(probes::MissingField).expect("install");
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            tree.set_root(&document());
        })
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<probes::MissingField>()
            .expect("read"),
        None,
        "a live read of an absent slot answers like the snapshot, not with a panic"
    );
}

#[test]
fn a_shrunk_variant_does_not_fabricate_a_field() {
    let runtime = Runtime::new();
    runtime.install(probes::FirstField).expect("install");
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            tree.set_root(&named_document());
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<probes::FirstField>().expect("read"),
        Some(irrigation::TreeValue::Text(String::from("f")))
    );

    // `Name` has one leaf field; `Call` has none. The replace preserves the
    // identity, so the dropped slot must read as absent afterwards.
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            let name = tree.root().items()[0].clone();
            tree.replace_expr(
                name,
                &syntax::Expr::Call {
                    callee: Box::new(syntax::Expr::Name(String::from("g"))),
                    args: Vec::new(),
                },
            );
        })
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<probes::FirstField>().expect("read"),
        None,
        "a slot the new variant does not have is absent, not a unit payload"
    );
}

// ---------------------------------------------------------------------------
// A tree patch is read through its root
// ---------------------------------------------------------------------------

#[tree_view(root = Doc, view = Published, kind = derived)]
pub mod published {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Doc {
        Module { items: Vec<Item> },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Item {
        Num(u64),
        Name(String),
    }
}

/// Trigger for the next patch the producer publishes.
#[irrigation::view(source)]
pub struct PatchStep {
    step: u64,
}

const PUBLISHED_ROOT: u64 = 0;
const PUBLISHED_CHILD: u64 = 1;

fn patch_root(children: Vec<irrigation::ChildSlot>) -> irrigation::TreePatchNode {
    irrigation::TreePatchNode {
        id: PUBLISHED_ROOT,
        parent: None,
        type_index: 0,
        variant: 0,
        fields: Vec::new(),
        children,
    }
}

/// Publishes a scripted sequence of tree patches.
struct PatchProducer;

impl irrigation::GeneralComponent for PatchProducer {
    type State = irrigation::TreeStorage;
    type Output = ();

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00C0_0031);
    const NAME: &'static str = "tests::PatchProducer";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<PatchStep as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<published::Published as irrigation::ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> irrigation::DependencyUpdates {
        irrigation::DependencyUpdates::Incremental
    }

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let _: u64 = cx.read_typed(step_unit())?;
        let patch = irrigation::TreePatch {
            root: Some(PUBLISHED_ROOT),
            nodes: vec![
                patch_root(vec![irrigation::ChildSlot::Many(vec![PUBLISHED_CHILD])]),
                irrigation::TreePatchNode {
                    id: PUBLISHED_CHILD,
                    parent: Some(PUBLISHED_ROOT),
                    type_index: 1,
                    variant: 1,
                    fields: vec![irrigation::TreeValue::Text(String::from("a"))],
                    children: Vec::new(),
                },
            ],
            removed: Vec::new(),
        };
        let storage = irrigation::TreeOutputExt::tree_output(
            cx,
            <published::Published as irrigation::ViewSchema>::view_id(),
        )
        .apply_patch(&irrigation::TreeStorage::default(), &patch);
        Ok((storage, ()))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let step: u64 = cx.read_typed(step_unit())?;
        let patch = match step {
            // The root loses its only child slot: the stored slot must be
            // cleared, or the accessor keeps returning the retracted child.
            1 => irrigation::TreePatch {
                root: Some(PUBLISHED_ROOT),
                nodes: vec![patch_root(Vec::new())],
                removed: Vec::new(),
            },
            // The child is named as removed while the new children set still
            // names it: the reachable identity must survive.
            _ => irrigation::TreePatch {
                root: Some(PUBLISHED_ROOT),
                nodes: vec![patch_root(vec![irrigation::ChildSlot::Many(vec![
                    PUBLISHED_CHILD,
                ])])],
                removed: vec![PUBLISHED_CHILD],
            },
        };
        let storage = irrigation::TreeOutputExt::tree_output(
            cx,
            <published::Published as irrigation::ViewSchema>::view_id(),
        )
        .apply_patch(state, &patch);
        Ok((storage, ()))
    }
}

fn step_unit() -> irrigation::UnitKey {
    irrigation::UnitKey::field(
        <PatchStep as irrigation::ViewSchema>::DESCRIPTOR.id,
        irrigation::FieldId(0),
        irrigation::UnitKind::Value,
    )
}

/// Demands the published tree through its ROOT and children units.
struct PatchDemand;

impl irrigation::GeneralComponent for PatchDemand {
    type State = ();
    type Output = (Option<u64>, usize);

    const ID: irrigation::ComponentId = irrigation::ComponentId(0x00C0_0032);
    const NAME: &'static str = "tests::PatchDemand";
    const VIEWS: &'static [&'static irrigation::ViewDescriptor] =
        &[<published::Published as irrigation::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static irrigation::ViewDescriptor] = &[];

    fn dependency_updates(&self) -> irrigation::DependencyUpdates {
        irrigation::DependencyUpdates::Reset
    }

    fn initialize(
        &self,
        cx: &mut irrigation::GeneralCx,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let view = <published::Published as irrigation::ViewSchema>::DESCRIPTOR.id;
        let root: Option<u64> = cx.read_typed(irrigation::UnitKey::field(
            view,
            irrigation::FieldId(0),
            irrigation::UnitKind::Value,
        ))?;
        let children = match root {
            Some(root) => {
                let slot: irrigation::ChildSlot = cx.read_typed(irrigation::UnitKey::entity(
                    view,
                    irrigation::FieldId(irrigation::tree_slots::CHILDREN),
                    irrigation::UnitKind::Structure,
                    irrigation::any_key((root, 0u16)),
                ))?;
                slot.ids().len()
            }
            None => 0,
        };
        Ok(((), (root, children)))
    }

    fn update(
        &self,
        cx: &mut irrigation::GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

/// The shape a reader sees walking the published tree's root.
fn published_shape(runtime: &Runtime) -> (usize, bool) {
    let snapshot = runtime.snapshot();
    let tree = snapshot.view::<published::Published>();
    match tree.root() {
        Some(root) => {
            let items = root.items();
            let named = match items.first() {
                Some(item) => item.kind() == Some(published::ItemKind::Name),
                None => false,
            };
            (items.len(), named)
        }
        None => (0, false),
    }
}

#[test]
fn a_tree_patch_decides_what_the_root_reaches() {
    let runtime = Runtime::new();
    runtime.register_view(<published::Published as irrigation::ViewSchema>::DESCRIPTOR);
    runtime.register_view(<PatchStep as irrigation::ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_component(PatchProducer)
        .expect("producer");
    runtime.install_general(PatchDemand).expect("demand");
    assert_eq!(
        runtime
            .read_outputs_general::<PatchDemand>()
            .expect("initial"),
        (Some(PUBLISHED_ROOT), 1)
    );
    assert_eq!(published_shape(&runtime), (1, true));

    // The root loses its only child slot. The content drops it, and the live
    // slot unit must be cleared with it or a reader keeps seeing the child.
    runtime
        .edit(|edit| edit.view::<PatchStep>().set_step(1))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<PatchDemand>()
            .expect("after the slot shrink"),
        (Some(PUBLISHED_ROOT), 0),
        "a restated node with no child slots must not keep the old ones"
    );
    assert_eq!(published_shape(&runtime), (0, false));

    // The child is named as removed while the new children set still names it:
    // deleting it would leave the child slot pointing at no node.
    runtime
        .edit(|edit| edit.view::<PatchStep>().set_step(2))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<PatchDemand>()
            .expect("after the removal"),
        (Some(PUBLISHED_ROOT), 1)
    );
    assert_eq!(
        published_shape(&runtime),
        (1, true),
        "a node the new children set still names is preserved"
    );
}
