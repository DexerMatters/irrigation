//! Unit tests for `src/lexer/guard.rs`.

use super::{Action, EvalContext, GuardExpr, LexValue};

#[test]
fn parses_and_evaluates_comparisons() {
    let expr = GuardExpr::parse("$name == top_value and column > 0").unwrap();
    let ranges = [Some((0, 3))];
    let values = [LexValue::from("abc")];
    let cx = EvalContext {
        column: 4,
        text: "abc",
        lexeme: "abc",
        capture_names: &["name".to_string()],
        capture_ranges: &ranges,
        values: &values,
        eof: false,
    };
    assert!(expr.eval_bool(&cx).unwrap());
}

#[test]
fn orders_text_lexicographically() {
    let empty = EvalContext {
        column: 0,
        text: "",
        lexeme: "",
        capture_names: &[],
        capture_ranges: &[],
        values: &[],
        eof: false,
    };
    let holds = |source: &str| GuardExpr::parse(source).unwrap().eval_bool(&empty).unwrap();
    assert!(holds("\"abc\" < \"abd\""));
    assert!(holds("\"abc\" <= \"abd\""));
    assert!(!holds("\"abd\" < \"abc\""));
    assert!(!holds("\"abd\" <= \"abc\""));
    assert!(holds("\"abd\" > \"abc\""));
    assert!(holds("\"abd\" >= \"abc\""));
    assert!(!holds("\"abc\" > \"abd\""));
    assert!(holds("\"abc\" <= \"abc\""));
    // The empty text sorts before every non-empty text.
    assert!(holds("\"\" < \"a\""));
    assert!(holds("\"\" <= \"\""));
    assert!(!holds("\"\" > \"\""));
}

#[test]
fn top_value_is_unit_when_empty() {
    let expr = GuardExpr::parse("top_value == \"x\"").unwrap();
    let cx = EvalContext {
        column: 0,
        text: "",
        lexeme: "",
        capture_names: &[],
        capture_ranges: &[],
        values: &[],
        eof: false,
    };
    assert!(!expr.eval_bool(&cx).unwrap());
}

#[test]
fn parses_actions() {
    let actions = Action::parse_list("push_value($name), switch(tag)").unwrap();
    assert_eq!(actions.len(), 2);
    assert!(matches!(actions[0], Action::PushValue(_)));
    assert_eq!(actions[1], Action::Switch("tag".to_string()));
}

#[test]
fn rejects_unknown_names() {
    assert!(GuardExpr::parse("bogus").is_err());
    assert!(Action::parse("nope").is_err());
}
