//! A JSON board: text, lexer, parser, and a sink that reads the tree.
//!
//! Run with
//! `cargo run --example observatory_json -p reactive --features observatory -- --bind 127.0.0.1:7791`.
//!
//! Three stages are ordinary `GeneralComponent`s from `reactive-syntax` — the
//! document, the lexer, the parser — and the fourth is a plain `#[component]`
//! that walks the published AST and publishes a keyed table of JSON paths. The
//! page draws all four the same way, because that is the only rule the
//! projection knows: a component, the Views it reads, the Views it defines.
//!
//! Edit the document in place. The lexer relexes the edit window, the parser
//! resumes from a checkpoint, and the sink rewrites only the paths whose value
//! moved — every row on the board cites its own unit, so the plate says which
//! paths changed and leaves the rest alone.

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

/// The document on the board.
fn source() -> &'static str {
    r#"{
  "name": "observatory",
  "version": 3,
  "live": true,
  "tags": ["runtime", "canvas"],
  "limits": { "rows": 64, "depth": 8 }
}"#
}

/// A small JSON grammar: objects, arrays, strings, integers, and keywords.
fn grammar() -> reactive_syntax::Grammar {
    reactive_syntax::Grammar::compile(
        r#"
        grammar Json {
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

// ------------------------------------------------------------------- the sink

/// One row of the sink's table: a JSON path and what sits at it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonPath {
    /// Dotted path from the document root, with array indices.
    pub path: String,
    /// The leaf's kind, as the lexer named it.
    pub kind: String,
    /// The leaf's text.
    pub value: String,
}

/// The sink's output: one row per leaf, in document order.
///
/// A list, not a map: a list replaces its identities on every run, so deleting a
/// key from the document deletes its row. Every row has its own identity, so an
/// edit to one value rewrites one unit.
#[reactive::view(derived)]
pub struct JsonPaths(List<JsonPath>);

/// A stable identity for one path: FNV-1a, so the same path keeps its unit.
fn path_id(path: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// The sink: it reads the published tree and writes the leaf table.
///
/// Nothing here is incremental by hand. The tree is a tree View, so each node's
/// structure and each token's payload are separate units; the walk reads the ones
/// it needs and the runtime decides what that means for the next revision. The
/// table is published with one identity per path, so only the paths whose value
/// moved are rewritten.
#[reactive::reactive]
mod sink {
    use super::{JsonPath, JsonPaths, path_id};
    use reactive_syntax::{AstMeta, AstNodeRef, AstTree, Tokens};
    use std::collections::HashMap;
    use std::sync::Arc;

    #[component]
    pub fn json_sink(ast: &AstTree, meta: &AstMeta, tokens: &Tokens) -> usize {
        let leaves = leaves(ast, meta, tokens);
        JsonPaths.define_keyed(
            leaves
                .iter()
                .map(|leaf| (path_id(&leaf.path), leaf.clone())),
        );
        leaves.len()
    }

    /// Every scalar in the document, in order, under its path.
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "the View API hands these zero-sized handles out by reference"
    )]
    fn leaves(ast: &AstTree, meta: &AstMeta, tokens: &Tokens) -> Vec<JsonPath> {
        let Some(root) = ast.root() else {
            return Vec::new();
        };
        let payloads: HashMap<u64, Arc<str>> = tokens
            .payloads()
            .to_vec()
            .into_iter()
            .map(|(id, payload)| (id.0, Arc::clone(&payload.text)))
            .collect();
        let rules: HashMap<u64, Arc<str>> = meta.rules().to_vec().into_iter().collect();
        let mut out = Vec::new();
        walk(&root, String::from("$"), &payloads, &rules, &mut out);
        out
    }

    /// Walks one node, appending a leaf per scalar.
    fn walk(
        node: &AstNodeRef,
        path: String,
        payloads: &HashMap<u64, Arc<str>>,
        rules: &HashMap<u64, Arc<str>>,
        out: &mut Vec<JsonPath>,
    ) {
        let children = node.children();
        if children.is_empty() {
            if let Some(token) = node.token_id() {
                let raw = payloads
                    .get(&token)
                    .map_or_else(String::new, std::string::ToString::to_string);
                out.push(JsonPath {
                    kind: token_kind(&raw).to_owned(),
                    value: unquote(&raw),
                    path,
                });
            }
            return;
        }
        let rule = node
            .rule()
            .and_then(|rule| rules.get(&rule))
            .map(std::convert::AsRef::as_ref);
        match rule {
            // An object is a run of pairs between the braces; a pair is a key
            // token, a colon, and a value.
            Some("JsonObject") => {
                for child in &children {
                    let pair = child
                        .rule()
                        .and_then(|rule| rules.get(&rule))
                        .is_some_and(|name| name.as_ref() == "JsonPair");
                    if !pair {
                        continue;
                    }
                    let parts = child.children();
                    let Some(key) = parts.first() else { continue };
                    let Some(value) = parts.get(2) else { continue };
                    let name = key
                        .token_id()
                        .and_then(|token| payloads.get(&token))
                        .map_or_else(|| format!("#{}", key.raw()), |text| unquote(text));
                    walk(value, format!("{path}.{name}"), payloads, rules, out);
                }
            }
            // An array is a run of values between the brackets; the brackets and
            // the commas themselves carry no identity.
            Some("JsonArray") => {
                let mut index = 0usize;
                for child in &children {
                    if child.token_id().is_some() {
                        continue;
                    }
                    walk(child, format!("{path}[{index}]"), payloads, rules, out);
                    index += 1;
                }
            }
            // A wrapper or a transparent group: keep the path it holds.
            _ => {
                for child in &children {
                    walk(child, path.clone(), payloads, rules, out);
                }
            }
        }
    }

    /// The text of a string token without its quotes.
    fn unquote(raw: &str) -> String {
        raw.strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(raw)
            .to_owned()
    }

    /// The leaf's kind, as the lexer named it.
    fn token_kind(raw: &str) -> &'static str {
        let first = raw.chars().next().unwrap_or(' ');
        if raw.starts_with('"') {
            "string"
        } else if first.is_ascii_digit() || first == '-' {
            "number"
        } else {
            "keyword"
        }
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

/// The token stream from its payload field: one row per token, one unit per row.
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

/// The AST as an outline; rule nodes cite their tag unit, leaves their payload.
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
            rules.get(&rule).map_or_else(
                || format!("rule {rule:#x}"),
                std::string::ToString::to_string,
            ),
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

/// The metadata the parser attached: the semantic values, then the spans.
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
    // Diagnostics are the parser's own words about the document: an edit that
    // breaks the JSON shows up here instead of quietly shrinking the sink.
    let diagnostics: Vec<MapRow> = view
        .diagnostics()
        .into_iter()
        .map(|(id, diagnostic)| MapRow {
            key: format!("{} @{}", id.code, id.start),
            value: Some(format!("{:?}: {}", diagnostic.severity, diagnostic.message)),
            unit: Some(UnitRef::entity(
                AstMeta::view_id(),
                AstMetaField::DIAGNOSTICS,
                UnitKind::Value,
                id,
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
        FieldRender {
            field: AstMetaField::DIAGNOSTICS,
            total: diagnostics.len(),
            model: RenderModel::Map(diagnostics),
            truncated: false,
        },
    ]
}

/// The sink's table: a path and what sits at it, one unit per path.
fn json_paths(snapshot: &reactive::Snapshot) -> RenderModel {
    let rows: Vec<MapRow> = snapshot
        .view::<JsonPaths>()
        .field0()
        .iter()
        .map(|leaf| MapRow {
            key: leaf.path.clone(),
            value: Some(format!("{} · {}", leaf.value, leaf.kind)),
            unit: Some(UnitRef::entity(
                JsonPaths::view_id(),
                FieldId(0),
                UnitKind::Value,
                path_id(&leaf.path),
            )),
        })
        .collect();
    RenderModel::Map(rows)
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
    runtime.install(sink::JsonSink).expect("sink");

    let document = Arc::new(Mutex::new(TextDocument::new(source())));
    runtime
        .edit(|edit| {
            edit.view::<SourceFile>()
                .set_document(document.lock().expect("document").clone());
        })
        .expect("seed");
    let _ = runtime.read_outputs::<sink::JsonSink>();

    Observatory::builder(runtime.clone(), recorder)
        .bind("127.0.0.1:7791")
        .bind_from_args()
        .renderer::<SourceFile>(source_rows)
        .renderer::<Text>(text)
        .field_renderer::<Tokens>(tokens)
        .renderer::<AstTree>(ast)
        .field_renderer::<AstMeta>(meta_fields)
        .renderer::<JsonPaths>(json_paths)
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
        .formatter::<JsonPaths>(|snapshot| {
            preview(
                "List<JsonPath>",
                snapshot.view::<JsonPaths>().field0().len(),
            )
        })
        .text_view::<SourceFile>(
            {
                let document = Arc::clone(&document);
                move || document.lock().expect("document").text().to_owned()
            },
            {
                let runtime = runtime;
                let document = Arc::clone(&document);
                move |text: &str| {
                    let next = {
                        let current = document.lock().expect("document").clone();
                        current.replace_with(text).unwrap_or(current)
                    };
                    runtime
                        .edit(|edit| edit.view::<SourceFile>().set_document(next.clone()))
                        .map_err(|error| format!("{error}"))?;
                    *document.lock().expect("document") = next;
                    let _ = runtime.read_outputs::<sink::JsonSink>();
                    Ok(format!("{} chars", text.len()))
                }
            },
        )
        .serve()
}
