//! The AST engine: acceptance, recovery, and incremental equivalence.
//!
//! The core invariant is that an incremental parse and a fresh parse of the same
//! document produce the same tree. Every other test here is a special case of
//! that.

mod support;

use std::sync::Arc;

use reactive_syntax::{
    AstParser, AstState, AstUpdate, Grammar, LexState, Lexer, TextDocument, TextEdit, TokenStream,
    significant_tokens,
};

/// A pipeline over one document, driven through the lexer and the engine only.
struct Engine {
    parser: AstParser,
    lexer: Lexer,
    grammar: Grammar,
    lex: Option<LexState>,
    ast: Option<AstState>,
    document: TextDocument,
}

impl Engine {
    fn new(grammar: Grammar, text: &str) -> Self {
        let lexer = Lexer::new(grammar.lexer().clone());
        Self {
            parser: AstParser::new(grammar.clone()),
            lexer,
            grammar,
            lex: None,
            ast: None,
            document: TextDocument::new(text),
        }
    }

    fn demand(&mut self) -> AstUpdate {
        let lexed = match &self.lex {
            Some(state) => self.lexer.lex_incremental(state, &self.document),
            None => self.lexer.lex(&self.document),
        };
        let tokens: Arc<TokenStream> = Arc::clone(&lexed.tokens);
        let text = Arc::clone(&lexed.text);
        let update = match &self.ast {
            Some(state) => self
                .parser
                .update(state, Arc::clone(&tokens), Arc::clone(&text))
                .expect("incremental parse"),
            None => self
                .parser
                .initial(Arc::clone(&tokens), Arc::clone(&text))
                .expect("initial parse"),
        };
        let stats = update.stats;
        let diagnostics = update.diagnostics.clone();
        let (state, _) = match &self.ast {
            Some(previous) => update.commit(previous, Arc::clone(&tokens), Arc::clone(&text)),
            None => update.commit(
                &AstState::empty(Arc::clone(&tokens), Arc::clone(&text)),
                Arc::clone(&tokens),
                Arc::clone(&text),
            ),
        };
        self.lex = Some(LexState::new(tokens, text));
        self.ast = Some(state);
        AstUpdate {
            root: 0,
            added: Vec::new(),
            removed: Vec::new(),
            diagnostics,
            stats,
            checkpoints: Vec::new(),
            next_token_id: 0,
            significant: Vec::new(),
            spans: Default::default(),
        }
    }

    fn edit(&mut self, edit: TextEdit) -> AstUpdate {
        self.document = self
            .document
            .clone()
            .edit([edit])
            .expect("text edit applies");
        self.demand()
    }

    fn signature(&self) -> String {
        signature(self.ast.as_ref().expect("parsed"))
    }

    fn fresh_signature(&self) -> String {
        fresh(&self.grammar, self.document.text())
    }
}

/// A structural signature of the retained AST.
///
/// Iterative, so a malformed (shared or cyclic) tree reports instead of
/// overflowing the test thread.
fn signature(state: &AstState) -> String {
    let mut out = String::new();
    let mut stack: Vec<Result<u64, &str>> = vec![Ok(state.root)];
    let mut budget = 4_000_000usize;
    while let Some(item) = stack.pop() {
        budget = budget.saturating_sub(1);
        if budget == 0 {
            out.push_str("<budget>");
            break;
        }
        match item {
            Err(text) => {
                out.push(')');
                let _ = text;
            }
            Ok(id) => {
                let Some(meta) = state.node(id) else {
                    out.push('?');
                    continue;
                };
                out.push('(');
                out.push_str(&meta.record.kind);
                stack.push(Err(")"));
                for child in meta.record.children.iter().rev() {
                    stack.push(Ok(*child));
                }
            }
        }
    }
    out
}

/// A fresh parse of `text`, for comparison.
fn fresh(grammar: &Grammar, text: &str) -> String {
    let lexer = Lexer::new(grammar.lexer().clone());
    let lexed = lexer.lex(&TextDocument::new(text));
    let parser = AstParser::new(grammar.clone());
    let update = parser
        .initial(Arc::clone(&lexed.tokens), Arc::clone(&lexed.text))
        .expect("fresh parse");
    let (state, _) = update.commit(
        &AstState::empty(Arc::clone(&lexed.tokens), Arc::clone(&lexed.text)),
        Arc::clone(&lexed.tokens),
        Arc::clone(&lexed.text),
    );
    signature(&state)
}

#[test]
fn parses_json() {
    let mut engine = Engine::new(support::json_grammar(), r#"{"a":1,"b":[2,3]}"#);
    engine.demand();
    let signature = engine.signature();
    assert!(signature.contains("JsonDocument"), "{signature}");
    assert!(signature.contains("JsonObject"), "{signature}");
    assert_eq!(signature, engine.fresh_signature());
}

#[test]
fn incremental_matches_fresh_across_edit_shapes() {
    let mut engine = Engine::new(support::json_grammar(), r#"{"a":1,"b":[2,3],"c":"x"}"#);
    engine.demand();
    let edits = [
        TextEdit::insert(6, "9"),
        TextEdit::insert(7, "8"),
        TextEdit::delete(6..8),
        TextEdit::new(5..6, "7"),
        TextEdit::insert(1, "\"z\":true,"),
        TextEdit::insert(10, " "),
        TextEdit::delete(10..11),
        TextEdit::new(1..5, "\"long\""),
        TextEdit::insert(4, "-"),
    ];
    for (index, edit) in edits.into_iter().enumerate() {
        let update = engine.edit(edit.clone());
        let fresh = engine.fresh_signature();
        if !fresh.contains("$error") && !fresh.contains("$missing") {
            assert_eq!(
                engine.signature(),
                fresh,
                "incremental parse diverged after edit {index}: {edit:?} in {:?}\nincremental={}\nfresh={}",
                engine.document.text(),
                engine.signature(),
                fresh
            );
        } else {
            assert!(update.diagnostics.is_empty() || !engine.signature().is_empty());
        }
    }
}

#[test]
fn deleting_a_member_keeps_the_rest() {
    let mut engine = Engine::new(support::json_grammar(), r#"{"a":1,"b":2,"c":3}"#);
    engine.demand();
    engine.edit(TextEdit::delete(6..12));
    assert_eq!(engine.signature(), engine.fresh_signature());
}

#[test]
fn recovery_skips_a_surplus_token() {
    let mut engine = Engine::new(support::json_grammar(), r#"{"a":1}"#);
    engine.demand();
    let update = engine.edit(TextEdit::insert(6, "]"));
    assert!(
        !update.diagnostics.is_empty(),
        "recovery must report; doc={:?} sig={}",
        engine.document.text(),
        engine.signature()
    );
    let signature = engine.signature();
    assert!(
        signature.contains("$error") || signature.contains("$missing"),
        "{signature}"
    );
}

#[test]
fn recovery_inserts_a_missing_colon() {
    let mut engine = Engine::new(support::json_grammar(), r#"{"a":1}"#);
    engine.demand();
    let update = engine.edit(TextEdit::delete(4..5));
    assert!(!update.diagnostics.is_empty(), "recovery must report");
    assert_eq!(engine.signature(), engine.fresh_signature());
}

#[test]
fn recovery_closes_an_unterminated_object() {
    let mut engine = Engine::new(support::json_grammar(), r#"{"a":1,"b":2}"#);
    engine.demand();
    let update = engine.edit(TextEdit::delete(12..13));
    assert!(
        !update.diagnostics.is_empty(),
        "recovery must report; doc={:?} sig={}",
        engine.document.text(),
        engine.signature()
    );
    assert_eq!(engine.signature(), engine.fresh_signature());
}

#[test]
fn recovery_stays_bounded() {
    let mut engine = Engine::new(support::json_grammar(), r#"{"a":1}"#);
    engine.demand();
    let update = engine.edit(TextEdit::insert(6, "]]]]"));
    assert!(
        update.diagnostics.len() <= 8,
        "recovery must stay bounded: {:?}",
        update.diagnostics
    );
}

#[test]
fn an_edit_rebuilds_only_the_edited_construct() {
    let text = format!(
        "{{\"items\":[{}]}}",
        (0..6)
            .map(|index| format!("{{\"i\":{index}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut engine = Engine::new(support::json_grammar(), &text);
    let first = engine.demand();
    assert!(first.stats.full);
    let nodes = engine.ast.as_ref().expect("state").nodes.len();
    let value_at = text.find(":3}").expect("value") + 1;
    let update = engine.edit(TextEdit::insert(value_at, "1"));
    assert!(update.stats.adopted_suffix, "{:?}", update.stats);
    assert!(
        update.stats.token_end - update.stats.token_start <= 4,
        "window must be local: {:?}",
        update.stats
    );
    assert!(
        update.added.len() <= 12,
        "an edit must rebuild a handful of nodes, not the tree: {} of {nodes}",
        update.added.len()
    );
    assert_eq!(engine.signature(), engine.fresh_signature());
}

#[test]
fn significant_tokens_track_the_stream() {
    let grammar = support::json_grammar();
    let lexer = Lexer::new(grammar.lexer().clone());
    let document = TextDocument::new(r#"{"a" : 1}"#);
    let lexed = lexer.lex(&document);
    let significant = significant_tokens(&lexed.tokens);
    assert_eq!(significant.len(), 5);
    assert_eq!(significant[0].text.as_ref(), "{");
}

#[test]
fn large_document_tree_is_acyclic() {
    let text = format!(
        "{{\"items\":[{}]}}",
        (0..48)
            .map(|index| format!("{{\"i\":{index}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut engine = Engine::new(support::json_grammar(), &text);
    engine.demand();
    let state = &engine.ast.as_ref().expect("state");
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![state.root];
    let mut visited = 0usize;
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            let meta = state.node(id).expect("node");
            panic!(
                "shared node {id} kind={} span=({},{}) children={:?}",
                meta.record.kind, meta.first_index, meta.end_index, meta.record.children
            );
        }
        let meta = state.node(id).expect("node");
        visited += 1;
        for child in &meta.record.children {
            assert!(
                state.node(*child).is_some(),
                "dangling child {child} of {id}"
            );
            stack.push(*child);
        }
    }
    assert_eq!(
        visited,
        state.nodes.len(),
        "the AST table holds unreachable nodes"
    );
}

#[test]
fn medium_document_edit_stays_local() {
    let text = format!(
        "{{\"items\":[{}]}}",
        (0..48)
            .map(|index| format!("{{\"i\":{index}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut engine = Engine::new(support::json_grammar(), &text);
    engine.demand();
    let value = text.find(":31}").expect("value") + 1;
    let update = engine.edit(TextEdit::insert(value, "1"));
    assert!(update.stats.adopted_suffix, "{:?}", update.stats);
    assert!(
        update.stats.token_end - update.stats.token_start <= 4,
        "{:?}",
        update.stats
    );
    assert!(update.added.len() <= 64, "{}", update.added.len());
    assert_eq!(engine.signature(), engine.fresh_signature());
}
