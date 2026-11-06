//! The HTTP surface: JSON endpoints, SSE frames, and the read-only promise.
//!
//! Every test starts a real server on an ephemeral port and speaks HTTP/1.1 to
//! it, because the page's contract is the wire, not a Rust API.

use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use reactive::{Runtime, RuntimeConfig, reactive, view};
use reactive_observe::{ListRow, Observatory, ObserveLevel, RenderModel, recorder};

#[view(source)]
pub struct Numbers {
    values: List<u32>,
}

#[view(derived)]
pub struct Doubled(List<u32>);

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
    runtime: Runtime,
    port: u16,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.observatory.shutdown();
    }
}

fn start(with_renderer: bool) -> Harness {
    let recorder = recorder(ObserveLevel::Nodes);
    let observer: Arc<dyn reactive_observe::RuntimeObserver> = Arc::clone(&recorder) as Arc<_>;
    let runtime = Runtime::with_observer(RuntimeConfig::default(), observer);
    runtime.install(analysis::Double).expect("install");
    runtime
        .edit(|edit| {
            let mut numbers = edit.view::<Numbers>();
            let mut values = numbers.values();
            values.push(1);
            values.push(2);
        })
        .expect("seed");
    let _ = runtime.read_outputs::<analysis::Double>();

    let mut builder = Observatory::builder(runtime.clone(), Arc::clone(&recorder))
        .bind("127.0.0.1:0")
        .history(16);
    if with_renderer {
        builder = builder.renderer::<Doubled>(|snapshot| {
            let items = snapshot.view::<Doubled>().field0();
            RenderModel::List(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| ListRow {
                        index,
                        value: value.to_string(),
                        unit: None,
                    })
                    .collect(),
            )
        });
    }
    let observatory = builder.start().expect("start");
    let port = observatory
        .url()
        .rsplit(':')
        .next()
        .expect("port")
        .parse()
        .expect("numeric port");
    Harness {
        observatory,
        runtime,
        port,
    }
}

fn get(harness: &Harness, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", harness.port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
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

fn post_body(harness: &Harness, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", harness.port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("write");
    let mut response = String::new();
    let _ = BufReader::new(stream).read_to_string(&mut response);
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

#[test]
fn health_and_meta_describe_the_runtime() {
    let harness = start(false);
    assert_eq!(get(&harness, "/healthz").0, 200);
    let (status, body) = get(&harness, "/api/v1/meta");
    assert_eq!(status, 200);
    assert!(body.contains("\"protocol\":1"), "{body}");
    assert!(body.contains("\"level\":\"nodes\""), "{body}");
}

#[test]
fn views_report_identity_and_granularity() {
    let harness = start(false);
    let (status, body) = get(&harness, "/api/v1/views");
    assert_eq!(status, 200);
    assert!(body.contains("\"granularity\":\"whole_value\""), "{body}");
    assert!(body.contains("\"elements\":2"), "{body}");
    assert!(body.contains("\"kind\":\"list\""), "{body}");
}

#[test]
fn a_view_without_a_host_renderer_is_drawn_from_the_facts() {
    let harness = start(false);
    let view = view_key_of(&harness, "Doubled");
    let (status, body) = get(&harness, &format!("/api/v1/views/{view}/render"));
    assert_eq!(status, 200);
    // No host renderer, but the runtime still knows the rows.
    assert!(body.contains("\"source\":\"generic\""), "{body}");
    assert!(body.contains("\"kind\":\"list\""), "{body}");
    assert!(body.contains("\"value\":\"2\""), "{body}");
}

#[test]
fn a_renderer_draws_bounded_rows() {
    let harness = start(true);
    let view = view_key_of(&harness, "Doubled");
    let (status, body) = get(&harness, &format!("/api/v1/views/{view}/render"));
    assert_eq!(status, 200);
    assert!(body.contains("\"source\":\"host_renderer\""), "{body}");
    assert!(body.contains("\"kind\":\"list\""), "{body}");
    assert!(body.contains("\"total\":2"), "{body}");
}

#[test]
fn reading_the_page_never_demands_computation() {
    let harness = start(true);
    let before = harness.runtime.stats();
    let view = view_key_of(&harness, "Doubled");
    assert_eq!(get(&harness, "/api/v1/workspace").0, 200);
    assert_eq!(get(&harness, "/api/v1/views").0, 200);
    assert_eq!(
        get(&harness, &format!("/api/v1/views/{view}/render")).0,
        200
    );
    assert_eq!(get(&harness, "/api/v1/revisions").0, 200);
    assert_eq!(get(&harness, "/api/v1/graph").0, 200);
    let after = harness.runtime.stats();
    assert_eq!(
        after.executions, before.executions,
        "inspection executed work"
    );
    assert_eq!(after.validations, before.validations);
    assert_eq!(after.demands, before.demands);
    assert_eq!(after.tasks.queued, before.tasks.queued);
}

#[test]
fn unknown_nodes_and_revisions_are_404() {
    let harness = start(false);
    assert_eq!(get(&harness, "/api/v1/nodes/n:9999@0").0, 404);
    assert_eq!(get(&harness, "/api/v1/revisions/9999").0, 404);
}

#[test]
fn a_view_text_channel_round_trips_through_http() {
    let recorder = recorder(ObserveLevel::Summary);
    let observer: Arc<dyn reactive_observe::RuntimeObserver> = Arc::clone(&recorder) as Arc<_>;
    let runtime = Runtime::with_observer(RuntimeConfig::default(), observer);
    runtime.install(analysis::Double).expect("install");
    runtime
        .edit(|edit| {
            let mut numbers = edit.view::<Numbers>();
            let mut values = numbers.values();
            values.push(1);
            values.push(2);
        })
        .expect("seed");
    let document = Arc::new(std::sync::Mutex::new(String::from("1\n2")));
    let stored = Arc::clone(&document);
    let harness_runtime = runtime.clone();
    let runtime_for_write = runtime.clone();
    let document_for_write = Arc::clone(&document);
    let observatory = Observatory::builder(runtime, recorder)
        .bind("127.0.0.1:0")
        .text_view::<Numbers>(
            move || stored.lock().expect("document").clone(),
            move |text: &str| {
                let values: Vec<u32> = text
                    .lines()
                    .map(|line| {
                        line.trim()
                            .parse::<u32>()
                            .map_err(|_| format!("`{line}` is not a number"))
                    })
                    .collect::<Result<_, _>>()?;
                if values.is_empty() {
                    return Err(String::from("empty list"));
                }
                let published = runtime_for_write.revision();
                runtime_for_write
                    .edit(|edit| {
                        let mut numbers = edit.view::<Numbers>();
                        let mut list = numbers.values();
                        for value in values {
                            list.push(value);
                        }
                    })
                    .map_err(|error| format!("{error}"))?;
                *document_for_write.lock().expect("document") = text.to_owned();
                let published = runtime_for_write.revision() > published;
                Ok(if published {
                    String::from("committed")
                } else {
                    String::from("no change")
                })
            },
        )
        .start()
        .expect("start");
    let port: u16 = observatory
        .url()
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let harness = Harness {
        observatory,
        runtime: harness_runtime,
        port,
    };
    let view = view_key_of(&harness, "Numbers");

    let (status, body) = get(&harness, &format!("/api/v1/views/{view}/text"));
    assert_eq!(status, 200);
    assert!(body.contains("1\\n2"), "{body}");

    let (status, body) = post_body(&harness, &format!("/api/v1/views/{view}/text"), "3\n4");
    assert_eq!(status, 200);
    assert!(body.contains("\"published\":true"), "{body}");

    let (status, body) = post_body(&harness, &format!("/api/v1/views/{view}/text"), "nope");
    assert_eq!(status, 400);
    assert!(body.contains("is not a number"), "{body}");

    // A rejected write keeps the previous document.
    let (_, body) = get(&harness, &format!("/api/v1/views/{view}/text"));
    assert!(body.contains("3\\n4"), "{body}");
}

#[test]
fn a_view_without_a_text_channel_says_404() {
    let harness = start(false);
    let view = view_key_of(&harness, "Numbers");
    assert_eq!(get(&harness, &format!("/api/v1/views/{view}/text")).0, 404);
    assert_eq!(
        post_body(&harness, &format!("/api/v1/views/{view}/text"), "anything").0,
        404
    );
}

#[test]
fn the_workspace_names_the_components_behind_each_link() {
    let harness = start(false);
    let (status, body) = get(&harness, "/api/v1/workspace");
    assert_eq!(status, 200);
    assert!(body.contains("\"components\":["), "{body}");
    // The scalar producer reads Numbers and defines Doubled.
    assert!(body.contains("analysis::double"), "{body}");
}

fn view_key_of(harness: &Harness, suffix: &str) -> String {
    let (_, body) = get(harness, "/api/v1/views");
    // View names are fully qualified, so the suffix is what a test can pin.
    let mut search = 0;
    while let Some(found) = body[search..].find("\"name\":\"") {
        let name_start = search + found + 8;
        let Some(name_end) = body[name_start..].find('"') else {
            break;
        };
        let name = &body[name_start..name_start + name_end];
        if name.ends_with(suffix) {
            let key_start = body[..search + found].rfind("\"view\":\"").expect("key") + 8;
            let key_end = body[key_start..].find('"').expect("closing quote") + key_start;
            return body[key_start..key_end].to_owned();
        }
        search = name_start + name_end;
    }
    panic!("missing {suffix}: {body}");
}

// --------------------------------------------------------- general components

/// A source scalar the general component watches.
#[view(source)]
pub struct Ticks {
    value: u64,
}

/// A minimal custom algorithm: it reads one unit and publishes an output.
struct Watcher;

impl reactive::GeneralComponent for Watcher {
    type State = u64;
    type Output = u64;

    const ID: reactive::ComponentId = reactive::ComponentId(0x00BB_0001);
    const NAME: &'static str = "server::Watcher";
    const VIEWS: &'static [&'static reactive::ViewDescriptor] =
        &[<Ticks as reactive::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut reactive::GeneralCx,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        let unit = reactive::UnitKey::field(
            <Ticks as reactive::ViewSchema>::DESCRIPTOR.id,
            reactive::FieldId(0),
            reactive::UnitKind::Value,
        );
        let value: u64 = cx.read_typed(unit)?;
        Ok((value, value * 2))
    }

    fn update(
        &self,
        cx: &mut reactive::GeneralCx,
        _state: &Self::State,
        changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        let mut value = 0;
        for change in changes.changes() {
            value = cx.read_typed(change.unit.clone())?;
        }
        Ok((value, value * 2))
    }
}

#[test]
fn a_general_component_is_a_first_class_node() {
    let recorder = recorder(ObserveLevel::Summary);
    let observer: Arc<dyn reactive_observe::RuntimeObserver> = Arc::clone(&recorder) as Arc<_>;
    let runtime = Runtime::with_observer(RuntimeConfig::default(), observer);
    runtime.install_general(Watcher).expect("install general");
    runtime
        .edit(|edit| {
            edit.view::<Ticks>().set_value(7);
        })
        .expect("seed");
    assert_eq!(
        runtime.read_outputs_general::<Watcher>().expect("outputs"),
        14
    );

    let observatory = Observatory::builder(runtime.clone(), recorder)
        .bind("127.0.0.1:0")
        .start()
        .expect("start");
    let port: u16 = observatory
        .url()
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let harness = Harness {
        observatory,
        runtime,
        port,
    };
    let (status, body) = get(&harness, "/api/v1/workspace");
    assert_eq!(status, 200);
    assert!(body.contains("\"name\":\"server::Watcher\""), "{body}");
    assert!(body.contains("\"kind\":\"general\""), "{body}");
    // It reads the source View, and it is listed whether or not it defines one.
    assert!(body.contains("\"reads\":[\"v:"), "{body}");
}
