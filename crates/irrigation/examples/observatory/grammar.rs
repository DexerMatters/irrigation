//! The demo language, as one grammar the syntax pipeline compiles.
//!
//! A line per statement: an assignment (`name := value`) or a print
//! (`print(value, ...)`). Expressions are names, integers, and calls. The
//! grammar is deliberately tiny; the board's interest is in the pipeline, not
//! the language. Every production that builds a node names the tree variant it
//! builds (`@tree`), and every child it feeds is named (`name: ident`).

use irrigation_syntax::{Grammar, TextDocument};

/// The seed program.
pub const SEED: &str = "\
x := 1
y := 2
print(show(x), y)";

/// The seed document.
///
/// # Panics
///
/// Panics only when the fixed seed violates the document's invariants.
#[must_use]
pub fn seed() -> TextDocument {
    TextDocument::new(SEED)
}

/// Compiles the grammar.
///
/// # Panics
///
/// Panics when the grammar is wrong; it is a constant in this example.
#[must_use]
pub fn grammar() -> Grammar {
    Grammar::compile(
        r#"
        grammar Demo {
            lexer {
                initial root;
                state root {
                    skip ws = /[ \t\r\n]+/;
                    token number = /-?[0-9]+/;
                    // Keywords before the identifier rule: a tie on length goes
                    // to the earlier rule, so `print` must not lex as a name.
                    token "print" ;
                    token ident = /[A-Za-z_][A-Za-z0-9_]*/ @word;
                    token ":=" ; token "(" ; token ")" ; token "," ;
                }
            }
            parser {
                Program = items: Statement* @tree(Document::Program);
                Statement = Assign | Print;
                Assign = name: ident ":=" value: Expression @tree(Statement::Assign);
                Print = "print" "(" args: (Expression ("," Expression)*)? ")" @tree(Statement::Print);
                Expression = Call | Number | Name;
                Call = callee: ident "(" args: (Expression ("," Expression)*)? ")" @tree(Expression::Call);
                Number = value: number @tree(Expression::Number);
                Name = name: ident @tree(Expression::Name);
            }
        }
        "#,
    )
    .expect("the demo grammar")
}
