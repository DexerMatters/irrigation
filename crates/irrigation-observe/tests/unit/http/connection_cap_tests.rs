//! Unit tests for `src/http.rs`.

use super::*;
use std::io::Read;

fn test_collector() -> Arc<Collector> {
    let recorder = crate::recorder::recorder(irrigation_runtime::observe::ObserveLevel::Nodes);
    let runtime = irrigation_runtime::Runtime::with_observer(
        irrigation_runtime::RuntimeConfig::default(),
        recorder.clone(),
    );
    Collector::new(runtime, recorder, 8)
}

/// A connection past the cap is refused rather than given a thread.
///
/// Every held socket in this test is silent, so each owns a thread until the read timeout; the
/// point is that the peer past the cap is answered *without* one.
#[test]
fn a_connection_past_the_cap_is_refused() {
    let server = serve(
        test_collector(),
        HttpConfig {
            bind: String::from("127.0.0.1:0"),
            ..HttpConfig::default()
        },
    )
    .expect("bind");

    // Fill the cap with silent peers, then ask once more and read the refusal.
    let held: Vec<std::net::TcpStream> = (0..MAX_CONNECTIONS)
        .map(|_| std::net::TcpStream::connect(server.addr()).expect("connect"))
        .collect();
    // The accept loop needs a moment to accept them all; the cap is what makes the next one fail.
    std::thread::sleep(Duration::from_millis(500));

    let mut extra = std::net::TcpStream::connect(server.addr()).expect("connect");
    extra
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let mut response = String::new();
    extra.read_to_string(&mut response).expect("read");
    assert!(
        response.starts_with("HTTP/1.1 503"),
        "the peer past the cap is refused: {response:?}"
    );
    drop(held);
}
