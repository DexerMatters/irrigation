//! A JSON board: text, lexer, parser, and a sink that reads the tree.
//!
//! Run with
//! `cargo run --example observatory_json -p irrigation --features observatory -- --bind 127.0.0.1:7791`.
//!
//! Three stages are ordinary `GeneralComponent`s from `irrigation-syntax` — the
//! document, the lexer, the parser — and the fourth is a plain `#[component]`
//! that walks the published AST and publishes a keyed table of JSON paths. The
//! page draws all four the same way, because that is the only rule the
//! projection knows: a component, the Views it reads, the Views it defines.
//!
//! Edit the document in place. The lexer relexes the edit window, the parser
//! resumes from a checkpoint, and the sink rewrites only the leaves whose value
//! moved — every row on the board cites its own unit, so the plate says which
//! leaves changed and leaves the rest alone.

use std::sync::{Arc, Mutex};

use irrigation_observe::{Observatory, ObserveLevel, observed_runtime};
use irrigation_syntax::{LexerComponent, ParserComponent, SourceFile, TextComponent, TextDocument};

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
fn grammar() -> irrigation_syntax::Grammar {
    irrigation_syntax::Grammar::compile(
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
                Document = value: Value @tree(JsonDocument::JsonDocument);
                Value = Object | Array | Str | Num | Word;
                Object = "{" pairs: (Pair ("," Pair)*)? "}" @tree(JsonValue::JsonObject);
                Pair = key: string ":" value: Value @tree(JsonPair::JsonPair);
                Array = "[" values: (Value ("," Value)*)? "]" @tree(JsonValue::JsonArray);
                Str = text: string @tree(JsonValue::JsonString);
                Num = value: number @tree(JsonValue::JsonNumber);
                Word = word: ident @tree(JsonValue::JsonLiteral);
            }
        }
        "#,
    )
    .expect("grammar")
}

// ------------------------------------------------------------- the declared tree

/// The JSON document, declared once.
///
/// The grammar says which production builds which variant; the parser publishes
/// this tree from the same parse records as the raw AST, with the same identities.
#[allow(dead_code)]
#[irrigation::tree_view(kind = derived, root = JsonDocument, view = JsonSyntax)]
pub mod json_ast {
    /// A parsed document.
    #[derive(Clone, Debug, PartialEq)]
    pub enum JsonDocument {
        /// The document's single value.
        JsonDocument { value: JsonValue },
    }

    /// One JSON value.
    #[derive(Clone, Debug, PartialEq)]
    pub enum JsonValue {
        /// An object and its members, in source order.
        JsonObject { pairs: Vec<JsonPair> },
        /// An array and its elements, in source order.
        JsonArray { values: Vec<JsonValue> },
        /// A string literal.
        JsonString { text: String },
        /// An integer literal.
        JsonNumber { value: i64 },
        /// A keyword (`true`, `false`, `null`).
        JsonLiteral { word: String },
    }

    /// One object member.
    #[derive(Clone, Debug, PartialEq)]
    pub enum JsonPair {
        /// A key and its value.
        JsonPair { key: String, value: JsonValue },
    }
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
/// A row is keyed by the leaf's AST occurrence, not by its path: the path is
/// what the leaf is called, and a rename changes that without replacing the
/// leaf. Keying by the occurrence is what makes a value edit update one row and
/// a deleted leaf retract its own.
#[irrigation::view(derived)]
pub struct JsonPaths(List<JsonPath>);

/// The sink: it reads the published tree and writes the leaf table.
///
/// Nothing here is incremental by hand. The tree is a tree View, so each node's
/// structure, each child slot, and each leaf payload are separate units; the walk
/// reads the ones it needs and the runtime decides what that means for the next
/// revision. The table is published with one identity per declared leaf, so only
/// the row whose value (or path) moved is rewritten. There is no rule-name table
/// and no payload table to materialize: the variant *is* the kind, and the leaf
/// accessor reads one unit.
#[irrigation::irrigation]
mod sink {
    use super::json_ast::{JsonDocumentRef, JsonValueKind, JsonValueRef};
    use super::{JsonPath, JsonPaths};

    #[component]
    pub fn json_sink(tree: &super::json_ast::JsonSyntax) -> usize {
        let leaves = match tree.root() {
            Some(document) => leaves(&document),
            None => Vec::new(),
        };
        JsonPaths.define_keyed(leaves.iter().map(|(id, leaf)| (*id, leaf.clone())));
        leaves.len()
    }

    /// Every scalar in the document, in order, under its path.
    fn leaves(document: &JsonDocumentRef) -> Vec<(u64, JsonPath)> {
        let mut out = Vec::new();
        if let Some(value) = document.value() {
            walk(&value, String::from("$"), &mut out);
        }
        out
    }

    /// Walks one value, appending `(identity, row)` per scalar.
    ///
    /// The identity is the declared leaf's node, which is the parser's node, so a
    /// re-spelling or a rename keeps the row and changes what it says.
    fn walk(value: &JsonValueRef, path: String, out: &mut Vec<(u64, JsonPath)>) {
        match value.kind() {
            Some(JsonValueKind::JsonObject) => {
                for pair in value.pairs() {
                    let Some(child) = pair.value() else {
                        continue;
                    };
                    let name = pair
                        .key()
                        .map_or_else(|| format!("#{}", pair.raw()), |key| unquote(&key));
                    walk(&child, format!("{path}.{name}"), out);
                }
            }
            Some(JsonValueKind::JsonArray) => {
                for (index, child) in value.values().into_iter().enumerate() {
                    walk(&child, format!("{path}[{index}]"), out);
                }
            }
            Some(JsonValueKind::JsonString) => out.push((
                value.raw(),
                JsonPath {
                    kind: String::from("string"),
                    value: unquote(&value.text().unwrap_or_default()),
                    path,
                },
            )),
            Some(JsonValueKind::JsonNumber) => out.push((
                value.raw(),
                JsonPath {
                    kind: String::from("number"),
                    value: value.value().unwrap_or_default().to_string(),
                    path,
                },
            )),
            Some(JsonValueKind::JsonLiteral) => out.push((
                value.raw(),
                JsonPath {
                    kind: String::from("keyword"),
                    value: value.word().unwrap_or_default(),
                    path,
                },
            )),
            // A recovery node builds no declared variant; the parse diagnostics
            // already explain it.
            None => {}
        }
    }

    /// The text of a string token without its quotes.
    fn unquote(raw: &str) -> String {
        raw.strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(raw)
            .to_owned()
    }
}

fn main() -> std::io::Result<()> {
    let (runtime, recorder) = observed_runtime(ObserveLevel::Nodes);

    let grammar = grammar();
    runtime
        .install_general_producer(TextComponent)
        .expect("text component");
    runtime
        .install_general_producer(LexerComponent::new(grammar.lexer().clone()))
        .expect("lexer component");
    runtime
        .install_general_producer(
            ParserComponent::with_tree::<json_ast::JsonSyntax>(grammar)
                .expect("the grammar maps onto the declared tree"),
        )
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

#[cfg(test)]
mod tests {
    use irrigation::{Runtime, ViewSchema};
    use irrigation_syntax::{
        LexerComponent, ParserComponent, SourceFile, Text, TextComponent, TextDocument, Tokens,
    };

    use super::JsonPaths;

    /// A board with the syntax pipeline and the sink, over `text`.
    fn board(text: &str) -> Runtime {
        let runtime = Runtime::new();
        let grammar = super::grammar();
        runtime.register_view(<Text as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<Tokens as ViewSchema>::DESCRIPTOR);
        runtime
            .install_general_producer(TextComponent)
            .expect("text component");
        runtime
            .install_general_producer(LexerComponent::new(grammar.lexer().clone()))
            .expect("lexer component");
        runtime
            .install_general_producer(
                ParserComponent::with_tree::<super::json_ast::JsonSyntax>(grammar)
                    .expect("the grammar maps onto the declared tree"),
            )
            .expect("parser component");
        runtime.install(super::sink::JsonSink).expect("sink");
        runtime
            .edit(|edit| {
                edit.view::<SourceFile>()
                    .set_document(TextDocument::new(text))
            })
            .expect("seed");
        let _ = runtime.read_outputs::<super::sink::JsonSink>();
        runtime
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
        let _ = runtime.read_outputs::<super::sink::JsonSink>();
    }

    /// `(row identity, row)` pairs, in document order.
    fn rows(runtime: &Runtime) -> Vec<(u64, super::JsonPath)> {
        let snapshot = runtime.read().expect("snapshot");
        let paths = snapshot.view::<JsonPaths>();
        paths
            .field0_ids()
            .iter()
            .copied()
            .map(|id| (id, paths.field0_key(id).expect("row")))
            .collect()
    }

    fn paths(runtime: &Runtime) -> Vec<String> {
        rows(runtime).into_iter().map(|(_, row)| row.path).collect()
    }

    #[test]
    fn a_value_edit_keeps_the_row() {
        let runtime = board(super::source());
        let before = rows(&runtime);
        let text = runtime
            .read()
            .expect("snapshot")
            .view::<SourceFile>()
            .document()
            .text()
            .to_owned();
        let at = text.find("\"version\": 3").expect("the version") + "\"version\": ".len();
        let edit_text = format!("{}{}{}", &text[..at], "4", &text[at + 1..]);
        edit(&runtime, &edit_text);

        let after = rows(&runtime);
        assert_eq!(
            after.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            before.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            "a value edit must not replace the row"
        );
        assert!(
            after
                .iter()
                .any(|(_, row)| row.path == "$.version" && row.value == "4"),
            "{after:?}"
        );
        assert!(!after.iter().any(|(_, row)| row.value == "3"));
    }

    #[test]
    fn a_rename_keeps_the_row_and_moves_its_path() {
        let runtime = board(super::source());
        let before = rows(&runtime);
        let text = {
            let snapshot = runtime.read().expect("snapshot");
            snapshot.view::<SourceFile>().document().text().to_owned()
        };
        let renamed = text.replace("\"name\"", "\"title\"");
        assert_ne!(renamed, text);
        edit(&runtime, &renamed);

        let after = rows(&runtime);
        assert_eq!(
            after.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            before.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            "a key rename must not replace the value's row"
        );
        let moved = after
            .iter()
            .find(|(_, row)| row.path == "$.title")
            .expect("the renamed path");
        let source = after
            .iter()
            .find(|(_, row)| row.value == "observatory")
            .expect("the value");
        assert_eq!(moved.0, source.0, "the same leaf, addressed differently");
        assert_eq!(moved.1.value, "observatory");
    }

    #[test]
    fn a_deleted_leaf_retracts_its_row() {
        let runtime = board(super::source());
        let before = rows(&runtime);
        let text = {
            let snapshot = runtime.read().expect("snapshot");
            snapshot.view::<SourceFile>().document().text().to_owned()
        };
        let deleted = text.replace(",\n  \"limits\": { \"rows\": 64, \"depth\": 8 }", "");
        assert_ne!(deleted, text);
        edit(&runtime, &deleted);

        let after = rows(&runtime);
        assert!(
            after.len() < before.len(),
            "the deleted leaves retire their rows"
        );
        assert!(
            after
                .iter()
                .all(|(id, _)| before.iter().any(|(old, _)| old == id)),
            "a deletion removes identities, it does not move them into new ones"
        );
        assert!(
            !paths(&runtime)
                .iter()
                .any(|path| path.starts_with("$.limits"))
        );
    }
}
