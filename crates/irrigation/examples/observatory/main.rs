//! A reference host for the Observatory.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example observatory -p irrigation --features observatory
//! ```
//!
//! The board is the compiler pipeline, with no exception: an editable source
//! document, the framework's text, lexer, and parser components, and two
//! ordinary components over the published AST. The host owns the text channel
//! and nothing else; there is no parser in the callback and no syntax tree that
//! pretends to be source.

mod demo;
mod grammar;

use std::sync::{Arc, Mutex};

use irrigation::{Runtime, RuntimeConfig};
use irrigation_observe::{Observatory, ObserveLevel, recorder};
use irrigation_syntax::{LexerComponent, ParserComponent, SourceFile, TextComponent};

use demo::{Weights, resolve};

fn main() -> std::io::Result<()> {
    let recorder = recorder(ObserveLevel::Nodes);
    let observer: Arc<dyn irrigation_observe::RuntimeObserver> = Arc::clone(&recorder) as Arc<_>;
    let runtime = Runtime::with_observer(RuntimeConfig::default(), observer);

    let grammar = grammar::grammar();
    runtime
        .install_general_producer(TextComponent)
        .expect("text component");
    runtime
        .install_general_producer(LexerComponent::new(grammar.lexer().clone()))
        .expect("lexer component");
    runtime
        .install_general_producer(
            ParserComponent::with_tree::<demo::ast::Syntax>(grammar)
                .expect("the grammar maps onto the declared tree"),
        )
        .expect("parser component");
    runtime
        .install(resolve::ResolveProgram)
        .expect("install resolver");

    let document = Arc::new(Mutex::new(grammar::seed()));
    let weights = Arc::new(Mutex::new(vec![4u64, 6, 2]));
    runtime
        .edit(|edit| {
            edit.view::<SourceFile>()
                .set_document(document.lock().expect("document").clone());
        })
        .expect("seed the document");
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Weights>();
            let mut values = view.field0();
            for value in [4u64, 6, 2] {
                values.push(value);
            }
        })
        .expect("seed the weights");
    // One demand so the page opens on a resolved revision rather than a
    // half-observed one.
    let _ = runtime.read_outputs::<resolve::ResolveProgram>();

    let observatory = Observatory::builder(runtime.clone(), recorder)
        .bind("127.0.0.1:7777")
        .bind_from_args()
        .static_dir(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/observatory/dist"
        ))
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
                    let _ = runtime.read_outputs::<resolve::ResolveProgram>();
                    Ok(format!("{} chars", text.len()))
                }
            },
        )
        .text_view::<Weights>(
            {
                let weights = Arc::clone(&weights);
                move || {
                    weights
                        .lock()
                        .expect("weights")
                        .iter()
                        .map(std::string::ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            },
            {
                let runtime = runtime;
                let weights = Arc::clone(&weights);
                // One number per line. The diff is minimal: only the positions
                // whose value differs are staged, so only those units blink.
                move |text: &str| {
                    let next: Vec<u64> = text
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(|line| {
                            line.parse::<u64>()
                                .map_err(|_| format!("`{line}` is not a number"))
                        })
                        .collect::<Result<_, _>>()?;
                    let old = weights.lock().expect("weights").clone();
                    let mut changed = 0usize;
                    runtime
                        .edit(|edit| {
                            let mut view = edit.view::<Weights>();
                            let mut values = view.field0();
                            for (index, value) in next.iter().enumerate() {
                                if index < old.len() {
                                    // Only the positions whose value differs are
                                    // staged, so only those units blink.
                                    if old[index] != *value {
                                        values.set(index, *value);
                                        changed += 1;
                                    }
                                } else {
                                    values.push(*value);
                                    changed += 1;
                                }
                            }
                            for _ in next.len()..old.len() {
                                values.remove(next.len());
                            }
                        })
                        .map_err(|error| format!("{error}"))?;
                    *weights.lock().expect("weights") = next;
                    let _ = runtime.read_outputs::<resolve::ResolveProgram>();
                    Ok(format!("{changed} weights changed"))
                }
            },
        )
        .start()?;

    println!("Observatory listening on {}", observatory.url());
    println!("Edit the source plate: the lexer and parser are components on the board.");
    observatory.wait();
    Ok(())
}
