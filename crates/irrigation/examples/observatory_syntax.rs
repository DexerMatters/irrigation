//! The board over a real compiler pipeline.
//!
//! Run with
//! `cargo run --example observatory_syntax -p irrigation --features observatory -- --bind 127.0.0.1:7790`.
//!
//! Every stage is a `GeneralComponent` from `irrigation-syntax`: the text
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

use std::sync::{Arc, Mutex};

use irrigation_observe::{Observatory, ObserveLevel, observed_runtime};
use irrigation_syntax::{LexerComponent, ParserComponent, SourceFile, TextComponent, TextDocument};

/// A real JSON document: the pipeline's input.
fn source() -> &'static str {
    r#"{
  "name": "observatory",
  "views": 6,
  "live": true,
  "tags": ["runtime", "canvas"]
}"#
}

fn grammar() -> irrigation_syntax::Grammar {
    irrigation_syntax::Grammar::compile(
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
#[irrigation::irrigation]
mod app {
    use irrigation_syntax::{AstTree, Tokens};

    #[component]
    pub fn board(tokens: &Tokens, ast: &AstTree) -> (usize, bool) {
        (tokens.order().len(), ast.root().is_some())
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
        .install_general_producer(ParserComponent::new(grammar))
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
        // The source View is the text the compiler reads. Editing it re-runs the
        // chain, because the root component demands the published tree.
        .text_view::<SourceFile>(
            {
                let document = Arc::clone(&document);
                move || document.lock().expect("document").text().to_owned()
            },
            {
                let runtime = runtime;
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
