//! The identity contract is a framework guarantee, not an example detail.
//!
//! A host writes its own components, and their entities have to survive an edit
//! just like the example's do. The guarantee has one shape:
//!
//! * a component invocation is identified by its arguments, so an argument must
//!   be an *occurrence* — a token, an AST node, an entity — never a spelling or
//!   a value that is merely data;
//! * a callable component then gets `emission_id` identities, stable exactly
//!   while those arguments are;
//! * a `GeneralComponent` producer owns the keys it writes, so it derives them
//!   from the source occurrence too.
//!
//! These gates run the three authoring styles over a real lexer/parser board and
//! pin the difference between an occurrence argument and a value argument.

use std::collections::BTreeSet;

use irrigation::{GeneralCx, Result, Runtime, ViewSchema, component, view};
use irrigation_syntax::{
    AstNodeRef, AstTree, LexerComponent, ParserComponent, SourceFile, Text, TextComponent,
    TextDocument, TokenId, Tokens,
};

/// A small JSON-like language: objects, names, and numbers.
fn grammar() -> irrigation_syntax::Grammar {
    irrigation_syntax::Grammar::compile(
        r#"
        grammar Probe {
            lexer {
                initial root;
                state root {
                    skip ws = /[ \t\r\n]+/;
                    token string = /"([^"\\]|\\.)*"/;
                    token number = /-?(0|[1-9][0-9]*)/;
                    token ident = /[A-Za-z_][A-Za-z0-9_]*/ @word;
                    token "{" ; token "}" ; token ":" ; token "," ;
                }
            }
            parser {
                Document = Value @ast(ProbeDocument { value: 0 });
                Value = Object | string | number | ident;
                Object = "{" (Pair ("," Pair)*)? "}" @ast(ProbeObject);
                Pair = key: string ":" value: Value @ast(ProbePair { key: 0, value: 2 });
            }
        }
        "#,
    )
    .expect("the probe grammar")
}

// ---------------------------------------------------------------- callable

/// One graph node per AST occurrence plus a keyed map of the same entities.
#[view(derived)]
pub struct Lowered {
    /// The lowered tree: one node per AST node, one edge per child.
    pub graph: irrigation::Graph<u64, u64>,
    /// The same entities keyed by the source occurrence.
    pub terms: irrigation::Map<u64, u64>,
}

/// Lowers one AST occurrence, recursively.
///
/// The argument is the occurrence; the emitted identities follow from it. Each
/// child is matched where it is read, so the match observes the tree unit that
/// produced it. (A local bound through an iterator adapter carries no node, so
/// matching on a hoisted `.cloned()` local would freeze at its first answer.)
#[component(callable)]
pub fn lower(node: AstNodeRef) -> u64 {
    let graph = Lowered.graph();
    let rule = node.rule_id().unwrap_or_default().raw();
    let id = graph.emit_node(rule);
    Lowered.define_terms(node.raw(), rule);

    let children = node.children();
    match children.first() {
        Some(first) => {
            let child = lower(first.clone());
            Lowered.graph().emit_edge(id, child, 0);
        }
        None => {}
    }
    match children.get(1) {
        Some(second) => {
            let child = lower(second.clone());
            Lowered.graph().emit_edge(id, child, 1);
        }
        None => {}
    }
    match children.get(2) {
        Some(third) => {
            let child = lower(third.clone());
            Lowered.graph().emit_edge(id, child, 2);
        }
        None => {}
    }
    match children.get(3) {
        Some(fourth) => {
            let child = lower(fourth.clone());
            Lowered.graph().emit_edge(id, child, 3);
        }
        None => {}
    }
    match children.get(4) {
        Some(fifth) => {
            let child = lower(fifth.clone());
            Lowered.graph().emit_edge(id, child, 4);
        }
        None => {}
    }
    id
}

// ----------------------------------------------------------------- producer

/// A producer's own view: one node per token, keyed by the token occurrence.
#[view(derived)]
pub struct TokenGraph {
    /// Token occurrence -> lexeme length, plus a `next` edge between tokens.
    pub graph: irrigation::Graph<u64, u64>,
}

/// A `GeneralComponent` that derives a view from the token stream.
///
/// It reads published units and writes its output keys itself; deriving them
/// from the token ids is what makes an edit a payload change.
#[derive(Debug)]
pub struct TokenGraphProducer;

#[irrigation::general_component(writes(TokenGraph), reads(Tokens))]
impl TokenGraphProducer {
    fn run(cx: &mut GeneralCx) -> Result<()> {
        let order = Tokens::ORDER.ids_in(cx)?;
        let mut nodes = Vec::with_capacity(order.len());
        for id in order.iter() {
            let payload = Tokens::PAYLOADS.get_in(cx, &TokenId(*id))?;
            let length = payload.map_or(0, |payload| payload.text.len() as u64);
            nodes.push((*id, length));
        }
        let mut edges = Vec::with_capacity(nodes.len().saturating_sub(1));
        for pair in nodes.windows(2) {
            let id = pair[0].0.wrapping_mul(1_000_003) ^ pair[1].0;
            edges.push((id, pair[0].0, pair[1].0, 0));
        }
        cx.write(TokenGraph::GRAPH).define_graph(&nodes, &edges);
        Ok(())
    }
}

// ---------------------------------------------------------- negative control

/// Emitted by a component whose argument is a *spelling*, not an occurrence.
#[view(derived)]
pub struct Spelled {
    /// One node per invocation; the payload is the spelling's length.
    pub graph: irrigation::Graph<u64, u64>,
}

#[component(callable)]
pub fn spell(term: AstNodeRef, spelling: String) -> u64 {
    let _ = term;
    Spelled.graph().emit_node(spelling.len() as u64)
}

/// The first leaf under `node`, in source order.
///
/// Descends through rule children first and falls back to the first token, so
/// punctuation before the first real child does not become "the leaf".
fn first_leaf(node: &AstNodeRef) -> AstNodeRef {
    let mut first_rule = None;
    let mut first_token = None;
    for child in node.children() {
        if child.token_id().is_some() {
            if first_token.is_none() {
                first_token = Some(child);
            }
        } else if first_rule.is_none() {
            first_rule = Some(child);
        }
    }
    match first_rule {
        Some(child) => first_leaf(&child),
        None => first_token.unwrap_or_else(|| node.clone()),
    }
}

/// The text of the leaf's token, read as one payload unit.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "the View API hands these zero-sized handles out by reference"
)]
///
/// A plain function, so the read stays inside the caller's computation without
/// the argument hoisting a component body applies to its own expressions.
fn leaf_text(tokens: &Tokens, leaf: &AstNodeRef) -> String {
    match leaf.token_id() {
        Some(token) => match tokens.payloads().get(&TokenId(token)) {
            Some(payload) => payload.text.to_string(),
            None => String::new(),
        },
        None => String::new(),
    }
}

/// The board's root: it demands every style's output so each one runs.
///
/// One root per runtime is the framework's rule, so the styles compose here
/// rather than as three separate roots.
#[component(root)]
pub fn probe(tree: &AstTree, tokens: &Tokens, graph: &TokenGraph) -> u64 {
    let lowered = match tree.root() {
        Some(root) => lower(root),
        None => 0,
    };
    let produced = graph.graph().node_count();
    // The negative control: the invocation below takes the leaf's spelling as
    // an argument, so it is content-addressed and a rename is a new entity.
    let spelled = match tree.root() {
        Some(root) => {
            let leaf = first_leaf(&root);
            let text = leaf_text(tokens, &leaf);
            spell(leaf, text)
        }
        None => 0,
    };
    lowered + produced as u64 + spelled
}

// -------------------------------------------------------------------- board

fn board(text: &str) -> Runtime {
    let runtime = Runtime::new();
    runtime.register_view(<Text as ViewSchema>::DESCRIPTOR);
    runtime.register_view(<Tokens as ViewSchema>::DESCRIPTOR);
    runtime.register_view(<Lowered as ViewSchema>::DESCRIPTOR);
    runtime.register_view(<TokenGraph as ViewSchema>::DESCRIPTOR);
    runtime.register_view(<Spelled as ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_producer(TextComponent)
        .expect("text component");
    runtime
        .install_general_producer(LexerComponent::new(grammar().lexer().clone()))
        .expect("lexer component");
    runtime
        .install_general_producer(ParserComponent::new(grammar()))
        .expect("parser component");
    runtime
        .install_general_producer(TokenGraphProducer)
        .expect("producer");
    runtime.install(Probe).expect("root");
    runtime
        .edit(|edit| {
            edit.view::<SourceFile>()
                .set_document(TextDocument::new(text))
        })
        .expect("seed");
    demand(&runtime);
    runtime
}

/// Demands the root, so every style's output is materialized.
fn demand(runtime: &Runtime) {
    let _ = runtime.read_outputs::<Probe>();
}

fn edit(runtime: &Runtime, text: &str) {
    let current = {
        let snapshot = runtime.read().expect("snapshot");
        snapshot.view::<SourceFile>().document()
    };
    let next = current.replace_with(text).unwrap_or(current);
    runtime
        .edit(|edit| edit.view::<SourceFile>().set_document(next))
        .expect("edit");
    demand(runtime);
}

fn lowered_nodes(runtime: &Runtime) -> Vec<u64> {
    let snapshot = runtime.read().expect("snapshot");
    snapshot.view::<Lowered>().graph_nodes()
}

fn lowered_edges(runtime: &Runtime) -> Vec<u64> {
    let snapshot = runtime.read().expect("snapshot");
    snapshot.view::<Lowered>().graph_edges()
}

fn lowered_terms(runtime: &Runtime) -> Vec<(u64, u64)> {
    let snapshot = runtime.read().expect("snapshot");
    snapshot.view::<Lowered>().terms()
}

fn lowered_payloads(runtime: &Runtime) -> Vec<(u64, Option<u64>)> {
    let snapshot = runtime.read().expect("snapshot");
    let view = snapshot.view::<Lowered>();
    view.graph_nodes()
        .into_iter()
        .map(|id| (id, view.graph_node(id)))
        .collect()
}

fn token_graph(runtime: &Runtime) -> Vec<(u64, Option<u64>)> {
    let snapshot = runtime.read().expect("snapshot");
    let view = snapshot.view::<TokenGraph>();
    view.graph_nodes()
        .into_iter()
        .map(|id| (id, view.graph_node(id)))
        .collect()
}

fn spelled_nodes(runtime: &Runtime) -> BTreeSet<u64> {
    let snapshot = runtime.read().expect("snapshot");
    snapshot
        .view::<Spelled>()
        .graph_nodes()
        .into_iter()
        .collect()
}

#[test]
fn a_callable_component_keeps_its_entities_across_a_respelling() {
    let runtime = board(r#"{"a":1}"#);
    let nodes = lowered_nodes(&runtime);
    let edges = lowered_edges(&runtime);
    let terms = lowered_terms(&runtime);
    let payloads = lowered_payloads(&runtime);

    edit(&runtime, r#"{"a":9}"#);

    assert_eq!(
        lowered_nodes(&runtime),
        nodes,
        "a re-spelling must not re-identify the lowered tree"
    );
    assert_eq!(lowered_edges(&runtime), edges, "nor its edges");
    assert_eq!(lowered_terms(&runtime), terms, "nor the keyed map");
    assert_eq!(
        lowered_payloads(&runtime),
        payloads,
        "and the payloads are unchanged too"
    );
}

#[test]
fn a_callable_component_retains_the_surviving_entities_on_a_structural_edit() {
    let runtime = board(r#"{"a":1}"#);
    let before = lowered_nodes(&runtime);
    let keys: Vec<u64> = lowered_terms(&runtime)
        .into_iter()
        .map(|(key, _)| key)
        .collect();

    edit(&runtime, r#"{"a":1,"b":2}"#);

    let after = lowered_nodes(&runtime);
    let retained: Vec<u64> = before
        .iter()
        .copied()
        .filter(|id| after.contains(id))
        .collect();
    assert!(
        retained.len() >= before.len() - 2,
        "an insertion before the closing brace keeps the existing nodes: {before:?} -> {after:?}"
    );
    assert!(after.len() > before.len(), "the new member is lowered");
    let after_keys: Vec<u64> = lowered_terms(&runtime)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    assert!(keys.iter().all(|key| after_keys.contains(key)));
}

#[test]
fn a_general_producer_keys_its_output_by_the_source_occurrence() {
    let runtime = board(r#"{"a":1}"#);
    let nodes = token_graph(&runtime);
    let ids: Vec<u64> = nodes.iter().map(|(id, _)| *id).collect();
    assert!(!ids.is_empty(), "the producer published its view");

    edit(&runtime, r#"{"a":99}"#);

    let after = token_graph(&runtime);
    assert_eq!(
        after.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        ids,
        "a producer that keys by occurrence keeps its identities"
    );
    let changed: Vec<u64> = after
        .iter()
        .filter(|(id, payload)| {
            nodes
                .iter()
                .any(|(old_id, old)| old_id == id && old != payload)
        })
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        changed.len(),
        1,
        "one token's payload moved, its identity did not"
    );
}

#[test]
fn a_value_argument_is_content_addressed() {
    let runtime = board(r#"{"a":1}"#);
    let before = spelled_nodes(&runtime);
    assert_eq!(before.len(), 1);

    // The first leaf is the value; re-spelling it changes the argument.
    edit(&runtime, r#"{"a":9}"#);

    let after = spelled_nodes(&runtime);
    assert_eq!(after.len(), 1);
    assert_ne!(
        before, after,
        "a component that takes a spelling as its argument is a new invocation"
    );
}
