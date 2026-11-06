//! Shared test helpers: a JSON pipeline, its edits, and AST readers.

// Three test binaries include this module, so a helper used by only one of
// them is still used; dead-code analysis runs per binary and reports the rest.
#![allow(dead_code)]

use std::sync::Arc;

use reactive::{
    ComponentId, DependencyUpdates, FieldId, GeneralCx, Runtime, UnitKey, UnitKind, ViewSchema,
};
use reactive_syntax::{
    AstMeta, Diagnostic, Grammar, LexerComponent, ParserComponent, SourceFile, StatId,
    StatsSnapshot, Text, TextDocument, TextEdit, Tokens,
};

/// The JSON grammar used by every pipeline test.
#[must_use]
pub fn json_grammar() -> Grammar {
    Grammar::compile(
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
                Document = Value @ast(JsonDocument { value: 0 });
                Value = Object | Array | string | number | ident;
                Object = "{" (Pair ("," Pair)*)? "}" @ast(JsonObject);
                Pair = key: string ":" value: Value @ast(JsonPair { key: 0, value: 2 });
                Array = "[" (Value ("," Value)*)? "]" @ast(JsonArray);
            }
        }
        "#,
    )
    .expect("json grammar")
}

/// One measured edit.
#[derive(Clone, Debug)]
pub struct Row {
    /// Wall time for the whole edit plus demand.
    pub micros: f64,
    /// Significant tokens whose payload was rebuilt.
    pub tokens_relexed: u64,
    /// Bytes relexed.
    pub bytes_relexed: u64,
    /// Significant tokens in the rebuilt window.
    pub tokens_reparsed: u64,
    /// Bytes in the rebuilt window.
    pub bytes_reparsed: u64,
    /// AST nodes the parse built.
    pub nodes_built: u64,
    /// AST nodes added or replaced.
    pub nodes_added: u64,
    /// AST nodes retracted.
    pub nodes_removed: u64,
    /// Metadata units written.
    pub meta_units_written: u64,
    /// Token units the parser read.
    pub token_units_read: u64,
    /// Parser window escalations.
    pub escalations: u64,
    /// Whether the parse spliced the recorded suffix.
    pub adopted: u64,
    /// Runtime executions.
    pub executions: u64,
    /// Failures.
    pub failure: Option<String>,
}

impl Row {
    /// The counters as one snapshot.
    #[must_use]
    pub fn stats(&self) -> StatsSnapshot {
        let mut stats = StatsSnapshot::default();
        stats.set(StatId::TokensReparsed, self.tokens_reparsed);
        stats.set(StatId::BytesReparsed, self.bytes_reparsed);
        stats.set(StatId::NodesBuilt, self.nodes_built);
        stats.set(StatId::NodesAdded, self.nodes_added);
        stats.set(StatId::NodesRemoved, self.nodes_removed);
        stats.set(StatId::MetaUnitsWritten, self.meta_units_written);
        stats.set(StatId::TokenUnitsRead, self.token_units_read);
        stats.set(StatId::Escalations, self.escalations);
        stats.set(StatId::AdoptedSuffix, self.adopted);
        stats
    }
}

/// A stable textual signature of one AST node and its children.
fn walk_signature(node: &reactive_syntax::AstNodeRef, out: &mut String) {
    use std::fmt::Write as _;
    out.push('(');
    if let Some(token) = node.token_id() {
        let _ = write!(out, "t{token}");
    } else if let Some(rule) = node.rule() {
        let _ = write!(out, "r{rule}");
    } else {
        out.push('$');
    }
    for child in node.children() {
        walk_signature(&child, out);
    }
    for child in node.recovered() {
        walk_signature(&child, out);
    }
    out.push(')');
}

/// The same signature with token texts in place of identities.
fn walk_text_signature(
    node: &reactive_syntax::AstNodeRef,
    payloads: &std::collections::HashMap<u64, Arc<str>>,
    out: &mut String,
) {
    use std::fmt::Write as _;
    out.push('(');
    if let Some(token) = node.token_id() {
        out.push_str(
            payloads
                .get(&token)
                .map_or("<missing>", |text| text.as_ref()),
        );
    } else if let Some(rule) = node.rule() {
        let _ = write!(out, "r{rule}");
    } else {
        out.push('$');
    }
    for child in node.children() {
        walk_text_signature(&child, payloads, out);
    }
    for child in node.recovered() {
        walk_text_signature(&child, payloads, out);
    }
    out.push(')');
}

/// A running JSON pipeline.
pub struct Session {
    /// The runtime under test.
    pub runtime: Runtime,
    /// The document currently installed.
    pub document: TextDocument,
}

impl Session {
    /// Builds a pipeline over `text`.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let grammar = json_grammar();
        let runtime = Runtime::new();
        runtime.register_view(<SourceFile as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<Text as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<Tokens as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<AstMeta as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<reactive_syntax::AstTree as ViewSchema>::DESCRIPTOR);
        runtime.register_view(<reactive_syntax::PipelineStats as ViewSchema>::DESCRIPTOR);
        runtime
            .install_general_component(reactive_syntax::TextComponent)
            .expect("text component");
        runtime
            .install_general_component(LexerComponent::new(grammar.lexer().clone()))
            .expect("lexer");
        runtime
            .install_general_component(ParserComponent::new(grammar))
            .expect("parser");
        runtime
            .install_general(reactive_syntax::AstConsumer)
            .expect("root");
        let document = TextDocument::new(text);
        runtime
            .edit(|edit| edit.view::<SourceFile>().set_document(document.clone()))
            .expect("initial edit");
        let _ = runtime.read_outputs_general::<reactive_syntax::AstConsumer>();
        Self { runtime, document }
    }

    /// Applies one text edit and demands the AST.
    pub fn apply(&mut self, edit: TextEdit) -> Row {
        let next = self
            .document
            .clone()
            .edit([edit])
            .expect("text edit applies");
        let before = self.runtime.stats();
        let started = std::time::Instant::now();
        self.runtime
            .edit(|e| e.view::<SourceFile>().set_document(next.clone()))
            .expect("runtime edit");
        let failure = self
            .runtime
            .read_outputs_general::<reactive_syntax::AstConsumer>()
            .err()
            .map(|error| format!("{error}"));
        let micros = started.elapsed().as_secs_f64() * 1e6;
        let after = self.runtime.stats();
        self.document = next;
        let stats = self.stats();
        Row {
            micros,
            tokens_relexed: stats.tokens_relexed,
            bytes_relexed: stats.bytes_relexed,
            tokens_reparsed: stats.tokens_reparsed,
            bytes_reparsed: stats.bytes_reparsed,
            nodes_built: stats.nodes_built,
            nodes_added: stats.nodes_added,
            nodes_removed: stats.nodes_removed,
            meta_units_written: stats.meta_units_written,
            token_units_read: stats.token_units_read,
            escalations: stats.escalations,
            adopted: stats.adopted_suffix,
            executions: after.executions.saturating_sub(before.executions),
            failure,
        }
    }

    /// Replaces one byte range.
    pub fn replace(&mut self, range: std::ops::Range<usize>, text: &str) -> Row {
        self.apply(TextEdit::new(range, text))
    }

    /// Inserts text at `offset`.
    pub fn insert(&mut self, offset: usize, text: &str) -> Row {
        self.apply(TextEdit::insert(offset, text))
    }

    /// Deletes one byte range.
    pub fn delete(&mut self, range: std::ops::Range<usize>) -> Row {
        self.apply(TextEdit::delete(range))
    }

    /// The published work counters.
    #[must_use]
    pub fn stats(&self) -> StatsSnapshot {
        let snapshot = self.runtime.snapshot();
        let stats = snapshot.view::<reactive_syntax::PipelineStats>();
        let mut out = StatsSnapshot::default();
        for (stat, value) in stats.counters() {
            out.set(stat, value);
        }
        out
    }

    /// The AST root.
    #[must_use]
    pub fn root(&self) -> Option<u64> {
        self.runtime
            .snapshot()
            .view::<reactive_syntax::AstTree>()
            .root()
            .map(|node| node.raw())
    }

    /// A stable textual signature of the published AST.
    #[must_use]
    pub fn signature(&self) -> String {
        let snapshot = self.runtime.snapshot();
        let tree = snapshot.view::<reactive_syntax::AstTree>();
        let mut out = String::new();
        if let Some(root) = tree.root() {
            walk_signature(&root, &mut out);
        }
        out
    }

    /// The AST signature with token texts instead of identities.
    #[must_use]
    pub fn text_signature(&self) -> String {
        use std::collections::HashMap;
        let snapshot = self.runtime.snapshot();
        let tokens = snapshot.view::<Tokens>();
        let payloads: HashMap<u64, Arc<str>> = tokens
            .payloads()
            .into_iter()
            .map(|(id, payload)| (id.0, Arc::clone(&payload.text)))
            .collect();
        let tree = snapshot.view::<reactive_syntax::AstTree>();
        let mut out = String::new();
        if let Some(root) = tree.root() {
            walk_text_signature(&root, &payloads, &mut out);
        }
        out
    }

    /// Byte span of one AST node.
    #[must_use]
    pub fn span_of(&self, node: u64) -> Option<std::ops::Range<usize>> {
        let snapshot = self.runtime.snapshot();
        let meta = snapshot.view::<AstMeta>();
        let document = self.document.snapshot();
        let start = document.resolve(
            meta.span_starts()
                .into_iter()
                .find(|(id, _)| *id == node)?
                .1,
        )?;
        let end = document.resolve(meta.span_ends().into_iter().find(|(id, _)| *id == node)?.1)?;
        Some(start..end)
    }

    /// All AST node spans, as byte ranges.
    #[must_use]
    pub fn spans(&self) -> Vec<(u64, std::ops::Range<usize>)> {
        let snapshot = self.runtime.snapshot();
        let meta = snapshot.view::<AstMeta>();
        let document = self.document.snapshot();
        let starts: std::collections::HashMap<u64, _> = meta.span_starts().into_iter().collect();
        let mut out = Vec::new();
        for (id, end) in meta.span_ends() {
            let Some(start) = starts.get(&id) else {
                continue;
            };
            if let (Some(start), Some(end)) = (document.resolve(*start), document.resolve(end)) {
                out.push((id, start..end));
            }
        }
        out
    }

    /// Parser diagnostics published with the AST.
    #[must_use]
    pub fn ast_diagnostics(&self) -> Vec<Diagnostic> {
        self.runtime
            .snapshot()
            .view::<AstMeta>()
            .diagnostics()
            .into_iter()
            .map(|(_, diagnostic)| diagnostic)
            .collect()
    }

    /// Lexical diagnostics published with the tokens.
    #[must_use]
    pub fn lex_diagnostics(&self) -> Vec<Diagnostic> {
        self.runtime
            .snapshot()
            .view::<Tokens>()
            .diagnostics()
            .into_iter()
            .map(|(_, diagnostic)| diagnostic)
            .collect()
    }

    /// Significant token ids in source order.
    #[must_use]
    pub fn significant_tokens(&self) -> Vec<u64> {
        let snapshot = self.runtime.snapshot();
        let tokens = snapshot.view::<Tokens>();
        let trivia: std::collections::HashSet<u64> =
            tokens.trivia().into_iter().map(|id| id.0).collect();
        tokens
            .order()
            .into_iter()
            .filter(|id| !trivia.contains(&id.0))
            .map(|id| id.0)
            .collect()
    }

    /// Token ids reached by the AST, in traversal order.
    #[must_use]
    pub fn ast_tokens(&self) -> Vec<u64> {
        use reactive_syntax::AstNodeRef;
        fn walk(node: &AstNodeRef, out: &mut Vec<u64>, seen: &mut std::collections::HashSet<u64>) {
            if !seen.insert(node.raw()) {
                return;
            }
            if let Some(token) = node.token_id() {
                out.push(token);
                return;
            }
            for child in node.children() {
                walk(&child, out, seen);
            }
            for child in node.recovered() {
                walk(&child, out, seen);
            }
        }
        let snapshot = self.runtime.snapshot();
        let tree = snapshot.view::<reactive_syntax::AstTree>();
        let mut ids = Vec::new();
        let mut seen = std::collections::HashSet::new();
        if let Some(root) = tree.root() {
            walk(&root, &mut ids, &mut seen);
        }
        ids
    }

    /// Number of trivia tokens.
    #[must_use]
    pub fn trivia_count(&self) -> usize {
        self.runtime.snapshot().view::<Tokens>().trivia().len()
    }

    /// Token ids published in `AstMeta`.
    #[must_use]
    pub fn meta_tokens(&self) -> Vec<u64> {
        self.runtime
            .snapshot()
            .view::<AstMeta>()
            .tokens()
            .into_iter()
            .map(|(_, token)| token.0)
            .collect()
    }

    /// Whether any significant token is missing from the AST.
    #[must_use]
    pub fn ast_covers_tokens(&self) -> bool {
        let significant: std::collections::HashSet<u64> =
            self.significant_tokens().into_iter().collect();
        let ast: std::collections::HashSet<u64> = self.ast_tokens().into_iter().collect();
        significant == ast
    }
}

/// A component identity that does not collide with the pipeline's.
pub const TEST_COMPONENT: ComponentId = ComponentId(0x00F0_0001);

/// A helper that reads one unit value.
pub fn read_unit<T: Clone + Send + Sync + 'static>(cx: &mut GeneralCx, unit: UnitKey) -> T {
    cx.read_typed(unit).expect("unit")
}

/// Field key of a scalar view field.
pub fn field(view: reactive::ViewId, index: u16) -> UnitKey {
    UnitKey::field(view, FieldId(index), UnitKind::Value)
}

/// A component that does nothing, used to keep imports honest.
pub struct Noop;

impl reactive::GeneralComponent for Noop {
    type State = ();
    type Output = ();

    const ID: ComponentId = TEST_COMPONENT;
    const NAME: &'static str = "tests::Noop";

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Reset
    }

    fn initialize(&self, _cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }

    fn update(
        &self,
        _cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }
}
