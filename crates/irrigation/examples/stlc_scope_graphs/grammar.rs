//! The surface language, as one grammar the syntax pipeline compiles.
//!
//! Every production that builds a typed node says so (`@tree`), and every child
//! is named (`name: ident`, `body: Term`): those names are the only link between
//! this grammar and the tree declared in [`super::syntax`]. A rule whose node
//! adds no meaning (`Parens`, `ParenType`) is declared a wrapper instead.

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
                Program = term: Term @tree(Document::Program);
                Term = Lam | Let | If | App;
                Lam = "\\" name: ident ":" ty: Type "." body: Term @tree(Term::Lam);
                Let = "let" name: ident "=" value: Term "in" body: Term @tree(Term::Let);
                If = "if" cond: Term "then" yes: Term "else" no: Term @tree(Term::If);
                App = func: App arg: Atom @tree(Term::App) | Atom;
                Atom = Variable | Integer | Boolean | Parens;
                Variable = name: ident @tree(Term::Var);
                Integer = int: number @tree(Term::Int);
                Boolean = flag: "true" @tree(Term::Bool) | flag: "false" @tree(Term::Bool);
                Parens = "(" inner: Term ")" @wrap(inner);
                Type = TypeArrow | TypeAtom;
                TypeArrow = domain: TypeAtom "->" codomain: Type @tree(Ty::Arrow);
                TypeAtom = IntType | BoolType | ParenType;
                IntType = "Int" @tree(Ty::Int);
                BoolType = "Bool" @tree(Ty::Bool);
                ParenType = "(" inner: Type ")" @wrap(inner);
            }
        }
        "#,
    )
    .expect("the STLC grammar")
}
