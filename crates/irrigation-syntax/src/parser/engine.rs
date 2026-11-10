//! The AST LR/GLR engine.
//!
//! The engine reduces productions straight into [`AstNodeRecord`]s. There is no
//! concrete tree: the LR stack holds `(node identity, token span)` frames, and a
//! reduction allocates one AST node from its popped children.
//!
//! Two properties make the engine incremental:
//!
//! * a [`Checkpoint`] snapshots the whole configuration (LR states, stack
//!   frames, pending recovery nodes, budgets) at a token boundary, so a later
//!   edit can resume in the middle of the document instead of from the start;
//! * a [`SpliceSource`] offers previously parsed subtrees that begin at the
//!   parser's current token. When the entry state matches, the parser pushes the
//!   subtree whole and jumps over its tokens, which is what keeps a reparse
//!   local: the edit's ancestors are rebuilt, their unchanged siblings are not.
//!
//! Splicing is a heuristic: equal states do not prove equal semantics under GLR
//! ambiguity. The caller validates every proposal with the after-diff and grows
//! the window until the proposal holds.

use std::collections::VecDeque;
use std::sync::Arc;

use irrigation::PSeq;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::diagnostic::Diagnostic;
use crate::lexer::{TokenFlags, TokenId, TokenKindId};
use crate::text::{TextAnchor, TextRange, TextSnapshot};

use super::ast::{
    AstNodeRecord, ErrorCategoryId, ast_rule_id, ast_token_id, error_id, literal_value, mix,
    rule_id,
};
use super::grammar::{Grammar, NonterminalId, Production, TerminalId};
use super::lr::Action;

/// One significant token as the engine sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseToken {
    /// Stable identity.
    pub id: TokenId,
    /// Token kind.
    pub kind: TokenKindId,
    /// Lexeme.
    pub text: Arc<str>,
    /// Source range in the version the token was lexed from.
    pub range: TextRange,
    /// Flags.
    pub flags: TokenFlags,
}

impl ParseToken {
    /// Whether the token was synthesized by recovery.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        self.flags.contains(TokenFlags::MISSING)
    }
}

/// Engine budgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineOptions {
    /// Maximum live GLR configurations.
    pub max_configs: usize,
    /// Maximum parse steps across all configurations.
    pub max_steps: usize,
    /// Maximum recovery cost explored for one parse.
    pub max_error_cost: u32,
    /// Maximum missing tokens inserted along one path.
    pub max_missing: u32,
    /// Token distance between checkpoints.
    pub checkpoint_stride: usize,
    /// Maximum retained checkpoints.
    pub max_checkpoints: usize,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            max_configs: 64,
            max_steps: 2_000_000,
            max_error_cost: 16,
            max_missing: 3,
            checkpoint_stride: 1,
            max_checkpoints: 16_384,
        }
    }
}

/// One AST frame on the LR stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackFrame {
    /// AST identity the frame stands for.
    pub ast: u64,
    /// First significant token index covered.
    pub first_index: usize,
    /// End of the covered token range.
    pub end_index: usize,
    /// First token covered, when the frame covers at least one.
    pub first_token: Option<TokenId>,
    /// Last token covered, when the frame covers at least one.
    pub last_token: Option<TokenId>,
    /// Source span.
    pub range: TextRange,
    /// Accumulated dynamic precedence.
    pub dynamic: i32,
    /// Flattened children of an invisible wrapper. Empty for a real node.
    ///
    /// Persistent so that a checkpoint is a snapshot in `O(log n)`: every
    /// checkpoint of a list would otherwise have to copy the children
    /// accumulated so far, which makes dense checkpoints quadratic.
    pub items: PSeq<u64>,
}

/// A complete parser configuration at one token boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    /// Significant token index (the boundary this configuration is at).
    pub index: usize,
    /// LR states, one more than there are frames.
    pub states: Vec<usize>,
    /// Stack frames.
    pub frames: Vec<StackFrame>,
    /// Recovery nodes waiting to be attached to the next reduction.
    pub pending: Vec<StackFrame>,
    /// Recovery cost so far.
    pub error_cost: u32,
    /// Next synthesized token identity.
    pub next_token_id: u64,
}

/// A previously parsed subtree the parser may adopt whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpliceEntry {
    /// Node identity.
    pub node: u64,
    /// LR state the subtree was reduced from.
    pub start_state: usize,
    /// LR state the parser is in after adopting the subtree.
    pub end_state: usize,
    /// Nonterminal the subtree reduces to, or `None` for a token leaf.
    pub lhs: Option<NonterminalId>,
    /// First token covered.
    pub first_token: TokenId,
    /// Last token covered.
    pub last_token: TokenId,
    /// Number of significant tokens covered.
    pub length: usize,
    /// Source span.
    pub range: TextRange,
    /// Accumulated dynamic precedence.
    pub dynamic: i32,
}

/// Supplies splice candidates.
pub trait SpliceSource {
    /// The largest subtree starting at `index` with entry state `state`.
    fn find(&self, index: usize, state: usize) -> Option<SpliceEntry>;
}

/// Where a parse starts.
#[derive(Clone, Debug)]
pub enum Start {
    /// Start of the document.
    Fresh,
    /// Resume from a recorded configuration.
    Resume(Box<Checkpoint>),
}

/// What one parse produced.
#[derive(Debug)]
pub struct ParseResult {
    /// Root identity.
    pub root: u64,
    /// Nodes this parse built.
    pub build: Build,
    /// Recovery diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Recovery cost of the accepted parse.
    pub recovery_cost: u32,
    /// Configurations at stride boundaries this parse passed.
    pub checkpoints: Vec<Checkpoint>,
    /// Number of spliced subtrees.
    pub spliced: usize,
    /// Token index of the first splice, when one happened.
    pub first_splice: Option<usize>,
    /// Highest token index the parse read.
    pub tokens_scanned: usize,
    /// Next synthesized token identity.
    pub next_token_id: u64,
}

/// Token span of one node the parse produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpanInfo {
    first_index: usize,
    end_index: usize,
    first_token: Option<TokenId>,
    last_token: Option<TokenId>,
    recovered: bool,
    start_state: Option<usize>,
    end_state: Option<usize>,
    lhs: Option<NonterminalId>,
    dynamic: i32,
}

/// Facts about how a node was reduced, needed to splice it later.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeAttrs {
    /// LR state the node was reduced from.
    pub start_state: Option<usize>,
    /// LR state the parser is in after adopting the node.
    pub end_state: Option<usize>,
    /// Nonterminal the node reduces to.
    pub lhs: Option<NonterminalId>,
    /// Accumulated dynamic precedence.
    pub dynamic: i32,
}

impl SpanInfo {
    /// Span of a retained node.
    #[must_use]
    pub(crate) fn from_meta(meta: &super::state::NodeMeta) -> Self {
        Self {
            first_index: meta.first_index,
            end_index: meta.end_index,
            first_token: meta.first_token,
            last_token: meta.last_token,
            recovered: meta.record.is_recovery(),
            start_state: meta.start_state,
            end_state: meta.end_state,
            lhs: meta.lhs,
            dynamic: meta.dynamic,
        }
    }

    /// First significant token index covered.
    #[must_use]
    pub fn first(&self) -> usize {
        self.first_index
    }

    /// End of the significant token range covered.
    #[must_use]
    pub fn end(&self) -> usize {
        self.end_index
    }

    /// First token covered, when the node covers at least one.
    #[must_use]
    pub fn first_token(&self) -> Option<TokenId> {
        self.first_token
    }

    /// Last token covered, when the node covers at least one.
    #[must_use]
    pub fn last_token(&self) -> Option<TokenId> {
        self.last_token
    }

    /// Whether the node was produced by recovery.
    #[must_use]
    pub fn recovered(&self) -> bool {
        self.recovered
    }

    /// LR state the node was reduced from.
    #[must_use]
    pub fn start_state(&self) -> Option<usize> {
        self.start_state
    }

    /// LR state after the node.
    #[must_use]
    pub fn end_state(&self) -> Option<usize> {
        self.end_state
    }

    /// Nonterminal the node reduces to.
    #[must_use]
    pub fn lhs(&self) -> Option<NonterminalId> {
        self.lhs
    }

    /// Accumulated dynamic precedence.
    #[must_use]
    pub fn dynamic(&self) -> i32 {
        self.dynamic
    }
}

/// AST nodes produced by one parse.
#[derive(Clone, Debug)]
pub struct Build {
    added: Vec<AstNodeRecord>,
    by_id: FxHashMap<u64, usize>,
    spans: FxHashMap<u64, SpanInfo>,
    spliced: FxHashSet<u64>,
}

impl Default for Build {
    fn default() -> Self {
        Self::new()
    }
}

impl Build {
    fn new() -> Self {
        Self {
            added: Vec::new(),
            by_id: FxHashMap::default(),
            spans: FxHashMap::default(),
            spliced: FxHashSet::default(),
        }
    }

    /// Adds a record, replacing a record with the same identity.
    ///
    /// A reduction whose extent equals one of its children's extent would
    /// otherwise produce a self-reference, so an identity already taken by a
    /// *different* node is bumped deterministically.
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        mut record: AstNodeRecord,
        first_index: usize,
        end_index: usize,
        first_token: Option<TokenId>,
        last_token: Option<TokenId>,
        recovered: bool,
        attrs: NodeAttrs,
    ) -> u64 {
        // An identity already used in this parse belongs to another node. The
        // only way two reductions can compute the same identity is a shared
        // extent (a parent whose first child spans exactly the parent's tokens),
        // and reusing it would make the published tree self-referential, so bump
        // deterministically instead.
        let natural = record.id;
        let mut guard = 1u64;
        while self.by_id.contains_key(&record.id) {
            // Mix the attempt count in: a fixed point of the mix would otherwise
            // make the loop spin without changing the identity.
            record.id = mix(natural ^ guard, 0x51ED_2701_9E37_79B9);
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        let id = record.id;
        if let Some(index) = self.by_id.get(&id).copied() {
            self.added[index] = record;
        } else {
            self.by_id.insert(id, self.added.len());
            self.added.push(record);
        }
        self.spans.insert(
            id,
            SpanInfo {
                first_index,
                end_index,
                first_token,
                last_token,
                recovered,
                start_state: attrs.start_state,
                end_state: attrs.end_state,
                lhs: attrs.lhs,
                dynamic: attrs.dynamic,
            },
        );
        id
    }

    /// Takes the produced records and their token spans.
    #[must_use]
    pub fn into_parts(self) -> (Vec<AstNodeRecord>, FxHashMap<u64, SpanInfo>) {
        (self.added, self.spans)
    }

    /// The produced records.
    #[must_use]
    pub fn records(&self) -> &[AstNodeRecord] {
        &self.added
    }

    /// Iterates the produced records with their token spans.
    pub fn iter(&self) -> impl Iterator<Item = (&AstNodeRecord, SpanInfo)> {
        self.added
            .iter()
            .map(|record| (record, self.spans[&record.id]))
    }

    /// Token span of one produced node.
    #[must_use]
    pub fn span(&self, id: u64) -> Option<SpanInfo> {
        self.spans.get(&id).copied()
    }

    /// Identities this parse adopted from the previous tree.
    #[must_use]
    pub fn spliced(&self) -> &FxHashSet<u64> {
        &self.spliced
    }

    /// Marks one identity as adopted.
    fn mark_spliced(&mut self, id: u64) {
        self.spliced.insert(id);
    }

    /// Number of records produced.
    #[must_use]
    pub fn len(&self) -> usize {
        self.added.len()
    }
}

/// The AST parser: grammar tables plus budgets.
#[derive(Clone, Debug)]
pub struct AstParser {
    grammar: Arc<Grammar>,
    options: EngineOptions,
    terminal_by_kind: FxHashMap<TokenKindId, TerminalId>,
}

impl AstParser {
    /// Builds an AST parser from a compiled grammar.
    #[must_use]
    pub fn new(grammar: Grammar) -> Self {
        Self::with_options(grammar, EngineOptions::default())
    }

    /// Builds an AST parser with explicit budgets.
    #[must_use]
    pub fn with_options(grammar: Grammar, options: EngineOptions) -> Self {
        let terminal_by_kind = grammar
            .terminals()
            .iter()
            .map(|terminal| (terminal.token_kind, terminal.id))
            .collect();
        Self {
            grammar: Arc::new(grammar),
            options,
            terminal_by_kind,
        }
    }

    /// The grammar this parser was built from.
    #[must_use]
    pub fn grammar(&self) -> &Grammar {
        &self.grammar
    }

    /// Engine budgets.
    #[must_use]
    pub fn options(&self) -> EngineOptions {
        self.options
    }

    /// Runs one parse.
    ///
    /// Returns `None` when no configuration accepted the token stream: the
    /// caller decides whether to try a wider window or to publish a salvage
    /// tree, so a failed attempt never passes for an accepted one.
    pub fn parse(&self, request: Request<'_>) -> Option<ParseResult> {
        let Request {
            tokens,
            text,
            start,
            splice,
        } = request;
        let fresh_next_token_id = next_synthesized_token_id(tokens);
        let (initial, next_token_id) = match start {
            Start::Fresh => (
                vec![Config {
                    states: vec![0],
                    frames: Vec::new(),
                    pending: Vec::new(),
                    index: 0,
                    missing: 0,
                    error_cost: 0,
                    splice_start: None,
                    spliced: 0,
                    build: Build::new(),
                    diagnostics: Vec::new(),
                }],
                fresh_next_token_id,
            ),
            Start::Resume(checkpoint) => (
                vec![Config {
                    states: checkpoint.states.clone(),
                    frames: checkpoint.frames.clone(),
                    pending: checkpoint.pending.clone(),
                    index: checkpoint.index,
                    missing: 0,
                    error_cost: checkpoint.error_cost,
                    splice_start: None,
                    spliced: 0,
                    build: Build::new(),
                    diagnostics: Vec::new(),
                }],
                // A resumed parse continues the previous identity space; it may
                // never go back behind an identity the stream already uses.
                checkpoint.next_token_id.max(fresh_next_token_id),
            ),
        };
        let mut ctx = Ctx {
            tokens,
            text,
            next_token_id,
        };
        let mut configs: VecDeque<Config> = initial.into();
        let mut accepted: Vec<Accepted> = Vec::new();
        let mut steps = 0usize;
        let mut deterministic = true;
        let mut checkpoints: Vec<Checkpoint> = Vec::new();
        let mut seen: FxHashMap<(usize, Vec<usize>), u32> = FxHashMap::default();
        let mut scanned = 0usize;

        while let Some(mut config) = configs.pop_front() {
            if configs.len() >= self.options.max_configs || steps >= self.options.max_steps {
                break;
            }
            let key = (config.index, config.states.clone());
            if seen
                .get(&key)
                .is_some_and(|cost| config.error_cost >= *cost)
            {
                continue;
            }
            seen.insert(key, config.error_cost);
            if !configs.is_empty() {
                deterministic = false;
                checkpoints.clear();
            }
            loop {
                if steps >= self.options.max_steps {
                    break;
                }
                steps += 1;
                scanned = scanned.max(config.index);
                let state = *config.states.last().expect("state stack is never empty");
                if let Some(source) = splice {
                    if let Some(entry) = source.find(config.index, state) {
                        if self.valid_splice(&entry, tokens, config.index) {
                            config.build.mark_spliced(entry.node);
                            config.build.spans.insert(
                                entry.node,
                                SpanInfo {
                                    first_index: config.index,
                                    end_index: config.index + entry.length,
                                    first_token: Some(entry.first_token),
                                    last_token: Some(entry.last_token),
                                    recovered: false,
                                    start_state: Some(entry.start_state),
                                    end_state: Some(entry.end_state),
                                    lhs: entry.lhs,
                                    dynamic: entry.dynamic,
                                },
                            );
                            config.frames.push(StackFrame {
                                ast: entry.node,
                                first_index: config.index,
                                end_index: config.index + entry.length,
                                first_token: Some(entry.first_token),
                                last_token: Some(entry.last_token),
                                range: entry.range,
                                dynamic: entry.dynamic,
                                items: PSeq::new(),
                            });
                            config.states.push(entry.end_state);
                            config.splice_start.get_or_insert(config.index);
                            config.index += entry.length;
                            config.spliced += 1;
                            continue;
                        }
                    }
                }
                let terminal = tokens
                    .get(config.index)
                    .and_then(|token| self.terminal_by_kind.get(&token.kind).copied())
                    .unwrap_or_else(|| self.grammar.eof_terminal());
                let actions = self.grammar.table().actions(state, terminal);
                if actions.is_empty() {
                    // No action for this lookahead. A reduction that is legal for
                    // *some* lookahead is often the only way forward: at EOF the
                    // last value still has to reduce before the missing closer
                    // can be inserted.
                    let mut reduced = false;
                    for candidate in 0..self.grammar.eof_terminal() {
                        for action in self.grammar.table().actions(state, candidate) {
                            let Action::Reduce(_) = action else {
                                continue;
                            };
                            let mut next = config.clone();
                            if matches!(self.apply(&mut next, *action, &mut ctx), Step::Continue)
                                && configs.len() < self.options.max_configs
                            {
                                configs.push_back(next);
                                reduced = true;
                            }
                        }
                    }
                    if reduced {
                        break;
                    }
                    self.recover(&config, &mut configs, &mut ctx, terminal);
                    break;
                }
                if actions
                    .iter()
                    .any(|action| matches!(action, Action::Accept))
                {
                    let root = accept(&mut config, &mut ctx);
                    accepted.push(config.into_accepted(root));
                    break;
                }
                if actions.len() > 1 {
                    deterministic = false;
                    checkpoints.clear();
                    for action in actions {
                        let mut next = config.clone();
                        match self.apply(&mut next, *action, &mut ctx) {
                            Step::Continue => {
                                if configs.len() < self.options.max_configs {
                                    configs.push_back(next);
                                }
                            }
                            Step::Accept(root) => accepted.push(next.into_accepted(root)),
                            Step::Fail => {}
                        }
                    }
                    break;
                }
                let before = config.index;
                match self.apply(&mut config, actions[0], &mut ctx) {
                    Step::Continue => {
                        if deterministic && config.index > before {
                            self.maybe_checkpoint(&config, ctx.next_token_id, &mut checkpoints);
                        }
                    }
                    Step::Accept(root) => {
                        accepted.push(config.into_accepted(root));
                        break;
                    }
                    Step::Fail => {
                        break;
                    }
                }
            }
        }

        let (root, recovery_cost, splice_start, spliced_count, build, diagnostics) = {
            let chosen = accepted.into_iter().min_by(|left, right| {
                left.error_cost
                    .cmp(&right.error_cost)
                    .then_with(|| right.dynamic.cmp(&left.dynamic))
                    .then_with(|| right.index.cmp(&left.index))
            });
            let accepted = chosen?;
            (
                accepted.root,
                accepted.error_cost,
                accepted.splice_start,
                accepted.spliced,
                accepted.build,
                accepted.diagnostics,
            )
        };

        if !deterministic {
            checkpoints.clear();
        }
        if checkpoints.len() > self.options.max_checkpoints {
            let stride = checkpoints.len() / self.options.max_checkpoints + 1;
            checkpoints = checkpoints.into_iter().step_by(stride).collect();
        }
        Some(ParseResult {
            root,
            build,
            diagnostics,
            recovery_cost,
            checkpoints,
            spliced: spliced_count,
            first_splice: splice_start,
            tokens_scanned: scanned,
            next_token_id: ctx.next_token_id,
        })
    }

    /// Whether a splice candidate is legal in the current stream.
    fn valid_splice(&self, entry: &SpliceEntry, tokens: &[ParseToken], index: usize) -> bool {
        if entry.length == 0 {
            return false;
        }
        let Some(first) = tokens.get(index) else {
            return false;
        };
        if first.id != entry.first_token {
            return false;
        }
        let Some(last) = tokens.get(index + entry.length - 1) else {
            return false;
        };
        if last.id != entry.last_token {
            return false;
        }
        match entry.lhs {
            Some(lhs) => self.grammar.table().goto(entry.start_state, lhs) == Some(entry.end_state),
            None => self
                .grammar
                .terminal_for_kind(first.kind)
                .is_some_and(|terminal| {
                    super::state::shift_target(&self.grammar, entry.start_state, terminal)
                        == Some(entry.end_state)
                }),
        }
    }

    /// Records a checkpoint when the parse crosses a stride boundary.
    fn maybe_checkpoint(&self, config: &Config, next_token_id: u64, out: &mut Vec<Checkpoint>) {
        if config.index % self.options.checkpoint_stride != 0 {
            return;
        }
        if out.last().is_some_and(|last| last.index == config.index) {
            return;
        }
        out.push(Checkpoint {
            index: config.index,
            states: config.states.clone(),
            frames: config.frames.clone(),
            pending: config.pending.clone(),
            error_cost: config.error_cost,
            next_token_id,
        });
    }

    /// Applies one table action to a configuration.
    fn apply(&self, config: &mut Config, action: Action, ctx: &mut Ctx<'_>) -> Step {
        match action {
            Action::Shift(next) => {
                let Some(token) = ctx.tokens.get(config.index) else {
                    return Step::Fail;
                };
                let index = config.index;
                let previous_state = config.states.last().copied();
                let ast = token_record(
                    token,
                    index,
                    previous_state.unwrap_or(next),
                    &mut config.build,
                );
                config.frames.push(StackFrame {
                    ast,
                    first_index: index,
                    end_index: index + 1,
                    first_token: Some(token.id),
                    last_token: Some(token.id),
                    range: token.range,
                    dynamic: 0,
                    items: PSeq::new(),
                });
                let _ = previous_state;
                config.states.push(next);
                config.index += 1;
                Step::Continue
            }
            Action::Reduce(production_id) => {
                let production = &self.grammar.productions()[production_id];
                let count = production.rhs.len();
                if config.frames.len() < count {
                    return Step::Fail;
                }
                let mut children = config.frames.split_off(config.frames.len() - count);
                config.states.truncate(config.states.len() - count);
                children.append(&mut config.pending);
                self.reduce(config, production, children, ctx);
                Step::Continue
            }
            Action::Accept => Step::Accept(accept(config, ctx)),
            Action::ReduceRn { .. } => Step::Fail,
        }
    }

    /// Reduces one production over `children` into a new frame.
    fn reduce(
        &self,
        config: &mut Config,
        production: &Production,
        mut children: Vec<StackFrame>,
        ctx: &mut Ctx<'_>,
    ) {
        // Pending recovery nodes are appended when a reduction consumes them;
        // sorting restores the source order every later pass relies on.
        children.sort_by_key(|child| (child.first_index, child.end_index));
        let left_state = *config.states.last().expect("state stack is never empty");
        // Invisible wrappers (`$groupN`, `$repeatN`) contribute their children to
        // the parent instead of a node of their own: a repetition is a list, not
        // a chain that every later edit has to rebuild.
        let mut child_ids: Vec<u64> = Vec::with_capacity(children.len());
        let mut positions: Vec<Option<u64>> = Vec::with_capacity(children.len());
        for child in &children {
            if child.ast == 0 {
                child_ids.extend(child.items.iter().copied());
                positions.push(
                    child
                        .items
                        .get(child.items.len().saturating_sub(1))
                        .copied(),
                );
            } else {
                child_ids.push(child.ast);
                positions.push(Some(child.ast));
            }
        }
        let kind = production
            .ast_action
            .as_ref()
            .map_or_else(|| production.label.clone(), |action| action.name.clone());
        let rule = rule_id(&kind);
        let (first_index, end_index, range) = span_of(&children, config.index, ctx);
        // A left-recursive chain starts with a zero-width base case, so the
        // first *token-bearing* descendant is the node's first token.
        let first_token = children.iter().find_map(|child| child.first_token);
        let last_token = children.iter().rev().find_map(|child| child.last_token);
        let dynamic = production.dynamic + children.iter().map(|child| child.dynamic).sum::<i32>();
        let fields = production
            .ast_action
            .as_ref()
            .map(|action| {
                action
                    .fields
                    .iter()
                    .filter_map(|(name, position)| {
                        let child = positions.get(*position).copied().flatten()?;
                        Some((name.clone(), child))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let goto = self
            .grammar
            .table()
            .goto(left_state, production.lhs)
            .unwrap_or(left_state);
        if production.transparent {
            config.frames.push(StackFrame {
                ast: 0,
                first_index,
                end_index,
                first_token,
                last_token,
                range,
                dynamic,
                items: child_ids.into_iter().collect(),
            });
            config.states.push(goto);
            return;
        }
        let id = ast_rule_id(
            rule,
            first_token,
            last_token,
            ctx.tokens
                .get(first_index.saturating_sub(1))
                .map(|token| token.id),
            ctx.tokens.get(first_index).map(|token| token.id),
        );
        let recovered = children.iter().any(|child| {
            child.ast == 0
                && child.items.iter().any(|item| {
                    config
                        .build
                        .spans
                        .get(item)
                        .is_some_and(SpanInfo::recovered)
                })
                || config
                    .build
                    .spans
                    .get(&child.ast)
                    .is_some_and(SpanInfo::recovered)
        });
        let id = config.build.push(
            AstNodeRecord {
                id,
                kind,
                rule,
                span_start: range.start,
                span_end: range.end,
                children: child_ids,
                token: None,
                text: None,
                category: None,
                ignored: false,
                value: None,
                fields,
            },
            first_index,
            end_index,
            first_token,
            last_token,
            recovered,
            NodeAttrs {
                start_state: Some(left_state),
                end_state: Some(goto),
                lhs: Some(production.lhs),
                dynamic,
            },
        );
        config.frames.push(StackFrame {
            ast: id,
            first_index,
            end_index,
            first_token,
            last_token,
            range,
            dynamic,
            items: PSeq::new(),
        });
        config.states.push(goto);
    }

    /// Explores recovery for a configuration with no table action.
    ///
    /// Each alternative forks the configuration before it builds its recovery
    /// node, so the record belongs to the branch whose frames reference it and a
    /// branch that loses leaves nothing behind in the published build.
    fn recover(
        &self,
        config: &Config,
        configs: &mut VecDeque<Config>,
        ctx: &mut Ctx<'_>,
        terminal: TerminalId,
    ) {
        if config.error_cost >= self.options.max_error_cost {
            return;
        }
        let state = *config.states.last().expect("state stack is never empty");

        // 1. Skip the offending token.
        if let Some(token) = ctx.tokens.get(config.index) {
            let range = token.range;
            let index = config.index;
            let mut next = config.clone();
            let id = next.build.push(
                AstNodeRecord {
                    id: error_id(ErrorCategoryId::RECOVERY, [token.id.0]),
                    kind: String::from("$error"),
                    rule: 0,
                    span_start: token.range.start,
                    span_end: token.range.end,
                    children: Vec::new(),
                    token: Some(token.id),
                    text: Some(std::sync::Arc::clone(&token.text)),
                    category: Some(ErrorCategoryId::RECOVERY),
                    ignored: token.flags.contains(TokenFlags::ERROR),
                    value: None,
                    fields: Vec::new(),
                },
                index,
                index + 1,
                Some(token.id),
                Some(token.id),
                true,
                NodeAttrs::default(),
            );
            next.pending.push(StackFrame {
                ast: id,
                first_index: index,
                end_index: index + 1,
                first_token: Some(token.id),
                last_token: Some(token.id),
                range,
                dynamic: 0,
                items: PSeq::new(),
            });
            next.index += 1;
            next.error_cost += 1;
            next.diagnostics.push(Diagnostic::error(
                "parse.recover",
                format!("skipped unexpected token `{}`", token.text),
                range,
            ));
            configs.push_back(next);
        }

        // 2. Close an incomplete construct by popping to a state that can
        //    continue with the current lookahead.
        for depth in (1..config.states.len()).rev() {
            let state_at = config.states[depth - 1];
            if self.grammar.table().actions(state_at, terminal).is_empty() {
                continue;
            }
            let popped = config.frames[depth - 1..].to_vec();
            let (first_index, end_index, range) = span_of(&popped, config.index, ctx);
            // A transparent frame contributes its children, never its own
            // sentinel identity: a child id that names no node would make the
            // recovery record a dangling reference.
            let mut child_ids: Vec<u64> = Vec::with_capacity(popped.len());
            for frame in &popped {
                if frame.ast == 0 {
                    child_ids.extend(frame.items.iter().copied());
                } else {
                    child_ids.push(frame.ast);
                }
            }
            let mut next = config.clone();
            next.frames.truncate(depth - 1);
            next.states.truncate(depth);
            let id = next.build.push(
                AstNodeRecord {
                    id: error_id(ErrorCategoryId::RECOVERY, child_ids.iter().copied()),
                    kind: String::from("$error"),
                    rule: 0,
                    span_start: range.start,
                    span_end: range.end,
                    children: child_ids,
                    token: None,
                    text: None,
                    category: Some(ErrorCategoryId::RECOVERY),
                    ignored: false,
                    value: None,
                    fields: Vec::new(),
                },
                first_index,
                end_index,
                popped.first().and_then(|frame| frame.first_token),
                popped.last().and_then(|frame| frame.last_token),
                true,
                NodeAttrs::default(),
            );
            next.pending.push(StackFrame {
                ast: id,
                first_index,
                end_index,
                first_token: popped.first().and_then(|frame| frame.first_token),
                last_token: popped.last().and_then(|frame| frame.last_token),
                range,
                dynamic: 0,
                items: PSeq::new(),
            });
            next.error_cost += 1;
            next.diagnostics.push(Diagnostic::error(
                "parse.recover_state",
                String::from("recovered by closing an incomplete construct"),
                range,
            ));
            configs.push_back(next);
            break;
        }

        // 3. Insert a missing token when one shift would unblock the state.
        let mut inserted = 0u32;
        for candidate in 0..self.grammar.eof_terminal() {
            if inserted >= self.options.max_missing || config.missing >= self.options.max_missing {
                break;
            }
            let shift = self
                .grammar
                .table()
                .actions(state, candidate)
                .iter()
                .find_map(|action| match action {
                    Action::Shift(target) => Some(*target),
                    _ => None,
                });
            let Some(shift) = shift else {
                continue;
            };
            if self.grammar.table().actions(shift, terminal).is_empty() {
                continue;
            }
            let anchor = ctx
                .tokens
                .get(config.index)
                .map_or_else(|| end_anchor(ctx.text), |token| token.range.start);
            let range = TextRange::empty(anchor);
            // The gap is the node's identity: the token before it and the
            // lookahead it was inserted for. Neither changes when the parse
            // takes a different path, so an unchanged missing token keeps its
            // identity across revisions.
            let before = config
                .index
                .checked_sub(1)
                .and_then(|index| ctx.tokens.get(index))
                .map(|token| token.id.0);
            let after = ctx.tokens.get(config.index).map(|token| token.id.0);
            let id = error_id(
                ErrorCategoryId::MISSING,
                [
                    before.unwrap_or(u64::MAX),
                    after.unwrap_or(u64::MAX),
                    candidate as u64,
                ],
            );
            // A synthesized token covers no source text, so it has no identity
            // in the token stream: publishing one would claim a real token's
            // `TOKENS` unit.
            let mut next = config.clone();
            let id = next.build.push(
                AstNodeRecord {
                    id,
                    kind: String::from("$missing"),
                    rule: 0,
                    span_start: range.start,
                    span_end: range.end,
                    children: Vec::new(),
                    token: None,
                    text: None,
                    category: Some(ErrorCategoryId::MISSING),
                    ignored: false,
                    value: None,
                    fields: Vec::new(),
                },
                config.index,
                config.index,
                None,
                None,
                true,
                NodeAttrs::default(),
            );
            let frame = StackFrame {
                ast: id,
                first_index: config.index,
                end_index: config.index,
                first_token: None,
                last_token: None,
                range,
                dynamic: 0,
                items: PSeq::new(),
            };
            next.missing += 1;
            next.error_cost += 1;
            next.states.push(shift);
            next.frames.push(frame);
            next.diagnostics.push(Diagnostic::error(
                "parse.missing",
                format!(
                    "inserted missing `{}`",
                    self.grammar.terminals()[candidate].name
                ),
                range,
            ));
            configs.push_back(next);
            inserted += 1;
        }
    }
}

/// Mutable state of one parse.
struct Ctx<'a> {
    tokens: &'a [ParseToken],
    text: &'a Arc<TextSnapshot>,
    next_token_id: u64,
}

/// Builds the root from an accepting configuration.
fn accept(config: &mut Config, ctx: &mut Ctx<'_>) -> u64 {
    let mut frames = std::mem::take(&mut config.frames);
    frames.append(&mut config.pending);
    if let [frame] = frames.as_slice() {
        // A transparent reduction is only a container for its children: it has
        // no identity of its own, and at the root there is no parent to hand
        // them to. Fall through and synthesize the `$root` node instead of
        // publishing the sentinel as the tree's root.
        if frame.ast != 0 {
            return frame.ast;
        }
    }
    let (first_index, end_index, range) = span_of_config(&frames, config.index, ctx);
    let mut child_ids: Vec<u64> = Vec::with_capacity(frames.len());
    for frame in &frames {
        if frame.ast == 0 {
            child_ids.extend(frame.items.iter().copied());
        } else {
            child_ids.push(frame.ast);
        }
    }
    let rule = rule_id("$root");
    let id = ast_rule_id(
        rule,
        frames.first().and_then(|frame| frame.first_token),
        frames.last().and_then(|frame| frame.last_token),
        None,
        None,
    );
    config.build.push(
        AstNodeRecord {
            id,
            kind: String::from("$root"),
            rule,
            span_start: range.start,
            span_end: range.end,
            children: child_ids,
            token: None,
            text: None,
            category: None,
            ignored: false,
            value: None,
            fields: Vec::new(),
        },
        first_index,
        end_index,
        frames.first().and_then(|frame| frame.first_token),
        frames.last().and_then(|frame| frame.last_token),
        false,
        NodeAttrs::default(),
    )
}

/// Builds a recovery tree covering the whole token stream.
fn salvage(tokens: &[ParseToken], text: &TextSnapshot, build: &mut Build) -> u64 {
    let mut children = Vec::with_capacity(tokens.len());
    for (index, token) in tokens.iter().enumerate() {
        children.push(token_record(token, index, 0, build));
    }
    let first_token = tokens.first().map(|token| token.id);
    let last_token = tokens.last().map(|token| token.id);
    let start = tokens
        .first()
        .map_or_else(|| end_anchor(text), |token| token.range.start);
    let end = tokens
        .last()
        .map_or_else(|| end_anchor(text), |token| token.range.end);
    let rule = rule_id("$error");
    let id = ast_rule_id(rule, first_token, last_token, None, None);
    build.push(
        AstNodeRecord {
            id,
            kind: String::from("$error"),
            rule,
            span_start: start,
            span_end: end,
            children,
            token: None,
            text: None,
            category: Some(ErrorCategoryId::RECOVERY),
            ignored: false,
            value: None,
            fields: Vec::new(),
        },
        0,
        tokens.len(),
        first_token,
        last_token,
        true,
        NodeAttrs::default(),
    )
}

/// Next synthesized token identity for a stream.
pub(crate) fn next_synthesized_token_id(tokens: &[ParseToken]) -> u64 {
    tokens
        .iter()
        .map(|token| token.id.0)
        .max()
        .map_or(0, |max| max + 1)
}

/// The tree a from-scratch parse publishes when it cannot accept the input.
///
/// A failed incremental attempt must be distinguishable from an accepted tree,
/// so this lives on the from-scratch path only: the repair ladder and the
/// component's full-parse fallback both need `parse` to report failure.
pub(crate) fn salvage_result(
    tokens: &[ParseToken],
    text: &TextSnapshot,
) -> (u64, Build, Vec<Diagnostic>) {
    let mut build = Build::new();
    let root = salvage(tokens, text, &mut build);
    // The diagnostic marks the region that could not be structured -- the same span
    // the recovery node covers. It used to be an empty range at the end of the
    // document, which pointed at the one place the failure is *not*: an unparsable
    // prefix reported its error past the end of its own text.
    let span = match (tokens.first(), tokens.last()) {
        (Some(first), Some(last)) => TextRange::new(first.range.start, last.range.end),
        _ => TextRange::empty(end_anchor(text)),
    };
    let diagnostics = vec![Diagnostic::error(
        "parse.failed",
        String::from("no configuration accepted the input"),
        span,
    )];
    (root, build, diagnostics)
}

/// Builds the record of one token leaf and returns its identity.
fn token_record(token: &ParseToken, index: usize, state: usize, build: &mut Build) -> u64 {
    let (id, kind, category) = if token.is_missing() {
        (
            error_id(ErrorCategoryId::MISSING, [token.id.0]),
            "$missing",
            Some(ErrorCategoryId::MISSING),
        )
    } else {
        (ast_token_id(token.id), "$token", None)
    };
    let value = if category.is_none() {
        Some(literal_value(token.text.as_ref()))
    } else {
        None
    };
    build.push(
        AstNodeRecord {
            id,
            kind: String::from(kind),
            rule: 0,
            span_start: token.range.start,
            span_end: token.range.end,
            children: Vec::new(),
            token: Some(token.id),
            text: Some(std::sync::Arc::clone(&token.text)),
            category,
            ignored: false,
            value,
            fields: Vec::new(),
        },
        index,
        index + 1,
        Some(token.id),
        Some(token.id),
        category.is_some(),
        NodeAttrs {
            start_state: Some(state),
            end_state: None,
            lhs: None,
            dynamic: 0,
        },
    );
    id
}

/// Span of a run of frames.
fn span_of_config(frames: &[StackFrame], index: usize, ctx: &Ctx<'_>) -> (usize, usize, TextRange) {
    span_of(frames, index, ctx)
}

/// Span of a run of frames.
fn span_of(frames: &[StackFrame], index: usize, ctx: &Ctx<'_>) -> (usize, usize, TextRange) {
    match (frames.first(), frames.last()) {
        (Some(first), Some(last)) => (
            first.first_index,
            last.end_index,
            TextRange::new(first.range.start, last.range.end),
        ),
        _ => {
            let anchor = ctx
                .tokens
                .get(index)
                .map_or_else(|| end_anchor(ctx.text), |token| token.range.start);
            (index, index, TextRange::empty(anchor))
        }
    }
}

/// Anchor at the end of the document.
fn end_anchor(text: &TextSnapshot) -> TextAnchor {
    text.anchor(text.len()).expect("document end anchor")
}

impl Config {
    /// Freezes the accepted branch: its build and diagnostics are the result.
    fn into_accepted(mut self, root: u64) -> Accepted {
        Accepted {
            root,
            error_cost: self.error_cost,
            dynamic: dynamic_total(&self),
            index: self.index,
            splice_start: self.splice_start,
            spliced: self.spliced,
            build: std::mem::take(&mut self.build),
            diagnostics: std::mem::take(&mut self.diagnostics),
        }
    }
}

fn dynamic_total(config: &Config) -> i32 {
    config
        .frames
        .iter()
        .chain(config.pending.iter())
        .map(|frame| frame.dynamic)
        .sum()
}

/// One configuration.
#[derive(Clone, Debug)]
struct Config {
    states: Vec<usize>,
    frames: Vec<StackFrame>,
    pending: Vec<StackFrame>,
    index: usize,
    missing: u32,
    error_cost: u32,
    /// Token index of this branch's first splice.
    splice_start: Option<usize>,
    /// Subtrees this branch adopted.
    spliced: usize,
    /// Nodes this branch built. Per branch, so an abandoned GLR alternative
    /// never contributes a record to the published build.
    build: Build,
    /// Diagnostics this branch produced.
    diagnostics: Vec<Diagnostic>,
}

struct Accepted {
    root: u64,
    error_cost: u32,
    dynamic: i32,
    index: usize,
    splice_start: Option<usize>,
    spliced: usize,
    build: Build,
    diagnostics: Vec<Diagnostic>,
}

enum Step {
    Continue,
    Accept(u64),
    Fail,
}

/// One request to the engine.
pub struct Request<'a> {
    /// Significant tokens.
    pub tokens: &'a [ParseToken],
    /// Coordinate space of the tokens.
    pub text: &'a Arc<TextSnapshot>,
    /// Where to start.
    pub start: Start,
    /// Subtrees that can be adopted whole.
    pub splice: Option<&'a dyn SpliceSource>,
}

impl std::fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("tokens", &self.tokens.len())
            .field("text", &self.text.len())
            .field("splice", &self.splice.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod salvage_span_tests {
    use super::*;

    fn token(id: u64, text: &TextSnapshot, start: usize, end: usize) -> ParseToken {
        ParseToken {
            id: TokenId(id),
            kind: TokenKindId(0),
            text: Arc::from("x"),
            range: TextRange::new(
                TextAnchor::new(text.version(), start),
                TextAnchor::new(text.version(), end),
            ),
            flags: TokenFlags::default(),
        }
    }

    /// The failure diagnostic marks the region that could not be structured. It used
    /// to be an empty range at the end of the document, so an unparsable prefix
    /// reported its error past the end of its own text.
    #[test]
    fn a_failed_parse_marks_the_region_it_could_not_structure() {
        let text = TextSnapshot::new("xx");
        let tokens = [token(0, &text, 0, 1), token(1, &text, 1, 2)];
        let (_root, _build, diagnostics) = salvage_result(&tokens, &text);
        let span = diagnostics[0].range;
        assert_eq!(
            (span.start.offset, span.end.offset),
            (0, 2),
            "the diagnostic covers the salvaged tokens"
        );
    }

    /// An empty stream has no region to mark, so it still points at the end.
    #[test]
    fn an_empty_stream_still_points_at_the_end() {
        let text = TextSnapshot::new("");
        let (_root, _build, diagnostics) = salvage_result(&[], &text);
        let span = diagnostics[0].range;
        assert_eq!(span.start.offset, span.end.offset);
    }
}
