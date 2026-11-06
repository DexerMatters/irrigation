//! Ten Views on one board: the G10 shape.
//!
//! Run with
//! `cargo run --example observe_board -p reactive --features observatory -- --bind 127.0.0.1:7780`.
//!
//! Every field kind appears once, so the page has to lay out ten modules, each
//! with its own drawing. The workload is deliberately small; the point is the
//! board, not the numbers.

use reactive::{reactive, tree_view, view};
use reactive_observe::{
    ListRow, MapRow, Observatory, ObserveLevel, RenderModel, ScalarRow, SetMember, TreeRow,
    ValuePreview, observed_runtime,
};

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

impl reactive::Reduction for Sum {
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

#[reactive]
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

// ------------------------------------------------------------------ renderers

fn lines(snapshot: &reactive::Snapshot) -> RenderModel {
    RenderModel::List(
        snapshot
            .view::<Lines>()
            .lines()
            .into_iter()
            .enumerate()
            .map(|(index, value)| ListRow {
                index,
                value,
                unit: None,
            })
            .collect(),
    )
}

fn symbols(snapshot: &reactive::Snapshot) -> RenderModel {
    RenderModel::Map(
        snapshot
            .view::<Symbols>()
            .entries()
            .into_iter()
            .map(|(key, value)| MapRow {
                key,
                value: Some(value.to_string()),
                unit: None,
            })
            .collect(),
    )
}

fn flags(snapshot: &reactive::Snapshot) -> RenderModel {
    let view = snapshot.view::<Flags>();
    RenderModel::Set(
        view.members()
            .into_iter()
            .map(|member| SetMember {
                member,
                present: true,
                unit: None,
            })
            .collect(),
    )
}

fn limits(snapshot: &reactive::Snapshot) -> RenderModel {
    let view = snapshot.view::<Limits>();
    RenderModel::Scalar(vec![
        ScalarRow {
            name: String::from("budget"),
            value: view.budget().to_string(),
            unit: None,
        },
        ScalarRow {
            name: String::from("retries"),
            value: view.retries().to_string(),
            unit: None,
        },
    ])
}

fn versions(snapshot: &reactive::Snapshot) -> RenderModel {
    let view = snapshot.view::<Versions>();
    RenderModel::Map(
        view.pins()
            .into_iter()
            .map(|(key, value)| MapRow {
                key,
                value: Some(value),
                unit: None,
            })
            .collect(),
    )
}

fn weights(snapshot: &reactive::Snapshot) -> RenderModel {
    let view = snapshot.view::<Weights>();
    RenderModel::Reduced {
        aggregate: view.values_aggregate().to_string(),
        reduction: String::from("sum"),
        sample: view
            .values()
            .into_iter()
            .take(3)
            .map(|value| value.to_string())
            .collect(),
    }
}

fn outline(snapshot: &reactive::Snapshot) -> RenderModel {
    let view = snapshot.view::<outline::Outline>();
    let mut rows = Vec::new();
    let Some(document) = view.root() else {
        return RenderModel::Tree(rows);
    };
    rows.push(TreeRow {
        key: String::from("root"),
        depth: 0,
        kind: String::from("Graph"),
        label: format!("{} items", document.nodes().len()),
        value: None,
        unit: None,
    });
    for (index, item) in document.nodes().iter().enumerate() {
        rows.push(TreeRow {
            key: format!("i{index}"),
            depth: 1,
            kind: if item.name().is_some() {
                String::from("Named")
            } else {
                String::from("Empty")
            },
            label: item.name().unwrap_or_default(),
            value: None,
            unit: None,
        });
    }
    RenderModel::Tree(rows)
}

fn list_of(rows: Vec<String>) -> RenderModel {
    RenderModel::List(
        rows.into_iter()
            .enumerate()
            .map(|(index, value)| ListRow {
                index,
                value,
                unit: None,
            })
            .collect(),
    )
}

fn preview(kind: &str, count: usize) -> ValuePreview {
    ValuePreview {
        kind: kind.to_owned(),
        summary: format!("whole value · {count} rows"),
        detail: None,
        bytes: 0,
        truncated: false,
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
                pins.insert(String::from("reactive"), String::from("0.1"));
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
        .renderer::<Lines>(lines)
        .renderer::<Symbols>(symbols)
        .renderer::<Flags>(flags)
        .renderer::<Limits>(limits)
        .renderer::<Versions>(versions)
        .renderer::<Weights>(weights)
        .renderer::<outline::Outline>(outline)
        .renderer::<Report>(|snapshot| list_of(snapshot.view::<Report>().field0()))
        .renderer::<Issues>(|snapshot| list_of(snapshot.view::<Issues>().field0()))
        .renderer::<Mirror>(|snapshot| {
            list_of(
                snapshot
                    .view::<Mirror>()
                    .field0()
                    .into_iter()
                    .map(|value| value.to_string())
                    .collect(),
            )
        })
        .formatter::<Report>(|snapshot| {
            preview("List<String>", snapshot.view::<Report>().field0().len())
        })
        .formatter::<Issues>(|snapshot| {
            preview("List<String>", snapshot.view::<Issues>().field0().len())
        })
        .formatter::<Mirror>(|snapshot| {
            preview("List<u32>", snapshot.view::<Mirror>().field0().len())
        })
        .serve()
}
