#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use irrigation_syntax::{Lexer, LexerSpec, TextDocument};

fn lexer() -> &'static Lexer {
    static LEXER: OnceLock<Lexer> = OnceLock::new();
    LEXER.get_or_init(|| {
        let spec = LexerSpec::parse(
            r#"
            grammar Lex {
                lexer {
                    initial root;
                    state root {
                        skip ws = /[ \t\r\n]+/;
                        token ident = /[A-Za-z_][A-Za-z0-9_]*/;
                        token number = /[0-9]+/;
                        token "(" ; token ")" ; token ";" ;
                    }
                }
            }
            "#,
        )
        .expect("lexer");
        Lexer::new(spec)
    })
}

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = lexer().lex(&TextDocument::new(text));
    }
});
