//! The surface language, as one grammar the syntax pipeline compiles.
//!
//! The start production carries an `@ast` action so the parser publishes a
//! `Program` root; every other rule is a named node the lowering stage can
//! recognise by position.

use irrigation_syntax::Grammar;

/// The seed program.
///
/// The `x` inside `f` must resolve to the *first* `x` even though a second `x`
/// is declared later; the second `x` shadows the first. That is the property the
/// scope graph exists to get right.
pub const SEED: &str = "\
let x = 1 in
let f = \\y:Int. x in
let x = true in
f 2";

/// Compiles the grammar.
///
/// # Panics
///
/// Panics when the grammar is wrong; it is a constant in this example.
#[must_use]
pub fn grammar() -> Grammar {
    Grammar::compile(
        r#"
        grammar Stlc {
            lexer {
                initial root;
                state root {
                    skip ws = /[ \t\r\n]+/;
                    token "\\" ; token ":" ; token "." ; token "->" ; token "(" ; token ")" ; token "=" ;
                    token "let" ; token "in" ; token "if" ; token "then" ; token "else" ;
                    token "Int" ; token "Bool" ; token "true" ; token "false" ;
                    token number = /[0-9]+/;
                    token ident = /[A-Za-z_][A-Za-z0-9_']*/ @word;
                }
            }
            parser {
                Program = Term @ast(Program { term: 0 });
                Term = Lam | Let | If | App;
                Lam = "\\" ident ":" Type "." Term;
                Let = "let" ident "=" Term "in" Term;
                If = "if" Term "then" Term "else" Term;
                App = App Atom | Atom;
                Atom = Variable | Integer | Boolean | Parens;
                Variable = ident;
                Integer = number;
                Boolean = "true" | "false";
                Parens = "(" Term ")";
                Type = TypeArrow | TypeAtom;
                TypeArrow = TypeAtom "->" Type;
                TypeAtom = "Int" | "Bool" | "(" Type ")";
            }
        }
        "#,
    )
    .expect("the STLC grammar")
}
