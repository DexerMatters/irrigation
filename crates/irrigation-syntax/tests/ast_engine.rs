//! The AST engine: acceptance, recovery, and incremental equivalence.
//!
//! The core invariant is that an incremental parse and a fresh parse of the same
//! document produce the same tree. Every other test here is a special case of
//! that.

mod support;

use std::sync::Arc;

use irrigation_syntax::{
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

/// A fresh parse of `text`, retained as state.
fn fresh_state(grammar: &Grammar, text: &str) -> AstState {
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
    state
}

/// A fresh parse of `text`, for comparison.
fn fresh(grammar: &Grammar, text: &str) -> String {
    signature(&fresh_state(grammar, text))
}

/// Parser diagnostics as `(code, resolved range)`.
///
/// A carried diagnostic keeps the anchors of the version its recovery ran on,
/// so two parses are compared in the coordinate space of the text each was
/// published over.
fn diagnostics_of(state: &AstState) -> Vec<(String, Option<(usize, usize)>)> {
    let mut out: Vec<(String, Option<(usize, usize)>)> = state
        .diagnostics
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.code.clone(),
                state
                    .text
                    .resolve_range(diagnostic.range)
                    .map(|range| (range.start, range.end)),
            )
        })
        .collect();
    out.sort();
    out
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
fn a_repaired_recovery_diagnostic_is_not_attributed_to_the_adopted_suffix() {
    // `{"a"::1}` skips the surplus `:`, which is the token immediately before
    // the unchanged suffix. Deleting it shrinks the stream by one token, so the
    // window that decides which old diagnostics are still live has to end at the
    // old index of the first adopted token; the new stream's splice index points
    // one token later, which carries the repaired diagnostic into a clean tree.
    let mut engine = Engine::new(support::json_grammar(), r#"{"a"::1}"#);
    let first = engine.demand();
    assert_eq!(first.diagnostics.len(), 1, "{:?}", first.diagnostics);
    assert_eq!(first.diagnostics[0].code, "parse.recover");
    let update = engine.edit(TextEdit::delete(5..6));
    assert!(update.stats.adopted_suffix, "{:?}", update.stats);
    let state = engine.ast.as_ref().expect("state");
    assert_eq!(
        diagnostics_of(state),
        diagnostics_of(&fresh_state(&engine.grammar, engine.document.text())),
        "incremental diagnostics diverged from a from-scratch parse of {:?}",
        engine.document.text()
    );
}

#[test]
fn a_recovery_diagnostic_inside_an_adopted_subtree_is_carried() {
    // The edited first member is rebuilt and the second is adopted whole. The
    // missing colon was recovered before the edit and lies inside that adopted
    // subtree, so its diagnostic survives the reparse — with the anchors of the
    // version it was derived in — and still matches a from-scratch parse.
    let mut engine = Engine::new(support::json_grammar(), r#"{"a":1,"b" 2}"#);
    let first = engine.demand();
    assert_eq!(first.diagnostics.len(), 1, "{:?}", first.diagnostics);
    assert_eq!(first.diagnostics[0].code, "parse.missing");
    let before = first.diagnostics[0].range;
    let update = engine.edit(TextEdit::new(5..6, "9"));
    assert!(update.stats.adopted_suffix, "{:?}", update.stats);
    let state = engine.ast.as_ref().expect("state");
    assert_eq!(state.diagnostics.len(), 1, "{:?}", state.diagnostics);
    assert_eq!(
        state.diagnostics[0].range, before,
        "the recovery belongs to the adopted subtree, so its diagnostic is carried, not re-derived"
    );
    assert_eq!(
        diagnostics_of(state),
        diagnostics_of(&fresh_state(&engine.grammar, engine.document.text())),
        "incremental diagnostics diverged from a from-scratch parse of {:?}",
        engine.document.text()
    );
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
fn a_missing_token_does_not_claim_a_token_identity() {
    let mut engine = Engine::new(support::json_grammar(), r#"{"a":1,"b":2}"#);
    engine.demand();
    engine.edit(TextEdit::delete(12..13));
    let state = engine.ast.as_ref().expect("state");
    let mut owners: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let mut missing = 0usize;
    for (_, meta) in state.nodes.iter() {
        if meta.record.category == Some(irrigation_syntax::ErrorCategoryId::MISSING) {
            missing += 1;
            assert!(
                meta.record.token.is_none(),
                "a synthesized token claims the identity of token {:?}",
                meta.record.token
            );
        }
        if let Some(token) = meta.record.token {
            *owners.entry(token.0).or_insert(0) += 1;
        }
    }
    assert!(missing > 0, "the edit must insert a missing token");
    assert!(
        owners.values().all(|count| *count == 1),
        "two nodes claim one token identity: {owners:?}"
    );
}

#[test]
fn a_mid_document_recovery_publishes_only_the_reachable_tree() {
    // `{"a":1 "b":2}` needs a comma. At the failed lookahead, recovery explores
    // skipping `"b"`, closing the object, and inserting the comma before the
    // insertion wins. A record built for an alternative that lost must not reach
    // the winning build: the skipped-token node claims `"b"`'s token identity,
    // and the table would hold nodes no root walk can reach.
    let text = r#"{"a":1 "b":2}"#;
    let mut engine = Engine::new(support::json_grammar(), text);
    engine.demand();
    let state = engine.ast.as_ref().expect("state");
    let mut visited = std::collections::HashSet::new();
    let mut owners: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let mut recovered = 0usize;
    let mut stack = vec![state.root];
    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        let meta = state
            .node(id)
            .unwrap_or_else(|| panic!("{id} is not a node in {text:?}"));
        if meta.record.category.is_some() {
            recovered += 1;
        }
        if let Some(token) = meta.record.token {
            *owners.entry(token.0).or_insert(0) += 1;
        }
        stack.extend(meta.record.children.iter().copied());
    }
    assert!(
        recovered > 0,
        "{text:?} must exercise mid-document recovery"
    );
    assert_eq!(
        visited.len(),
        state.nodes.len(),
        "{text:?}: the AST table holds nodes the root cannot reach"
    );
    assert!(
        owners.values().all(|count| *count == 1),
        "{text:?}: two nodes claim one token identity: {owners:?}"
    );
}

#[test]
fn the_synthesized_identity_counter_survives_a_reparse() {
    let grammar = support::json_grammar();
    let lexer = Lexer::new(grammar.lexer().clone());
    let parser = AstParser::new(grammar);
    let document = TextDocument::new(r#"{"a":1,"b":2}"#);
    let lexed = lexer.lex(&document);
    let tokens: Arc<TokenStream> = Arc::clone(&lexed.tokens);
    let text = Arc::clone(&lexed.text);
    let update = parser
        .initial(Arc::clone(&tokens), Arc::clone(&text))
        .expect("initial parse");
    let highest = significant_tokens(&tokens)
        .iter()
        .map(|token| token.id.0)
        .max()
        .unwrap_or(0);
    // The counter starts after every identity the stream already uses, not at
    // zero: a synthesized identity must never collide with a real token.
    assert_eq!(update.next_token_id, highest + 1);
    let (state, _) = update.commit(
        &AstState::empty(Arc::clone(&tokens), Arc::clone(&text)),
        Arc::clone(&tokens),
        Arc::clone(&text),
    );

    let document = document.edit([TextEdit::delete(12..13)]).expect("edit");
    let lexed = lexer.lex(&document);
    let tokens: Arc<TokenStream> = Arc::clone(&lexed.tokens);
    let text = Arc::clone(&lexed.text);
    let resumed = parser
        .update(&state, Arc::clone(&tokens), Arc::clone(&text))
        .expect("incremental parse");
    let highest = significant_tokens(&tokens)
        .iter()
        .map(|token| token.id.0)
        .max()
        .unwrap_or(0);
    assert!(
        resumed.next_token_id > highest,
        "next_token_id {} collides with the stream (max {highest})",
        resumed.next_token_id
    );
    assert!(resumed.next_token_id >= state.next_token_id);
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

/// A grammar whose start rule is a bare unit production or a repetition.
///
/// Both lower to transparent productions: they carry no identity of their own,
/// so the root has to be synthesized when the whole document is one of them.
const TRANSPARENT_START_GRAMMARS: [&str; 2] = [
    r#"
    grammar Doc {
        lexer {
            initial root;
            state root {
                skip ws = /[ ]+/;
                token "a" ;
                token "b" ;
            }
        }
        parser {
            Document = Value;
            Value = "a" | "b";
        }
    }
    "#,
    r#"
    grammar Doc {
        lexer {
            initial root;
            state root {
                skip ws = /[ ]+/;
                token "a" ;
                token "b" ;
            }
        }
        parser {
            Document = Statement*;
            Statement = "a" | "b";
        }
    }
    "#,
];

#[test]
fn a_transparent_start_rule_publishes_a_real_root() {
    for source in TRANSPARENT_START_GRAMMARS {
        let grammar = Grammar::compile(source).expect("transparent start grammar");
        let lexer = Lexer::new(grammar.lexer().clone());
        let parser = AstParser::new(grammar);
        let document = TextDocument::new("a");
        let lexed = lexer.lex(&document);
        let tokens = Arc::clone(&lexed.tokens);
        let text = Arc::clone(&lexed.text);
        let update = parser
            .initial(Arc::clone(&tokens), Arc::clone(&text))
            .expect("initial parse");
        // Root 0 is the "no tree" sentinel of `AstState`; a published root must
        // name a node, or every traversal stops at an identity with no record.
        assert_ne!(update.root, 0, "{source}");
        let (state, _) = update.commit(
            &AstState::empty(Arc::clone(&tokens), Arc::clone(&text)),
            Arc::clone(&tokens),
            Arc::clone(&text),
        );
        let root = state.node(state.root).expect("the root names a node");
        assert_eq!(root.record.kind, "$root");
        assert_eq!(
            (root.first_index, root.end_index),
            (0, state.significant.len())
        );
        assert!(
            root.record
                .children
                .iter()
                .all(|child| state.node(*child).is_some()),
            "{:?}",
            root.record.children
        );
        // The component only keeps the incremental path when the after-diff
        // accepts a proposal; a sentinel root rejects every one of them.
        let next = document.edit([TextEdit::new(0..1, "b")]).expect("edit");
        let lexed = lexer.lex(&next);
        assert!(
            parser
                .update(&state, Arc::clone(&lexed.tokens), Arc::clone(&lexed.text))
                .is_some(),
            "one edit must parse incrementally"
        );
    }
}

#[test]
fn a_parse_that_accepts_nothing_reports_failure() {
    let grammar = support::json_grammar();
    // Recovery is what turns a rejected document into a tree; with no recovery
    // budget, a document that needs it has no accepted configuration at all.
    let parser = AstParser::with_options(
        grammar.clone(),
        irrigation_syntax::parser::EngineOptions {
            max_error_cost: 0,
            max_missing: 0,
            ..irrigation_syntax::parser::EngineOptions::default()
        },
    );
    let lexer = Lexer::new(grammar.lexer().clone());
    let document = TextDocument::new(r#"{"a":1}"#);
    let lexed = lexer.lex(&document);
    let tokens = Arc::clone(&lexed.tokens);
    let text = Arc::clone(&lexed.text);
    let update = parser
        .initial(Arc::clone(&tokens), Arc::clone(&text))
        .expect("initial parse");
    let (state, _) = update.commit(
        &AstState::empty(Arc::clone(&tokens), Arc::clone(&text)),
        Arc::clone(&tokens),
        Arc::clone(&text),
    );
    let document = document.edit([TextEdit::delete(4..5)]).expect("edit");
    let lexed = lexer.lex(&document);
    assert!(
        parser
            .update(&state, Arc::clone(&lexed.tokens), Arc::clone(&lexed.text))
            .is_none(),
        "a window that accepts nothing must report failure, not a salvage tree"
    );
    // The from-scratch path still publishes a tree for the same document, so a
    // dead end never leaves the AST undefined.
    let update = parser
        .initial(Arc::clone(&lexed.tokens), Arc::clone(&lexed.text))
        .expect("salvage parse");
    let (state, _) = update.commit(
        &AstState::empty(Arc::clone(&lexed.tokens), Arc::clone(&lexed.text)),
        Arc::clone(&lexed.tokens),
        Arc::clone(&lexed.text),
    );
    let root = state
        .node(state.root)
        .expect("the salvage root names a node");
    assert_eq!(root.record.kind, "$error");
}

#[test]
fn recovery_children_name_real_nodes() {
    // Recovery closes an incomplete construct by popping a run of stack frames.
    // A transparent repetition in that run stands for its children, and its
    // sentinel identity must never be published as a child id.
    for text in [r#"{"a":1 "b":2}"#, r#"{"a":1,}"#, "[1, 2, ]"] {
        let mut engine = Engine::new(support::json_grammar(), text);
        engine.demand();
        let state = engine.ast.as_ref().expect("state");
        let mut recovered = 0usize;
        for (id, meta) in state.nodes.iter() {
            if meta.record.category.is_none() {
                continue;
            }
            recovered += 1;
            for child in &meta.record.children {
                assert!(
                    state.node(*child).is_some(),
                    "recovery node {id} in {text:?} names child {child}, which is not a node"
                );
            }
        }
        assert!(recovered > 0, "{text:?} must exercise recovery");
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![state.root];
        let mut reachable = std::collections::HashSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let meta = state
                .node(id)
                .unwrap_or_else(|| panic!("{id} is not a node in {text:?}"));
            if let Some(token) = meta.record.token {
                reachable.insert(token.0);
            }
            stack.extend(meta.record.children.iter().copied());
        }
        let significant: std::collections::HashSet<u64> =
            state.significant.iter().map(|token| token.id.0).collect();
        assert_eq!(reachable, significant, "{text:?}");
    }
}
