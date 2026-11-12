//! Unit tests for `src/text.rs`.

use super::{TextAnchor, TextDocument, TextEdit, TextRange, TextVersion};

#[test]
fn anchors_resolve_forward_across_edits() {
    let doc = TextDocument::new("hello world");
    let at_hello_end = doc.snapshot().anchor(5).unwrap();
    let doc = doc.replace(0..5, "hi").unwrap();
    assert_eq!(doc.snapshot().resolve(at_hello_end), Some(2));
    let version_zero = doc.snapshot().anchor(2).unwrap();
    let doc = doc.replace(0..0, "> ").unwrap();
    assert_eq!(doc.snapshot().resolve(version_zero), Some(4));
}

#[test]
fn a_whole_buffer_edit_becomes_the_one_edit_that_separates_it() {
    let doc = TextDocument::new("{\"rows\": 64}");
    let next = doc.replace_with("{\"rows\": 640}").expect("one edit");
    assert_eq!(next.text(), "{\"rows\": 640}");
    assert_eq!(next.snapshot().edits().len(), 1);
    let edit = next.snapshot().edits().first().expect("an edit");
    // The minimal diff is the one keystroke, not the whole number.
    assert_eq!(edit.range, 11..11);
    assert_eq!(&*edit.replacement, "0");
    assert_eq!(
        next.snapshot()
            .parent()
            .map(|parent| parent.text().to_owned()),
        Some(String::from("{\"rows\": 64}"))
    );
}

#[test]
fn an_unchanged_buffer_is_not_an_edit() {
    let doc = TextDocument::new("{}");
    assert!(doc.replace_with("{}").is_none());
}

#[test]
fn a_buffer_edit_never_splits_a_character() {
    let doc = TextDocument::new("naive: é");
    let next = doc.replace_with("naive: è").expect("one edit");
    assert_eq!(next.text(), "naive: è");
    assert_eq!(next.snapshot().edits().len(), 1);
}

#[test]
fn anchors_inside_replaced_range_do_not_resolve() {
    let doc = TextDocument::new("abcdef");
    let inside = TextAnchor::new(TextVersion(0), 3);
    let doc = doc.replace(1..5, "XYZ").unwrap();
    assert_eq!(doc.snapshot().resolve(inside), None);
    assert_eq!(
        doc.snapshot().resolve(TextAnchor::new(TextVersion(0), 1)),
        Some(1)
    );
    assert_eq!(
        doc.snapshot().resolve(TextAnchor::new(TextVersion(0), 5)),
        Some(4)
    );
}

#[test]
fn multiple_edits_are_sorted_and_resolved() {
    let doc = TextDocument::new("0123456789");
    let old = doc.snapshot().clone();
    let doc = doc
        .edit([TextEdit::new(7..9, "AB"), TextEdit::new(1..3, "x")])
        .unwrap();
    assert_eq!(doc.text(), "0x3456AB9");
    let r = TextRange::new(
        TextAnchor::new(old.version(), 4),
        TextAnchor::new(old.version(), 6),
    );
    assert_eq!(doc.snapshot().resolve_range(r), Some(3..5));
}

#[test]
fn edits_since_direct_parent_is_exact() {
    let doc = TextDocument::new("abc");
    let doc = doc
        .edit([TextEdit::new(1..2, "X"), TextEdit::new(2..2, "!")])
        .unwrap();
    let edits = doc.edits_since(TextVersion(0)).unwrap();
    assert_eq!(edits.len(), 2);
    assert_eq!(doc.text(), "aX!c");
}

#[test]
fn edits_since_long_chain_is_a_coarse_replacement() {
    let doc = TextDocument::new("hello world");
    let doc = doc.replace(0..5, "hi").unwrap();
    let doc = doc.replace(2..2, "!").unwrap();
    let edits = doc.edits_since(TextVersion(0)).unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(doc.text(), "hi! world");
}

#[test]
fn overlapping_edits_are_rejected() {
    let doc = TextDocument::new("abcdef");
    assert!(
        doc.edit([TextEdit::new(1..4, "x"), TextEdit::new(3..5, "y")])
            .is_err()
    );
}

#[test]
fn line_and_column_are_generic() {
    let doc = TextDocument::new("a\nbc\n");
    assert_eq!(doc.line_column(0), Some((0, 0)));
    assert_eq!(doc.line_column(2), Some((1, 0)));
    assert_eq!(doc.line_column(4), Some((1, 2)));
}
