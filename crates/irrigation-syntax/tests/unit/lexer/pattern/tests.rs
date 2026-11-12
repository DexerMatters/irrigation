//! Unit tests for `src/lexer/pattern.rs`.

use super::{Pattern, PatternError};

#[test]
fn longest_match_is_first() {
    let pattern = Pattern::parse(r"a+").unwrap();
    let matches = pattern.find_at("aaab", 0);
    assert_eq!(matches[0].end, 3);
    assert_eq!(matches.last().unwrap().end, 1);
}

#[test]
fn alternation_and_class() {
    let pattern = Pattern::parse(r"(?:[a-c]+|\d+)").unwrap();
    assert_eq!(pattern.find_at("abc1", 0)[0].end, 3);
    assert_eq!(pattern.find_at("123a", 0)[0].end, 3);
}

#[test]
fn named_captures_are_recorded() {
    let pattern = Pattern::parse(r"<(?P<name>[A-Za-z]+)>").unwrap();
    assert_eq!(pattern.capture_names(), &["name".to_string()]);
    let found = pattern.find_at("<abc>", 0);
    assert_eq!(found[0].end, 5);
    assert_eq!(found[0].captures[0], Some((1, 4)));
}

#[test]
fn empty_repetition_does_not_loop() {
    let pattern = Pattern::parse(r"(?:a*)*").unwrap();
    let found = pattern.find_at("aaa", 0);
    assert_eq!(found[0].end, 3);
}

#[test]
fn malformed_patterns_are_rejected() {
    assert!(matches!(Pattern::parse("("), Err(PatternError { .. })));
    assert!(Pattern::parse("[a-").is_err());
    assert!(Pattern::parse("[z-a]").is_err());
    assert!(Pattern::parse("[^]").is_err());
}

#[test]
fn exact_literals_use_the_fast_path() {
    let pattern = Pattern::parse("abc").unwrap();
    assert_eq!(pattern.exact_literal(), Some("abc".to_string()));
    assert_eq!(pattern.find_at("abcdef", 0)[0].end, 3);
    assert!(pattern.find_at("abx", 0).is_empty());
    let pattern = Pattern::parse("a+").unwrap();
    assert_eq!(pattern.exact_literal(), None);
}

#[test]
fn first_byte_filter_is_conservative() {
    let literal = Pattern::parse("abc").unwrap();
    assert!(literal.can_start_with_byte(b'a'));
    assert!(!literal.can_start_with_byte(b'z'));
    let class = Pattern::parse("[0-9]+").unwrap();
    assert!(class.can_start_with_byte(b'7'));
    assert!(!class.can_start_with_byte(b'a'));
    let optional = Pattern::parse("a*").unwrap();
    assert!(optional.can_start_with_byte(b'z'));
    let unicode = Pattern::parse("é").unwrap();
    assert!(unicode.can_start_with_byte(0xC3));
}

/// The filter must never answer `false` for a byte the matcher accepts.
///
/// `0x0B` (vertical tab) is whitespace in Unicode but not `is_ascii_whitespace`, so a space
/// class filtered it out while `CharClass::matches` accepted it -- a rule the lexer would never
/// try at that byte.
#[test]
fn the_byte_filter_agrees_with_the_matcher_on_a_vertical_tab() {
    let class = Pattern::parse(r"[\s]+").unwrap();
    assert!(
        class.find_at("\u{0B}x", 0).iter().any(|hit| hit.end == 1),
        "the matcher accepts a vertical tab"
    );
    assert!(
        class.can_start_with_byte(0x0B),
        "so the filter may not reject it"
    );
}

#[test]
fn examined_covers_the_byte_that_ended_a_dfa_scan() {
    let pattern = Pattern::parse("a+").unwrap();
    let found = pattern.find_at("aaab", 0);
    assert_eq!(found[0].end, 3);
    // The scan had to read `b` to learn that the match stopped before it.
    assert_eq!(found[0].examined, 4);

    let literal = Pattern::parse("abc").unwrap();
    assert_eq!(literal.exact_literal().as_deref(), Some("abc"));
    assert_eq!(literal.find_at("abcdef", 0)[0].examined, 3);
}

#[test]
fn search_reports_the_furthest_read_even_when_nothing_matches() {
    // A rule that fails still reads on the way to failing, and the caller
    // needs that position to widen its relex window; an empty match vector
    // cannot carry it.
    let dfa = Pattern::parse("<[a-z]+>").unwrap();
    let (matches, examined) = dfa.search("<abc", 0);
    assert!(matches.is_empty());
    assert_eq!(examined, 4);

    let backtracking = Pattern::parse(r"<(?P<name>[a-z]+)>").unwrap();
    let (matches, examined) = backtracking.search("<abc", 0);
    assert!(matches.is_empty());
    assert_eq!(examined, 4);

    // The literal fast path examines the byte that disagreed, because
    // replacing it would make the literal match.
    let literal = Pattern::parse("abc").unwrap();
    assert_eq!(literal.exact_literal().as_deref(), Some("abc"));
    let (matches, examined) = literal.search("abx", 0);
    assert!(matches.is_empty());
    assert_eq!(examined, 3);
}

#[test]
fn examined_reaches_past_a_failed_longer_alternative() {
    // Matching `<abc` explores `<(?P<name>[a-z]+)>` all the way to the end
    // before the bare `<` alternative wins, so the exam must cover the text
    // the failed branch read.
    let pattern = Pattern::parse(r"<(?P<name>[a-z]+)>|<").unwrap();
    let found = pattern.find_at("<abc", 0);
    assert_eq!(found[0].end, 1, "{found:?}");
    assert!(
        found.iter().all(|matched| matched.examined >= 4),
        "{found:?}"
    );
}

#[test]
fn examined_is_never_below_the_match_end() {
    let patterns = [
        Pattern::parse("a*").unwrap(),
        Pattern::parse(r"(?:ab|a)").unwrap(),
        Pattern::parse(r"(?P<x>a)*").unwrap(),
        Pattern::parse(r"[a-c]+").unwrap(),
        Pattern::parse("").unwrap(),
    ];
    for pattern in &patterns {
        for text in ["", "a", "aaab", "bc", "x"] {
            for matched in pattern.find_at(text, 0) {
                assert!(
                    matched.examined >= matched.end,
                    "{:?} on {text:?}: {matched:?}",
                    pattern.expr
                );
                assert!(matched.examined <= text.len());
            }
        }
    }
}

#[test]
fn dfa_and_backtracking_agree_on_end_positions() {
    let dfa = Pattern::parse("a+b?").unwrap();
    let backtracking = Pattern::parse("(a+)(b?)").unwrap();
    for text in ["", "a", "ab", "aaab", "b", "aaac"] {
        let mut left = dfa
            .find_at(text, 0)
            .into_iter()
            .map(|matched| matched.end)
            .collect::<Vec<_>>();
        let mut right = backtracking
            .find_at(text, 0)
            .into_iter()
            .map(|matched| matched.end)
            .collect::<Vec<_>>();
        left.sort_unstable();
        left.dedup();
        right.sort_unstable();
        right.dedup();
        assert_eq!(left, right, "{text:?}");
    }
}

#[test]
fn unicode_and_escapes() {
    let pattern = Pattern::parse(r"\u{1f600}|\x41").unwrap();
    assert_eq!(pattern.find_at("😀", 0)[0].end, 4);
    assert_eq!(pattern.find_at("A", 0)[0].end, 1);
}
