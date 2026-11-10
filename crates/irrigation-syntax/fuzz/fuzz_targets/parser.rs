#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use irrigation_syntax::{AstParser, EngineOptions, Grammar, Lexer, TextDocument};

fn parser() -> &'static (AstParser, Lexer) {
    static PARSER: OnceLock<(AstParser, Lexer)> = OnceLock::new();
    PARSER.get_or_init(|| {
        let grammar = Grammar::compile(
            r#"
            grammar Json {
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
                parser {
                    Document = Value;
                    Value = Object | Array | string | number | ident;
                    Object = "{" (Pair ("," Pair)*)? "}";
                    Pair = key: string ":" value: Value;
                    Array = "[" (Value ("," Value)*)? "]";
                }
            }
            "#,
        )
        .expect("grammar");
        let options = EngineOptions {
            max_steps: 20_000,
            ..EngineOptions::default()
        };
        let lexer = Lexer::new(grammar.lexer().clone());
        (AstParser::with_options(grammar, options), lexer)
    })
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let (parser, lexer) = parser();
    let lexed = lexer.lex(&TextDocument::new(text));
    let _ = parser.initial(lexed.tokens, lexed.text);
});
