//! Parser component: turns the token delta into the `AstTree` and `AstMeta`.
//!
//! The component is an ordinary incremental component: it reads the token delta,
//! resumes the parse from a checkpoint, and publishes only what the edit
//! touched. There is no concrete tree, no full parse after the first one, and no
//! runtime-side knowledge of what it does.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use irrigation::{
    ChangeBatch, ChildSlot, ComponentId, DependencyUpdates, FieldId, GeneralComponent, GeneralCx,
    Result, TreeOutputExt, TreePatch, TreePatchNode, TreeStorage, TreeValue, UnitKey, UnitKind,
    ViewDescriptor, ViewSchema,
};

use crate::diagnostic::Diagnostic;
use crate::lexer::{LexConfig, ModeId, Token, TokenId, TokenStream};
use crate::mapping::TreeMapping;
use crate::parser::ast::{AstNodeRecord, RuleId};
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
    rules: HashSet<RuleId>,
    /// The generic parse tree.
    storage: TreeStorage,
    /// The declared tree, when this parser publishes one.
    typed: Option<TreeStorage>,
}

/// Where the declared tree goes and how its nodes are built.
#[derive(Clone, Debug)]
struct TypedTree {
    descriptor: &'static ViewDescriptor,
    mapping: TreeMapping,
}

/// Parser component: reads `Tokens`, owns the AST Views.
///
/// With [`ParserComponent::with_tree`] it also publishes a tree the grammar maps
/// onto, from the same parse records and with the same node identities.
#[derive(Clone, Debug)]
pub struct ParserComponent {
    parser: AstParser,
    tree: Option<TypedTree>,
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
            tree: None,
        }
    }

    /// Builds a parser that also publishes the declared tree `S`.
    ///
    /// The grammar's `@tree` productions are resolved against `S::LAYOUT` and
    /// every mismatch is reported here, before a revision is parsed: a grammar
    /// that cannot fill the declaration is a construction error, not a missing
    /// child a reader discovers later.
    ///
    /// # Errors
    ///
    /// Returns the mapping failures, one per line.
    pub fn with_tree<S: irrigation::TreeSchema>(grammar: crate::Grammar) -> Result<Self> {
        let mapping = grammar.map_tree(S::LAYOUT).map_err(|errors| {
            let mut message = format!(
                "the grammar does not map onto `{}`: {} mismatch(es)",
                <S as ViewSchema>::DESCRIPTOR.name,
                errors.len()
            );
            for error in &errors {
                let _ = write!(message, "\n  - {error}");
            }
            irrigation::ComputeError::internal(message)
        })?;
        Ok(Self {
            parser: AstParser::new(grammar),
            tree: Some(TypedTree {
                descriptor: <S as ViewSchema>::DESCRIPTOR,
                mapping,
            }),
        })
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
        grammar: &crate::Grammar,
        previous: Option<&AstTable>,
        rules: &mut HashSet<RuleId>,
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
            if record.rule != RuleId::default() && rules.insert(record.rule) {
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
            if let Some(child) = wrapped_child(grammar, record) {
                cx.write(AstMeta::WRAPS).entry(record.id).set(child);
                written += 2;
            }
        }
        for record in added {
            // A rebuilt node keeps its identity, so metadata the previous
            // record carried and this one does not would survive as a stale
            // unit: a field that named an optional child, a wrap that the new
            // production does not declare, a value a token used to have.
            let Some(previous) = previous
                .and_then(|nodes| nodes.get(&record.id))
                .map(|meta| &meta.record)
            else {
                continue;
            };
            if previous.token.is_some() && record.token.is_none() {
                cx.write(AstMeta::TOKENS).entry(record.id).retract();
            }
            if previous.value.is_some() && record.value.is_none() {
                cx.write(AstMeta::VALUES).entry(record.id).retract();
            }
            if wrapped_child(grammar, previous).is_some()
                && wrapped_child(grammar, record).is_none()
            {
                cx.write(AstMeta::WRAPS).entry(record.id).retract();
            }
        }
        for id in removed {
            cx.write(AstMeta::SPAN_STARTS).entry(*id).retract();
            cx.write(AstMeta::SPAN_ENDS).entry(*id).retract();
            cx.write(AstMeta::TOKENS).entry(*id).retract();
            cx.write(AstMeta::VALUES).entry(*id).retract();
            cx.write(AstMeta::WRAPS).entry(*id).retract();
            errors.remove(id);
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

    /// A parser that publishes a declared tree owns one more View.
    ///
    /// The trait documents construction-chosen outputs for exactly this case;
    /// the identity is mixed with the tree's so a plain and a typed parser can
    /// live in one runtime without colliding.
    fn output_views(&self) -> Vec<&'static ViewDescriptor> {
        let mut views = Self::OUTPUT_VIEWS.to_vec();
        if let Some(tree) = &self.tree {
            views.push(tree.descriptor);
        }
        views
    }

    fn component_id(&self) -> ComponentId {
        match &self.tree {
            None => Self::ID,
            Some(tree) => ComponentId(irrigation::hash_one(&(Self::NAME, tree.descriptor.id))),
        }
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
        let (tree_nodes, parents) = tree_nodes(&added, state.root, &TreeStorage::default());
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
            self.parser.grammar(),
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
        let mut typed = None;
        publish_typed(
            cx,
            self.parser.grammar(),
            self.tree.as_ref(),
            None,
            &added,
            &[],
            &TreeStorage::default(),
            &parents,
            state.root,
            true,
            false,
            &mut typed,
        );
        Ok((
            ParserComponentState {
                ast: state,
                diagnostics,
                rules,
                storage,
                typed,
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
                None => {
                    if std::env::var_os("IRRIGATION_PARSER_DEBUG").is_some() {
                        eprintln!("[parser] incremental update returned no tree");
                    }
                    self.parser
                        .initial(Arc::clone(&input.tokens), Arc::clone(&text))
                        .ok_or_else(|| {
                            irrigation::ComputeError::internal(
                                "the incremental parse found no tree",
                            )
                        })?
                }
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

        if !consistent && std::env::var_os("IRRIGATION_PARSER_DEBUG").is_some() {
            eprintln!(
                "[parser] inconsistent incremental tree: added={} removed={} significant={}",
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
            // A rebuilt node can keep its identity, so "added" counts the ids
            // that are genuinely new, not the records this revision rewrote.
            nodes_added: added
                .iter()
                .filter(|record| state.ast.nodes.get(&record.id).is_none())
                .count() as u64,
            nodes_removed: removed.len() as u64,
            token_units_read: input.units_read as u64,
            // The published numbers describe the tree that was published. On
            // the fallback path `built` describes the attempt that was rejected.
            escalations: u64::from(parse_stats.escalations),
            adopted_suffix: u64::from(parse_stats.adopted_suffix),
            ..StatsSnapshot::default()
        };
        let (patch_nodes, added_for_meta, parents, generic_previous) = if rebuilt_all {
            let all: Vec<crate::AstNodeRecord> = ast
                .nodes
                .iter()
                .map(|(_, meta)| meta.record.clone())
                .collect();
            // The full parse rebuilt every record, but the tree's *previous*
            // content is still the honest baseline: applying the patch against
            // an empty storage would mark every node changed and recreate the
            // whole tree even when the ids and payloads are identical.
            let (nodes, parents) = tree_nodes(&all, root, &state.storage);
            (nodes, all, parents, state.storage.clone())
        } else {
            let (nodes, parents) = tree_nodes(&added, root, &state.storage);
            (nodes, added, parents, state.storage.clone())
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
        let mut storage = state.storage.clone();
        Self::publish(
            cx,
            self.parser.grammar(),
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
        let mut typed = state.typed.clone();
        let recovered = ast
            .nodes
            .iter()
            .any(|(_, meta)| meta.record.category.is_some())
            || parse_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.starts_with("parse."));
        publish_typed(
            cx,
            self.parser.grammar(),
            self.tree.as_ref(),
            Some(&state.ast.nodes),
            &added,
            &removed,
            &generic_previous,
            &parents,
            root,
            false,
            recovered,
            &mut typed,
        );
        Ok((
            ParserComponentState {
                ast,
                diagnostics,
                rules,
                storage,
                typed,
            },
            (),
        ))
    }
}

/// Everything one typed publication resolves names against.
struct TypedSource<'a> {
    grammar: &'a crate::Grammar,
    mapping: &'a TreeMapping,
    added: HashMap<u64, &'a AstNodeRecord>,
    previous: Option<&'a AstTable>,
    /// The generic tree before this revision's patch: its parent links name the
    /// ancestors this revision does not rewrite.
    generic: &'a TreeStorage,
    /// Parent links from this revision's own records.
    parents: &'a HashMap<u64, u64>,
}

impl TypedSource<'_> {
    fn record(&self, id: u64) -> Option<&AstNodeRecord> {
        self.added.get(&id).copied().or_else(|| {
            self.previous
                .and_then(|nodes| nodes.get(&id))
                .map(|meta| &meta.record)
        })
    }

    /// The typed identity of `id`: the node itself when its production builds a
    /// declared variant, or the payload it hands over when it is a wrapper.
    fn typed_id(&self, id: u64) -> Option<u64> {
        let mut current = id;
        for _ in 0..64 {
            let record = self.record(current)?;
            let production = self.grammar.productions().get(record.production?)?;
            if production.wrap.is_some() {
                current = wrapped_child(self.grammar, record)?;
                continue;
            }
            return self.mapping.build(production.id).map(|_| current);
        }
        None
    }

    fn generic_parent(&self, id: u64) -> Option<u64> {
        self.parents
            .get(&id)
            .copied()
            .or_else(|| self.generic.nodes.get(&id).and_then(|node| node.parent))
    }

    /// The nearest ancestor that has a typed identity of its own.
    ///
    /// A wrapper's typed identity is its payload, so a node under a wrapper sees
    /// that payload as its parent — unless it *is* the payload, in which case the
    /// walk continues, so a node never parents itself.
    fn typed_ancestor(&self, id: u64) -> Option<u64> {
        let mut cursor = self.generic_parent(id);
        for _ in 0..256 {
            let parent = cursor?;
            if let Some(typed) = self.typed_id(parent) {
                if typed != id {
                    return Some(typed);
                }
            }
            cursor = self.generic_parent(parent);
        }
        None
    }
}

/// The typed patch for one revision, and its root when the parse reached one.
fn typed_patch(
    source: &TypedSource<'_>,
    added: &[AstNodeRecord],
    root: u64,
) -> (Option<u64>, Vec<TreePatchNode>) {
    let mut nodes = Vec::new();
    for record in added {
        let Some(production) = record
            .production
            .and_then(|id| source.grammar.productions().get(id))
        else {
            continue;
        };
        let Some(build) = source.mapping.build(production.id) else {
            continue;
        };
        let variant = build.variant_layout;
        let mut leaves = vec![TreeValue::Unit; variant.leaves.len()];
        let mut children: Vec<ChildSlot> = vec![ChildSlot::None; variant.children.len()];
        for (field, feed) in &build.feeds {
            let Some((_, range)) = record.fields.iter().find(|(declared, _)| declared == field)
            else {
                continue;
            };
            match feed {
                crate::mapping::Feed::Leaf { slot, kind } => {
                    let child = record.children.get(range.start).copied();
                    leaves[usize::from(*slot)] = leaf_value(source, child, *kind);
                }
                crate::mapping::Feed::Child { slot, cardinality } => {
                    let ids: Vec<u64> = record.children[range.clone()]
                        .iter()
                        .filter_map(|child| source.typed_id(*child))
                        .collect();
                    let index = usize::from(*slot);
                    children[index] = match cardinality {
                        irrigation::Cardinality::One => {
                            ids.first().copied().map_or(ChildSlot::None, ChildSlot::One)
                        }
                        irrigation::Cardinality::Optional => {
                            ChildSlot::Optional(ids.first().copied())
                        }
                        irrigation::Cardinality::Many => ChildSlot::Many(ids),
                    };
                }
            }
        }
        nodes.push(TreePatchNode {
            id: record.id,
            parent: source.typed_ancestor(record.id),
            type_index: build.type_index,
            variant: build.variant,
            fields: leaves,
            children,
        });
    }
    (source.typed_id(root), nodes)
}

/// One leaf slot's payload, read from the token the field names.
fn leaf_value(
    source: &TypedSource<'_>,
    child: Option<u64>,
    kind: irrigation::LeafKind,
) -> TreeValue {
    use irrigation::LeafKind;
    let Some(record) = child.and_then(|id| source.record(id)) else {
        return TreeValue::Unit;
    };
    match kind {
        LeafKind::Text => record
            .text
            .as_deref()
            .map_or(TreeValue::Unit, |text| TreeValue::Text(text.to_owned())),
        LeafKind::Int => record
            .value
            .as_ref()
            .and_then(crate::AstValue::as_int)
            .or_else(|| record.text.as_deref().and_then(|text| text.parse().ok()))
            .map_or(TreeValue::Unit, TreeValue::Int),
        LeafKind::UInt => record
            .value
            .as_ref()
            .and_then(crate::AstValue::as_uint)
            .or_else(|| record.text.as_deref().and_then(|text| text.parse().ok()))
            .map_or(TreeValue::Unit, TreeValue::UInt),
        LeafKind::Float => record
            .value
            .as_ref()
            .and_then(crate::AstValue::as_float)
            .or_else(|| record.text.as_deref().and_then(|text| text.parse().ok()))
            .map_or(TreeValue::Unit, TreeValue::Float),
        LeafKind::Bool => record
            .value
            .as_ref()
            .and_then(crate::AstValue::as_bool)
            .or(match record.text.as_deref() {
                Some("true") => Some(true),
                Some("false") => Some(false),
                _ => None,
            })
            .map_or(TreeValue::Unit, TreeValue::Bool),
    }
}

/// The child that stands in for one record, when its production is a `@wrap`.
///
/// A wrapper names exactly one field, and a parse publishes that field's child
/// run. The relation is derived from the record, never stored twice.
fn wrapped_child(grammar: &crate::Grammar, record: &AstNodeRecord) -> Option<u64> {
    let production = grammar.productions().get(record.production?)?;
    let field = production.wrap?;
    let (_, range) = record
        .fields
        .iter()
        .find(|(declared, _)| *declared == field)?;
    match range.len() {
        1 => record.children.get(range.start).copied(),
        _ => None,
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
///
/// The parent map is returned because a second publisher of the same records
/// needs it: the typed tree's ancestors are the same ones.
fn tree_nodes(
    added: &[crate::AstNodeRecord],
    _root: u64,
    previous: &TreeStorage,
) -> (Vec<TreePatchNode>, HashMap<u64, u64>) {
    let mut parents: HashMap<u64, u64> = HashMap::new();
    for record in added {
        for child in &record.children {
            parents.insert(*child, record.id);
        }
    }
    let nodes = added
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
                        TreeValue::UInt(record.rule.raw()),
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
        .collect();
    (nodes, parents)
}

/// Publishes the declared tree, when this parser has one.
#[allow(clippy::too_many_arguments)]
fn publish_typed(
    cx: &mut GeneralCx,
    grammar: &crate::Grammar,
    tree: Option<&TypedTree>,
    previous: Option<&AstTable>,
    added: &[AstNodeRecord],
    removed: &[u64],
    generic: &TreeStorage,
    parents: &HashMap<u64, u64>,
    root: u64,
    reset: bool,
    keep: bool,
    storage: &mut Option<TreeStorage>,
) {
    let Some(tree) = tree else {
        return;
    };
    // A parse with a recovery node has no faithful typed tree to publish. Keep
    // the last well-formed one and let the diagnostics carry the error: the
    // stages above read the typed tree, and replacing it with a fragment would
    // retract and re-create every unit they emitted for a transient keystroke.
    // A later error-free parse publishes the new tree normally.
    if std::env::var_os("IRRIGATION_TYPED_DEBUG").is_some() {
        eprintln!(
            "[typed] keep={keep} reset={reset} storage={} added={} removed={}",
            storage.is_some(),
            added.len(),
            removed.len()
        );
    }
    if keep && storage.is_some() {
        return;
    }
    let source = TypedSource {
        grammar,
        mapping: &tree.mapping,
        added: added.iter().map(|record| (record.id, record)).collect(),
        previous,
        generic,
        parents,
    };
    let (typed_root, nodes) = typed_patch(&source, added, root);
    if reset {
        *storage = Some(TreeStorage::default());
    }
    let typed = storage.get_or_insert_with(TreeStorage::default);
    let patch = TreePatch {
        root: typed_root,
        nodes,
        removed: removed.to_vec(),
    };
    *typed = cx
        .tree_output(tree.descriptor.id)
        .apply_patch(typed, &patch);
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
