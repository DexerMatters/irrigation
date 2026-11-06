use reactive_syntax::{LexState, Lexer, LexerSpec, TextDocument, TextEdit, TokenFlags};

fn lex(spec: &str, text: &str) -> (Lexer, reactive_syntax::LexOutput) {
    let spec = LexerSpec::parse(spec).expect("valid lexer spec");
    let lexer = Lexer::new(spec);
    let document = TextDocument::new(text);
    let output = lexer.lex(&document);
    (lexer, output)
}

fn texts(output: &reactive_syntax::LexOutput) -> Vec<&str> {
    output
        .tokens()
        .iter()
        .map(|token| token.text.as_ref())
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
        state = LexState::new(
            std::sync::Arc::clone(&output.tokens),
            std::sync::Arc::clone(&output.text),
        );
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
    let error = reactive_syntax::LexerSpec::parse(
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
