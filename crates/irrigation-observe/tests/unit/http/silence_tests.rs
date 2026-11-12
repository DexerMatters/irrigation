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

/// A client that connects and says nothing is dropped, so its thread is reclaimed.
///
/// This stands up the real server rather than asserting that `set_read_timeout` was called: the
/// call succeeding says nothing about whether a read ever respects it.
#[test]
fn a_silent_connection_is_closed() {
    let server = serve(
        test_collector(),
        HttpConfig {
            bind: String::from("127.0.0.1:0"),
            ..HttpConfig::default()
        },
    )
    .expect("bind");
    let mut stream = std::net::TcpStream::connect(server.addr()).expect("connect");
    stream
        .set_read_timeout(Some(READ_TIMEOUT * 3))
        .expect("client timeout");
    let mut buffer = [0u8; 1];
    // Nothing is written: the server's own timeout closes the connection and the read sees EOF.
    match stream.read(&mut buffer) {
        Ok(0) => {}
        Ok(read) => panic!("the server answered a silent connection with {read} bytes"),
        Err(error) => panic!("the connection was not closed by the server: {error}"),
    }
}
