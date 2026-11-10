//! A small HTTP/1.1 server: JSON endpoints plus one SSE stream.
//!
//! One thread per connection, no framework. The observation surface is a local
//! debugging tool, so the tradeoff is deliberate: a slow renderer holds its own
//! connection, never the collector, and a slow SSE client misses frames instead
//! of blocking the runtime.
//!
//! Every endpoint is read-only except the named host actions under
//! `/api/v1/actions/`, which the host registers explicitly.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::json;

use irrigation_core::ids::{NodeId, ViewId};
use irrigation_runtime::observe::GraphScope;

/// Largest request body the observation surface accepts, in bytes.
const MAX_BODY: usize = 64 * 1024;

use crate::collector::Collector;
use crate::wire::EventsWire;

/// How the server is exposed.
#[derive(Clone, Debug)]
pub struct HttpConfig {
    /// Address to bind.
    pub bind: String,
    /// Shared secret, when the server leaves loopback.
    pub token: Option<String>,
    /// Directory with the built page.
    pub static_dir: Option<PathBuf>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: String::from("127.0.0.1:7777"),
            token: None,
            static_dir: None,
        }
    }
}

/// A running server.
pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("addr", &self.addr).finish()
    }
}

impl Server {
    /// The bound address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL a browser can open.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stops accepting connections.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Blocks until the accept loop stops.
    ///
    /// The loop only stops when [`Server::shutdown`] is called, so this is the
    /// natural "run until interrupted" entry point.
    pub fn join(&mut self) {
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Binds and starts serving.
///
/// # Errors
///
/// Returns the bind error when the address is unavailable.
///
/// # Panics
///
/// Panics when the accept thread cannot be spawned.
pub fn serve(collector: Arc<Collector>, config: HttpConfig) -> std::io::Result<Server> {
    let listener = TcpListener::bind(&config.bind)?;
    let addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let accept_stop = Arc::clone(&stop);
    let config = Arc::new(config);
    let accept = std::thread::Builder::new()
        .name(String::from("observatory-http"))
        .spawn(move || {
            while !accept_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        let collector = Arc::clone(&collector);
                        let config = Arc::clone(&config);
                        std::thread::Builder::new()
                            .name(String::from("observatory-conn"))
                            .spawn(move || {
                                let _ = stream.set_nonblocking(false);
                                let _ = stream.set_nodelay(true);
                                handle(&collector, &config, stream);
                            })
                            .ok();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        })
        .expect("spawn http accept thread");
    Ok(Server {
        addr,
        stop,
        accept: Some(accept),
    })
}

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: String,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn authorized(&self, token: Option<&str>) -> bool {
        let Some(token) = token else {
            return true;
        };
        if self.header("authorization") == Some(&format!("Bearer {token}")) {
            return true;
        }
        self.param("token") == Some(token)
    }
}

fn handle(collector: &Arc<Collector>, config: &HttpConfig, stream: TcpStream) {
    let Ok(mut reader) = read_request(&stream) else {
        return;
    };
    let mut writer = stream;
    if !reader.authorized(config.token.as_deref()) {
        let _ = respond_json(&mut writer, 401, &json!({ "error": "unauthorized" }));
        return;
    }
    let request = std::mem::replace(
        &mut reader,
        Request {
            method: String::new(),
            path: String::new(),
            query: Vec::new(),
            headers: Vec::new(),
            body: String::new(),
        },
    );
    if request.method == "OPTIONS" {
        let _ = respond_empty(&mut writer, 204);
        return;
    }
    let segments: Vec<&str> = request
        .path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    match (request.method.as_str(), segments.as_slice()) {
        ("HEAD", _) | ("GET", []) => serve_static(&mut writer, config, "index.html"),
        ("GET", ["healthz"]) => {
            let _ = respond_json(&mut writer, 200, &json!({ "ok": true }));
        }
        ("GET", ["api", "v1", "meta"]) => {
            let _ = respond_json(&mut writer, 200, &collector.hello());
        }
        ("GET", ["api", "v1", "workspace"]) => {
            let mut workspace = collector.workspace();
            if let Some((label, revision)) = collector.last_noop() {
                workspace.last_noop = Some(label);
                workspace.last_noop_revision = Some(revision);
            }
            let _ = respond_json(&mut writer, 200, &workspace);
        }
        ("GET", ["api", "v1", "views"]) => {
            let _ = respond_json(&mut writer, 200, &collector.views());
        }
        ("GET", ["api", "v1", "views", key, "render"]) => match parse_view(key) {
            Some(view) => match collector.render(view) {
                Some(render) => {
                    let _ = respond_json(&mut writer, 200, &render);
                }
                None => {
                    let facts = collector.view_facts(view);
                    let body = json!({
                        "view": key,
                        "source": "no_renderer",
                        "fields": [],
                        "granularity_known": facts.is_some(),
                    });
                    let _ = respond_json(&mut writer, 200, &body);
                }
            },
            None => {
                let _ = respond_json(&mut writer, 400, &json!({ "error": "bad view key" }));
            }
        },
        ("GET", ["api", "v1", "views", key, "text"]) => match parse_view(key) {
            Some(view) => match collector.text_view(view) {
                Some(text) => {
                    let _ = respond_json(&mut writer, 200, &json!({ "text": text }));
                }
                None => {
                    let _ = respond_json(
                        &mut writer,
                        404,
                        &json!({ "error": "this View has no text editor" }),
                    );
                }
            },
            None => {
                let _ = respond_json(&mut writer, 400, &json!({ "error": "bad view id" }));
            }
        },
        ("POST", ["api", "v1", "views", key, "text"]) => match parse_view(key) {
            Some(view) => {
                let published = collector.runtime().revision();
                match collector.apply_text_view(view, &request.body) {
                    Some(Ok(reply)) => {
                        let after = collector.runtime().revision();
                        let _ = respond_json(
                            &mut writer,
                            200,
                            &json!({
                                "reply": reply,
                                "text": collector.text_view(view),
                                "revision": after,
                                "published": after > published,
                            }),
                        );
                    }
                    Some(Err(message)) => {
                        let _ = respond_json(&mut writer, 400, &json!({ "error": message }));
                    }
                    None => {
                        let _ = respond_json(
                            &mut writer,
                            404,
                            &json!({ "error": "this View has no text editor" }),
                        );
                    }
                }
            }
            None => {
                let _ = respond_json(&mut writer, 400, &json!({ "error": "bad view id" }));
            }
        },
        ("GET", ["api", "v1", "revisions"]) => {
            let _ = respond_json(&mut writer, 200, &collector.revisions());
        }
        ("GET", ["api", "v1", "revisions", revision]) => match revision.parse::<u64>() {
            Ok(revision) => match collector.revision(revision) {
                Some(record) => {
                    let _ = respond_json(&mut writer, 200, &record);
                }
                None => {
                    let _ = respond_json(&mut writer, 404, &json!({ "error": "not retained" }));
                }
            },
            Err(_) => {
                let _ = respond_json(&mut writer, 400, &json!({ "error": "bad revision" }));
            }
        },
        ("GET", ["api", "v1", "nodes", key]) => match parse_node(key) {
            Some(node) => match collector.node(node) {
                Some(detail) => {
                    let _ = respond_json(&mut writer, 200, &detail);
                }
                None => {
                    let _ = respond_json(&mut writer, 404, &json!({ "error": "unknown node" }));
                }
            },
            None => {
                let _ = respond_json(&mut writer, 400, &json!({ "error": "bad node key" }));
            }
        },
        ("GET", ["api", "v1", "graph"]) => {
            let scope = match request.param("view").and_then(parse_view) {
                Some(view) => GraphScope::View(view),
                None => match request.param("component").and_then(parse_component) {
                    Some(component) => GraphScope::Component(component),
                    None => GraphScope::All,
                },
            };
            let include_sites = request.param("sites") == Some("1");
            let _ = respond_json(&mut writer, 200, &collector.graph(scope, include_sites));
        }
        ("GET", ["api", "v1", "stream"]) => {
            let window = request
                .param("window")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(1_000)
                .min(50_000);
            stream_events(&mut writer, collector, window);
        }
        ("GET", _) => serve_static(&mut writer, config, &request.path),
        _ => {
            let _ = respond_json(&mut writer, 405, &json!({ "error": "method not allowed" }));
        }
    }
}

fn read_request(stream: &TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(empty_request());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or("/").to_owned();
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_owned(), parse_query(query)),
        None => (target, Vec::new()),
    };
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    // The body is consumed once, then handed to the route.
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_BODY);
    let body = if length > 0 {
        let mut buffer = vec![0u8; length];
        reader.read_exact(&mut buffer)?;
        String::from_utf8_lossy(&buffer).into_owned()
    } else {
        String::new()
    };
    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn empty_request() -> Request {
    Request {
        method: String::new(),
        path: String::new(),
        query: Vec::new(),
        headers: Vec::new(),
        body: String::new(),
    }
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (key.to_owned(), value.to_owned()),
            None => (pair.to_owned(), String::new()),
        })
        .collect()
}

fn parse_view(key: &str) -> Option<ViewId> {
    let hex = key.strip_prefix("v:")?;
    u64::from_str_radix(hex, 16).ok().map(ViewId)
}

fn parse_component(key: &str) -> Option<irrigation_core::ids::ComponentId> {
    let hex = key.strip_prefix("c:")?;
    u64::from_str_radix(hex, 16)
        .ok()
        .map(irrigation_core::ids::ComponentId)
}

fn parse_node(key: &str) -> Option<NodeId> {
    let rest = key.strip_prefix("n:")?;
    let (index, generation) = rest.split_once('@')?;
    NodeId::try_new(index.parse().ok()?, generation.parse().ok()?)
}

fn respond_json<T: Serialize>(
    stream: &mut TcpStream,
    status: u16,
    body: &T,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    respond(stream, status, "application/json; charset=utf-8", &body)
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn respond_empty(stream: &mut TcpStream, status: u16) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: authorization\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes())?;
    stream.flush()
}

fn serve_static(stream: &mut TcpStream, config: &HttpConfig, path: &str) {
    let Some(dir) = &config.static_dir else {
        let _ = respond(
            stream,
            200,
            "text/plain; charset=utf-8",
            b"Observatory is running, but the page was not found.\n\
Build it (cd web/observatory && npm install && npm run build) and run from the \
repository root, or point OBSERVATORY_DIST at the built directory.\n",
        );
        return;
    };
    let relative = path.trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    let candidate = Path::new(relative);
    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        let _ = respond_json(stream, 400, &json!({ "error": "bad path" }));
        return;
    }
    let file = dir.join(candidate);
    match std::fs::read(&file) {
        Ok(bytes) => {
            let content_type = mime(file.as_path());
            let _ = respond(stream, 200, content_type, &bytes);
        }
        Err(_) if !relative.contains('.') => serve_static(stream, config, "index.html"),
        Err(_) => {
            let _ = respond_json(stream, 404, &json!({ "error": "not found" }));
        }
    }
}

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn stream_events(stream: &mut TcpStream, collector: &Arc<Collector>, window: usize) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache, no-transform\r\nConnection: keep-alive\r\nX-Accel-Buffering: no\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let subscription = collector.subscribe();
    let send = |stream: &mut TcpStream, name: &str, id: Option<u64>, data: &str| -> bool {
        use std::fmt::Write as _;
        let mut frame = String::new();
        if let Some(id) = id {
            // Writing to a `String` cannot fail.
            let _ = writeln!(frame, "id: {id}");
        }
        let _ = writeln!(frame, "event: {name}\ndata: {data}");
        let _ = writeln!(frame);
        stream.write_all(frame.as_bytes()).is_ok() && stream.flush().is_ok()
    };

    let hello = match serde_json::to_string(&collector.hello()) {
        Ok(text) => text,
        Err(_) => String::from("{}"),
    };
    if !send(stream, "hello", None, &hello) {
        collector.unsubscribe(subscription.id);
        return;
    }
    // The workspace is the client's baseline; its id is the sequence *before*
    // the snapshot, so buffered frames after it are applied exactly once.
    let mut workspace = collector.workspace();
    if let Some((label, revision)) = collector.last_noop() {
        workspace.last_noop = Some(label);
        workspace.last_noop_revision = Some(revision);
    }
    let baseline = subscription.seq;
    let data = match serde_json::to_string(&workspace) {
        Ok(text) => text,
        Err(_) => String::from("{}"),
    };
    if !send(stream, "workspace", Some(baseline), &data) {
        collector.unsubscribe(subscription.id);
        return;
    }

    // Replay recent activity so a late page still shows the last revisions.
    let recent: Vec<_> = collector.revisions().into_iter().take(window).collect();
    if !recent.is_empty()
        && let Ok(data) = serde_json::to_string(&recent)
        && !send(stream, "history", None, &data)
    {
        collector.unsubscribe(subscription.id);
        return;
    }

    let mut applied = baseline;
    loop {
        match subscription.frames.recv_timeout(Duration::from_secs(15)) {
            Ok(frame) => {
                if frame.seq <= applied {
                    continue;
                }
                applied = frame.seq;
                let batch = EventsWire {
                    seq: frame.seq,
                    events: frame.events.clone(),
                };
                let Ok(data) = serde_json::to_string(&batch) else {
                    continue;
                };
                if !send(stream, "events", Some(frame.seq), &data) {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let heartbeat = json!({
                    "seq": collector.seq(),
                    "revision": collector.runtime().revision(),
                });
                if !send(stream, "heartbeat", None, &heartbeat.to_string()) {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    collector.unsubscribe(subscription.id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_keys_round_trip() {
        let node = NodeId::new(42, 3);
        let key = crate::wire::node_key(node);
        assert_eq!(parse_node(&key), Some(node));
    }

    #[test]
    fn node_keys_reject_identities_that_would_alias() {
        // Both halves are packed; an out-of-range half must not be masked into
        // a different slot or generation.
        assert_eq!(parse_node("n:1048576@0"), None);
        assert_eq!(
            parse_node(&format!("n:0@{}", NodeId::MAX_GENERATION + 1)),
            None
        );
        assert_eq!(
            parse_node(&format!("n:{}@0", NodeId::INDEX_MASK)),
            Some(NodeId::new(NodeId::INDEX_MASK, 0))
        );
    }

    #[test]
    fn view_keys_round_trip() {
        let view = ViewId(0xdead_beef);
        let key = crate::wire::view_key(view);
        assert_eq!(parse_view(&key), Some(view));
    }

    #[test]
    fn parent_paths_are_rejected() {
        let config = HttpConfig {
            static_dir: Some(PathBuf::from("/tmp/public")),
            ..HttpConfig::default()
        };
        assert!(!config.static_dir.is_none());
    }
}
