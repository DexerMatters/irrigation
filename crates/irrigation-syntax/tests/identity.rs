//! Identity is the occurrence, not the spelling.
//!
//! The syntax layer decides the identity every later entity inherits: the parser
//! keys a node by the tokens it covers, and every component above it keys its
//! own emissions by the node ids it was given. A re-spelling must therefore be a
//! payload change with the same identity. Comparing lexemes instead re-minted
//! the token, re-keyed every extent that covered it, and turned a one-character
//! edit into a new subtree.
//!
//! The gates below pin both directions of the contract: an in-place edit keeps
//! the token and the whole AST; a structural edit keeps every occurrence that
//! survived and re-keys only what changed.

mod support;

use std::collections::BTreeSet;

use irrigation_syntax::{
    AstMeta, AstNodeRef, AstTree, AstValue, LexState, Lexer, LexerSpec, TextDocument, Tokens,
};

/// A lexer for the identity gates: words, numbers, and JSON punctuation.
fn lexer() -> Lexer {
    let spec = LexerSpec::parse(
        r#"
        grammar Probe {
            lexer {
                initial root;
                state root {
                    skip ws = /[ \t\r\n]+/;
                    token string = /"([^"\\]|\\.)*"/;
                    token number = /-?(0|[1-9][0-9]*)/;
                    token ident = /[A-Za-z_][A-Za-z0-9_]*/ @word;
                    token "{" ; token "}" ; token "[" ; token "]" ; token ":" ; token "," ;
                }
            }
        }
        "#,
    )
    .expect("valid lexer spec");
    Lexer::new(spec)
}

/// Lex `text`, then apply `edit` incrementally and return both token streams.
fn respell(
    lexer: &Lexer,
    text: &str,
    edit: irrigation_syntax::TextEdit,
) -> (irrigation_syntax::LexOutput, irrigation_syntax::LexOutput) {
    let document = TextDocument::new(text);
    let before = lexer.lex(&document);
    let edited = document.edit([edit]).expect("the edit is in the document");
    let after = lexer.lex_incremental(&LexState::from_output(&before), &edited);
    (before, after)
}

fn ids(output: &irrigation_syntax::LexOutput) -> Vec<u64> {
    output.tokens().iter().map(|token| token.id.0).collect()
}

fn texts(output: &irrigation_syntax::LexOutput) -> Vec<String> {
    output
        .tokens()
        .iter()
        .map(|token| token.text.to_string())
        .collect()
}

#[test]
fn a_respelled_token_keeps_its_identity() {
    let lexer = lexer();
    let text = r#"{"a":1}"#;
    let (before, after) = respell(&lexer, text, irrigation_syntax::TextEdit::new(5..6, "9"));

    assert_eq!(
        ids(&after),
        ids(&before),
        "re-spelling a token must not re-identify the stream"
    );
    assert_eq!(
        texts(&after),
        vec!["{", r#""a""#, ":", "9", "}"],
        "the payload is the only thing that changed"
    );
}

#[test]
fn a_token_that_grows_in_place_keeps_its_identity() {
    let lexer = lexer();
    let text = r#"{"a":1,"b":2}"#;
    let (before, after) = respell(&lexer, text, irrigation_syntax::TextEdit::new(5..6, "1234"));

    assert_eq!(
        ids(&after),
        ids(&before),
        "an in-place edit of any length is the same occurrence"
    );
    assert_eq!(texts(&after)[3], "1234");
}

#[test]
fn a_kind_change_replaces_the_identity() {
    let lexer = lexer();
    let text = r#"{"a":1}"#;
    let (before, after) = respell(&lexer, text, irrigation_syntax::TextEdit::new(5..6, "x"));

    let ids_before = ids(&before);
    let ids_after = ids(&after);
    assert_eq!(ids_before.len(), ids_after.len());
    let replaced: Vec<usize> = (0..ids_after.len())
        .filter(|index| ids_after[*index] != ids_before[*index])
        .collect();
    assert_eq!(
        replaced,
        vec![3],
        "a different terminal is a different occurrence: {ids_before:?} then {ids_after:?}"
    );
}

#[test]
fn a_collapsed_start_keeps_the_surviving_occurrence() {
    let lexer = lexer();
    let text = r#"{"a":1,"b":2}"#;
    let (before, after) = respell(&lexer, text, irrigation_syntax::TextEdit::new(6..12, ""));

    // `,"b":2` is removed. The closing brace moves onto the start the comma had,
    // so two old tokens map to one position; the kind selects the survivor.
    let ids_before = ids(&before);
    let ids_after = ids(&after);
    assert_eq!(ids_after.len(), ids_before.len() - 4);
    assert_eq!(
        ids_after.last(),
        ids_before.last(),
        "the brace survives at a collapsed start"
    );
    assert_eq!(
        &ids_after[..4],
        &ids_before[..4],
        "everything before the deletion keeps its identity"
    );
}

/// One significant token identity per document order.
fn session_token_ids(session: &support::Session) -> Vec<u64> {
    session
        .runtime
        .snapshot()
        .view::<Tokens>()
        .order()
        .into_iter()
        .map(|id| id.0)
        .collect()
}

/// Every AST node identity, by walking the published tree.
fn ast_ids(session: &support::Session) -> BTreeSet<u64> {
    fn walk(node: &AstNodeRef, out: &mut BTreeSet<u64>) {
        if !out.insert(node.raw()) {
            return;
        }
        for child in node.children() {
            walk(&child, out);
        }
        for child in node.recovered() {
            walk(&child, out);
        }
    }

    let snapshot = session.runtime.snapshot();
    let mut out = BTreeSet::new();
    if let Some(root) = snapshot.view::<AstTree>().root() {
        walk(&root, &mut out);
    }
    out
}

fn token_text(session: &support::Session, id: u64) -> Option<String> {
    session
        .runtime
        .snapshot()
        .view::<Tokens>()
        .payloads()
        .to_vec()
        .into_iter()
        .find(|(token, _)| token.0 == id)
        .map(|(_, payload)| payload.text.to_string())
}

fn values(session: &support::Session) -> Vec<(u64, AstValue)> {
    session
        .runtime
        .snapshot()
        .view::<AstMeta>()
        .values()
        .into_iter()
        .collect()
}

#[test]
fn a_respelled_leaf_keeps_every_ast_identity() {
    let mut session = support::Session::new(r#"{"a":1}"#);
    let before = ast_ids(&session);
    let tokens = session_token_ids(&session);
    let at = session.document.text().find('1').expect("the value");

    let row = session.replace(at..at + 1, "9");
    assert!(row.failure.is_none(), "{:?}", row.failure);

    assert_eq!(
        ast_ids(&session),
        before,
        "every AST node keeps its identity across a re-spelling"
    );
    assert_eq!(
        session_token_ids(&session),
        tokens,
        "the token stream keeps its order and identities"
    );
    assert_eq!(
        session.stats().nodes_added,
        0,
        "no identity is new: the leaf was rebuilt in place"
    );
    assert!(
        session.stats().nodes_built > 0,
        "the leaf and its ancestors are still rebuilt so their payload is fresh"
    );

    let values = values(&session);
    assert!(
        values
            .iter()
            .any(|(_, value)| matches!(value, AstValue::Int(9))),
        "the rebuilt leaf publishes the new value: {values:?}"
    );
    assert!(
        !values
            .iter()
            .any(|(_, value)| matches!(value, AstValue::Int(1))),
        "the old value is not left behind: {values:?}"
    );

    let leaf = values
        .iter()
        .find(|(_, value)| matches!(value, AstValue::Int(9)))
        .map(|(id, _)| *id)
        .expect("the value unit");
    let span = session
        .spans()
        .into_iter()
        .find(|(id, _)| *id == leaf)
        .map(|(_, span)| span)
        .expect("the leaf's span");
    assert_eq!(
        &session.document.text()[span],
        "9",
        "the span describes the rebuilt leaf"
    );
}

#[test]
fn a_middle_edit_in_a_large_document_is_local_and_fresh() {
    let keys: Vec<String> = (0..24)
        .map(|index| format!("\"k{index}\":{index}"))
        .collect();
    let text = format!("{{{}}}", keys.join(","));
    let mut session = support::Session::new(&text);
    let before = ast_ids(&session);
    let at = session
        .document
        .text()
        .find(r#""k12":12"#)
        .expect("the middle member")
        + r#""k12":"#.len();

    let row = session.replace(at..at + 2, "99");
    assert!(row.failure.is_none(), "{:?}", row.failure);

    assert_eq!(ast_ids(&session), before, "the tree keeps its identity");
    assert_eq!(session.stats().nodes_added, 0);

    let after = values(&session);
    assert!(
        after
            .iter()
            .any(|(_, value)| matches!(value, AstValue::Int(99))),
        "the edited value is published: {after:?}"
    );
    // The published tree must describe the current text, not the previous one.
    assert_eq!(
        session.text_signature(),
        support::Session::new(session.document.text()).text_signature(),
        "the incremental tree disagrees with a from-scratch parse"
    );
}

#[test]
fn a_deleted_member_retracts_only_its_subtree() {
    let mut session = support::Session::new(r#"[1,2,3]"#);
    let before = ast_ids(&session);
    let text = session.document.text().to_string();
    let at = text.find(",3").expect("the third member");
    let row = session.delete(at..at + 2);
    assert!(row.failure.is_none(), "{:?}", row.failure);

    let after = ast_ids(&session);
    assert!(
        after.is_subset(&before),
        "a deletion removes identities, it does not move them: {after:?} vs {before:?}"
    );
    assert!(after.len() < before.len(), "the deleted leaf retracts");
    let values = values(&session);
    assert!(
        !values
            .iter()
            .any(|(_, value)| matches!(value, AstValue::Int(3))),
        "the deleted value is retracted: {values:?}"
    );
    assert_eq!(
        token_text(&session, session_token_ids(&session)[3]).as_deref(),
        Some("2"),
        "the surviving member keeps its text"
    );
}

#[test]
fn a_structural_append_keeps_the_root_identity() {
    let mut session = support::Session::new(r#"{"a":1}"#);
    let root = session.root().expect("the seed has a root");
    let at = session
        .document
        .text()
        .find('}')
        .expect("the closing brace");
    let row = session.insert(at, r#","b":2"#);
    assert!(row.failure.is_none(), "{:?}", row.failure);
    assert_eq!(
        session.root(),
        Some(root),
        "the root is the tree's occurrence, not its extent"
    );
}

#[test]
fn an_invalid_character_keeps_the_root_identity() {
    let mut session = support::Session::new(r#"{"a":1}"#);
    let root = session.root().expect("the seed has a root");
    let row = session.insert(1, "!");
    assert!(row.failure.is_none(), "{:?}", row.failure);
    assert_eq!(
        session.root(),
        Some(root),
        "a failed parse edits the tree; it does not replace its root"
    );
}

/// Every node's span contains every child's span.
fn assert_spans_contained(session: &support::Session) {
    use std::collections::HashMap;

    fn walk(
        node: &irrigation_syntax::AstNodeRef,
        spans: &HashMap<u64, std::ops::Range<usize>>,
        failures: &mut Vec<String>,
    ) {
        let Some(parent) = spans.get(&node.raw()) else {
            return;
        };
        for child in node.children() {
            if let Some(span) = spans.get(&child.raw())
                && (span.start < parent.start || span.end > parent.end)
            {
                failures.push(format!(
                    "child {}..{:?} is outside parent {}..{:?}",
                    span.start, span.end, parent.start, parent.end
                ));
            }
            walk(&child, spans, failures);
        }
        for child in node.recovered() {
            walk(&child, spans, failures);
        }
    }

    let spans: HashMap<u64, std::ops::Range<usize>> = session.spans().into_iter().collect();
    let snapshot = session.runtime.snapshot();
    let tree = snapshot.view::<AstTree>();
    let mut failures = Vec::new();
    if let Some(root) = tree.root() {
        walk(&root, &spans, &mut failures);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn spans_stay_contained_across_structural_and_invalid_edits() {
    let mut session = support::Session::new(r#"{"a":1,"b":[2,3]}"#);
    assert_spans_contained(&session);

    let at = session
        .document
        .text()
        .find('}')
        .expect("the closing brace");
    let row = session.insert(at, r#","c":{"d":4}"#);
    assert!(row.failure.is_none(), "{:?}", row.failure);
    assert_spans_contained(&session);

    let row = session.insert(1, "!");
    assert!(row.failure.is_none(), "{:?}", row.failure);
    assert_spans_contained(&session);

    let row = session.replace(1..2, "");
    assert!(row.failure.is_none(), "{:?}", row.failure);
    assert_spans_contained(&session);
}
