use irrigation_syntax::{LexState, Lexer, LexerSpec, TextDocument, TextEdit, TokenFlags};

fn lex(spec: &str, text: &str) -> (Lexer, irrigation_syntax::LexOutput) {
    let spec = LexerSpec::parse(spec).expect("valid lexer spec");
    let lexer = Lexer::new(spec);
    let document = TextDocument::new(text);
    let output = lexer.lex(&document);
    (lexer, output)
}

fn texts(output: &irrigation_syntax::LexOutput) -> Vec<&str> {
    output
        .tokens()
        .iter()
        .map(|token| token.text.as_ref())
        .collect()
}

/// Diagnostics as (owning token index, code, message).
///
/// A diagnostic is created from one token's span, so the token it repeats is
/// the invariant to compare: a carried diagnostic keeps the anchors it was
/// created with, exactly like the reused token it describes.
fn diagnostics(output: &irrigation_syntax::LexOutput) -> Vec<(usize, String, String)> {
    let tokens = output.tokens();
    output
        .diagnostics()
        .iter()
        .map(|diagnostic| {
            let index = tokens
                .iter()
                .position(|token| token.range == diagnostic.range)
                .unwrap_or_else(|| panic!("no token for {diagnostic:?}"));
            (index, diagnostic.code.clone(), diagnostic.message.clone())
        })
        .collect()
}

#[test]
fn simple_tokens_and_trivia() {
    let (lexer, output) = lex(
        r#"
        grammar Json {
            lexer {
                initial root;
                state root {
                    skip ws = /[ \t\r\n]+/;
                    token ident = /[A-Za-z_][A-Za-z0-9_]*/ @word;
                    token number = /[0-9]+/;
                    token "{" ;
                    token "}" ;
                    token ":" ;
                }
            }
        }
        "#,
        "{x: 12 }",
    );
    assert!(output.diagnostics().is_empty());
    let tokens = texts(&output);
    assert_eq!(tokens, ["{", "x", ":", " ", "12", " ", "}"]);
    let ident = lexer.spec().kind_by_name("ident").expect("ident kind");
    assert!(output.tokens().iter().any(|token| token.kind == ident));
    let trivia_count = output
        .tokens()
        .iter()
        .filter(|token| token.flags.contains(TokenFlags::TRIVIA))
        .count();
    assert_eq!(trivia_count, 2);
}

#[test]
fn stateful_tag_matching() {
    let (lexer, output) = lex(
        r#"
        grammar Xml {
            lexer {
                initial text;
                state text {
                    skip ws = /[ \t\r\n]+/;
                    token Text = /[^<]+/;
                    token StartTag = /<(?P<name>[A-Za-z_][A-Za-z0-9_]*)/
                                   -> push_value($name), switch(tag);
                    token EndTag = /<\/(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*>/
                                 when $name == top_value
                                 -> pop_value;
                }
                state tag {
                    token Attr = /[A-Za-z_][A-Za-z0-9_]*/;
                    token "=" ;
                    token Value = /"[^"]*"/;
                    token ">" -> switch(text);
                    token "/>" -> pop_value, switch(text);
                }
            }
        }
        "#,
        "<a><b>x</b></a>",
    );
    assert!(
        output.diagnostics().is_empty(),
        "{:?}",
        output.diagnostics()
    );
    let tokens = texts(&output);
    assert_eq!(tokens, ["<a", ">", "<b", ">", "x", "</b>", "</a>"]);
    let kinds = output
        .tokens()
        .iter()
        .map(|token| lexer.spec().kinds()[token.kind.0 as usize].name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(kinds[0], "StartTag");
    assert_eq!(kinds[5], "EndTag");
}

#[test]
fn stateful_tag_mismatch_is_an_error_token() {
    let (_, output) = lex(
        r#"
        grammar Xml {
            lexer {
                initial text;
                state text {
                    token StartTag = /<(?P<name>[A-Za-z]+)/ -> push_value($name), switch(tag);
                    token EndTag = /<\/(?P<name>[A-Za-z]+)>/ when $name == top_value -> pop_value;
                    token Text = /[^<]+/;
                }
                state tag { token ">" -> switch(text); }
            }
        }
        "#,
        "<a></b>",
    );
    assert!(
        output
            .tokens()
            .iter()
            .any(|token| token.flags.contains(TokenFlags::ERROR))
    );
    assert!(!output.diagnostics().is_empty());
}

#[test]
fn indentation_tokens_are_zero_width_and_generic() {
    let (lexer, output) = lex(
        r#"
        grammar Indented {
            lexer {
                initial line_start;
                state line_start {
                    skip CodeStart = "" when depth == 0
                                   -> push_value(0), switch(code);
                    skip SameLine  = "" when depth > 0 and column == top_value
                                   -> switch(code);
                    token Indent   = "" when depth > 0 and column > top_value
                                   -> push_value(column), switch(code);
                    token Dedent   = "" when depth > 0 and column < top_value
                                   -> pop_value;
                    token Newline  = /\r?\n/;
                    skip Space     = /[ \t]+/;
                }
                state code {
                    skip Space = /[ \t]+/;
                    token Ident = /[A-Za-z_][A-Za-z0-9_]*/ @word;
                    token Newline = /\r?\n/ -> switch(line_start);
                }
            }
        }
        "#,
        "a\n  b\nc\n",
    );
    assert!(
        output.diagnostics().is_empty(),
        "{:?}",
        output.diagnostics()
    );
    let names = output
        .tokens()
        .iter()
        .map(|token| lexer.spec().kinds()[token.kind.0 as usize].name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"Indent"));
    assert!(names.contains(&"Dedent"));
    assert_eq!(names.iter().filter(|name| **name == "Ident").count(), 3);
}

#[test]
fn incremental_lex_matches_from_scratch_and_reuses_suffix() {
    let spec = r#"
    grammar Json {
        lexer {
            initial root;
            state root {
                skip ws = /[ \t\r\n]+/;
                token ident = /[A-Za-z_][A-Za-z0-9_]*/ @word;
                token string = /"([^"\\]|\\.)*"/;
                token number = /[0-9]+/;
                token "{" ; token "}" ; token "[" ; token "]" ; token ":" ; token "," ;
            }
        }
    }
    "#;
    let spec = LexerSpec::parse(spec).unwrap();
    let lexer = Lexer::new(spec);
    let first = TextDocument::new(r#"{"a": 1, "b": [2, 3]}"#);
    let before = lexer.lex(&first);
    let second = first.replace(18..19, "30").unwrap();
    let after = lexer.lex_incremental(
        &LexState::new(
            std::sync::Arc::clone(&before.tokens),
            std::sync::Arc::clone(&before.text),
        ),
        &second,
    );
    let scratch = lexer.lex(&second);
    assert_eq!(after.tokens().len(), scratch.tokens().len());
    for (left, right) in after.tokens().iter().zip(scratch.tokens().iter()) {
        assert_eq!(left.text, right.text);
        assert_eq!(left.kind, right.kind);
    }
    assert!(after.relexed_chars < second.text().len());
    let suffix_id = after.tokens().get(after.tokens().len() - 1).unwrap().id;
    assert_eq!(
        suffix_id,
        before.tokens().get(before.tokens().len() - 1).unwrap().id
    );
}

#[test]
fn incremental_lex_matches_from_scratch_after_multiple_edits() {
    let spec = LexerSpec::parse(
        r#"
        grammar T {
            lexer {
                initial root;
                state root {
                    skip ws = /[ \t\r\n]+/;
                    token ident = /[A-Za-z_]+/;
                    token number = /[0-9]+/;
                    token "+" ; token "-" ; token "(" ; token ")" ;
                }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let mut doc = TextDocument::new("a + b");
    let mut state = {
        let output = lexer.lex(&doc);
        LexState::new(
            std::sync::Arc::clone(&output.tokens),
            std::sync::Arc::clone(&output.text),
        )
    };
    for (range, replacement) in [(0..1, "alpha"), (9..10, "beta"), (4..5, "42"), (4..6, "x")] {
        if range.end > doc.text().len() {
            continue;
        }
        doc = doc.replace(range, replacement).unwrap();
        let output = lexer.lex_incremental(&state, &doc);
        let scratch = lexer.lex(&doc);
        assert_eq!(
            output
                .tokens()
                .iter()
                .map(|t| t.text.as_ref())
                .collect::<Vec<_>>(),
            scratch
                .tokens()
                .iter()
                .map(|t| t.text.as_ref())
                .collect::<Vec<_>>()
        );
        state = LexState::new(
            std::sync::Arc::clone(&output.tokens),
            std::sync::Arc::clone(&output.text),
        );
    }
}

#[test]
fn unknown_character_produces_an_error_token_and_diagnostic() {
    let (_, output) = lex(
        r#"
        grammar T {
            lexer {
                initial root;
                state root { token ident = /[A-Za-z]+/; skip ws = /[ \t]+/; }
            }
        }
        "#,
        "abc @ def",
    );
    assert_eq!(output.tokens().len(), 5);
    assert!(
        output
            .tokens()
            .iter()
            .any(|t| t.flags.contains(TokenFlags::ERROR))
    );
    assert!(!output.diagnostics().is_empty());
}

#[test]
fn edits_inside_tokens_relex_only_the_affected_region() {
    let spec = LexerSpec::parse(
        r#"
        grammar T {
            lexer {
                initial root;
                state root {
                    token string = /"([^"\\]|\\.)*"/;
                    skip ws = /[ \t]+/;
                    token ident = /[A-Za-z_]+/;
                }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let doc = TextDocument::new(r#"alpha "hello" beta "world" gamma"#);
    let before = lexer.lex(&doc);
    let state = LexState::new(
        std::sync::Arc::clone(&before.tokens),
        std::sync::Arc::clone(&before.text),
    );
    let doc = doc.edit([TextEdit::new(7..12, "HELLO")]).unwrap();
    let after = lexer.lex_incremental(&state, &doc);
    let scratch = lexer.lex(&doc);
    assert_eq!(
        after
            .tokens()
            .iter()
            .map(|t| t.text.as_ref())
            .collect::<Vec<_>>(),
        scratch
            .tokens()
            .iter()
            .map(|t| t.text.as_ref())
            .collect::<Vec<_>>()
    );
    assert!(after.relexed_chars < doc.text().len());
}

#[test]
fn incremental_lex_backs_off_when_a_token_depended_on_later_text() {
    // `<` is accepted by the tag rule only because the longer alternative
    // failed against text *past* the token, so an edit inside that text must
    // re-examine the `<` too. Reusing it yields `<` + `ab>` where a from-scratch
    // lex yields the single tag `<ab>`.
    let spec = LexerSpec::parse(
        r#"
        grammar Tag {
            lexer {
                initial root;
                state root {
                    token tag = /<(?P<name>[a-z]+)>|</;
                    token Text = /[^<]+/;
                }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let first = TextDocument::new("<abc");
    let before = lexer.lex(&first);
    assert_eq!(texts(&before), ["<", "abc"]);
    let second = first.replace(3..4, ">").unwrap();
    let after = lexer.lex_incremental(
        &LexState::new(
            std::sync::Arc::clone(&before.tokens),
            std::sync::Arc::clone(&before.text),
        ),
        &second,
    );
    let scratch = lexer.lex(&second);
    let after: Vec<_> = after
        .tokens()
        .iter()
        .map(|token| (token.kind, token.text.clone()))
        .collect();
    let scratch: Vec<_> = scratch
        .tokens()
        .iter()
        .map(|token| (token.kind, token.text.clone()))
        .collect();
    assert_eq!(after, scratch);
    assert_eq!(texts(&lexer.lex(&second)), ["<ab>"]);
}

#[test]
fn incremental_lex_backs_off_when_a_losing_sibling_rule_read_later_text() {
    // `<` wins only because the `tag` rule failed against text past it. The
    // loser's read must count too: once `>` closes the tag, `<` and the text
    // after it become one token, and the reused lexemes still spell the
    // document, so the cover check cannot notice the divergence.
    let spec = LexerSpec::parse(
        r#"
        grammar Tag {
            lexer {
                initial root;
                state root {
                    token tag = /<[a-z]+>/;
                    token lt = /</;
                    token Text = /[^<]+/;
                }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let first = TextDocument::new("<abc");
    let before = lexer.lex(&first);
    assert_eq!(texts(&before), ["<", "abc"]);
    let lt = before.tokens().get(0).expect("lt token");
    let lookahead = before
        .text
        .resolve(lt.lookahead_end)
        .expect("lookahead anchor");
    assert!(
        lookahead > 1,
        "the losing rule read past the `<`: {lookahead}"
    );
    for (edit, expected) in [
        (TextEdit::new(3..4, ">"), vec!["<ab>"]),
        (TextEdit::new(3..3, ">"), vec!["<ab>", "c"]),
    ] {
        let second = first.edit([edit]).unwrap();
        assert_eq!(texts(&lexer.lex(&second)), expected);
        let after = lexer.lex_incremental(&LexState::from_output(&before), &second);
        assert_eq!(after.relexed_tokens.start, 0, "the `<` read past the edit");
        let scratch = lexer.lex(&second);
        let after: Vec<_> = after
            .tokens()
            .iter()
            .map(|token| (token.kind, token.text.clone()))
            .collect();
        let scratch: Vec<_> = scratch
            .tokens()
            .iter()
            .map(|token| (token.kind, token.text.clone()))
            .collect();
        assert_eq!(after, scratch);
    }
}

#[test]
fn back_off_covers_a_match_a_guard_rejected() {
    // `tag` matched `<ab>` but its guard rejected the name, so `lt` won the
    // `<`. The rejected match is still a read: shortening the name to the one
    // the guard wants must re-examine the `<`, not start after it.
    let spec = LexerSpec::parse(
        r#"
        grammar Tag {
            lexer {
                initial root;
                state root {
                    token tag = /<(?P<name>[a-z]+)>/ when $name == "a";
                    token lt = /</;
                    token Text = /[^<]+/;
                }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let first = TextDocument::new("<ab>");
    let before = lexer.lex(&first);
    assert_eq!(texts(&before), ["<", "ab>"]);
    let lt = before.tokens().get(0).expect("lt token");
    let lookahead = before
        .text
        .resolve(lt.lookahead_end)
        .expect("lookahead anchor");
    assert_eq!(lookahead, 4, "the rejected match read to the `>`");
    let second = first.replace(2..3, "").unwrap();
    assert_eq!(texts(&lexer.lex(&second)), ["<a>"]);
    let after = lexer.lex_incremental(&LexState::from_output(&before), &second);
    assert_eq!(after.relexed_tokens.start, 0, "the `<` read past the edit");
    let scratch = lexer.lex(&second);
    let after: Vec<_> = after
        .tokens()
        .iter()
        .map(|token| (token.kind, token.text.clone()))
        .collect();
    let scratch: Vec<_> = scratch
        .tokens()
        .iter()
        .map(|token| (token.kind, token.text.clone()))
        .collect();
    assert_eq!(after, scratch);
}

#[test]
fn error_token_records_what_the_failed_match_read() {
    // `@ab` has no match, so the error token covers one scalar, but proving
    // that read to the end of the text. Ending the address with `>` there makes
    // the rule match, so the error token needs the same relex window as any
    // other token.
    let spec = LexerSpec::parse(
        r#"
        grammar T {
            lexer {
                initial root;
                state root {
                    skip ws = /[ ]+/;
                    token ident = /[a-z]+/;
                    token email = /@[a-z]+>/;
                }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let first = TextDocument::new("x @ab");
    let before = lexer.lex(&first);
    assert_eq!(texts(&before), ["x", " ", "@", "ab"]);
    let error = before.tokens().get(2).expect("error token");
    let lookahead = before
        .text
        .resolve(error.lookahead_end)
        .expect("lookahead anchor");
    assert_eq!(lookahead, first.text().len());
    let second = first.replace(4..5, ">").unwrap();
    assert_eq!(texts(&lexer.lex(&second)), ["x", " ", "@a>"]);
    let after = lexer.lex_incremental(&LexState::from_output(&before), &second);
    assert!(
        after.relexed_tokens.start <= 2,
        "the error token must be inside the relex window: {:?}",
        after.relexed_tokens
    );
    let scratch = lexer.lex(&second);
    let after: Vec<_> = after
        .tokens()
        .iter()
        .map(|token| (token.kind, token.text.clone()))
        .collect();
    let scratch: Vec<_> = scratch
        .tokens()
        .iter()
        .map(|token| (token.kind, token.text.clone()))
        .collect();
    assert_eq!(after, scratch);
}

#[test]
fn back_off_scans_past_a_token_that_read_less() {
    // `<` probed the whole document before `lt` won, but the token after it is
    // an exact literal that read nothing beyond itself. Lookahead is not
    // monotone, so the scan cannot stop at the first token that ends before the
    // edit: the `<` still has to be re-examined.
    let spec = LexerSpec::parse(
        r#"
        grammar Tag {
            lexer {
                initial root;
                state root {
                    token tag = /<[a-z]+>/;
                    token lt = /</;
                    token "a" ; token "b" ; token "c" ; token "d" ;
                }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let first = TextDocument::new("<abcd");
    let before = lexer.lex(&first);
    assert_eq!(texts(&before), ["<", "a", "b", "c", "d"]);
    let second = first.replace(3..4, ">").unwrap();
    assert_eq!(texts(&lexer.lex(&second)), ["<ab>", "d"]);
    let after = lexer.lex_incremental(&LexState::from_output(&before), &second);
    assert_eq!(after.relexed_tokens.start, 0, "the `<` read past the edit");
    let scratch = lexer.lex(&second);
    let after: Vec<_> = after
        .tokens()
        .iter()
        .map(|token| (token.kind, token.text.clone()))
        .collect();
    let scratch: Vec<_> = scratch
        .tokens()
        .iter()
        .map(|token| (token.kind, token.text.clone()))
        .collect();
    assert_eq!(after, scratch);
}

#[test]
fn back_off_covers_an_insertion_at_the_end_of_the_text() {
    // The `tag` probe stopped because the text ended, so `lt` was accepted on
    // the strength of the missing `>`. Appending it must re-examine the `<`:
    // the probe stopped at the document end, and an insertion there is text the
    // attempt never saw.
    let spec = LexerSpec::parse(
        r#"
        grammar Tag {
            lexer {
                initial root;
                state root {
                    token tag = /<[a-z]+>/;
                    token lt = /</;
                    token Text = /[^<]+/;
                }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let first = TextDocument::new("<abc");
    let before = lexer.lex(&first);
    assert_eq!(texts(&before), ["<", "abc"]);
    let second = first.replace(4..4, ">").unwrap();
    assert_eq!(texts(&lexer.lex(&second)), ["<abc>"]);
    let after = lexer.lex_incremental(&LexState::from_output(&before), &second);
    assert_eq!(after.relexed_tokens.start, 0, "the `<` read to the end");
    let scratch = lexer.lex(&second);
    let after: Vec<_> = after
        .tokens()
        .iter()
        .map(|token| (token.kind, token.text.clone()))
        .collect();
    let scratch: Vec<_> = scratch
        .tokens()
        .iter()
        .map(|token| (token.kind, token.text.clone()))
        .collect();
    assert_eq!(after, scratch);
}

#[test]
fn incremental_lex_keeps_diagnostics_outside_the_relex_window() {
    let spec = LexerSpec::parse(
        r#"
        grammar T {
            lexer {
                initial root;
                state root { token ident = /[A-Za-z]+/; skip ws = /[ \t]+/; }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let first = TextDocument::new("abc @");
    let before = lexer.lex(&first);
    assert_eq!(before.diagnostics().len(), 1);
    let second = first.replace(2..3, "d").unwrap();
    let after = lexer.lex_incremental(&LexState::from_output(&before), &second);
    let scratch = lexer.lex(&second);
    assert_eq!(diagnostics(&after), diagnostics(&scratch));
    let ranges = |output: &irrigation_syntax::LexOutput| {
        output
            .diagnostics()
            .iter()
            .map(|diagnostic| output.text.resolve_range(diagnostic.range))
            .collect::<Vec<_>>()
    };
    assert_eq!(ranges(&after), ranges(&scratch));
    assert_eq!(after.diagnostics().len(), 1, "{:?}", after.diagnostics());
}

#[test]
fn an_unchanged_revision_keeps_the_retained_diagnostics() {
    let spec = LexerSpec::parse(
        r#"
        grammar T {
            lexer {
                initial root;
                state root { token ident = /[A-Za-z]+/; skip ws = /[ \t]+/; }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let document = TextDocument::new("abc @");
    let before = lexer.lex(&document);
    let after = lexer.lex_incremental(&LexState::from_output(&before), &document);
    assert_eq!(diagnostics(&after), diagnostics(&before));
    assert_eq!(after.diagnostics().len(), 1);
}

#[test]
fn randomized_incremental_lexing_matches_from_scratch() {
    let spec = LexerSpec::parse(
        r#"
        grammar Random {
            lexer {
                initial root;
                state root {
                    skip ws = /[ \t\r\n]+/;
                    token string = /"([^"\\]|\\.)*"/;
                    token ident = /[A-Za-z_][A-Za-z0-9_]*/ @word;
                    token number = /[0-9]+/;
                    token "+" ; token "-" ; token "*" ; token "/" ;
                    token "(" ; token ")" ; token "{" ; token "}" ;
                    token "," ; token ":" ;
                }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let mut doc = TextDocument::new("let x = 1 + 2; f(a, \"b\")");
    let first = lexer.lex(&doc);
    let mut state = LexState::new(
        std::sync::Arc::clone(&first.tokens),
        std::sync::Arc::clone(&first.text),
    );
    let mut seed = 0x1234_5678u64;
    for _ in 0..300 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let len = doc.text().len();
        let a = (seed as usize) % (len + 1);
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let b = (seed as usize) % (len + 1);
        let range = a.min(b)..a.max(b);
        let replacement = match (seed >> 8) % 6 {
            0 => "",
            1 => "x",
            2 => "1",
            3 => "+",
            4 => " ",
            _ => "\"q\"",
        };
        doc = doc.replace(range, replacement).unwrap();
        let output = lexer.lex_incremental(&state, &doc);
        let scratch = lexer.lex(&doc);
        let left: Vec<_> = output
            .tokens()
            .iter()
            .map(|t| (t.kind, t.text.clone()))
            .collect();
        let right: Vec<_> = scratch
            .tokens()
            .iter()
            .map(|t| (t.kind, t.text.clone()))
            .collect();
        assert_eq!(left, right, "text: {:?}", doc.text());
        assert_eq!(
            diagnostics(&output),
            diagnostics(&scratch),
            "diagnostics diverged: {:?}",
            doc.text()
        );
        state = LexState::from_output(&output);
    }
}

#[test]
fn incremental_lexing_with_insertions_at_boundaries() {
    let spec = LexerSpec::parse(
        r#"
        grammar T {
            lexer {
                initial root;
                state root {
                    token ident = /[A-Za-z]+/;
                    skip ws = /[ \t]+/;
                }
            }
        }
        "#,
    )
    .unwrap();
    let lexer = Lexer::new(spec);
    let doc = TextDocument::new("aa bb cc");
    let before = lexer.lex(&doc);
    let state = LexState::new(
        std::sync::Arc::clone(&before.tokens),
        std::sync::Arc::clone(&before.text),
    );
    for (range, text) in [(0..0, "x"), (2..2, " y"), (8..8, " z"), (4..4, " ")] {
        let next = doc.replace(range, text).unwrap();
        let output = lexer.lex_incremental(&state, &next);
        let scratch = lexer.lex(&next);
        assert_eq!(
            output
                .tokens()
                .iter()
                .map(|t| t.text.as_ref())
                .collect::<Vec<_>>(),
            scratch
                .tokens()
                .iter()
                .map(|t| t.text.as_ref())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn division_and_comments_work_inside_guards_and_actions() {
    // `column / 2` is division, not the start of a regex; the comments between
    // guard tokens are grammar comments, not expression text. The value the
    // `Far` action pushes is read back by the `Deep` guard, so the action's
    // division is observed, not merely parsed.
    let (lexer, output) = lex(
        r#"
        grammar Math {
            lexer {
                initial root;
                state root {
                    skip ws = /[ ]+/;
                    token Deep = /[a-z]+/ when top_value == 3;
                    token Far = /[a-z]+/
                        when column / 2 >= 2  // only once the column is wide enough
                        -> push_value(column / 2) /* value = column / 2 */;
                    token Near = /[a-z]+/;
                }
            }
        }
        "#,
        "  abc def ghi",
    );
    assert!(
        output.diagnostics().is_empty(),
        "{:?}",
        output.diagnostics()
    );
    let kinds = output
        .tokens()
        .iter()
        .filter(|token| !token.flags.contains(TokenFlags::TRIVIA))
        .map(|token| lexer.spec().kinds()[token.kind.0 as usize].name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(kinds, ["Near", "Far", "Deep"]);
}

#[test]
fn conflicting_literal_rules_are_diagnosed() {
    let error = LexerSpec::parse(
        r#"
        grammar Conflict {
            lexer {
                initial root;
                state root {
                    token "a" ;
                    token "a" ;
                }
            }
        }
        "#,
    )
    .expect_err("duplicate literal should be rejected");
    assert!(
        error.to_string().contains("conflicting lexer rules"),
        "{error}"
    );
}

#[test]
fn message_attribute_requires_an_error_rule() {
    let error = irrigation_syntax::LexerSpec::parse(
        r#"
        lexer {
            initial root;
            state root {
                token a = "a" @message("not an error");
            }
        }
        "#,
    )
    .expect_err("message on normal token");
    assert!(error.to_string().contains("@message"));
}
