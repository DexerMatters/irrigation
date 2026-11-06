//! The board over a real compiler pipeline.
//!
//! Run with
//! `cargo run --example observatory_syntax -p reactive --features observatory -- --bind 127.0.0.1:7790`.
//!
//! Every stage is a `GeneralComponent` from `reactive-syntax`: the text
//! document, the lexer, the parser. The root reads what the pipeline publishes,
//! so all of it is demanded together. The page draws them exactly like the root
//! — a component with the Views it reads and the Views it defines — because that
//! is the only rule the projection knows. Nothing in this file names a parser or
//! a lexer in the frontend sense; it only installs components and registers
//! renderers.
//!
//! Each renderer cites a unit per row where the View has per-entity identity, so
//! an edit lights the rows the pipeline actually rewrote and leaves the rest
//! alone.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use reactive::{FieldId, UnitKind, ViewSchema, tree_units};
use reactive_observe::{
    FieldRender, ListRow, MapRow, Observatory, ObserveLevel, RenderModel, TreeRow, UnitRef,
    ValuePreview, observed_runtime,
};
use reactive_syntax::{
    AstMeta, AstMetaField, AstNodeRef, AstTree, LexerComponent, ParserComponent, PipelineStats,
    SourceFile, StatId, Text, TextComponent, TextDocument, Tokens,
};

/// A real JSON document: the pipeline's input.
fn source() -> &'static str {
    r#"{
  "name": "observatory",
  "views": 6,
  "live": true,
  "tags": ["runtime", "canvas"]
}"#
}

fn grammar() -> reactive_syntax::Grammar {
    reactive_syntax::Grammar::compile(
        r#"
        grammar JsonBoard {
            lexer {
                initial root;
                state root {
                    skip ws = /[ \t\r\n]+/;
                    token string = /"([^"\\]|\\.)*"/;
                    token number = /-?(0|[1-9][0-9]*)/;
                    token ident = /[A-Za-z_][A-Za-z0-9_]*/ @word;
                    token "{" ; token "}" ; token "[" ; token "]" ; token ":" ; token "," ;
                }
            }
            parser {
                Document = Value @ast(JsonDocument { value: 0 });
                Value = Object | Array | string | number | ident;
                Object = "{" (Pair ("," Pair)*)? "}" @ast(JsonObject);
                Pair = key: string ":" value: Value @ast(JsonPair { key: 0, value: 2 });
                Array = "[" (Value ("," Value)*)? "]" @ast(JsonArray);
            }
        }
        "#,
    )
    .expect("grammar")
}

/// The board's root: it reads what the pipeline publishes, so the whole chain is
/// demanded together and every stage reports a revision.
#[reactive::reactive]
mod app {
    use reactive_syntax::{AstTree, Tokens};

    #[component]
    pub fn board(tokens: &Tokens, ast: &AstTree) -> (usize, bool) {
        (tokens.order().len(), ast.root().is_some())
    }
}

// ------------------------------------------------------------------ renderers

fn lines(text: &str) -> RenderModel {
    RenderModel::List(
        text.lines()
            .enumerate()
            .map(|(index, line)| ListRow {
                index,
                value: line.to_owned(),
                // A line has no identity inside the View: the document is one
                // value, so these rows never claim a change of their own.
                unit: None,
            })
            .collect(),
    )
}

fn source_rows(snapshot: &reactive::Snapshot) -> RenderModel {
    lines(snapshot.view::<SourceFile>().document().text())
}

fn text(snapshot: &reactive::Snapshot) -> RenderModel {
    lines(snapshot.view::<Text>().document().text())
}

/// The token stream, drawn from its payload field: one entry per token and one
/// unit per entry, so a retyped token lights only its own row.
fn tokens(snapshot: &reactive::Snapshot) -> Vec<FieldRender> {
    let view = snapshot.view::<Tokens>();
    let mut rows: Vec<MapRow> = view
        .payloads()
        .into_iter()
        .map(|(id, payload)| MapRow {
            key: format!("{:#x}", id.0),
            value: Some(format!("{} {}", payload.kind.0, payload.text)),
            unit: Some(UnitRef::entity(
                Tokens::view_id(),
                FieldId(5),
                UnitKind::Value,
                id,
            )),
        })
        .collect();
    rows.sort_by(|left, right| left.key.cmp(&right.key));
    let total = rows.len();
    vec![FieldRender {
        field: FieldId(5),
        model: RenderModel::Map(rows),
        total,
        truncated: false,
    }]
}

/// The AST as an outline. A rule node cites its tag unit, so a structural edit
/// marks the nodes it rebuilt; a token leaf cites its own payload slot.
fn ast(snapshot: &reactive::Snapshot) -> RenderModel {
    let view = snapshot.view::<AstTree>();
    let names: HashMap<u64, Arc<str>> = snapshot.view::<AstMeta>().rules().into_iter().collect();
    let mut rows = Vec::new();
    let Some(root) = view.root() else {
        return RenderModel::Tree(rows);
    };
    push_ast(&mut rows, 0, &root, &names);
    RenderModel::Tree(rows)
}

fn push_ast(
    rows: &mut Vec<TreeRow>,
    depth: u16,
    node: &AstNodeRef,
    rules: &HashMap<u64, Arc<str>>,
) {
    let (kind, label, unit) = if let Some(token) = node.token_id() {
        (
            String::from("token"),
            format!("token {token:#x}"),
            tree_units::field(AstTree::view_id(), node.raw(), 0),
        )
    } else if let Some(rule) = node.rule() {
        (
            String::from("rule"),
            rules
                .get(&rule)
                .map_or_else(|| format!("rule {rule:#x}"), |name| name.to_string()),
            tree_units::tag(AstTree::view_id(), node.raw()),
        )
    } else {
        (
            String::from("error"),
            format!("recovered {:?}", node.category()),
            tree_units::tag(AstTree::view_id(), node.raw()),
        )
    };
    rows.push(TreeRow {
        key: format!("a{}", node.raw()),
        depth,
        kind,
        label,
        value: None,
        unit: Some(UnitRef::from_key(&unit)),
    });
    for child in node.children() {
        push_ast(rows, depth + 1, &child, rules);
    }
}

/// The metadata the parser attached, one entry per node. Two fields are drawn,
/// because the plate lets the reader switch between them: the semantic value and
/// the source span describe the same node in different terms.
fn meta_fields(snapshot: &reactive::Snapshot) -> Vec<FieldRender> {
    let view = snapshot.view::<AstMeta>();
    let starts = view.span_starts().into_iter().collect::<HashMap<_, _>>();
    let ends = view.span_ends().into_iter().collect::<HashMap<_, _>>();
    let spans: Vec<MapRow> = starts
        .into_iter()
        .map(|(node, start)| {
            let end = ends.get(&node).map(|anchor| anchor.offset);
            MapRow {
                key: format!("{node:#x}"),
                value: Some(match end {
                    Some(end) => format!("{}..{end}", start.offset),
                    None => format!("{}..", start.offset),
                }),
                unit: Some(UnitRef::entity(
                    AstMeta::view_id(),
                    AstMetaField::SPAN_STARTS,
                    UnitKind::Value,
                    node,
                )),
            }
        })
        .collect();
    let values: Vec<MapRow> = view
        .values()
        .into_iter()
        .map(|(node, value)| MapRow {
            key: format!("{node:#x}"),
            value: Some(format!("{value:?}")),
            unit: Some(UnitRef::entity(
                AstMeta::view_id(),
                AstMetaField::VALUES,
                UnitKind::Value,
                node,
            )),
        })
        .collect();
    vec![
        FieldRender {
            field: AstMetaField::VALUES,
            total: values.len(),
            model: RenderModel::Map(values),
            truncated: false,
        },
        FieldRender {
            field: AstMetaField::SPAN_STARTS,
            total: spans.len(),
            model: RenderModel::Map(spans),
            truncated: false,
        },
    ]
}

/// The pipeline's own counters: what the last revision actually cost.
fn stats(snapshot: &reactive::Snapshot) -> RenderModel {
    let counters = snapshot.view::<PipelineStats>().counters();
    RenderModel::Map(
        StatId::ALL
            .iter()
            .filter_map(|stat| {
                counters
                    .iter()
                    .find(|(key, _)| key == stat)
                    .map(|(_, value)| MapRow {
                        key: stat.name().to_owned(),
                        value: Some(value.to_string()),
                        unit: None,
                    })
            })
            .collect(),
    )
}

fn preview(kind: &str, count: usize) -> ValuePreview {
    ValuePreview {
        kind: kind.to_owned(),
        summary: format!("{count} row(s)"),
        detail: None,
        bytes: 0,
        truncated: false,
    }
}

fn main() -> std::io::Result<()> {
    let (runtime, recorder) = observed_runtime(ObserveLevel::Nodes);

    let grammar = grammar();
    runtime
        .install_general_component(TextComponent)
        .expect("text component");
    runtime
        .install_general_component(LexerComponent::new(grammar.lexer().clone()))
        .expect("lexer component");
    runtime
        .install_general_component(ParserComponent::new(grammar))
        .expect("parser component");
    runtime.install(app::Board).expect("board root");

    let document = Arc::new(Mutex::new(TextDocument::new(source())));
    runtime
        .edit(|edit| {
            edit.view::<SourceFile>()
                .set_document(document.lock().expect("document").clone());
        })
        .expect("seed");
    let _ = runtime.read_outputs::<app::Board>();

    Observatory::builder(runtime.clone(), recorder)
        .bind("127.0.0.1:7790")
        .bind_from_args()
        .renderer::<SourceFile>(source_rows)
        .renderer::<Text>(text)
        .field_renderer::<Tokens>(tokens)
        .renderer::<AstTree>(ast)
        .field_renderer::<AstMeta>(meta_fields)
        .renderer::<PipelineStats>(stats)
        .formatter::<SourceFile>(|snapshot| {
            preview(
                "TextDocument",
                snapshot.view::<SourceFile>().document().text().len(),
            )
        })
        .formatter::<Text>(|snapshot| {
            preview(
                "TextDocument",
                snapshot.view::<Text>().document().text().lines().count(),
            )
        })
        .formatter::<Tokens>(|snapshot| {
            preview("List<TokenId>", snapshot.view::<Tokens>().order().len())
        })
        .formatter::<AstTree>(|snapshot| {
            preview(
                "AstTree",
                usize::from(snapshot.view::<AstTree>().root().is_some()),
            )
        })
        .formatter::<AstMeta>(|snapshot| {
            preview("AstMeta", snapshot.view::<AstMeta>().values().len())
        })
        // The source View is the text the compiler reads. Editing it re-runs the
        // chain, because the root component demands the published tree.
        .text_view::<SourceFile>(
            {
                let document = Arc::clone(&document);
                move || document.lock().expect("document").text().to_owned()
            },
            {
                let runtime = runtime.clone();
                let document = Arc::clone(&document);
                move |text: &str| {
                    // The editor sends its whole buffer; commit the one edit that
                    // separates it from the document the pipeline holds, so the
                    // lexer and parser see a keystroke and not a new file.
                    let next = {
                        let current = document.lock().expect("document").clone();
                        current.replace_with(text).unwrap_or(current)
                    };
                    runtime
                        .edit(|edit| edit.view::<SourceFile>().set_document(next.clone()))
                        .map_err(|error| format!("{error}"))?;
                    *document.lock().expect("document") = next;
                    let _ = runtime.read_outputs::<app::Board>();
                    Ok(format!("{} chars", text.len()))
                }
            },
        )
        .serve()
}
