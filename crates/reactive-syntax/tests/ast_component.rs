//! The reactive pipeline: publication, coverage, and locality through Views.

mod support;

use reactive_syntax::{AstMeta, TextEdit, Tokens};

#[test]
fn pipeline_publishes_a_consistent_ast() {
    let mut session = support::Session::new(r#"{"a": 1, "b": [2, 3]}"#);
    assert!(session.root().is_some());
    assert!(
        session
            .signature()
            .contains(&format!("r{}", reactive_syntax::rule_id("JsonObject")))
    );
    assert!(session.ast_covers_tokens());
    assert!(session.lex_diagnostics().is_empty());
    assert!(session.ast_diagnostics().is_empty());
    let stats = session.stats();
    assert!(stats.nodes_built > 0);
    assert_eq!(stats.adopted_suffix, 0);
    let _ = &mut session;
}

#[test]
fn an_edit_republishes_only_the_edited_region() {
    let mut session = support::Session::new(r#"{"a":1,"b":[2,3],"c":"x"}"#);
    let row = session.replace(5..6, "12");
    assert!(row.failure.is_none(), "{:?}", row.failure);
    assert!(row.tokens_relexed <= 4, "{row:?}");
    assert!(row.token_units_read <= 8, "{row:?}");

    assert!(session.ast_covers_tokens());
}

#[test]
fn a_long_document_edit_stays_local() {
    let text = format!(
        "{{\"items\":[{}]}}",
        (0..48)
            .map(|index| format!("{{\"i\":{index}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut session = support::Session::new(&text);
    let value = session.document.text().find(":31}").expect("value") + 1;
    let row = session.replace(value..value, "1");
    assert!(row.failure.is_none(), "{:?}", row.failure);
    assert!(row.tokens_relexed <= 8, "{row:?}");
    assert!(row.token_units_read <= 8, "{row:?}");
    eprintln!("[test] row={row:?} stats={:?}", session.stats());
    if !session.ast_covers_tokens() {
        let significant: std::collections::HashSet<u64> =
            session.significant_tokens().into_iter().collect();
        let tree: std::collections::HashSet<u64> = session.ast_tokens().into_iter().collect();
        let meta: std::collections::HashSet<u64> = session.meta_tokens().into_iter().collect();
        eprintln!(
            "[test] significant={} tree={} meta={} in_state_but_not_tree={} in_meta_not_tree={}",
            significant.len(),
            tree.len(),
            meta.len(),
            significant.difference(&tree).count(),
            meta.difference(&tree).count()
        );
    }
    assert!(session.ast_covers_tokens());
}

#[test]
fn typing_keeps_the_ast_in_sync_with_the_tokens() {
    let mut session = support::Session::new(r#"{"name":"item"}"#);
    let start = session.document.text().find("item").expect("value");
    for (offset, ch) in "xyz".chars().enumerate() {
        let row = session.insert(start + offset, &ch.to_string());
        assert!(row.failure.is_none(), "{:?}", row.failure);
        assert!(session.ast_covers_tokens(), "tokens fell out of the AST");
    }
    let signature = session.text_signature();
    assert!(signature.contains("xyzitem"), "{signature}");
}

#[test]
fn deleting_a_member_removes_its_units() {
    let mut session = support::Session::new(r#"{"a":1,"b":2,"c":3}"#);
    let before = session.spans().len();
    let row = session.delete(6..12);
    assert!(row.failure.is_none(), "{:?}", row.failure);
    assert_eq!(session.text_signature().matches("\"b\"").count(), 0);
    let after = session.spans().len();
    assert!(after < before, "the deleted member must be retracted");
    assert!(session.ast_covers_tokens());
}

#[test]
fn value_units_follow_the_value_nodes() {
    let session = support::Session::new(r#"{"a":1}"#);
    let snapshot = session.runtime.snapshot();
    let meta = snapshot.view::<AstMeta>();
    let values = meta.values();
    assert!(!values.is_empty());
    let numbers = values
        .iter()
        .filter(|(_, value)| matches!(value, reactive_syntax::AstValue::Int(1)))
        .count();
    assert_eq!(numbers, 1);
    let tokens = snapshot.view::<Tokens>();
    assert!(tokens.payloads().len() >= 5);
}

#[test]
fn trivia_edits_do_not_rebuild_the_ast() {
    let mut session = support::Session::new(r#"{"a":1}"#);
    let root = session.root().expect("root");
    let row = session.insert(1, " ");
    assert!(row.failure.is_none(), "{:?}", row.failure);
    assert_eq!(session.root(), Some(root), "trivia must not move the root");
    assert_eq!(row.nodes_added, 0, "trivia must not rebuild nodes");
    assert_eq!(session.trivia_count(), 1);
}

#[test]
fn repeated_edits_stay_local_and_consistent() {
    let mut session = support::Session::new(r#"{"items":[1,2,3,4,5]}"#);
    for index in 0..12 {
        // Keep the cursor on the first array element so every edit stays valid.
        let value = session
            .document
            .text()
            .find('[')
            .expect("the array is still there")
            + 1;
        let row = if index % 3 == 0 {
            session.insert(value, "9")
        } else if index % 3 == 1 {
            session.delete(value..value + 1)
        } else {
            session.insert(value + 1, "0")
        };
        assert!(row.failure.is_none(), "{:?}", row.failure);
        assert!(session.ast_covers_tokens());
    }
    assert!(session.text_signature().contains("10"));
}

#[test]
fn a_text_replacement_keeps_the_ast_parseable() {
    let mut session = support::Session::new(r#"{"a":"one"}"#);
    let start = session.document.text().find("one").expect("value");
    let row = session.replace(start..start + 3, "two");
    assert!(row.failure.is_none(), "{:?}", row.failure);
    let signature = session.text_signature();
    assert!(signature.contains("two"), "{signature}");
    assert!(
        session.ast_diagnostics().is_empty(),
        "{:?}",
        session.ast_diagnostics()
    );
}

#[test]
fn an_empty_document_is_still_published() {
    let session = support::Session::new("");
    // Recovery still yields a tree: the pipeline never leaves the AST undefined.
    assert!(session.root().is_some());
    let mut session = support::Session::new("{}");
    assert!(session.ast_covers_tokens());
    let row = session.insert(1, "\"a\":1");
    assert!(row.failure.is_none(), "{:?}", row.failure);
    assert!(session.ast_covers_tokens());
    assert_eq!(session.ast_diagnostics().len(), 0);
    let _ = TextEdit::insert(0, "");
}
