//! A grammar mapped onto a declared tree, published by the parser.
//!
//! The tree here is a `#[tree_view]`; the grammar's `@tree` annotations say which
//! production builds which variant. The parser publishes both trees from the
//! same records, so the typed node identities are the generic ones.

use std::fmt::Write as _;

use irrigation::{Runtime, TreeSchema, ViewSchema, component, tree_view, view};
use irrigation_syntax::{
    Grammar, LexerComponent, ParserComponent, SourceFile, TextComponent, TextDocument,
    TreeMappingError,
};

/// The declared syntax tree. Deliberately shaped like a small language: wrapper
/// free at the top, a list field, and an optional field.
#[tree_view(root = Document, view = Syntax, kind = derived)]
pub mod ast {
    /// A parsed program.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Document {
        /// The program and its body.
        Program { term: Expr },
    }

    /// One expression.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Expr {
        /// A variable reference.
        Var { name: String },
        /// A lambda.
        Lam {
            name: String,
            ty: Ty,
            body: Box<Expr>,
        },
        /// A binding.
        Let {
            name: String,
            value: Box<Expr>,
            body: Box<Expr>,
        },
        /// A conditional.
        If {
            cond: Box<Expr>,
            yes: Box<Expr>,
            no: Box<Expr>,
        },
        /// An application.
        App { func: Box<Expr>, arg: Box<Expr> },
        /// An integer literal. Its payload is `int`, not `value`: `value` is
        /// `Let`'s expression, and one accessor name reads one kind of thing.
        Int { int: i64 },
        /// A boolean literal. `flag`, not `int`: one name, one type.
        Bool { flag: bool },
        /// A bracketed list, which fills a `Vec` child slot.
        List { items: Vec<Expr> },
        /// An optional payload, which fills an optional child slot.
        Opt { item: Option<Box<Expr>> },
    }

    /// A type annotation.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Ty {
        /// `Int`.
        Int,
        /// `Bool`.
        Bool,
    }
}

/// The surface language.
fn grammar() -> Grammar {
    Grammar::compile(
        r#"
        grammar Declared {
            lexer {
                initial root;
                state root {
                    skip ws = /[ \t\r\n]+/;
                    token "program" ; token "end" ; token "\\" ; token ":" ; token "." ;
                    token "(" ; token ")" ; token "let" ; token "=" ; token "in" ;
                    token "if" ; token "then" ; token "else" ; token "[" ; token "]" ;
                    token "," ; token "opt" ; token "!" ; token "Int" ; token "Bool" ;
                    token "true" ; token "false" ;
                    token number = /[0-9]+/;
                    token ident = /[A-Za-z_][A-Za-z0-9_']*/ @word;
                }
            }
            parser {
                Program = "program" term: Expr "end" @tree(Document::Program);
                Expr = Lam | Let | If | App;
                Lam = "\\" name: ident ":" ty: Ty "." body: Expr @tree(Expr::Lam);
                Let = "let" name: ident "=" value: Expr "in" body: Expr @tree(Expr::Let);
                If = "if" cond: Expr "then" yes: Expr "else" no: Expr @tree(Expr::If);
                App = func: App arg: Atom @tree(Expr::App) | Atom;
                Atom = Var | IntLit | BoolLit | List | Opt | Parens;
                Var = name: ident @tree(Expr::Var);
                IntLit = int: number @tree(Expr::Int);
                BoolLit = flag: "true" @tree(Expr::Bool) | flag: "false" @tree(Expr::Bool);
                List = "[" items: (Expr ("," Expr)*)? "]" @tree(Expr::List);
                Opt = "opt" "!" item: Expr? "!" @tree(Expr::Opt);
                Parens = "(" inner: Expr ")" @wrap(inner);
                Ty = TyInt | TyBool;
                TyInt = "Int" @tree(Ty::Int);
                TyBool = "Bool" @tree(Ty::Bool);
            }
        }
        "#,
    )
    .expect("the declared grammar")
}

/// One rendered typed node per line, with the typed root first.
#[view(derived)]
pub struct Typed(List<String>);

/// Reads the declared tree through its generated accessors.
#[component(root)]
pub fn read_typed_tree() -> Typed {
    Typed::from(rows(ast::Syntax.root()))
}

/// One `root=` row, then one row per typed node.
fn rows(root: Option<ast::DocumentRef>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(root) = root {
        out.push(format!("root={}", root.raw()));
        render_document(&root, &mut out);
    }
    out
}

fn render_document(document: &ast::DocumentRef, out: &mut Vec<String>) {
    if let Some(term) = document.term() {
        render_expr(&term, out);
    }
}

fn render_expr(expr: &ast::ExprRef, out: &mut Vec<String>) {
    let mut line = format!(
        "id={} kind={:?}",
        expr.raw(),
        expr.kind().expect("a declared variant")
    );
    match expr.kind() {
        Some(ast::ExprKind::Var) => {
            let _ = write!(line, " name={}", expr.name().unwrap_or_default());
        }
        Some(ast::ExprKind::Lam) | Some(ast::ExprKind::Let) => {
            let _ = write!(line, " name={}", expr.name().unwrap_or_default());
        }
        Some(ast::ExprKind::Int) => {
            let _ = write!(line, " value={}", expr.int().unwrap_or_default());
        }
        Some(ast::ExprKind::Bool) => {
            let _ = write!(line, " value={}", expr.flag().unwrap_or_default());
        }
        Some(ast::ExprKind::List) => {
            let _ = write!(line, " items={}", expr.items().len());
        }
        Some(ast::ExprKind::Opt) => {
            let _ = write!(line, " value={:?}", expr.item().map(|item| item.raw()));
        }
        _ => {}
    }
    for (field, child) in children(expr) {
        let _ = write!(line, " {field}={}", child.raw());
    }
    out.push(line);
    for child in children(expr).into_iter().map(|(_, child)| child) {
        render_expr(&child, out);
    }
}

/// Every child of one typed expression, in field order.
fn children(expr: &ast::ExprRef) -> Vec<(&'static str, ast::ExprRef)> {
    match expr.kind() {
        Some(ast::ExprKind::Lam) => expr
            .body()
            .map(|body| vec![("body", body)])
            .unwrap_or_default(),
        Some(ast::ExprKind::Let) => {
            let mut out = Vec::new();
            if let Some(value) = expr.value() {
                out.push(("value", value));
            }
            if let Some(body) = expr.body() {
                out.push(("body", body));
            }
            out
        }
        Some(ast::ExprKind::If) => {
            let mut out = Vec::new();
            if let Some(cond) = expr.cond() {
                out.push(("cond", cond));
            }
            if let Some(yes) = expr.yes() {
                out.push(("yes", yes));
            }
            if let Some(no) = expr.no() {
                out.push(("no", no));
            }
            out
        }
        Some(ast::ExprKind::App) => {
            let mut out = Vec::new();
            if let Some(func) = expr.func() {
                out.push(("func", func));
            }
            if let Some(arg) = expr.arg() {
                out.push(("arg", arg));
            }
            out
        }
        Some(ast::ExprKind::List) => expr
            .items()
            .into_iter()
            .map(|item| ("item", item))
            .collect(),
        Some(ast::ExprKind::Opt) => expr
            .item()
            .map(|item| vec![("value", item)])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// A running pipeline that publishes both trees.
struct Session {
    runtime: Runtime,
    document: TextDocument,
}

impl Session {
    fn new(text: &str) -> Self {
        let grammar = grammar();
        let runtime = Runtime::new();
        runtime.register_view(<SourceFile as ViewSchema>::DESCRIPTOR);
        runtime
            .install_general_producer(TextComponent)
            .expect("text");
        runtime
            .install_general_producer(LexerComponent::new(grammar.lexer().clone()))
            .expect("lexer");
        runtime
            .install_general_producer(
                ParserComponent::with_tree::<ast::Syntax>(grammar).expect("the grammar maps"),
            )
            .expect("parser");
        runtime.install_root(ReadTypedTree).expect("the probe");
        let document = TextDocument::new(text);
        runtime
            .edit(|edit| edit.view::<SourceFile>().set_document(document.clone()))
            .expect("seed");
        let session = Self { runtime, document };
        session.demand();
        session
    }

    fn demand(&self) {
        if let Err(error) = self.runtime.read_outputs::<ReadTypedTree>() {
            panic!("the pipeline failed: {error}");
        }
    }

    fn edit(&mut self, text: &str) {
        let next = self
            .document
            .clone()
            .replace_with(text)
            .expect("one edit from this document");
        self.runtime
            .edit(|edit| edit.view::<SourceFile>().set_document(next.clone()))
            .expect("edit");
        self.document = next;
        self.demand();
    }

    fn lines(&self) -> Vec<String> {
        self.runtime.snapshot().view::<Typed>().field0()
    }

    fn typed_root(&self) -> Option<u64> {
        self.lines()
            .into_iter()
            .find_map(|line| line.strip_prefix("root=").and_then(|id| id.parse().ok()))
    }
}

#[test]
fn the_parser_publishes_the_declared_tree() {
    let session = Session::new("program let x = 1 in x end");
    let root = session.typed_root().expect("a typed root");
    assert_eq!(session.lines()[0], format!("root={root}"));
    let let_line = session
        .lines()
        .into_iter()
        .find(|line| line.contains("kind=Let"))
        .expect("a let node");
    assert!(let_line.contains("name=x"), "{let_line}");
    let int_line = session
        .lines()
        .into_iter()
        .find(|line| line.contains("kind=Int"))
        .expect("an int node");
    assert!(int_line.contains("value=1"), "{int_line}");
    let var_line = session
        .lines()
        .into_iter()
        .find(|line| line.contains("kind=Var"))
        .expect("a var node");
    assert!(var_line.contains("name=x"), "{var_line}");
}

#[test]
fn a_typed_node_keeps_the_parsers_identity() {
    let session = Session::new("program let x = 1 in x end");
    let snapshot = session.runtime.snapshot();
    let generic = snapshot
        .view::<irrigation_syntax::AstTree>()
        .root()
        .expect("a generic root");
    let typed = snapshot.view::<ast::Syntax>().root().expect("a typed root");
    assert_eq!(
        typed.raw(),
        generic.raw(),
        "the declared node is the parse's node: typed rows {lines:?}",
        lines = session.lines()
    );
    // And a nested node, found through the typed accessors and through the
    // generic tree's children, names the same identity.
    let let_node = typed.term().expect("the body");
    let generic_let = generic
        .children()
        .into_iter()
        .find(|child| child.token().is_none())
        .expect("the body");
    assert_eq!(let_node.raw(), generic_let.raw());
}

#[test]
fn a_wrapper_is_absent_from_the_typed_tree() {
    let session = Session::new("program (1) end");
    // The generic tree keeps the parentheses; the typed tree has only the literal.
    let snapshot = session.runtime.snapshot();
    let generic = snapshot
        .view::<irrigation_syntax::AstTree>()
        .root()
        .expect("a generic root");
    let parens = ["Parens"].into_iter().find_map(|rule| {
        let wanted = irrigation_syntax::rule_id(rule);
        let mut stack = vec![generic.clone()];
        while let Some(node) = stack.pop() {
            if node.rule_id() == Some(wanted) {
                return Some(node);
            }
            stack.extend(node.children());
        }
        None
    });
    assert!(parens.is_some(), "the generic tree keeps the wrapper");
    let typed = snapshot.view::<ast::Syntax>().root().expect("a typed root");
    let literal = typed.term().expect("the body");
    assert_eq!(literal.kind(), Some(ast::ExprKind::Int));
    assert_eq!(
        session.lines().len(),
        2,
        "one line for the program and one for the literal"
    );
}

#[test]
fn a_list_field_takes_every_child_of_a_repetition() {
    let session = Session::new("program [1, 2, 3] end");
    let line = session
        .lines()
        .into_iter()
        .find(|line| line.contains("kind=List"))
        .expect("the list node");
    assert!(
        line.contains("items=3"),
        "the list has three children: {line}"
    );
}

#[test]
fn an_optional_field_reads_absent_and_present() {
    let mut session = Session::new("program opt ! 1 ! end");
    let line = session
        .lines()
        .into_iter()
        .find(|line| line.contains("kind=Opt"))
        .expect("the opt node");
    assert!(!line.contains("value=None"), "{line}");
    session.edit("program opt ! ! end");
    let line = session
        .lines()
        .into_iter()
        .find(|line| line.contains("kind=Opt"))
        .expect("the opt node");
    assert!(line.contains("value=None"), "{line}");
}

#[test]
fn a_broken_edit_leaves_the_generic_tree_without_a_typed_one() {
    let mut session = Session::new("program 1 end");
    assert!(session.typed_root().is_some());
    session.edit("program end");
    let snapshot = session.runtime.snapshot();
    assert!(
        snapshot
            .view::<irrigation_syntax::AstTree>()
            .root()
            .is_some(),
        "the raw parse survives for the board"
    );
    assert!(
        !snapshot
            .view::<irrigation_syntax::AstMeta>()
            .diagnostics()
            .is_empty(),
        "and the parse says why"
    );
}

// ------------------------------------------------------------------ mapping

/// The errors `rules` produces when mapped onto the same declared tree.
fn map_errors(rules: &str) -> Vec<TreeMappingError> {
    let grammar = Grammar::compile(&format!(
        r#"
        grammar Variant {{
            lexer {{
                initial root;
                state root {{
                    skip ws = /[ \t\r\n]+/;
                    token "program" ; token "end" ; token "\\" ; token ":" ; token "." ;
                    token "(" ; token ")" ; token "let" ; token "=" ; token "in" ;
                    token "if" ; token "then" ; token "else" ; token "[" ; token "]" ;
                    token "," ; token "opt" ; token "!" ; token "Int" ; token "Bool" ;
                    token "true" ; token "false" ;
                    token number = /[0-9]+/;
                    token ident = /[A-Za-z_][A-Za-z0-9_']*/ @word;
                }}
            }}
            parser {{
                Number = number;
                {rules}
            }}
        }}
        "#
    ))
    .expect("the variant grammar compiles");
    grammar
        .map_tree(<ast::Syntax as TreeSchema>::LAYOUT)
        .expect_err("the grammar must not map")
}

#[test]
fn the_declared_grammar_maps_cleanly() {
    let grammar = grammar();
    let mapping = grammar
        .map_tree(<ast::Syntax as TreeSchema>::LAYOUT)
        .expect("the shipped grammar maps");
    assert_eq!(
        mapping.len(),
        13,
        "every production with @tree has a plan: program, lam, let, if, app, var, int, \
         bool x2, list, opt, type int, type bool"
    );
}

#[test]
fn a_mapping_names_a_declared_type_and_variant() {
    let errors =
        map_errors("Program = \"program\" term: Expr \"end\" @tree(Nope::Program); Expr = Number;");
    assert!(
        matches!(errors[0], TreeMappingError::UnknownType { .. }),
        "{errors:?}"
    );

    let errors =
        map_errors("Program = \"program\" term: Expr \"end\" @tree(Expr::Nope); Expr = Number;");
    assert!(
        matches!(errors[0], TreeMappingError::UnknownVariant { .. }),
        "{errors:?}"
    );
}

#[test]
fn a_production_and_its_variant_must_agree_on_fields() {
    let errors = map_errors(
        "Program = \"program\" term: Expr extra: Number \"end\" @tree(Document::Program); \
         Expr = Number;",
    );
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, TreeMappingError::UnknownField { field, .. } if field == "extra")),
        "{errors:?}"
    );

    let errors =
        map_errors("Program = \"program\" \"end\" @tree(Document::Program); Expr = Number;");
    assert!(
        errors.iter().any(
            |error| matches!(error, TreeMappingError::MissingField { field, .. } if field == "term")
        ),
        "{errors:?}"
    );
}

#[test]
fn a_field_and_its_position_must_agree_on_shape() {
    // A token fed to a rule-typed field.
    let errors = map_errors(
        "Program = \"program\" term: ident \"end\" @tree(Document::Program); Expr = Number;",
    );
    assert!(
        errors.iter().any(
            |error| matches!(error, TreeMappingError::PositionKind { field, .. } if field == "term")
        ),
        "{errors:?}"
    );

    // A repetition fed to a single-child field.
    let errors = map_errors(
        "Program = \"program\" term: Expr* \"end\" @tree(Document::Program); Expr = Number;",
    );
    assert!(
        errors.iter().any(
            |error| matches!(error, TreeMappingError::Cardinality { field, .. } if field == "term")
        ),
        "{errors:?}"
    );
}

#[test]
fn a_child_rule_must_build_a_node() {
    let errors = map_errors(
        "Program = \"program\" term: Expr \"end\" @tree(Document::Program); \
         Expr = Number;",
    );
    assert!(
        errors.iter().any(|error| matches!(
            error,
            TreeMappingError::UnmappedChildRule { child_rule, .. } if child_rule == "Number"
        )),
        "{errors:?}"
    );
}

#[test]
fn a_wrapper_cannot_build_a_node() {
    let errors = map_errors(
        "Program = \"program\" term: Expr \"end\" @tree(Document::Program) @wrap(term); \
         Expr = Number;",
    );
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, TreeMappingError::WrapAndTree { .. })),
        "{errors:?}"
    );
}

#[test]
fn the_start_rule_must_build_the_root_type() {
    let errors = map_errors(
        "Program = \"program\" term: Number \"end\"; \
         Expr = Number;",
    );
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, TreeMappingError::UnmappedRoot { .. })),
        "{errors:?}"
    );
}
