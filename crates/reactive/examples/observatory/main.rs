//! A reference host for the Observatory.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example observatory -p reactive --features observatory
//! ```
//!
//! It seeds a three-statement program, installs the resolver, and serves the
//! page. The named actions under `POST /api/v1/actions/<name>` are ordinary
//! host edits; the page records them as the cause, then observes what the
//! runtime did about it.

mod demo;
mod renderers;
mod source;

use std::sync::Arc;

use reactive::{Runtime, RuntimeConfig};
use reactive_observe::{Observatory, ObserveLevel, recorder};

fn main() -> std::io::Result<()> {
    let recorder = recorder(ObserveLevel::Nodes);
    let observer: Arc<dyn reactive_observe::RuntimeObserver> = Arc::clone(&recorder) as Arc<_>;
    let runtime = Runtime::with_observer(RuntimeConfig::default(), observer);
    runtime
        .install(resolve::ResolveProgram)
        .expect("install resolver");
    runtime
        .edit(|edit| {
            edit.view::<ast::Syntax>().set_root(&demo::program());
        })
        .expect("seed the program");
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

    // The page can read and replace each source View's text; the host owns both
    // languages. The two sources are different kinds on purpose.
    let document = Arc::new(Mutex::new(demo::program()));
    let weights = Arc::new(Mutex::new(vec![4u64, 6, 2]));

    // `--bind <addr>` keeps a second instance from fighting over the default.
    let mut bind = String::from("127.0.0.1:7777");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--bind" {
            if let Some(value) = args.next() {
                bind = value;
            }
        }
    }

    let observatory = Observatory::builder(runtime.clone(), recorder)
        .bind(bind.clone())
        .static_dir(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/observatory/dist"
        ))
        .renderer::<ast::Syntax>(renderers::syntax)
        .renderer::<Symbols>(renderers::symbols)
        .renderer::<Diagnostics>(renderers::diagnostics)
        .renderer::<Weights>(renderers::weights)
        .renderer::<Total>(renderers::total)
        .formatter::<Symbols>(renderers::symbols_preview)
        .formatter::<Diagnostics>(renderers::diagnostics_preview)
        .formatter::<Weights>(renderers::weights_preview)
        .text_view::<ast::Syntax>(
            {
                let document = Arc::clone(&document);
                move || source::print(&document.lock().expect("document"))
            },
            {
                let runtime = runtime.clone();
                let document = Arc::clone(&document);
                move |text: &str| {
                    let parsed = source::parse(text)?;
                    let statements = match &parsed {
                        ast::Document::Program { statements } => statements.len(),
                    };
                    runtime
                        .edit(|edit| {
                            edit.view::<ast::Syntax>().set_root(&parsed);
                        })
                        .map_err(|error| format!("{error}"))?;
                    *document.lock().expect("document") = parsed;
                    // An editor expects its outputs: demanding here is what
                    // makes the change flow down while the page watches.
                    let _ = runtime.read_outputs::<resolve::ResolveProgram>();
                    Ok(format!("{statements} statements"))
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
                        .map(|value| value.to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            },
            {
                let runtime = runtime.clone();
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
    observatory.wait();
    Ok(())
}

use demo::{Diagnostics, Symbols, Total, Weights, ast, resolve};
use std::sync::Mutex;
