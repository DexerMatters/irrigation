//! Ten Views on one board: the G10 shape.
//!
//! Run with
//! `cargo run --example observe_board -p irrigation --features observatory -- --bind 127.0.0.1:7780`.
//!
//! Every field kind appears once, so the page has to lay out ten modules, each
//! with its own drawing. The workload is deliberately small; the point is the
//! board, not the numbers.

use irrigation::{irrigation, tree_view, view};
use irrigation_observe::{Observatory, ObserveLevel, observed_runtime};

// ---------------------------------------------------------------- the Views

#[view(source)]
pub struct Lines {
    lines: List<String>,
}

#[view(source)]
pub struct Symbols {
    entries: Map<String, u32>,
}

#[view(source)]
pub struct Flags {
    members: Set<String>,
}

#[view(source)]
pub struct Limits {
    budget: u64,
    retries: u32,
}

#[view(source)]
pub struct Versions {
    pins: Map<String, String>,
}

#[view(source)]
pub struct Weights {
    values: Reduced<Sum>,
}

/// The sum reduction, so one module has something to aggregate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sum;

impl irrigation::Reduction for Sum {
    type Item = u64;
    type Aggregate = u64;

    fn leaf(item: &Self::Item) -> Self::Aggregate {
        *item
    }

    fn combine(left: &Self::Aggregate, right: &Self::Aggregate) -> Self::Aggregate {
        left + right
    }

    fn identity() -> Self::Aggregate {
        0
    }
}

/// A derived report: whole-value, so the page shows it as such.
#[view(derived)]
pub struct Report(List<String>);

/// A second derived list with a different source.
#[view(derived)]
pub struct Issues(List<String>);

/// A third derived list, to make the board wide.
#[view(derived)]
pub struct Mirror(List<u32>);

#[tree_view(root = Document, view = Outline)]
pub mod outline {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Document {
        Graph { nodes: Vec<Item> },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Item {
        Named { name: String },
        Empty,
    }
}

#[irrigation]
pub mod derive {
    use super::outline;

    /// One root component publishes every derived View: a runtime has one root,
    /// and a tuple output is how a single component defines several Views.
    #[component]
    pub fn all(
        symbols: &super::Symbols,
        outline: &outline::Outline,
    ) -> (super::Report, super::Issues, super::Mirror) {
        let entries = symbols.entries();
        let keys = entries.keys();
        let report = super::Report::from(
            keys.iter()
                .map(|key| {
                    let value = entries.get(key).unwrap_or_default();
                    format!("{key} = {value}")
                })
                .collect::<Vec<_>>(),
        );
        let mirror =
            super::Mirror::from(keys.iter().map(|key| key.len() as u32).collect::<Vec<_>>());
        let issues = match outline.root() {
            Some(document) => {
                let empty = document
                    .nodes()
                    .iter()
                    .filter(|item| item.name().unwrap_or_default().is_empty())
                    .count();
                super::Issues::from(if empty == 0 {
                    Vec::new()
                } else {
                    vec![format!("{empty} unnamed items")]
                })
            }
            None => super::Issues::from(Vec::new()),
        };
        (report, issues, mirror)
    }
}

fn program() -> outline::Document {
    outline::Document::Graph {
        nodes: vec![
            outline::Item::Named {
                name: String::from("parse"),
            },
            outline::Item::Named {
                name: String::from("check"),
            },
            outline::Item::Empty,
        ],
    }
}

fn main() -> std::io::Result<()> {
    let (runtime, recorder) = observed_runtime(ObserveLevel::Nodes);
    runtime.install(derive::All).expect("install");

    runtime
        .edit(|edit| {
            {
                let mut lines = edit.view::<Lines>();
                let mut values = lines.lines();
                for line in ["import a", "let b = a", "print(b)"] {
                    values.push(String::from(line));
                }
            }
            {
                let mut symbols = edit.view::<Symbols>();
                let mut entries = symbols.entries();
                for (index, name) in ["a", "b"].into_iter().enumerate() {
                    entries.insert(String::from(name), index as u32);
                }
            }
            {
                let mut flags = edit.view::<Flags>();
                let mut members = flags.members();
                members.insert(String::from("strict"));
                members.insert(String::from("verbose"));
            }
            {
                let mut limits = edit.view::<Limits>();
                limits.set_budget(1024);
                limits.set_retries(3);
            }
            {
                let mut versions = edit.view::<Versions>();
                let mut pins = versions.pins();
                pins.insert(String::from("irrigation"), String::from("0.1"));
            }
            {
                let mut weights = edit.view::<Weights>();
                let mut values = weights.values();
                values.push(4);
                values.push(6);
            }
            {
                let mut view = edit.view::<outline::Outline>();
                view.set_root(&program());
            }
        })
        .expect("seed");
    let _ = runtime.read_outputs::<derive::All>();

    Observatory::builder(runtime, recorder)
        .bind("127.0.0.1:7780")
        .bind_from_args()
        .serve()
}
