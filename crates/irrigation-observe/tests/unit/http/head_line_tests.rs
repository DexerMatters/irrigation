//! Unit tests for `src/http.rs`.

use super::*;
use std::io::Cursor;

#[test]
fn a_short_line_is_returned_whole() {
    let mut reader = Cursor::new(b"GET /x HTTP/1.1\r\n".to_vec());
    let mut line = String::new();
    let read = read_head_line(&mut reader, &mut line).expect("read");
    assert_eq!(read, Some(17), "the line including its newline");
    assert!(line.starts_with("GET /x"));
}

#[test]
fn the_end_of_the_stream_reads_as_zero() {
    let mut reader = Cursor::new(Vec::new());
    let mut line = String::new();
    assert_eq!(
        read_head_line(&mut reader, &mut line).expect("read"),
        Some(0)
    );
}

/// A client that never sends a newline cannot make the server allocate past the cap.
#[test]
fn a_line_longer_than_the_cap_is_refused() {
    let mut reader = Cursor::new(vec![b'a'; MAX_HEAD_LINE * 2]);
    let mut line = String::new();
    assert_eq!(
        read_head_line(&mut reader, &mut line).expect("read"),
        None,
        "an unterminated over-long line is not read to its end"
    );
    assert!(
        line.is_empty(),
        "and nothing partial is handed to the caller: {line:?}"
    );
}
