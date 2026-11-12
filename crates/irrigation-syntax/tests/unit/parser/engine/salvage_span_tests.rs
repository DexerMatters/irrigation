//! Unit tests for `src/parser/engine.rs`.

use super::*;

fn token(id: u64, text: &TextSnapshot, start: usize, end: usize) -> ParseToken {
    ParseToken {
        id: TokenId(id),
        kind: TokenKindId(0),
        text: Arc::from("x"),
        range: TextRange::new(
            TextAnchor::new(text.version(), start),
            TextAnchor::new(text.version(), end),
        ),
        flags: TokenFlags::default(),
    }
}

/// The failure diagnostic marks the region that could not be structured. It used
/// to be an empty range at the end of the document, so an unparsable prefix
/// reported its error past the end of its own text.
#[test]
fn a_failed_parse_marks_the_region_it_could_not_structure() {
    let text = TextSnapshot::new("xx");
    let tokens = [token(0, &text, 0, 1), token(1, &text, 1, 2)];
    let (_root, _build, diagnostics) = salvage_result(&tokens, &text);
    let span = diagnostics[0].range;
    assert_eq!(
        (span.start.offset, span.end.offset),
        (0, 2),
        "the diagnostic covers the salvaged tokens"
    );
}

/// An empty stream has no region to mark, so it still points at the end.
#[test]
fn an_empty_stream_still_points_at_the_end() {
    let text = TextSnapshot::new("");
    let (_root, _build, diagnostics) = salvage_result(&[], &text);
    let span = diagnostics[0].range;
    assert_eq!(span.start.offset, span.end.offset);
}
