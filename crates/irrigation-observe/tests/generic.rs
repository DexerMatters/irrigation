//! A View drawn without a host renderer.
//!
//! The promise of the generic projection is that a host which registers nothing
//! still gets an honest plate: rows, values, and change marks on the rows whose
//! unit moved. These tests speak the wire, because the wire is the contract.

use std::sync::Arc;

use irrigation::{Runtime, RuntimeConfig, irrigation, tree_view, view};
use irrigation_observe::{Observatory, ObserveLevel, recorder};

#[view(source)]
pub struct Names {
    values: List<String>,
}

#[view(source)]
pub struct Settings {
    budget: u64,
}

#[view(source)]
pub struct Table {
    entries: Map<u64, u64>,
}

#[view(derived)]
pub struct Sizes(List<u64>);

/// A graph with no host renderer: the generic drawing must still wire it.
#[view(derived)]
pub struct Net(Graph<u64, u64>);

/// A hidden View spells nothing: identity and marks only.
#[view(source, hide)]
pub struct Opaque {
    values: List<u64>,
}

#[tree_view(root = Document, view = Syntax)]
pub mod syntax {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Document {
        Module { items: Vec<Expr> },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Expr {
        Name(String),
        Int(i64),
    }
}

#[irrigation]
mod analysis {
    use super::{Names, Net, Settings, Sizes};

    #[component]
    pub fn publish_sizes(names: &Names, settings: &Settings) -> Sizes {
        let factor = settings.budget();
        Net.define_graph(&[(1u64, 11u64), (2, 22)], &[(10u64, 1, 2, 100u64)]);
        Sizes::from(
            names
                .values()
                .to_vec()
                .into_iter()
                .map(|_| factor)
                .collect::<Vec<_>>(),
        )
    }
}

struct Harness {
    observatory: Observatory,
    runtime: Runtime,
    port: u16,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.observatory.shutdown();
    }
}

fn start() -> Harness {
    let recorder = recorder(ObserveLevel::Nodes);
    let observer: Arc<dyn irrigation_observe::RuntimeObserver> = Arc::clone(&recorder) as Arc<_>;
    let runtime = Runtime::with_observer(RuntimeConfig::default(), observer);
    runtime.install(analysis::PublishSizes).expect("sizes");
    runtime
        .edit(|edit| {
            let mut tree = edit.view::<syntax::Syntax>();
            tree.set_root(&syntax::Document::Module {
                items: vec![syntax::Expr::Name(String::from("f")), syntax::Expr::Int(2)],
            });
        })
        .expect("tree");
    runtime
        .edit(|edit| {
            edit.view::<Settings>().set_budget(9);
            {
                let mut names = edit.view::<Names>();
                let mut values = names.values();
                values.push(String::from("alpha"));
                values.push(String::from("beta"));
            }
            {
                let mut opaque = edit.view::<Opaque>();
                let mut hidden = opaque.values();
                hidden.push(4);
                hidden.push(5);
            }
            {
                let mut table = edit.view::<Table>();
                let mut entries = table.entries();
                entries.insert(0, 10);
                entries.insert(1, 20);
            }
        })
        .expect("seed");
    let _ = runtime.read_outputs::<analysis::PublishSizes>();

    let observatory = Observatory::builder(runtime.clone(), recorder)
        .bind("127.0.0.1:0")
        .history(16)
        .start()
        .expect("start");
    let port = observatory
        .url()
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    Harness {
        observatory,
        runtime,
        port,
    }
}

fn get(harness: &Harness, path: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", harness.port)).expect("connect");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("write");
    let mut body = String::new();
    stream.read_to_string(&mut body).expect("read");
    let status = body
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = body
        .split_once("\r\n\r\n")
        .map_or(String::new(), |(_, rest)| rest.to_owned());
    (status, body)
}

fn view_key_of(harness: &Harness, suffix: &str) -> String {
    let (_, body) = get(harness, "/api/v1/workspace");
    let workspace: serde_json::Value = serde_json::from_str(&body).expect("workspace json");
    workspace["views"]
        .as_array()
        .expect("views")
        .iter()
        .find(|view| {
            view["name"]
                .as_str()
                .is_some_and(|name| name.ends_with(suffix))
        })
        .and_then(|view| view["view"].as_str())
        .expect("view present")
        .to_owned()
}

#[test]
fn a_scalar_view_draws_its_value() {
    let harness = start();
    let view = view_key_of(&harness, "Settings");
    let (status, body) = get(&harness, &format!("/api/v1/views/{view}/render"));
    assert_eq!(status, 200);
    assert!(body.contains("\"value\":\"9\""), "{body}");
    assert!(body.contains("\"state\":\"changed\""), "{body}");
    assert!(body.contains("\"inconsistent\":false"), "{body}");
}

#[test]
fn a_list_view_draws_its_rows() {
    let harness = start();
    let view = view_key_of(&harness, "Sizes");
    let (status, body) = get(&harness, &format!("/api/v1/views/{view}/render"));
    assert_eq!(status, 200);
    assert!(body.contains("\"kind\":\"list\""), "{body}");
    assert!(body.contains("\"total\":2"), "{body}");
    // One row per name, each spelling its value through `Debug`.
    assert!(body.contains("\"value\":\"9\""), "{body}");
}

/// A map row shows the value at its key, so a value edit marks that row even
/// though the key's membership unit never moved.
#[test]
fn a_map_row_marks_the_value_that_changed() {
    let harness = start();
    harness
        .runtime
        .edit(|edit| {
            let mut table = edit.view::<Table>();
            let mut entries = table.entries();
            entries.insert(1, 21);
        })
        .expect("edit one entry");
    let view = view_key_of(&harness, "Table");
    let (status, body) = get(&harness, &format!("/api/v1/views/{view}/render"));
    assert_eq!(status, 200, "{body}");
    let render: serde_json::Value = serde_json::from_str(&body).expect("render json");
    let rows = render["fields"][0]["model"]["rows"]
        .as_array()
        .expect("rows");
    let row = |key: &str| {
        rows.iter()
            .find(|row| row["label"].as_str() == Some(key))
            .unwrap_or_else(|| panic!("row {key} in {body}"))
    };
    assert_eq!(row("1")["value"], "21");
    assert_eq!(row("1")["state"], "changed", "{body}");
    assert_eq!(row("0")["value"], "10");
    assert_eq!(row("0")["state"], "clean", "{body}");
}

/// A graph View draws its edges, not just its nodes: the structure unit carries
/// the topology, and a generic plate is allowed to read it.
#[test]
fn a_graph_view_draws_its_edges() {
    let harness = start();
    let view = view_key_of(&harness, "Net");
    let (status, body) = get(&harness, &format!("/api/v1/views/{view}/render"));
    assert_eq!(status, 200, "{body}");
    let render: serde_json::Value = serde_json::from_str(&body).expect("render json");
    let model = &render["fields"][0]["model"];
    assert_eq!(model["kind"], "graph", "{body}");
    assert_eq!(model["nodes"].as_array().map(Vec::len), Some(2), "{body}");
    let edges = model["edges"].as_array().expect("edges");
    assert_eq!(edges.len(), 1, "{body}");
    assert_eq!(edges[0]["from"], "1", "{body}");
    assert_eq!(edges[0]["to"], "2", "{body}");
    // The edge payload is spelled like any other View value: the generic plate
    // can label a wire without asking the host.
    assert_eq!(edges[0]["label"], "100", "{body}");
    assert_eq!(model["nodes"][0]["state"], "changed", "{body}");
}

#[test]
fn a_hidden_view_draws_identity_only() {
    let harness = start();
    let view = view_key_of(&harness, "Opaque");
    let (status, body) = get(&harness, &format!("/api/v1/views/{view}/render"));
    assert_eq!(status, 200);
    // The rows exist, but nothing spells the values.
    assert!(body.contains("\"total\":2"), "{body}");
    assert!(!body.contains("\"value\":\"9\""), "{body}");
}

#[test]
fn a_tree_view_names_its_variants() {
    let harness = start();
    let view = view_key_of(&harness, "Syntax");
    let (status, body) = get(&harness, &format!("/api/v1/views/{view}/render"));
    assert_eq!(status, 200);
    assert!(body.contains("\"kind\":\"tree\""), "{body}");
    // The declared variant name, not a numeric index.
    assert!(body.contains("\"kind\":\"Module\""), "{body}");
    assert!(body.contains("\"depth\":1"), "{body}");
}
