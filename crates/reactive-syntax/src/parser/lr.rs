//! Canonical LR(1) table construction.
//!
//! The plan's default is LALR(1); canonical LR(1) is implemented first because
//! it is the simplest table whose reuse and conflict behavior can be verified
//! independently. The state representation and table interface are shared, so
//! LALR/IELR can replace the construction algorithm without changing the
//! runtime.

use std::collections::{BTreeMap, BTreeSet};

use super::grammar::{
    Associativity, NonterminalId, PrecedenceLevel, Production, ProductionId, Symbol, Terminal,
    TerminalId,
};

/// Parser action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Shift the lookahead and go to the state.
    Shift(usize),
    /// Reduce by the production.
    Reduce(ProductionId),
    /// Right-nullable prefix reduction with a prebuilt empty suffix.
    ReduceRn {
        /// Production.
        production: ProductionId,
        /// Number of RHS symbols in the consumed prefix.
        prefix: usize,
        /// Number of nullable suffix symbols represented by empty children.
        suffix_len: usize,
    },
    /// Accept the input.
    Accept,
}

/// One canonical LR(1) item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Item {
    /// Production.
    pub production: ProductionId,
    /// Dot position.
    pub dot: usize,
    /// Lookahead terminal.
    pub lookahead: TerminalId,
}

/// One LR state.
#[derive(Clone, Debug)]
pub struct State {
    /// Closed item set.
    pub items: Vec<Item>,
    /// Transitions by symbol.
    pub transitions: Vec<(Symbol, usize)>,
}

/// Table construction mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TableMode {
    /// Merge canonical LR(1) states with the same LR(0) core.
    #[default]
    Lalr,
    /// Keep canonical LR(1) states.
    Lr1,
    /// IELR mode. Currently uses canonical LR(1) states as a correctness
    /// fallback; no state-minimization pass is applied yet.
    Ielr,
}

/// Compiled LR table.
#[derive(Clone, Debug)]
pub struct RnReduction {
    /// State containing the prefix item.
    pub state: usize,
    /// Production being reduced.
    pub production: ProductionId,
    /// Number of RHS symbols consumed by the prefix.
    pub prefix: usize,
    /// Number of nullable RHS symbols represented by the prebuilt empty suffix.
    pub suffix_len: usize,
    /// Lookahead terminal.
    pub lookahead: TerminalId,
}

/// Compiled LR table.
#[derive(Clone, Debug)]
pub struct Table {
    states: Vec<State>,
    actions: Vec<Vec<Vec<Action>>>,
    gotos: Vec<Vec<Option<usize>>>,
    rn_reductions: Vec<RnReduction>,
    terminal_count: usize,
    nonterminal_count: usize,
    eof_terminal: TerminalId,
}

impl Table {
    /// All states.
    #[must_use]
    pub fn states(&self) -> &[State] {
        &self.states
    }

    /// Actions for one state and terminal.
    #[must_use]
    pub fn actions(&self, state: usize, terminal: TerminalId) -> &[Action] {
        &self.actions[state][terminal]
    }

    /// Goto state for a nonterminal.
    #[must_use]
    pub fn goto(&self, state: usize, nonterminal: NonterminalId) -> Option<usize> {
        self.gotos[state][nonterminal]
    }

    /// Number of states.
    #[must_use]
    pub fn state_count(&self) -> usize {
        self.states.len()
    }

    /// Number of terminals, including EOF.
    #[must_use]
    pub fn terminal_count(&self) -> usize {
        self.terminal_count
    }

    /// Number of nonterminals.
    #[must_use]
    pub fn nonterminal_count(&self) -> usize {
        self.nonterminal_count
    }

    /// Right-nullable table reduction entries.
    #[must_use]
    pub fn rn_reductions(&self) -> &[RnReduction] {
        &self.rn_reductions
    }

    /// EOF terminal.
    #[must_use]
    pub fn eof_terminal(&self) -> TerminalId {
        self.eof_terminal
    }
}

/// Table construction error.
#[derive(Debug, thiserror::Error)]
pub enum TableError {
    /// A table cell has multiple conflicting actions.
    #[error(
        "conflict in state {state} on terminal {terminal}: {actions:?}; add precedence/associativity or declare `conflict A, B;`"
    )]
    Conflict {
        /// State id.
        state: usize,
        /// Terminal id.
        terminal: TerminalId,
        /// Conflicting actions.
        actions: Vec<Action>,
        /// Production labels involved.
        productions: Vec<String>,
    },
    /// The start production is malformed.
    #[error("invalid start production")]
    InvalidStart,
}

/// Builds an LR table.
///
/// # Errors
///
/// Fails on an unresolved table conflict or a malformed start production.
#[allow(clippy::too_many_arguments)]
pub fn build(
    terminals: &[Terminal],
    nonterminals: &[super::grammar::Nonterminal],
    productions: &[Production],
    precedence: &[PrecedenceLevel],
    conflicts: &[(String, String)],
    start_production: ProductionId,
    eof_terminal: TerminalId,
    mode: TableMode,
) -> Result<Table, TableError> {
    let Some(start) = productions.get(start_production) else {
        return Err(TableError::InvalidStart);
    };
    if !start.augmented || start.rhs.len() != 1 {
        return Err(TableError::InvalidStart);
    }
    let terminal_count = terminals.len();
    let nonterminal_count = nonterminals.len();
    let canonical = build_states(
        terminals,
        productions,
        nonterminals,
        start_production,
        eof_terminal,
    );
    let states = match mode {
        TableMode::Lr1 | TableMode::Ielr => canonical,
        TableMode::Lalr => merge_lalr(canonical),
    };
    make_table(
        states,
        terminals,
        nonterminals,
        productions,
        precedence,
        conflicts,
        terminal_count,
        nonterminal_count,
        eof_terminal,
    )
}

fn build_states(
    terminals: &[Terminal],
    productions: &[Production],
    nonterminals: &[super::grammar::Nonterminal],
    start_production: ProductionId,
    eof_terminal: TerminalId,
) -> Vec<State> {
    let terminal_count = terminals.len();
    let nullable = nullable(nonterminals, productions);
    let first = first_sets(nonterminals, productions, terminal_count, &nullable);
    let by_lhs = productions_by_lhs(productions, nonterminals.len());
    let start_item = Item {
        production: start_production,
        dot: 0,
        lookahead: eof_terminal,
    };
    let start_items = closure(vec![start_item], productions, &by_lhs, &nullable, &first);

    let mut states: Vec<State> = Vec::new();
    let mut state_index: BTreeMap<Vec<Item>, usize> = BTreeMap::new();
    let mut queue: Vec<usize> = Vec::new();
    state_index.insert(start_items.clone(), 0);
    states.push(State {
        items: start_items,
        transitions: Vec::new(),
    });
    queue.push(0);

    let mut cursor = 0;
    while cursor < queue.len() {
        let state_id = queue[cursor];
        cursor += 1;
        let items = states[state_id].items.clone();
        let mut next_symbols = BTreeSet::new();
        for item in &items {
            if let Some(symbol) = production_symbol(productions, *item, item.dot) {
                next_symbols.insert(symbol);
            }
        }
        let mut transitions = Vec::new();
        for symbol in next_symbols {
            let advanced: Vec<Item> = items
                .iter()
                .filter_map(|item| {
                    (production_symbol(productions, *item, item.dot) == Some(symbol)).then_some(
                        Item {
                            production: item.production,
                            dot: item.dot + 1,
                            lookahead: item.lookahead,
                        },
                    )
                })
                .collect();
            let closed = closure(advanced, productions, &by_lhs, &nullable, &first);
            let next = if let Some(existing) = state_index.get(&closed) {
                *existing
            } else {
                let id = states.len();
                state_index.insert(closed.clone(), id);
                states.push(State {
                    items: closed,
                    transitions: Vec::new(),
                });
                queue.push(id);
                id
            };
            transitions.push((symbol, next));
        }
        states[state_id].transitions = transitions;
    }
    states
}

fn merge_lalr(canonical: Vec<State>) -> Vec<State> {
    let mut core_index: BTreeMap<Vec<(ProductionId, usize)>, usize> = BTreeMap::new();
    let mut canonical_to_merged = Vec::with_capacity(canonical.len());
    let mut merged: Vec<State> = Vec::new();
    for state in &canonical {
        let mut core: Vec<(ProductionId, usize)> = state
            .items
            .iter()
            .map(|item| (item.production, item.dot))
            .collect();
        core.sort_unstable();
        core.dedup();
        let id = if let Some(id) = core_index.get(&core) {
            *id
        } else {
            let id = merged.len();
            core_index.insert(core, id);
            merged.push(State {
                items: Vec::new(),
                transitions: Vec::new(),
            });
            id
        };
        canonical_to_merged.push(id);
    }
    for (canonical_id, state) in canonical.iter().enumerate() {
        let merged_id = canonical_to_merged[canonical_id];
        for item in &state.items {
            if !merged[merged_id].items.contains(item) {
                merged[merged_id].items.push(*item);
            }
        }
        for (symbol, target) in &state.transitions {
            let target = canonical_to_merged[*target];
            if !merged[merged_id]
                .transitions
                .iter()
                .any(|(existing, existing_target)| existing == symbol && *existing_target == target)
            {
                merged[merged_id].transitions.push((*symbol, target));
            }
        }
    }
    for state in &mut merged {
        state.items.sort_unstable();
        state.items.dedup();
        state.transitions.sort_unstable();
        state.transitions.dedup();
    }
    merged
}

#[allow(clippy::too_many_arguments)]
fn make_table(
    states: Vec<State>,
    terminals: &[Terminal],
    nonterminals: &[super::grammar::Nonterminal],
    productions: &[Production],
    precedence: &[PrecedenceLevel],
    conflicts: &[(String, String)],
    terminal_count: usize,
    nonterminal_count: usize,
    eof_terminal: TerminalId,
) -> Result<Table, TableError> {
    let mut actions: Vec<Vec<Vec<Action>>> = vec![vec![Vec::new(); terminal_count]; states.len()];
    let mut gotos: Vec<Vec<Option<usize>>> = vec![vec![None; nonterminal_count]; states.len()];

    for (state_id, state) in states.iter().enumerate() {
        for (symbol, target) in &state.transitions {
            match symbol {
                Symbol::Terminal(terminal) => {
                    push_action(
                        &mut actions[state_id][*terminal],
                        Action::Shift(*target),
                        productions,
                        terminals,
                        precedence,
                        conflicts,
                        state_id,
                        *terminal,
                    )?;
                }
                Symbol::Nonterminal(nonterminal) => {
                    gotos[state_id][*nonterminal] = Some(*target);
                }
            }
        }
        for item in &state.items {
            let production = &productions[item.production];
            if item.dot != production.rhs.len() {
                continue;
            }
            if production.augmented {
                if item.lookahead == eof_terminal {
                    push_action(
                        &mut actions[state_id][eof_terminal],
                        Action::Accept,
                        productions,
                        terminals,
                        precedence,
                        conflicts,
                        state_id,
                        eof_terminal,
                    )?;
                }
            } else {
                push_action(
                    &mut actions[state_id][item.lookahead],
                    Action::Reduce(item.production),
                    productions,
                    terminals,
                    precedence,
                    conflicts,
                    state_id,
                    item.lookahead,
                )?;
            }
        }
    }

    let mut rn_reductions = Vec::new();
    let nullable = nullable(nonterminals, productions);
    for (state_id, state) in states.iter().enumerate() {
        for item in &state.items {
            let production = &productions[item.production];
            if item.dot < production.rhs.len()
                && production.rhs[item.dot..]
                    .iter()
                    .all(|symbol| match symbol {
                        Symbol::Terminal(_) => false,
                        Symbol::Nonterminal(id) => nullable[*id],
                    })
            {
                rn_reductions.push(RnReduction {
                    state: state_id,
                    production: item.production,
                    prefix: item.dot,
                    suffix_len: production.rhs.len() - item.dot,
                    lookahead: item.lookahead,
                });
            }
        }
    }
    rn_reductions.sort_by_key(|reduction| {
        (
            reduction.state,
            reduction.production,
            reduction.prefix,
            reduction.lookahead,
        )
    });
    rn_reductions.dedup_by(|a, b| {
        a.state == b.state
            && a.production == b.production
            && a.prefix == b.prefix
            && a.lookahead == b.lookahead
    });
    Ok(Table {
        states,
        actions,
        gotos,
        rn_reductions,
        terminal_count,
        nonterminal_count,
        eof_terminal,
    })
}

#[allow(clippy::too_many_arguments)]
fn push_action(
    cell: &mut Vec<Action>,
    action: Action,
    productions: &[Production],
    terminals: &[Terminal],
    precedence: &[PrecedenceLevel],
    conflicts: &[(String, String)],
    state: usize,
    terminal: TerminalId,
) -> Result<(), TableError> {
    if cell.contains(&action) {
        return Ok(());
    }
    if cell.is_empty() {
        cell.push(action);
        return Ok(());
    }
    let mut actions = cell.clone();
    actions.push(action);

    if actions.len() == 2 {
        let shift = actions.iter().find_map(|action| match action {
            Action::Shift(target) => Some(*target),
            _ => None,
        });
        let reduce = actions.iter().find_map(|action| match action {
            Action::Reduce(production) => Some(*production),
            Action::ReduceRn { production, .. } => Some(*production),
            _ => None,
        });
        if let (Some(target), Some(reduction)) = (shift, reduce) {
            let reduction_level = productions[reduction].precedence;
            let terminal_level = terminals[terminal].precedence;
            if let (Some(reduction_level), Some(terminal_level)) = (reduction_level, terminal_level)
            {
                match reduction_level.cmp(&terminal_level) {
                    std::cmp::Ordering::Greater => {
                        *cell = vec![Action::Reduce(reduction)];
                        return Ok(());
                    }
                    std::cmp::Ordering::Less => {
                        *cell = vec![Action::Shift(target)];
                        return Ok(());
                    }
                    std::cmp::Ordering::Equal => {
                        let associativity = precedence
                            .iter()
                            .find(|level| level.level == terminal_level)
                            .map_or(Associativity::Left, |level| level.associativity);
                        match associativity {
                            Associativity::Left => {
                                *cell = vec![Action::Reduce(reduction)];
                                return Ok(());
                            }
                            Associativity::Right => {
                                *cell = vec![Action::Shift(target)];
                                return Ok(());
                            }
                            Associativity::NonAssoc => {
                                let labels = action_labels(&actions, productions);
                                return Err(TableError::Conflict {
                                    state,
                                    terminal,
                                    actions,
                                    productions: labels,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    if conflict_is_declared(&actions, productions, conflicts) {
        *cell = actions;
        return Ok(());
    }

    let labels = action_labels(&actions, productions);
    Err(TableError::Conflict {
        state,
        terminal,
        actions,
        productions: labels,
    })
}

fn action_labels(actions: &[Action], productions: &[Production]) -> Vec<String> {
    actions
        .iter()
        .map(|action| match action {
            Action::Reduce(production) | Action::ReduceRn { production, .. } => {
                productions[*production].label.clone()
            }
            Action::Shift(_) => "<shift>".to_string(),
            Action::Accept => "<accept>".to_string(),
        })
        .collect()
}

fn conflict_is_declared(
    actions: &[Action],
    productions: &[Production],
    conflicts: &[(String, String)],
) -> bool {
    let labels = action_labels(actions, productions);
    if conflicts.iter().any(|(left, right)| {
        labels.iter().any(|label| label == left) && labels.iter().any(|label| label == right)
    }) {
        return true;
    }
    // A shift has no production label. If the conflict declaration names the
    // reduce rule, allow the shift/reduce ambiguity to reach GLR.
    if actions
        .iter()
        .any(|action| matches!(action, Action::Shift(_)))
    {
        return actions.iter().any(|action| match action {
            Action::Reduce(production) | Action::ReduceRn { production, .. } => {
                let label = &productions[*production].label;
                conflicts
                    .iter()
                    .any(|(left, right)| left == label || right == label)
            }
            _ => false,
        });
    }
    false
}

fn nullable(nonterminals: &[super::grammar::Nonterminal], productions: &[Production]) -> Vec<bool> {
    let mut nullable = vec![false; nonterminals.len()];
    loop {
        let mut changed = false;
        for production in productions {
            if nullable[production.lhs] {
                continue;
            }
            if production
                .rhs
                .iter()
                .all(|symbol| matches!(symbol, Symbol::Nonterminal(id) if nullable[*id]))
            {
                nullable[production.lhs] = true;
                changed = true;
            }
        }
        if !changed {
            return nullable;
        }
    }
}

fn first_sets(
    nonterminals: &[super::grammar::Nonterminal],
    productions: &[Production],
    terminal_count: usize,
    nullable: &[bool],
) -> Vec<BTreeSet<TerminalId>> {
    let mut first = vec![BTreeSet::new(); nonterminals.len()];
    loop {
        let mut changed = false;
        for production in productions {
            let mut additions = BTreeSet::new();
            let mut all_nullable = true;
            for symbol in &production.rhs {
                match symbol {
                    Symbol::Terminal(terminal) => {
                        additions.insert(*terminal);
                        all_nullable = false;
                        break;
                    }
                    Symbol::Nonterminal(id) => {
                        additions.extend(first[*id].iter().copied());
                        if !nullable[*id] {
                            all_nullable = false;
                            break;
                        }
                    }
                }
            }
            let _ = (terminal_count, all_nullable);
            let target = &mut first[production.lhs];
            let before = target.len();
            target.extend(additions);
            if target.len() != before {
                changed = true;
            }
        }
        if !changed {
            return first;
        }
    }
}

fn productions_by_lhs(
    productions: &[Production],
    nonterminal_count: usize,
) -> Vec<Vec<ProductionId>> {
    let mut by_lhs = vec![Vec::new(); nonterminal_count];
    for production in productions {
        by_lhs[production.lhs].push(production.id);
    }
    by_lhs
}

fn closure(
    items: Vec<Item>,
    productions: &[Production],
    by_lhs: &[Vec<ProductionId>],
    nullable: &[bool],
    first: &[BTreeSet<TerminalId>],
) -> Vec<Item> {
    let mut set: BTreeSet<Item> = items.into_iter().collect();
    let mut queue: Vec<Item> = set.iter().copied().collect();
    while let Some(item) = queue.pop() {
        let production = &productions[item.production];
        let Some(Symbol::Nonterminal(next)) = production.rhs.get(item.dot).copied() else {
            continue;
        };
        let lookaheads = first_of_tail(
            &production.rhs[item.dot + 1..],
            item.lookahead,
            nullable,
            first,
        );
        for production_id in &by_lhs[next] {
            for lookahead in &lookaheads {
                let candidate = Item {
                    production: *production_id,
                    dot: 0,
                    lookahead: *lookahead,
                };
                if set.insert(candidate) {
                    queue.push(candidate);
                }
            }
        }
    }
    set.into_iter().collect()
}

fn first_of_tail(
    tail: &[Symbol],
    lookahead: TerminalId,
    nullable: &[bool],
    first: &[BTreeSet<TerminalId>],
) -> BTreeSet<TerminalId> {
    let mut out = BTreeSet::new();
    for symbol in tail {
        match symbol {
            Symbol::Terminal(terminal) => {
                out.insert(*terminal);
                return out;
            }
            Symbol::Nonterminal(id) => {
                out.extend(first[*id].iter().copied());
                if !nullable[*id] {
                    return out;
                }
            }
        }
    }
    out.insert(lookahead);
    out
}

fn production_symbol(productions: &[Production], item: Item, dot: usize) -> Option<Symbol> {
    productions[item.production].rhs.get(dot).copied()
}
