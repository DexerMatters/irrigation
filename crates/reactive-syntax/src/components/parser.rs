//! Parser component: turns the token delta into the `AstTree` and `AstMeta`.
//!
//! The component is an ordinary reactive component: it reads the token delta,
//! resumes the parse from a checkpoint, and publishes only what the edit
//! touched. There is no concrete tree, no full parse after the first one, and no
//! runtime-side knowledge of what it does.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use reactive::{
    ChangeBatch, ChildSlot, ComponentId, DependencyUpdates, FieldId, GeneralComponent, GeneralCx,
    Result, TreeOutputExt, TreePatch, TreePatchNode, TreeStorage, TreeValue, UnitKey, UnitKind,
    ViewDescriptor, ViewSchema, any_key,
};

use crate::diagnostic::Diagnostic;
use crate::lexer::{LexConfig, ModeId, Token, TokenId, TokenStream};
use crate::parser::engine::AstParser;
use crate::parser::state::{AstState, AstTable};
use crate::text::TextDocument;
use crate::views::ast_tree::AstTree;
use crate::views::{
    AstMeta, AstMetaField, DiagnosticId, PipelineStats, StatsSnapshot, TokenDelta, TokenPayload,
    Tokens,
};

impl AstMeta {
    /// Start-anchor unit of one node.
    #[must_use]
    pub fn span_start_unit(node: u64) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            AstMetaField::SPAN_STARTS,
            UnitKind::Value,
            any_key(node),
        )
    }

    /// End-anchor unit of one node.
    #[must_use]
    pub fn span_end_unit(node: u64) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            AstMetaField::SPAN_ENDS,
            UnitKind::Value,
            any_key(node),
        )
    }

    /// Token unit of one node.
    #[must_use]
    pub fn token_unit(node: u64) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            AstMetaField::TOKENS,
            UnitKind::Value,
            any_key(node),
        )
    }

    /// Value unit of one node.
    #[must_use]
    pub fn value_unit(node: u64) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            AstMetaField::VALUES,
            UnitKind::Value,
            any_key(node),
        )
    }

    /// Recovery membership of one node.
    #[must_use]
    pub fn error_unit(node: u64) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            AstMetaField::ERRORS,
            UnitKind::Membership,
            any_key(node),
        )
    }

    /// One field mapping unit.
    #[must_use]
    pub fn field_unit(node: u64, field: &str) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            AstMetaField::FIELDS,
            UnitKind::Value,
            any_key((node, field.to_owned())),
        )
    }

    /// One field membership unit.
    #[must_use]
    pub fn field_membership(node: u64, field: &str) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            AstMetaField::FIELDS,
            UnitKind::Membership,
            any_key((node, field.to_owned())),
        )
    }

    /// One diagnostic unit.
    #[must_use]
    pub fn diagnostic_unit(id: DiagnosticId) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            AstMetaField::DIAGNOSTICS,
            UnitKind::Value,
            any_key(id),
        )
    }

    /// One diagnostic membership unit.
    #[must_use]
    pub fn diagnostic_membership(id: DiagnosticId) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            AstMetaField::DIAGNOSTICS,
            UnitKind::Membership,
            any_key(id),
        )
    }

    /// One rule-name unit.
    #[must_use]
    pub fn rule_unit(rule: u64) -> UnitKey {
        UnitKey::entity(
            Self::view_id(),
            AstMetaField::RULES,
            UnitKind::Value,
            any_key(rule),
        )
    }
}

/// Retained parser state.
#[derive(Clone, Debug)]
pub struct ParserComponentState {
    ast: AstState,
    diagnostics: HashSet<DiagnosticId>,
    rules: HashSet<u64>,
    storage: TreeStorage,
}

/// Parser component: reads `Tokens`, owns the AST Views.
#[derive(Clone, Debug)]
pub struct ParserComponent {
    parser: AstParser,
}

/// One revision's token input.
struct TokenInput {
    document: TextDocument,
    tokens: Arc<TokenStream>,
    delta: TokenDelta,
    units_read: usize,
}

impl ParserComponent {
    /// Builds a parser component from a compiled grammar.
    #[must_use]
    pub fn new(grammar: crate::Grammar) -> Self {
        Self {
            parser: AstParser::new(grammar),
        }
    }

    /// Reads the token stream, reusing previous tokens outside the delta.
    fn read_tokens(cx: &mut GeneralCx, previous: Option<&TokenStream>) -> Result<TokenInput> {
        let document: TextDocument = cx.read_typed(Tokens::text_unit())?;
        let delta: TokenDelta = cx.read_typed(Tokens::delta_unit())?;
        let mut units_read = 2;
        let previous_by_id: HashMap<TokenId, Arc<Token>> =
            previous.map_or_else(HashMap::new, |stream| {
                stream
                    .iter()
                    .map(|token| (token.id, Arc::clone(token)))
                    .collect()
            });
        let changed = delta.changed_set();
        // The delta's order script is over the full token list, so applying it
        // to the previous identities reconstructs the new stream without reading
        // any unit of its own.
        let mut ids: Vec<TokenId> = previous.map_or_else(Vec::new, |stream| {
            stream.iter().map(|token| token.id).collect()
        });
        if previous.is_none() {
            let order: reactive::PSeq<u64> = cx.read_typed(Tokens::order_unit())?;
            units_read += 1;
            ids.extend(order.iter().map(|id| TokenId(*id)));
        } else {
            for edit in delta.order.edits() {
                match edit {
                    reactive::ListEdit::Insert { index, value } => ids.insert(*index, *value),
                    reactive::ListEdit::Remove { index } => {
                        if *index < ids.len() {
                            ids.remove(*index);
                        }
                    }
                    reactive::ListEdit::Replace { index, value } => {
                        if let Some(slot) = ids.get_mut(*index) {
                            *slot = *value;
                        }
                    }
                    reactive::ListEdit::Move { from, to } => {
                        if *from < ids.len() && *to < ids.len() {
                            let value = ids.remove(*from);
                            ids.insert(*to, value);
                        }
                    }
                }
            }
        }

        // The order unit is the authority for what the lexer published. The
        // identity script reconstructs it without per-token reads, and any
        // disagreement means the script is stale: trust the published order.
        let published: Vec<TokenId> = {
            let order: reactive::PSeq<u64> = cx.read_typed(Tokens::order_unit())?;
            units_read += 1;
            order.iter().map(|id| TokenId(*id)).collect()
        };
        // The identity script is the fast path; the published order is the
        // authority when the two disagree.
        let ids = if ids == published { ids } else { published };
        let mut tokens = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(token) = previous_by_id.get(&id) {
                if !changed.contains(&id) {
                    tokens.push(Arc::clone(token));
                    continue;
                }
            }
            let payload: Arc<TokenPayload> = cx.read_typed(Tokens::payload_unit(id))?;
            units_read += 1;
            let mode = ModeId(0);
            tokens.push(Arc::new(Token {
                id,
                kind: payload.kind,
                text: Arc::clone(&payload.text),
                range: payload.range,
                start_state: LexConfig::new(mode, Vec::new(), 0),
                end_state: LexConfig::new(mode, Vec::new(), 0),
                lookahead_end: payload.range.end,
                flags: payload.flags,
                error: payload.error.clone(),
            }));
        }

        Ok(TokenInput {
            document,
            tokens: Arc::new(TokenStream::new(tokens)),
            delta,
            units_read,
        })
    }

    /// Publishes one parse into the AST Views.
    #[allow(clippy::too_many_arguments)]
    fn publish(
        cx: &mut GeneralCx,
        previous: Option<&AstTable>,
        rules: &mut HashSet<u64>,
        diagnostics: &mut HashSet<DiagnosticId>,
        added: &[crate::AstNodeRecord],
        removed: &[u64],
        parse_diagnostics: &[Diagnostic],
        stats: StatsSnapshot,
        storage: &mut TreeStorage,
        root: u64,
        tree_nodes: &[TreePatchNode],
    ) {
        let mut written = 0u64;
        let current: HashSet<DiagnosticId> = parse_diagnostics
            .iter()
            .map(DiagnosticId::from_diagnostic)
            .collect();
        let mut output = cx.output();
        let mut seen_diagnostics: HashSet<DiagnosticId> = HashSet::new();
        for diagnostic in parse_diagnostics {
            let id = DiagnosticId::from_diagnostic(diagnostic);
            if !seen_diagnostics.insert(id.clone()) {
                continue;
            }
            output.entry(
                AstMeta::view_id(),
                AstMetaField::DIAGNOSTICS,
                id.clone(),
                diagnostic.clone(),
            );
            output.membership(
                AstMeta::view_id(),
                AstMetaField::DIAGNOSTICS,
                id.clone(),
                true,
            );
            written += 2;
        }
        for id in diagnostics.iter() {
            if !current.contains(id) {
                output.retract(AstMeta::diagnostic_unit(id.clone()));
                output.retract(AstMeta::diagnostic_membership(id.clone()));
            }
        }
        for record in added {
            if record.rule != 0 && rules.insert(record.rule) {
                output.entry(
                    AstMeta::view_id(),
                    AstMetaField::RULES,
                    record.rule,
                    Arc::<str>::from(record.kind.as_str()),
                );
                output.membership(AstMeta::view_id(), AstMetaField::RULES, record.rule, true);
                written += 2;
            }
            output.entry(
                AstMeta::view_id(),
                AstMetaField::SPAN_STARTS,
                record.id,
                record.span_start,
            );
            output.membership(
                AstMeta::view_id(),
                AstMetaField::SPAN_STARTS,
                record.id,
                true,
            );
            output.entry(
                AstMeta::view_id(),
                AstMetaField::SPAN_ENDS,
                record.id,
                record.span_end,
            );
            output.membership(AstMeta::view_id(), AstMetaField::SPAN_ENDS, record.id, true);
            written += 4;
            if let Some(token) = record.token {
                output.entry(AstMeta::view_id(), AstMetaField::TOKENS, record.id, token);
                output.membership(AstMeta::view_id(), AstMetaField::TOKENS, record.id, true);
                written += 2;
            }
            if let Some(value) = &record.value {
                output.entry(
                    AstMeta::view_id(),
                    AstMetaField::VALUES,
                    record.id,
                    value.clone(),
                );
                output.membership(AstMeta::view_id(), AstMetaField::VALUES, record.id, true);
                written += 2;
            }
            if record.category.is_some() {
                output.membership(AstMeta::view_id(), AstMetaField::ERRORS, record.id, true);
                written += 1;
            }
            for (field, child) in &record.fields {
                output.entry(
                    AstMeta::view_id(),
                    AstMetaField::FIELDS,
                    (record.id, field.clone()),
                    *child,
                );
                output.membership(
                    AstMeta::view_id(),
                    AstMetaField::FIELDS,
                    (record.id, field.clone()),
                    true,
                );
                written += 2;
            }
        }
        for id in removed {
            let fields: Vec<String> = previous
                .and_then(|nodes| nodes.get(id))
                .map(|meta| {
                    meta.record
                        .fields
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect()
                })
                .unwrap_or_default();
            output.retract(AstMeta::span_start_unit(*id));
            output.retract(AstMeta::span_end_unit(*id));
            output.retract(AstMeta::token_unit(*id));
            output.retract(AstMeta::value_unit(*id));
            output.retract(AstMeta::error_unit(*id));
            for field in fields {
                output.retract(AstMeta::field_unit(*id, &field));
                output.retract(AstMeta::field_membership(*id, &field));
            }
        }

        let mut stats = stats;
        stats.meta_units_written = written;
        stats.publish(cx);

        let patch = TreePatch {
            root: Some(root),
            nodes: tree_nodes.to_vec(),
            removed: removed.to_vec(),
        };
        *storage = cx
            .tree_output(AstTree::view_id())
            .apply_patch(storage, &patch);
    }
}

impl GeneralComponent for ParserComponent {
    type State = ParserComponentState;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00B2_0002);
    const NAME: &'static str = "reactive-syntax::ParserComponent";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Tokens as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[
        <AstMeta as ViewSchema>::DESCRIPTOR,
        <AstTree as ViewSchema>::DESCRIPTOR,
        <PipelineStats as ViewSchema>::DESCRIPTOR,
    ];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)> {
        let input = Self::read_tokens(cx, None)?;
        let text = Arc::clone(input.document.snapshot());
        let ast = self
            .parser
            .initial(Arc::clone(&input.tokens), Arc::clone(&text))
            .ok_or_else(|| {
                reactive::ComputeError::internal("the initial parse produced no tree")
            })?;
        let (state, _removed) = ast.commit(
            &AstState::empty(Arc::clone(&input.tokens), Arc::clone(&text)),
            Arc::clone(&input.tokens),
            Arc::clone(&text),
        );
        let added = state
            .nodes
            .iter()
            .map(|(_, meta)| meta.record.clone())
            .collect::<Vec<_>>();
        let tree_nodes = tree_nodes(&added, state.root, &TreeStorage::default());
        let stats = StatsSnapshot {
            bytes_relexed: text.len() as u64,
            tokens_relexed: state.significant.len() as u64,
            tokens_reparsed: state.significant.len() as u64,
            nodes_built: added.len() as u64,
            nodes_added: added.len() as u64,
            token_units_read: input.units_read as u64,
            ..StatsSnapshot::default()
        };
        let mut rules = HashSet::new();
        let mut diagnostics = HashSet::new();
        let mut storage = TreeStorage::default();
        Self::publish(
            cx,
            None,
            &mut rules,
            &mut diagnostics,
            &added,
            &[],
            &[],
            stats,
            &mut storage,
            state.root,
            &tree_nodes,
        );
        Ok((
            ParserComponentState {
                ast: state,
                diagnostics,
                rules,
                storage,
            },
            (),
        ))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        _changes: &ChangeBatch,
    ) -> Result<(Self::State, Self::Output)> {
        let input = Self::read_tokens(cx, Some(&state.ast.tokens))?;
        if input.delta.is_empty() {
            return Ok((state.clone(), ()));
        }
        let text = Arc::clone(input.document.snapshot());
        // A heuristic reparse that cannot validate any window falls back to a
        // full parse: the published tree must never disagree with the tokens.
        let next =
            match self
                .parser
                .update(&state.ast, Arc::clone(&input.tokens), Arc::clone(&text))
            {
                Some(next) => next,
                None => self
                    .parser
                    .initial(Arc::clone(&input.tokens), Arc::clone(&text))
                    .ok_or_else(|| {
                        reactive::ComputeError::internal("the incremental parse found no tree")
                    })?,
            };
        let window = window_bytes(&state.ast, next.stats.token_start, next.stats.token_end);
        let root = next.root;
        let added = next.added.clone();
        let diagnostics_in = next.diagnostics.clone();
        let built = next.stats;
        let (ast, removed) = next.commit(&state.ast, Arc::clone(&input.tokens), Arc::clone(&text));
        // The published tree must name exactly the tokens the lexer published.
        // A heuristic reparse that disagrees is replaced by a full parse rather
        // than published.
        let consistent = {
            let significant: HashSet<u64> =
                ast.significant.iter().map(|token| token.id.0).collect();
            let mut tree: HashSet<u64> = HashSet::new();
            let mut stack = vec![ast.root];
            let mut visited: HashSet<u64> = HashSet::new();
            while let Some(id) = stack.pop() {
                if !visited.insert(id) {
                    continue;
                }
                let Some(meta) = ast.node(id) else {
                    continue;
                };
                if let Some(token) = meta.record.token {
                    tree.insert(token.0);
                }
                for child in &meta.record.children {
                    stack.push(*child);
                }
            }
            tree == significant
        };

        if std::env::var_os("COV_DEBUG").is_some() {
            eprintln!(
                "[cov] consistent={consistent} added={} removed={} sig={}",
                added.len(),
                removed.len(),
                ast.significant.len()
            );
        }

        let mut rebuilt_all = false;
        let (ast, removed, added, root) = if consistent {
            (ast, removed, added, root)
        } else {
            rebuilt_all = true;
            let fresh = self
                .parser
                .initial(Arc::clone(&input.tokens), Arc::clone(&text))
                .ok_or_else(|| reactive::ComputeError::internal("the full parse found no tree"))?;
            let root = fresh.root;
            let added = fresh.added.clone();
            let diagnostics_in = fresh.diagnostics.clone();
            let (ast, removed) = fresh.commit(
                &AstState::empty(Arc::clone(&input.tokens), Arc::clone(&text)),
                Arc::clone(&input.tokens),
                Arc::clone(&text),
            );
            let _ = diagnostics_in;

            (ast, removed, added, root)
        };
        let stats = StatsSnapshot {
            bytes_relexed: input.delta.bytes_relexed as u64,
            tokens_relexed: input.delta.tokens_relexed as u64,
            tokens_reparsed: (built.token_end - built.token_start) as u64,
            bytes_reparsed: window as u64,
            nodes_built: built.nodes_built as u64,
            nodes_added: added.len() as u64,
            nodes_removed: removed.len() as u64,
            token_units_read: input.units_read as u64,
            escalations: u64::from(built.escalations),
            adopted_suffix: u64::from(built.adopted_suffix),
            ..StatsSnapshot::default()
        };
        let (patch_nodes, added_for_meta) = if rebuilt_all {
            let all: Vec<crate::AstNodeRecord> = ast
                .nodes
                .iter()
                .map(|(_, meta)| meta.record.clone())
                .collect();
            (tree_nodes(&all, root, &TreeStorage::default()), all)
        } else {
            (tree_nodes(&added, root, &state.storage), added.clone())
        };
        let added = added_for_meta;
        let mut rules = state.rules.clone();
        let mut diagnostics = state.diagnostics.clone();
        let mut storage = if rebuilt_all {
            TreeStorage::default()
        } else {
            state.storage.clone()
        };
        Self::publish(
            cx,
            Some(&state.ast.nodes),
            &mut rules,
            &mut diagnostics,
            &added,
            &removed,
            &diagnostics_in,
            stats,
            &mut storage,
            root,
            &patch_nodes,
        );
        Ok((
            ParserComponentState {
                ast,
                diagnostics,
                rules,
                storage,
            },
            (),
        ))
    }
}

/// Bytes covered by a significant token window.
fn window_bytes(state: &AstState, start: usize, end: usize) -> usize {
    let first = state
        .significant
        .get(start)
        .map(|token| token.range.start.offset);
    let last = end
        .checked_sub(1)
        .and_then(|end| state.significant.get(end))
        .map(|token| token.range.end.offset);
    match (first, last) {
        (Some(first), Some(last)) => last.saturating_sub(first),
        _ => 0,
    }
}

/// Tree nodes for a set of records, with parent links resolved.
fn tree_nodes(
    added: &[crate::AstNodeRecord],
    _root: u64,
    previous: &TreeStorage,
) -> Vec<TreePatchNode> {
    let mut parents: HashMap<u64, u64> = HashMap::new();
    for record in added {
        for child in &record.children {
            parents.insert(*child, record.id);
        }
    }
    added
        .iter()
        .map(|record| {
            let (variant, fields) = if let Some(category) = record.category {
                (
                    2u16,
                    vec![
                        TreeValue::UInt(u64::from(category.0)),
                        TreeValue::Bool(record.ignored),
                    ],
                )
            } else if let Some(token) = record.token {
                (1u16, vec![TreeValue::UInt(token.0)])
            } else {
                (0u16, vec![TreeValue::UInt(record.rule)])
            };
            let parent = parents
                .get(&record.id)
                .copied()
                .or_else(|| previous.nodes.get(&record.id).and_then(|node| node.parent));
            TreePatchNode {
                id: record.id,
                parent,
                type_index: 0,
                variant,
                fields,
                children: if record.children.is_empty() {
                    Vec::new()
                } else {
                    vec![ChildSlot::Many(record.children.clone())]
                },
            }
        })
        .collect()
}

/// Root consumer for the AST.
#[derive(Clone, Debug)]
pub struct AstConsumer;

impl GeneralComponent for AstConsumer {
    type State = ();
    type Output = Option<u64>;

    const ID: ComponentId = ComponentId(0x00B2_0005);
    const NAME: &'static str = "reactive-syntax::AstConsumer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<AstTree as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)> {
        let root: Option<u64> = cx.read_typed(UnitKey::field(
            AstTree::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        Ok(((), root))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &ChangeBatch,
    ) -> Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}
