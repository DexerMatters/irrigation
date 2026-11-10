//! The smallest host: two Views, one component, and the board.
//!
//! `cargo run --example observatory_min -p irrigation --features observatory`
//!
//! Everything a host has to write is here, and nothing else: instrument the
//! runtime, install the component, seed the source View, register how each View
//! should be drawn, and serve. Copy this file as the starting point; the board
//! discovers the built page from the repository root.
//!
//! What the page adds for free: the component appears as a chip between the two
//! plates, the wire it rides is the definition it recorded, and a plate you edit
//! reports the row that moved — because every row cites its unit. No renderer is
//! registered: the board draws `Counter` and `Doubled` from the runtime's facts,
//! spelling values with their `Debug` form.

use irrigation::{irrigation, view};
use irrigation_observe::{Observatory, ObserveLevel, observed_runtime};

/// A number the host owns. This plate can be edited in place.
#[view(source)]
pub struct Counter {
    value: u64,
}

/// What the component publishes from it: one row per doubling.
#[view(derived)]
pub struct Doubled(List<u64>);

/// One component: it reads `Counter` and defines `Doubled`.
#[irrigation]
mod pipeline {
    use super::{Counter, Doubled};

    #[component]
    pub fn double(counter: &Counter) -> Doubled {
        Doubled::from([counter.value().wrapping_mul(2)])
    }
}

fn main() -> std::io::Result<()> {
    // A runtime that reports what it does, with the recorder that collects it.
    let (runtime, recorder) = observed_runtime(ObserveLevel::Nodes);
    runtime.install(pipeline::Double).expect("install");
    runtime
        .edit(|edit| edit.view::<Counter>().set_value(21))
        .expect("seed");
    // Demand once, so the first frame is fully resolved instead of `suspect`.
    let _ = runtime.read_outputs::<pipeline::Double>();

    // The board draws both plated Views from the runtime's facts. There is no
    // second drawing to register: the View's shape and values are the drawing.
    Observatory::builder(runtime.clone(), recorder)
        .bind("127.0.0.1:7794")
        .bind_from_args()
        .text_view::<Counter>(
            {
                let runtime = runtime.clone();
                move || runtime.snapshot().view::<Counter>().value().to_string()
            },
            {
                let runtime = runtime;
                move |text: &str| {
                    let value: u64 = text
                        .trim()
                        .parse()
                        .map_err(|_| format!("`{}` is not a number", text.trim()))?;
                    runtime
                        .edit(|edit| edit.view::<Counter>().set_value(value))
                        .map_err(|error| format!("{error}"))?;
                    let _ = runtime.read_outputs::<pipeline::Double>();
                    Ok(format!("counter = {value}"))
                }
            },
        )
        .serve()
}
