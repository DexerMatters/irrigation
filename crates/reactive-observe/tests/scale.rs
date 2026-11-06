//! Scale: a large runtime must stay bounded on the wire and exact in its totals.
//!
//! G10 asks for a 50k-node runtime capped with exact totals and a working scoped
//! drill-down. This test builds that runtime and reads only through HTTP.

use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use reactive::{Runtime, RuntimeConfig, reactive, view};
use reactive_observe::{ListRow, Observatory, ObserveLevel, RenderModel, ValuePreview, recorder};

const ELEMENTS: usize = 50_000;

#[view(source)]
pub struct Numbers {
    values: List<u64>,
}

#[view(derived)]
pub struct Doubled(List<u64>);

#[reactive]
pub mod analysis {
    use super::{Doubled, Numbers};

    #[component]
    pub fn double(numbers: &Numbers) -> Doubled {
        Doubled::from(
            numbers
                .values()
                .to_vec()
                .iter()
                .map(|value| value * 2)
                .collect::<Vec<_>>(),
        )
    }
}

struct Harness {
    observatory: Observatory,
    port: u16,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.observatory.shutdown();
    }
}

fn body_of(harness: &Harness, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", harness.port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("write");
    let mut response = String::new();
    BufReader::new(stream)
        .read_to_string(&mut response)
        .expect("read");
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

fn start() -> Harness {
    let recorder = recorder(ObserveLevel::Summary);
    let observer: Arc<dyn reactive_observe::RuntimeObserver> = Arc::clone(&recorder) as Arc<_>;
    let runtime = Runtime::with_observer(RuntimeConfig::default(), observer);
    runtime.install(analysis::Double).expect("install");
    runtime
        .edit(|edit| {
            let mut numbers = edit.view::<Numbers>();
            let mut values = numbers.values();
            for value in 0..ELEMENTS as u64 {
                values.push(value);
            }
        })
        .expect("seed");
    let _ = runtime.read_outputs::<analysis::Double>().expect("read");

    let observatory = Observatory::builder(runtime, recorder)
        .bind("127.0.0.1:0")
        // A renderer that ignores every cap: the server must still bound it.
        .renderer::<Doubled>(|snapshot| {
            RenderModel::List(
                snapshot
                    .view::<Doubled>()
                    .field0()
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| ListRow {
                        index,
                        value: value.to_string(),
                        unit: None,
                    })
                    .collect(),
            )
        })
        .formatter::<Doubled>(|snapshot| ValuePreview {
            kind: String::from("List<u64>"),
            summary: format!(
                "{} doubled values",
                snapshot.view::<Doubled>().field0().len()
            ),
            detail: None,
            bytes: 0,
            truncated: false,
        })
        .start()
        .expect("start");
    let port = observatory
        .url()
        .rsplit(':')
        .next()
        .expect("port")
        .parse()
        .expect("port");
    Harness { observatory, port }
}

fn view_key_of(harness: &Harness, suffix: &str) -> String {
    let (_, body) = body_of(harness, "/api/v1/views");
    let marker = format!("\"name\":\"{suffix}\"");
    assert!(body.contains(&marker), "missing {suffix}: {body}");
    // The view key appears before the name in every ViewWire.
    let end = body.find(&marker).expect("marker");
    let start = body[..end].rfind("\"view\":\"").expect("key") + 8;
    body[start..end - 2].to_owned()
}

#[test]
fn a_fifty_thousand_node_graph_is_capped_with_an_exact_total() {
    let harness = start();
    let (status, body) = body_of(&harness, "/api/v1/graph");
    assert_eq!(status, 200);
    assert!(
        body.contains("\"truncated\":true"),
        "graph must admit the cap"
    );
    let total: usize = field(&body, "\"total_nodes\":");
    assert!(
        total >= ELEMENTS,
        "the total must count every node, got {total}"
    );
    let rendered = body.matches("\"key\":\"n:").count();
    assert!(
        rendered <= 800,
        "the graph must stay bounded, drew {rendered} nodes"
    );
}

#[test]
fn a_scoped_graph_query_is_exactly_the_scope() {
    let harness = start();

    // The source View owns one unit per element, so its scope is the big one.
    let numbers = view_key_of(&harness, "scale::Numbers");
    let (status, body) = body_of(&harness, &format!("/api/v1/graph?view={numbers}"));
    assert_eq!(status, 200);
    let scoped: usize = field(&body, "\"total_nodes\":");
    assert!(
        scoped >= ELEMENTS,
        "the scoped total must count the View's units, got {scoped}"
    );
    assert!(
        body.contains("\"truncated\":true"),
        "a big scope is still capped"
    );

    // The derived View is small, so its scope is complete and not truncated.
    let doubled = view_key_of(&harness, "scale::Doubled");
    let (status, body) = body_of(&harness, &format!("/api/v1/graph?view={doubled}"));
    assert_eq!(status, 200);
    let derived: usize = field(&body, "\"total_nodes\":");
    assert!(
        derived > 0 && derived < 20,
        "the derived scope should be a handful of nodes, got {derived}"
    );
    assert!(
        body.contains("\"truncated\":false"),
        "a small scope must say it is complete"
    );
}

#[test]
fn a_render_that_ignores_caps_is_cut_and_reports_the_real_total() {
    let harness = start();
    let doubled = view_key_of(&harness, "scale::Doubled");
    let (status, body) = body_of(&harness, &format!("/api/v1/views/{doubled}/render"));
    assert_eq!(status, 200);
    assert!(body.contains("\"truncated\":true"), "{body:.200}");
    let total: usize = field(&body, "\"total\":");
    assert_eq!(
        total, ELEMENTS,
        "the total must not be the page's row count"
    );
    let rows = body.matches("\"index\":").count();
    assert!(rows <= 200, "the default row cap is 200, drew {rows}");
}

#[test]
fn the_workspace_payload_does_not_grow_with_the_runtime() {
    let harness = start();
    let (status, body) = body_of(&harness, "/api/v1/workspace");
    assert_eq!(status, 200);
    assert!(
        body.len() < 8_000,
        "the headers must stay small, got {} bytes",
        body.len()
    );
    assert!(!body.contains("\"rows\""), "workspace frames carry no rows");
}

fn field(body: &str, key: &str) -> usize {
    let start = body.find(key).unwrap_or_else(|| panic!("missing {key}")) + key.len();
    let rest = &body[start..];
    let end = rest
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().unwrap_or(0)
}
