//! Parser component: turns the token delta into the `AstTree` and `AstMeta`.
//!
//! The component is an ordinary incremental component: it reads the token delta,
//! resumes the parse from a checkpoint, and publishes only what the edit
//! touched. There is no concrete tree, no full parse after the first one, and no
//! runtime-side knowledge of what it does.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use irrigation::{
    ChangeBatch, ChildSlot, ComponentId, DependencyUpdates, FieldId, GeneralComponent, GeneralCx,
    Result, TreeOutputExt, TreePatch, TreePatchNode, TreeStorage, TreeValue, UnitKey, UnitKind,
    ViewDescriptor, ViewSchema,
};

use crate::diagnostic::Diagnostic;
use crate::lexer::{LexConfig, ModeId, Token, TokenId, TokenStream};
use crate::parser::engine::AstParser;
use crate::parser::state::{AstState, AstTable};
use crate::text::TextDocument;
use crate::views::ast_tree::AstTree;
use crate::views::{
    AstMeta, DiagnosticId, PipelineStats, StatsSnapshot, TokenDelta, TokenPayload, Tokens,
};

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
        let document: TextDocument = Tokens::TEXT.read_in(cx)?;
        let delta: TokenDelta = Tokens::DELTA.read_in(cx)?;
        let mut units_read = 2;
        let previous_by_id: HashMap<TokenId, Arc<Token>> =
            previous.map_or_else(HashMap::new, |stream| {
                stream
                    .iter()
                    .map(|token| (token.id, Arc::clone(token)))
                    .collect()
            });
        let changed = delta.changed_set();
        // The published order is the only owner of the identity sequence. The
        // delta's order script describes the same edit; rebuilding the sequence
        // from it and then discarding it in favour of this read was a second,
        // provably redundant pass over every token.
        let order: irrigation::PSeq<u64> = Tokens::ORDER.ids_in(cx)?;
        units_read += 1;
        let ids: Vec<TokenId> = order.iter().map(|id| TokenId(*id)).collect();
        let mut tokens = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(token) = previous_by_id.get(&id) {
                if !changed.contains(&id) {
                    tokens.push(Arc::clone(token));
                    continue;
                }
            }
            let payload: Arc<TokenPayload> = Tokens::PAYLOADS
                .get_in(cx, &id)?
                .ok_or_else(|| irrigation::ComputeError::internal("token payload missing"))?;
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
        errors: &mut HashSet<u64>,
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
        let mut seen_diagnostics: HashSet<DiagnosticId> = HashSet::new();
        for diagnostic in parse_diagnostics {
            let id = DiagnosticId::from_diagnostic(diagnostic);
            if !seen_diagnostics.insert(id.clone()) {
                continue;
            }
            cx.write(AstMeta::DIAGNOSTICS)
                .entry(id.clone())
                .set(diagnostic.clone());
            written += 2;
        }
        for id in diagnostics.iter() {
            if !current.contains(id) {
                cx.write(AstMeta::DIAGNOSTICS).entry(id.clone()).retract();
            }
        }
        // The tracker is the membership the next revision retracts against: it
        // must name exactly what this revision published.
        diagnostics.clear();
        diagnostics.extend(current);

        for record in added {
            if record.rule != 0 && rules.insert(record.rule) {
                cx.write(AstMeta::RULES)
                    .entry(record.rule)
                    .set(Arc::<str>::from(record.kind.as_str()));
                written += 2;
            }
            cx.write(AstMeta::SPAN_STARTS)
                .entry(record.id)
                .set(record.span_start);
            cx.write(AstMeta::SPAN_ENDS)
                .entry(record.id)
                .set(record.span_end);
            written += 4;
            if let Some(token) = record.token {
                cx.write(AstMeta::TOKENS).entry(record.id).set(token);
                written += 2;
            }
            if let Some(value) = &record.value {
                cx.write(AstMeta::VALUES)
                    .entry(record.id)
                    .set(value.clone());
                written += 2;
            }
            if record.category.is_some() {
                errors.insert(record.id);
            }
            for (field, child) in &record.fields {
                cx.write(AstMeta::FIELDS)
                    .entry((record.id, field.clone()))
                    .set(*child);
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
            cx.write(AstMeta::SPAN_STARTS).entry(*id).retract();
            cx.write(AstMeta::SPAN_ENDS).entry(*id).retract();
            cx.write(AstMeta::TOKENS).entry(*id).retract();
            cx.write(AstMeta::VALUES).entry(*id).retract();
            errors.remove(id);
            for field in fields {
                cx.write(AstMeta::FIELDS).entry((*id, field)).retract();
            }
        }
        // The recovery set is one value per revision, so it is written as one.
        // Stated per node it cost one membership write per added node and one
        // *withdrawal* per removed node, and a withdrawal republishes the field's
        // whole content in the definition pipeline: deleting a region then paid
        // a content publish per removed node.
        cx.write(AstMeta::ERRORS).define_all(errors.iter().copied());
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
    const NAME: &'static str = "irrigation-syntax::ParserComponent";
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
                irrigation::ComputeError::internal("the initial parse produced no tree")
            })?;
        let parse_diagnostics = ast.diagnostics.clone();
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
        let mut errors = HashSet::new();
        let mut storage = TreeStorage::default();
        Self::publish(
            cx,
            None,
            &mut rules,
            &mut diagnostics,
            &mut errors,
            &added,
            &[],
            &parse_diagnostics,
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
                        irrigation::ComputeError::internal("the incremental parse found no tree")
                    })?,
            };
        let built = next.stats;
        let root = next.root;
        let added = next.added.clone();
        let diagnostics_in = next.diagnostics.clone();
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
        let (ast, removed, added, root, parse_stats, parse_diagnostics) = if consistent {
            (ast, removed, added, root, built, diagnostics_in)
        } else {
            rebuilt_all = true;
            let fresh = self
                .parser
                .initial(Arc::clone(&input.tokens), Arc::clone(&text))
                .ok_or_else(|| {
                    irrigation::ComputeError::internal("the full parse found no tree")
                })?;
            let (fresh_root, fresh_added) = (fresh.root, fresh.added.clone());
            let (fresh_stats, fresh_diagnostics) = (fresh.stats, fresh.diagnostics.clone());
            // The retained table is what makes the commit report the old nodes
            // the fresh tree dropped: the metadata retractions and the tree
            // patch are both authored from that diff.
            let (ast, removed) =
                fresh.commit(&state.ast, Arc::clone(&input.tokens), Arc::clone(&text));
            (
                ast,
                removed,
                fresh_added,
                fresh_root,
                fresh_stats,
                fresh_diagnostics,
            )
        };
        let stats = StatsSnapshot {
            bytes_relexed: input.delta.bytes_relexed as u64,
            tokens_relexed: input.delta.tokens_relexed as u64,
            tokens_reparsed: (parse_stats.token_end - parse_stats.token_start) as u64,
            bytes_reparsed: if rebuilt_all {
                text.len() as u64
            } else {
                // The window's indices are the published stream's; resolving
                // them against the previous stream measures the wrong range.
                window_bytes(&ast, parse_stats.token_start, parse_stats.token_end) as u64
            },
            nodes_built: parse_stats.nodes_built as u64,
            nodes_added: added.len() as u64,
            nodes_removed: removed.len() as u64,
            token_units_read: input.units_read as u64,
            // The published numbers describe the tree that was published. On
            // the fallback path `built` describes the attempt that was rejected.
            escalations: u64::from(parse_stats.escalations),
            adopted_suffix: u64::from(parse_stats.adopted_suffix),
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
        let mut errors: HashSet<u64> = state
            .ast
            .nodes
            .iter()
            .filter(|(_, meta)| meta.record.category.is_some())
            .map(|(id, _)| *id)
            .collect();
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
            &mut errors,
            &added,
            &removed,
            &parse_diagnostics,
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
                (
                    1u16,
                    vec![
                        TreeValue::UInt(token.0),
                        TreeValue::Text(record.text.as_deref().unwrap_or_default().to_owned()),
                    ],
                )
            } else {
                (
                    0u16,
                    vec![
                        TreeValue::UInt(record.rule),
                        TreeValue::Text(record.kind.clone()),
                    ],
                )
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
    const NAME: &'static str = "irrigation-syntax::AstConsumer";
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
