//! The grammar's declarations, published: wrappers, payloads, and the readers.
//!
//! A reader reads a View, so it is ambient: these tests read inside a component
//! and assert on what it published, which is also how a host uses them.

use std::sync::Arc;

use irrigation::{Runtime, ViewSchema, component, view};
use irrigation_syntax::{
    AstMeta, AstNodeRef, AstTree, Grammar, GrammarError, LexerComponent, ParserComponent, RuleId,
    SourceFile, Text, TextComponent, TextDocument, TokenId, Tokens,
};

/// A grammar with a wrapper root, a wrapper for parentheses, named fields, and
/// two payload kinds.
fn grammar() -> Grammar {
    Grammar::compile(
        r#"
        grammar Readers {
            lexer {
                initial root;
                state root {
                    skip ws = /[ \t\r\n]+/;
                    token "program" ; token "end" ; token "(" ; token ")" ; token ":=" ;
                    token "true" ; token "false" ;
                    token number = /[0-9]+/;
                    token ident = /[A-Za-z_][A-Za-z0-9_]*/ @word;
                }
            }
            parser {
                Program = "program" stmt: Statement "end" @wrap(stmt);
                Statement = Assign | Number | Boolean | Parens;
                Assign = name: ident ":=" value: Atom;
                Atom = Assign | Number | Boolean | Parens;
                Parens = "(" inner: Atom ")" @wrap(inner);
                Number = value: number;
                Boolean = value: "true" | value: "false";
            }
        }
        "#,
    )
    .expect("the readers grammar")
}

/// What the probe component found, one line per node.
#[view(derived)]
pub struct Findings(List<String>);

/// Reads every reader for every node, so a test can assert on one row.
///
/// The readers are ambient, so this is the only place a test can call them.
#[component(root)]
pub fn observe_tree() -> Findings {
    Findings::from(rows(AstTree.root()))
}

/// One row per node: identity, rule, payload, wrapper, and source range.
fn rows(root: Option<AstNodeRef>) -> Vec<String> {
    let document = Text.document();
    let mut out = Vec::new();
    if let Some(root) = root {
        collect(&root, &document, &mut out);
    }
    out
}

fn collect(node: &AstNodeRef, document: &TextDocument, out: &mut Vec<String>) {
    out.push(describe(node, document));
    for child in node.children() {
        collect(&child, document, out);
    }
}

fn describe(node: &AstNodeRef, document: &TextDocument) -> String {
    let rule = node
        .rule_id()
        .map_or_else(String::new, |rule| rule.to_string());
    let token = node
        .token()
        .map_or_else(String::new, |token| token.0.to_string());
    let integer = node
        .integer()
        .map_or_else(String::new, |value| value.to_string());
    let boolean = node
        .boolean()
        .map_or_else(String::new, |value| value.to_string());
    let text = node.text().unwrap_or_default();
    let value = node
        .value()
        .map_or_else(String::new, |value| format!("{value:?}"));
    let semantic = node.semantic().raw().to_string();
    let wrapped = node
        .wrapped()
        .map_or_else(String::new, |inner| inner.raw().to_string());
    let source = node
        .range_in(document)
        .and_then(|range| document.text().get(range))
        .map_or_else(String::new, str::to_owned);
    format!(
        "id={};rule={rule};token={token};int={integer};bool={boolean};text={text};\
         value={value};sem={semantic};wrap={wrapped};src={source};",
        node.raw()
    )
}

/// A running pipeline over the readers grammar.
struct Session {
    runtime: Runtime,
    document: TextDocument,
}

impl Session {
    fn new(text: &str) -> Self {
        let grammar = grammar();
        let runtime = Runtime::new();
        runtime.register_view(<SourceFile as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<Text as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<Tokens as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<AstMeta as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<AstTree as ViewSchema>::DESCRIPTOR);
        runtime
            .install_general_producer(TextComponent)
            .expect("text");
        runtime
            .install_general_producer(LexerComponent::new(grammar.lexer().clone()))
            .expect("lexer");
        runtime
            .install_general_producer(ParserComponent::new(grammar))
            .expect("parser");
        runtime.install_root(ObserveTree).expect("the probe");
        let document = TextDocument::new(text);
        runtime
            .edit(|edit| edit.view::<SourceFile>().set_document(document.clone()))
            .expect("seed");
        let session = Self { runtime, document };
        session.demand();
        session
    }

    fn demand(&self) {
        if let Err(error) = self.runtime.read_outputs::<ObserveTree>() {
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

    fn root(&self) -> AstNodeRef {
        self.runtime
            .snapshot()
            .view::<AstTree>()
            .root()
            .expect("a published tree")
    }

    /// The probe's row for one node.
    fn row(&self, node: &AstNodeRef) -> String {
        let rows = self.runtime.snapshot().view::<Findings>().field0();
        let prefix = format!("id={};", node.raw());
        rows.into_iter()
            .find(|row| row.starts_with(&prefix))
            .unwrap_or_else(|| panic!("no probe row for node {}", node.raw()))
    }

    /// The probe's rows that name `rule`.
    fn rows_for(&self, rule: &str) -> Vec<String> {
        let wanted = irrigation_syntax::rule_id(rule).to_string();
        self.runtime
            .snapshot()
            .view::<Findings>()
            .field0()
            .into_iter()
            .filter(|row| row.contains(&format!(";rule={wanted};")))
            .collect()
    }

    fn wraps(&self) -> Vec<(u64, u64)> {
        self.runtime.snapshot().view::<AstMeta>().wraps().to_vec()
    }
}

/// The first node with `rule` in the subtree, in source order.
fn find(node: &AstNodeRef, rule: RuleId) -> Option<AstNodeRef> {
    if node.rule_id() == Some(rule) {
        return Some(node.clone());
    }
    for child in node.children() {
        if let Some(found) = find(&child, rule) {
            return Some(found);
        }
    }
    None
}

#[test]
fn a_wrapper_is_published_and_skipped() {
    let session = Session::new("program x := (1) end");
    let root = session.root();
    assert_eq!(root.rule_id(), Some(irrigation_syntax::rule_id("Program")));
    assert_eq!(
        session.wraps().len(),
        2,
        "the root and the parentheses are the wrappers"
    );

    let row = session.row(&root);
    let assign = find(&root, irrigation_syntax::rule_id("Assign")).expect("the assignment");
    assert!(
        row.contains(&format!(";sem={};", assign.raw())),
        "the root's semantic node is the statement it wraps: {row}"
    );

    let parens = find(&root, irrigation_syntax::rule_id("Parens")).expect("the parens node");
    let number = find(&root, irrigation_syntax::rule_id("Number")).expect("the literal");
    let row = session.row(&parens);
    assert!(
        row.contains(&format!(";sem={};", number.raw())),
        "parentheses mean the literal inside them: {row}"
    );
}

#[test]
fn a_deleted_wrapper_relation_is_retracted() {
    let mut session = Session::new("program x := (1) end");
    assert_eq!(session.wraps().len(), 2);
    session.edit("program x := 1 end");
    assert_eq!(
        session.wraps().len(),
        1,
        "removing the parentheses retracts their wrapper unit, and nothing else"
    );
    let mut live = Vec::new();
    let mut stack = vec![session.root()];
    while let Some(node) = stack.pop() {
        live.push(node.raw());
        stack.extend(node.children());
    }
    for (wrapper, inner) in session.wraps() {
        assert!(live.contains(&wrapper), "every published wrapper is live");
        assert!(live.contains(&inner), "and so is its payload");
    }
}

#[test]
fn readers_read_the_published_payloads() {
    let session = Session::new("program x := 7 end");
    let root = session.root();
    let number = find(&root, irrigation_syntax::rule_id("Number")).expect("the literal");
    // A rule node carries no payload of its own: the literal is the token leaf
    // underneath it, and that leaf is what the parser classified.
    let literal = number
        .children()
        .into_iter()
        .next()
        .expect("the literal token");
    let row = session.row(&literal);
    assert!(
        row.contains(";int=7;"),
        "the payload is the parser's: {row}"
    );
    assert!(row.contains(";value=Int(7);"), "{row}");
    assert!(
        row.contains(";bool=;"),
        "an integer has no boolean value: {row}"
    );
    assert!(
        row.contains(";src=7;"),
        "the span resolves to the literal: {row}"
    );

    let assign = find(&root, irrigation_syntax::rule_id("Assign")).expect("the assignment");
    let name = assign
        .children()
        .into_iter()
        .find(|child| child.token().is_some())
        .expect("the name token");
    let row = session.row(&name);
    assert!(row.contains(";text=x;"), "the token's text: {row}");
    assert!(row.contains(";token="), "and a typed token identity: {row}");
    assert!(matches!(name.token(), Some(TokenId(_))));
    assert_eq!(name.text().as_deref(), Some("x"));
}

#[test]
fn a_boolean_literal_reads_as_a_boolean() {
    let session = Session::new("program x := true end");
    let root = session.root();
    let node = find(&root, irrigation_syntax::rule_id("Boolean")).expect("the literal");
    // As with an integer, the rule node has no payload: its token leaf does.
    let literal = node
        .children()
        .into_iter()
        .next()
        .expect("the literal token");
    let row = session.row(&literal);
    assert!(row.contains(";bool=true;"), "{row}");
    assert!(
        row.contains(";int=;"),
        "a boolean has no integer value: {row}"
    );
}

#[test]
fn the_whole_tree_reads_the_same_way() {
    let session = Session::new("program x := 7 end");
    assert_eq!(
        session.rows_for("Number").len(),
        1,
        "one row per rule node, published by one component"
    );
    let root = session.root();
    let row = session.row(&root);
    assert!(
        row.contains(";src=program x := 7 end;"),
        "the root covers the whole program: {row}"
    );
}

#[test]
fn the_rule_name_map_is_keyed_by_rule_identity() {
    let session = Session::new("program 1 end");
    let rules = session
        .runtime
        .snapshot()
        .view::<AstMeta>()
        .rules()
        .to_vec();
    let number: Arc<str> = rules
        .into_iter()
        .find(|(rule, _)| *rule == irrigation_syntax::rule_id("Number"))
        .map(|(_, name)| name)
        .expect("the rule is published");
    assert_eq!(number.as_ref(), "Number");
}

// ------------------------------------------------------------------ grammar

const BASE: &str = r#"
    grammar Rules {
        lexer {
            initial root;
            state root {
                skip ws = /[ \t\r\n]+/;
                token "(" ; token ")" ; token ":=" ; token "~" ;
                token number = /[0-9]+/;
                token ident = /[A-Za-z_][A-Za-z0-9_]*/ @word;
            }
        }
        parser {
            Number = number;
            PROGRAM
        }
    }
"#;

fn with_rules(rules: &str) -> String {
    BASE.replace("PROGRAM", rules)
}

/// The error message `source` fails to compile with.
fn compile_error(source: &str) -> String {
    match Grammar::compile(source) {
        Ok(_) => panic!("the grammar compiled"),
        Err(GrammarError::Spec { message, .. }) => message,
        Err(other) => panic!("expected a spec error, got {other}"),
    }
}

#[test]
fn an_inline_field_and_an_ast_field_must_agree() {
    let agree = Grammar::compile(&with_rules("Assign = name: ident \":=\" value: Number;"))
        .expect("the agreeing grammar compiles");
    let production = agree
        .productions()
        .iter()
        .find(|production| production.label == "Assign")
        .expect("the production");
    assert_eq!(production.fields.len(), 2, "both fields are declared");

    let message = compile_error(&with_rules(
        "Assign = name: ident \":=\" value: Number @ast(Assign { name: 2 });",
    ));
    assert!(
        message.contains("field `name`") && message.contains("position 0"),
        "{message}"
    );
}

#[test]
fn an_ast_field_position_must_exist() {
    let message = compile_error(&with_rules(
        "Assign = name: ident \":=\" Number @ast(Assign { extra: 5 });",
    ));
    assert!(message.contains("names position 5"), "{message}");
}

#[test]
fn a_wrap_must_name_a_declared_rule_field() {
    let message = compile_error(&with_rules(
        "Parens = \"(\" inner: Number \")\" @wrap(atom);",
    ));
    assert!(message.contains("unknown field `atom`"), "{message}");

    let message = compile_error(&with_rules(
        "Assign = name: ident \":=\" Number;\n            Parens = \"(\" inner: Number \")\" @wrap(name);",
    ));
    assert!(message.contains("declared on this production"), "{message}");

    let message = compile_error(&with_rules("Parens = \"(\" name: ident \")\" @wrap(name);"));
    assert!(message.contains("must name a rule"), "{message}");
}

#[test]
fn a_unit_production_needs_no_wrap() {
    let message = compile_error(&with_rules("Alias = other: Number @wrap(other);"));
    assert!(message.contains("redundant"), "{message}");
}
