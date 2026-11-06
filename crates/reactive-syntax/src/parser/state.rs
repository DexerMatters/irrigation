//! Retained AST state and the incremental reparse driver.
//!
//! An edit is handled by resuming the parse from the latest checkpoint before
//! the first changed token, rebuilding the affected construct, and splicing the
//! subtrees that the edit did not touch. Everything outside the rebuilt region
//! keeps its identity and its metadata units.
//!
//! The parse is heuristic, so every proposal is checked by an after-diff before
//! it is accepted: rebuilt nodes must tile their token ranges and the accepted
//! root must cover exactly the document. A proposal that fails grows the window
//! one construct at a time (changed tokens → enclosing rule → container → whole
//! document) and is retried.

use std::sync::Arc;

use reactive::PMap;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::diagnostic::Diagnostic;
use crate::lexer::{TokenFlags, TokenId, TokenStream};
use crate::text::TextSnapshot;

use super::ast::{AstNodeRecord, BuildStats};
use super::engine::{
    AstParser, Build, Checkpoint, ParseResult, ParseToken, SpanInfo, SpliceEntry, SpliceSource,
    Start,
};
use super::grammar::{Grammar, NonterminalId, TerminalId};
use super::lr::Action;

/// One retained AST node: the published record plus its parse-order facts.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeMeta {
    /// The published record.
    pub record: AstNodeRecord,
    /// First significant token index covered.
    pub first_index: usize,
    /// End of the significant token range covered.
    pub end_index: usize,
    /// First token covered, when the node covers at least one.
    pub first_token: Option<TokenId>,
    /// Last token covered, when the node covers at least one.
    pub last_token: Option<TokenId>,
    /// LR state the node was reduced from.
    pub start_state: Option<usize>,
    /// LR state after the node.
    pub end_state: Option<usize>,
    /// Nonterminal the node reduces to.
    pub lhs: Option<NonterminalId>,
    /// Accumulated dynamic precedence.
    pub dynamic: i32,
}

/// The retained AST, keyed by identity.
///
/// A persistent map, so the state clone the component contract performs is
/// O(1) and one edit costs `O(log n)` per changed node.
pub type AstTable = PMap<u64, Arc<NodeMeta>>;

/// Nodes that begin with one token, used to find splice candidates.
pub type StartIndex = PMap<TokenId, Vec<u64>>;

/// State retained between parses.
#[derive(Clone, Debug)]
pub struct AstState {
    /// Root identity.
    pub root: u64,
    /// Every live node.
    pub nodes: AstTable,
    /// Nodes keyed by their first token.
    pub starts: StartIndex,
    /// The full token stream, including trivia.
    pub tokens: Arc<TokenStream>,
    /// Coordinate space of the current tokens.
    pub text: Arc<TextSnapshot>,
    /// Significant tokens, in source order.
    pub significant: Vec<ParseToken>,
    /// Resumable configurations, in increasing token index.
    pub checkpoints: Vec<Checkpoint>,
    /// Next synthesized token identity.
    pub next_token_id: u64,
}

impl AstState {
    /// An empty state, used to commit a first parse.
    #[must_use]
    pub fn empty(tokens: Arc<TokenStream>, text: Arc<TextSnapshot>) -> Self {
        Self {
            root: 0,
            nodes: AstTable::new(),
            starts: StartIndex::new(),
            tokens,
            text,
            significant: Vec::new(),
            checkpoints: Vec::new(),
            next_token_id: 0,
        }
    }

    /// Looks up one node.
    #[must_use]
    pub fn node(&self, id: u64) -> Option<&NodeMeta> {
        self.nodes.get(&id).map(AsRef::as_ref)
    }
}

/// The result of one parse.
#[derive(Debug)]
pub struct AstUpdate {
    /// Root identity.
    pub root: u64,
    /// Nodes created or replaced.
    pub added: Vec<AstNodeRecord>,
    /// Identities that no longer exist.
    pub removed: Vec<u64>,
    /// Parser diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Locality measurements.
    pub stats: BuildStats,
    /// Configurations for the next edit.
    pub checkpoints: Vec<Checkpoint>,
    /// Next synthesized token identity.
    pub next_token_id: u64,
    /// Significant tokens of the parsed stream.
    pub significant: Vec<ParseToken>,
    /// Token spans and splice facts of the added nodes.
    pub spans: FxHashMap<u64, SpanInfo>,
}

impl AstUpdate {
    /// Assembles the next retained state.
    ///
    /// Returns the state and the identities that are no longer part of the tree,
    /// so the caller can retract their metadata units. The sweep makes the
    /// invariant unconditional: whatever the parse added or dropped, the table
    /// holds exactly the tree reachable from the root.
    #[must_use]
    pub fn commit(
        self,
        previous: &AstState,
        tokens: Arc<TokenStream>,
        text: Arc<TextSnapshot>,
    ) -> (AstState, Vec<u64>) {
        let mut nodes = previous.nodes.clone();
        let mut starts = previous.starts.clone();
        let added: FxHashSet<u64> = self.added.iter().map(|record| record.id).collect();
        let records: FxHashMap<u64, &AstNodeRecord> = self
            .added
            .iter()
            .map(|record| (record.id, record))
            .collect();
        for id in &self.removed {
            if added.contains(id) {
                continue;
            }
            if let Some(meta) = nodes.get(id).map(Arc::clone) {
                if let Some(first) = meta.first_token {
                    starts = starts
                        .insert(
                            first,
                            starts.get(&first).map_or_else(Vec::new, |ids| {
                                ids.iter().copied().filter(|each| each != id).collect()
                            }),
                        )
                        .0;
                }
            }
            nodes = nodes.remove(id).0;
        }
        // Rebuilt and adopted nodes both need current token indices: a later
        // edit compares them against the live stream.
        for (id, span) in &self.spans {
            let record = match records.get(id) {
                Some(record) => (*record).clone(),
                None => match previous.node(*id) {
                    Some(meta) if !self.removed.contains(id) => meta.record.clone(),
                    _ => continue,
                },
            };
            if let Some(first) = span.first_token() {
                let mut ids = starts.get(&first).cloned().unwrap_or_default();
                if !ids.contains(id) {
                    ids.push(*id);
                    starts = starts.insert(first, ids).0;
                }
            }
            nodes = nodes
                .insert(
                    *id,
                    Arc::new(NodeMeta {
                        record,
                        first_index: span.first(),
                        end_index: span.end(),
                        first_token: span.first_token(),
                        last_token: span.last_token(),
                        start_state: span.start_state(),
                        end_state: span.end_state(),
                        lhs: span.lhs(),
                        dynamic: span.dynamic(),
                    }),
                )
                .0;
        }
        // The sweep drops whatever the root cannot reach, but a node this parse
        // produced is published unconditionally, so it stays in the table.
        let swept: Vec<u64> = sweep(&nodes, self.root)
            .into_iter()
            .filter(|id| !added.contains(id))
            .collect();
        let mut removed = self.removed;
        // A rebuilt node can keep its identity; it is still in `added`, so it
        // must not also be reported as removed (that would stage its membership
        // twice).
        removed.retain(|id| !added.contains(id));
        for id in &swept {
            nodes = nodes.remove(id).0;
            if let Some(meta) = previous.node(*id) {
                if let Some(first) = meta.first_token {
                    starts = starts
                        .insert(
                            first,
                            starts.get(&first).map_or_else(Vec::new, |ids| {
                                ids.iter().copied().filter(|each| each != id).collect()
                            }),
                        )
                        .0;
                }
            }
        }
        removed.extend(swept);
        removed.sort_unstable();
        removed.dedup();
        (
            AstState {
                root: self.root,
                nodes,
                starts,
                tokens,
                text,
                significant: self.significant,
                checkpoints: self.checkpoints,
                next_token_id: self.next_token_id,
            },
            removed,
        )
    }
}

/// Identities in the table that the root can no longer reach.
///
/// The walk is bounded by the size of the retained tree; it runs once per edit
/// and keeps the published structure exactly equal to the tree.
fn sweep(nodes: &AstTable, root: u64) -> Vec<u64> {
    let mut seen: FxHashSet<u64> = FxHashSet::default();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(meta) = nodes.get(&id) else {
            continue;
        };
        for child in &meta.record.children {
            if nodes.get(child).is_some() {
                stack.push(*child);
            }
        }
    }
    if seen.len() == nodes.len() {
        return Vec::new();
    }
    nodes
        .iter()
        .filter(|(id, _)| !seen.contains(id))
        .map(|(id, _)| *id)
        .collect()
}

/// Splice candidates drawn from the previous AST.
struct SpliceIndex<'a> {
    nodes: &'a AstTable,
    starts: &'a StartIndex,
    tokens: &'a [ParseToken],
    grammar: &'a Grammar,
    /// Old index where the unchanged suffix begins.
    ///
    /// A candidate is only offered when its whole span lies inside the suffix:
    /// every token there is known to be unchanged, so adopting the subtree needs
    /// no per-token comparison.
    suffix_start: usize,
}

/// The state a shift of `terminal` leads to, when shifting is possible.
pub(crate) fn shift_target(grammar: &Grammar, state: usize, terminal: TerminalId) -> Option<usize> {
    grammar
        .table()
        .actions(state, terminal)
        .iter()
        .find_map(|action| match action {
            Action::Shift(next) => Some(*next),
            _ => None,
        })
}

impl SpliceSource for SpliceIndex<'_> {
    fn find(&self, index: usize, state: usize) -> Option<SpliceEntry> {
        let token = self.tokens.get(index)?;
        let ids = self.starts.get(&token.id)?;
        let mut best: Option<SpliceEntry> = None;
        for id in ids {
            let Some(meta) = self.nodes.get(id).map(AsRef::as_ref) else {
                continue;
            };
            if meta.first_index < self.suffix_start {
                continue;
            }
            if meta.first_token != Some(token.id) || meta.start_state != Some(state) {
                continue;
            }
            let Some(last) = meta.last_token else {
                continue;
            };
            // A token leaf is adopted by shifting the same token again; a
            // recorded reduction must reach the state the table picks for it.
            let (lhs, end_state) = match meta.lhs {
                Some(lhs) => {
                    let Some(end_state) = meta.end_state else {
                        continue;
                    };
                    if self.grammar.table().goto(state, lhs) != Some(end_state) {
                        continue;
                    }
                    (Some(lhs), end_state)
                }
                None => {
                    if meta.end_index != meta.first_index + 1 {
                        continue;
                    }
                    let Some(terminal) = self.grammar.terminal_for_kind(token.kind) else {
                        continue;
                    };
                    let Some(next) = shift_target(self.grammar, state, terminal) else {
                        continue;
                    };
                    (None, next)
                }
            };
            let length = meta.end_index.saturating_sub(meta.first_index);
            if length == 0 {
                continue;
            }
            if self.tokens.get(index + length - 1).map(|token| token.id) != Some(last) {
                continue;
            }
            let entry = SpliceEntry {
                node: *id,
                start_state: state,
                end_state,
                lhs,
                first_token: token.id,
                last_token: last,
                length,
                range: crate::TextRange::new(meta.record.span_start, meta.record.span_end),
                dynamic: meta.dynamic,
            };
            if best.is_none_or(|best| best.length < length) {
                best = Some(entry);
            }
        }
        best
    }
}

impl AstParser {
    /// Parses a document from scratch and records checkpoints.
    #[must_use]
    pub fn initial(&self, tokens: Arc<TokenStream>, text: Arc<TextSnapshot>) -> Option<AstUpdate> {
        let significant = significant_tokens(&tokens);
        let result = self.parse(super::engine::Request {
            tokens: &significant,
            text: &text,
            start: Start::Fresh,
            splice: None,
        })?;
        let splices = result.build.spliced().len();
        let (added, spans) = result.build.into_parts();
        let stats = BuildStats {
            token_start: 0,
            token_end: significant.len(),
            tokens_scanned: result.tokens_scanned,
            nodes_built: added.len(),
            adopted_suffix: false,
            splices,
            adopted_at: 0,
            escalations: 0,
            full: true,
        };
        Some(AstUpdate {
            root: result.root,
            added,
            removed: Vec::new(),
            diagnostics: result.diagnostics,
            stats,
            checkpoints: result.checkpoints,
            next_token_id: result.next_token_id,
            significant,
            spans,
        })
    }

    /// Reparses after an edit.
    ///
    /// Returns `None` when no window produced a valid tree.
    #[must_use]
    pub fn update(
        &self,
        state: &AstState,
        tokens: Arc<TokenStream>,
        text: Arc<TextSnapshot>,
    ) -> Option<AstUpdate> {
        let significant = significant_tokens(&tokens);
        let old = &state.significant;
        let (first_diff, common_suffix) = divergence(old, &significant);
        if first_diff == old.len() && old.len() == significant.len() {
            return Some(AstUpdate {
                root: state.root,
                added: Vec::new(),
                removed: Vec::new(),
                diagnostics: Vec::new(),
                stats: BuildStats {
                    token_start: first_diff,
                    token_end: first_diff,
                    ..BuildStats::default()
                },
                checkpoints: state.checkpoints.clone(),
                next_token_id: state.next_token_id,
                significant,
                spans: FxHashMap::default(),
            });
        }
        let splice = SpliceIndex {
            nodes: &state.nodes,
            starts: &state.starts,
            tokens: &significant,
            grammar: self.grammar(),
            suffix_start: old.len() - common_suffix,
        };
        let ladder = WindowLadder::new(state, first_diff);

        for level in 0..=ladder.levels() {
            let start_index = ladder.start(level);
            let start = resume(state, start_index);
            // Nodes the resumed configuration already stands for are part of the
            // new tree exactly like spliced subtrees: the parse continues on top
            // of them instead of rebuilding them.
            let mut carried: FxHashSet<u64> = match &start {
                Start::Resume(checkpoint) => {
                    let mut ids = FxHashSet::default();
                    for frame in &checkpoint.frames {
                        if frame.ast == 0 {
                            ids.extend(frame.items.iter().copied());
                        } else {
                            ids.insert(frame.ast);
                        }
                    }
                    ids
                }
                Start::Fresh => FxHashSet::default(),
            };
            let Some(result) = self.parse(super::engine::Request {
                tokens: &significant,
                text: &text,
                start,
                splice: Some(&splice),
            }) else {
                continue;
            };
            if !accepted(state, &significant, &result) {
                continue;
            }
            let ParseResult {
                root,
                build,
                diagnostics,
                checkpoints,
                spliced,
                first_splice,
                tokens_scanned,
                next_token_id,
                ..
            } = result;
            let window_end = first_splice
                .unwrap_or(state.significant.len())
                .max(start_index);
            let window = start_index..window_end.min(state.significant.len());
            let result_splices = build.spliced().len();
            carried.extend(build.spliced().iter().copied());
            let spliced_ids = carried;
            let (added, spans) = build.into_parts();
            let removed = removed_nodes(state, &added, &spliced_ids);
            let stats = BuildStats {
                token_start: window.start,
                token_end: window.end,
                tokens_scanned,
                nodes_built: added.len(),
                adopted_suffix: spliced > 0,
                splices: result_splices,
                adopted_at: first_splice.unwrap_or(0),
                escalations: level as u32,
                full: spliced == 0 && window.start == 0,
            };
            let delta = significant.len() as isize - old.len() as isize;
            let checkpoints = merge_checkpoints(
                state,
                &checkpoints,
                window,
                delta,
                first_diff,
                self.options().max_checkpoints,
            );
            return Some(AstUpdate {
                root,
                added,
                removed,
                diagnostics,
                stats,
                checkpoints,
                next_token_id,
                significant,
                spans,
            });
        }
        None
    }
}

/// The significant tokens of a stream.
#[must_use]
pub fn significant_tokens(tokens: &TokenStream) -> Vec<ParseToken> {
    tokens
        .iter()
        .filter(|token| !token.flags.contains(TokenFlags::TRIVIA))
        .map(|token| ParseToken {
            id: token.id,
            kind: token.kind,
            text: Arc::clone(&token.text),
            range: token.range,
            flags: token.flags,
        })
        .collect()
}

/// First changed index and the number of tokens that still match at the end.
fn divergence(old: &[ParseToken], new: &[ParseToken]) -> (usize, usize) {
    // Identity, not text: a token the lexer re-identified has a different leaf,
    // so the tree must be rebuilt there even when the lexeme is unchanged.
    let mut first = 0usize;
    while first < old.len() && first < new.len() && old[first].id == new[first].id {
        first += 1;
    }
    let mut suffix = 0usize;
    while suffix < old.len().saturating_sub(first)
        && suffix < new.len().saturating_sub(first)
        && old[old.len() - 1 - suffix].id == new[new.len() - 1 - suffix].id
    {
        suffix += 1;
    }
    (first, suffix)
}

/// The latest checkpoint at or before `index`, mapped to a parse start.
fn resume(state: &AstState, index: usize) -> Start {
    state
        .checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.index <= index)
        .max_by_key(|checkpoint| checkpoint.index)
        .map_or(Start::Fresh, |checkpoint| {
            Start::Resume(Box::new(checkpoint.clone()))
        })
}

/// The escalation ladder: one more enclosing construct per step.
struct WindowLadder {
    starts: Vec<usize>,
}

impl WindowLadder {
    fn new(state: &AstState, first_diff: usize) -> Self {
        let mut starts = vec![first_diff.saturating_sub(1)];
        // Walk the old tree from the root down to the changed token, and add one
        // enclosing construct per step.
        let mut path = Vec::new();
        let mut current = Some(state.root);
        let mut seen = FxHashSet::default();
        while let Some(id) = current {
            if !seen.insert(id) {
                break;
            }
            let Some(meta) = state.node(id) else {
                break;
            };
            path.push(meta.first_index);
            if first_diff < meta.first_index || first_diff >= meta.end_index {
                break;
            }
            current = meta
                .record
                .children
                .iter()
                .copied()
                .find(|child| {
                    state.node(*child).is_some_and(|child| {
                        child.first_index <= first_diff && first_diff < child.end_index
                    })
                })
                .or_else(|| {
                    meta.record.children.iter().copied().find(|child| {
                        state
                            .node(*child)
                            .is_some_and(|child| child.first_index == first_diff)
                    })
                });
        }
        for start in path.into_iter().rev() {
            if starts.last().is_some_and(|last| *last == start) {
                continue;
            }
            starts.push(start);
        }
        if starts.last().copied() != Some(0) {
            starts.push(0);
        }
        Self { starts }
    }

    fn levels(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    fn start(&self, level: usize) -> usize {
        self.starts[level.min(self.starts.len().saturating_sub(1))]
    }
}

/// The after-diff: verifies the proposal locally.
fn accepted(state: &AstState, significant: &[ParseToken], result: &ParseResult) -> bool {
    // The root must cover exactly the document.
    let root = match result.build.span(result.root) {
        Some(span) => (span.first(), span.end()),
        None => match state.node(result.root) {
            Some(meta) => (meta.first_index, meta.end_index),
            None => return false,
        },
    };
    if root != (0, significant.len()) {
        return false;
    }
    result.build.iter().all(|(record, span)| {
        span.recovered() || record.token.is_some() || tile(state, &result.build, record, span)
    })
}

/// Token span of one node: rebuilt first, retained second.
fn node_span(state: &AstState, build: &Build, id: u64) -> Option<SpanInfo> {
    build
        .span(id)
        .or_else(|| state.node(id).map(SpanInfo::from_meta))
}

/// Whether one rebuilt node's children tile its token range.
fn tile(state: &AstState, build: &Build, record: &AstNodeRecord, span: SpanInfo) -> bool {
    let mut cursor = span.first();
    for child in &record.children {
        let Some(child_span) = node_span(state, build, *child) else {
            return false;
        };
        if child_span.recovered() {
            cursor = cursor.max(child_span.end());
            continue;
        }
        let width = child_span.end().saturating_sub(child_span.first());
        if width == 0 {
            if child_span.first() < cursor {
                return false;
            }
            continue;
        }
        if child_span.first() != cursor {
            return false;
        }
        cursor = child_span.end();
    }
    if record.children.is_empty() {
        return span.first() == span.end();
    }
    cursor == span.end()
}

/// Old nodes the reparse replaced.
///
/// The new tree is exactly the rebuilt nodes plus the adopted subtrees, so the
/// old tree is walked from its root and pruned wherever a subtree was adopted:
/// everything left is stale. The walk therefore costs the rebuilt region plus
/// its ancestors, not the document.
fn removed_nodes(state: &AstState, added: &[AstNodeRecord], spliced: &FxHashSet<u64>) -> Vec<u64> {
    let added: FxHashSet<u64> = added.iter().map(|record| record.id).collect();
    let mut removed = Vec::new();
    collect_removed(&state.nodes, state.root, spliced, &added, &mut removed, 0);
    removed
}

fn collect_removed(
    nodes: &AstTable,
    id: u64,
    spliced: &FxHashSet<u64>,
    added: &FxHashSet<u64>,
    removed: &mut Vec<u64>,
    depth: usize,
) {
    if depth > 4_096 {
        return;
    }
    let Some(meta) = nodes.get(&id) else {
        return;
    };
    if spliced.contains(&id) {
        // This whole subtree is the new tree: nothing below it is stale.
        return;
    }
    if !added.contains(&id) {
        removed.push(id);
    }
    for child in &meta.record.children {
        if nodes.get(child).is_some() {
            collect_removed(nodes, *child, spliced, added, removed, depth + 1);
        }
    }
}

/// Combines this parse's checkpoints with the ones the splice kept valid.
fn merge_checkpoints(
    state: &AstState,
    recorded: &[Checkpoint],
    window: std::ops::Range<usize>,
    delta: isize,
    first_diff: usize,
    max: usize,
) -> Vec<Checkpoint> {
    // The parse recorded checkpoints for everything it walked. A retained
    // checkpoint before the change is still exact; one after it has to be moved
    // by the token delta (the prefix it resumes from is unchanged, only its
    // position moved), and one whose stack straddles the change is dropped. A
    // retained checkpoint *inside* the rebuilt window is stale — those tokens
    // were re-parsed — so it is replaced by whatever the new parse recorded.
    let mut by_index: std::collections::BTreeMap<usize, Checkpoint> = recorded
        .iter()
        .map(|checkpoint| (checkpoint.index, checkpoint.clone()))
        .collect();
    for checkpoint in &state.checkpoints {
        let candidate = if checkpoint.index <= first_diff {
            checkpoint.clone()
        } else if delta == 0 {
            // Same token count: the stack is unchanged, only its position moved.
            let straddled = checkpoint
                .frames
                .iter()
                .chain(checkpoint.pending.iter())
                .any(|frame| frame.first_index < first_diff && frame.end_index > first_diff);
            if straddled {
                continue;
            }
            checkpoint.clone()
        } else {
            // The token count changed, so a stack to the right of the change may
            // hold children that no longer exist. It is rebuilt as edits walk
            // forward instead of being guessed at.
            continue;
        };
        if window.contains(&candidate.index) {
            continue;
        }
        by_index.entry(candidate.index).or_insert(candidate);
    }
    let mut checkpoints: Vec<Checkpoint> = by_index.into_values().collect();
    checkpoints.sort_by_key(|checkpoint| checkpoint.index);
    checkpoints.dedup_by_key(|checkpoint| checkpoint.index);
    if checkpoints.len() > max {
        // Keep an even sample so both ends of the document stay reachable.
        let step = checkpoints.len().div_ceil(max);
        let mut sampled = Vec::with_capacity(max);
        for (index, checkpoint) in checkpoints.into_iter().enumerate() {
            if index % step == 0 {
                sampled.push(checkpoint);
            }
        }
        checkpoints = sampled;
    }
    checkpoints
}
