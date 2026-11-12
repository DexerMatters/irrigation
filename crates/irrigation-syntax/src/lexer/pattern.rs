//! The regular-pattern subset used by lexer rules.
//!
//! The implementation is a small backtracking matcher over a parsed AST. It is
//! not a general regex engine: it supports the plan's subset, including named
//! captures, and returns every reachable end position so the caller can apply
//! maximal munch and guard fallback. Lexer rules are short and the relex window
//! is bounded, so correctness and capture behavior matter more than raw regex
//! throughput here. A DFA fast path can replace the matcher later without
//! changing the spec.

use std::fmt;
use std::sync::{Arc, Mutex};

/// Maximum number of match states retained for one pattern attempt.
const MAX_RESULTS: usize = 256;
/// Maximum matcher steps for one pattern attempt.
const MAX_STEPS: usize = 200_000;

/// A parsed lexer pattern.
#[derive(Clone, Debug)]
pub struct Pattern {
    expr: Expr,
    capture_names: Vec<String>,
    dfa: Option<Arc<Mutex<Dfa>>>,
}

impl Pattern {
    /// Parses a pattern.
    ///
    /// # Errors
    ///
    /// Returns a structured parse error for unsupported or malformed syntax.
    pub fn parse(source: &str) -> Result<Self, PatternError> {
        Parser::new(source).parse()
    }

    /// Builds an exact literal pattern.
    #[must_use]
    pub fn literal(text: &str) -> Self {
        let expr = if text.is_empty() {
            Expr::Empty
        } else {
            Expr::Concat(text.chars().map(Expr::Literal).collect())
        };
        let nfa = Arc::new(Nfa::build(&expr));
        let dfa = Some(Arc::new(Mutex::new(Dfa::new(Arc::clone(&nfa)))));
        Self {
            expr,
            capture_names: Vec::new(),
            dfa,
        }
    }

    /// Named capture groups in index order.
    #[must_use]
    pub fn capture_names(&self) -> &[String] {
        &self.capture_names
    }

    /// Returns the exact literal text for patterns that have no operators or
    /// captures. The lexer uses this as a fast path.
    #[must_use]
    pub fn exact_literal(&self) -> Option<String> {
        fn collect(expr: &Expr, out: &mut String) -> bool {
            match expr {
                Expr::Empty => true,
                Expr::Literal(character) => {
                    out.push(*character);
                    true
                }
                Expr::Concat(items) => items.iter().all(|item| collect(item, out)),
                _ => false,
            }
        }
        let mut literal = String::new();
        collect(&self.expr, &mut literal).then_some(literal)
    }

    /// Conservative first-byte filter for the lexer.
    ///
    /// Returns `true` when the pattern can begin with `byte`, including when it
    /// can match empty or the pattern's first set is not ASCII-representable.
    #[must_use]
    pub fn can_start_with_byte(&self, byte: u8) -> bool {
        let (nullable, mask) = first_ascii(&self.expr);
        nullable || byte >= 128 || mask.is_none_or(|mask| mask & (1u128 << byte) != 0)
    }

    /// All matches starting exactly at `start`, sorted by longest first.
    #[must_use]
    pub fn find_at(&self, text: &str, start: usize) -> Vec<PatternMatch> {
        self.search(text, start).0
    }

    /// All matches at `start`, plus the furthest byte the attempt examined.
    ///
    /// The examination bound is reported even when nothing matched: a rule that
    /// fails still reads text on the way to failing, and the caller's relex
    /// window has to cover that read or a token recognized *because* the rule
    /// failed is reused after the text that made it fail changes.
    pub(crate) fn search(&self, text: &str, start: usize) -> (Vec<PatternMatch>, usize) {
        if start > text.len() || !text.is_char_boundary(start) {
            return (Vec::new(), start.min(text.len()));
        }
        if self.capture_names.is_empty() {
            if let Some(literal) = self.exact_literal() {
                return literal_search(&literal, text, start);
            }
            if let Some(dfa) = &self.dfa {
                return dfa.lock().expect("DFA lock").search(text, start);
            }
        }
        let mut matcher = Matcher {
            text,
            steps: MAX_STEPS,
            furthest: start,
        };
        let caps = vec![None; self.capture_names.len()];
        let mut states = matcher.eval(&self.expr, start, &caps);
        dedupe(&mut states);
        states.sort_by_key(|state| std::cmp::Reverse(state.0));
        states.truncate(MAX_RESULTS);
        // Every match this attempt produced was found by the same exploration,
        // which may have read well past the accepted end (a longer alternative
        // that failed, a repetition that stopped at a non-matching byte). The
        // furthest position is the honest answer for all of them: the caller's
        // relex window must cover everything the matcher looked at.
        let examined = matcher.furthest;
        let matches = states
            .into_iter()
            .map(|(end, captures)| PatternMatch {
                end,
                captures,
                examined: examined.max(end),
            })
            .collect();
        (matches, examined)
    }
}

/// Compares one exact literal at `start` and reports how far the comparison
/// read.
///
/// A mismatch is examined through the byte that disagreed, because replacing
/// that byte could make the literal match. Running out of text stops at the end
/// of the text: an insertion there is what the caller's end-of-text rule is for.
fn literal_search(literal: &str, text: &str, start: usize) -> (Vec<PatternMatch>, usize) {
    let mut offset = 0usize;
    let mut expected = literal.chars();
    let mut actual = text[start..].chars();
    loop {
        match (expected.next(), actual.next()) {
            (Some(left), Some(right)) if left == right => offset += right.len_utf8(),
            (Some(_), Some(right)) => return (Vec::new(), start + offset + right.len_utf8()),
            (Some(_), None) => return (Vec::new(), start + offset),
            (None, _) => {
                let end = start + offset;
                let matched = PatternMatch {
                    end,
                    captures: Vec::new(),
                    examined: end,
                };
                return (vec![matched], end);
            }
        }
    }
}

/// One successful match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternMatch {
    /// End byte offset.
    pub end: usize,
    /// Capture ranges; `None` for a group that did not participate.
    pub captures: Vec<Option<(usize, usize)>>,
    /// Furthest byte the matcher examined while producing the match.
    ///
    /// Never less than [`end`](Self::end). It can be larger: recognizing the
    /// token may have required reading text past the accepted match (the
    /// matcher tries longer alternatives first, and a guard can reject a
    /// longer end). A relex window must cover it or a token whose match
    /// depended on later text would be reused when that text changes.
    pub examined: usize,
}

/// Pattern parse error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternError {
    /// What went wrong.
    pub message: String,
    /// Byte offset in the pattern.
    pub offset: usize,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at pattern offset {}", self.message, self.offset)
    }
}

impl std::error::Error for PatternError {}

#[derive(Clone, Debug)]
enum Expr {
    Empty,
    Literal(char),
    Any,
    Class(CharClass),
    Concat(Vec<Expr>),
    Alt(Vec<Expr>),
    Repeat {
        expr: Box<Expr>,
        min: u32,
        max: Option<u32>,
    },
    Capture {
        index: usize,
        expr: Box<Expr>,
    },
}

#[derive(Clone, Debug)]
struct Nfa {
    states: Vec<NfaState>,
    start: usize,
}

#[derive(Clone, Debug)]
enum NfaState {
    Split(Vec<usize>),
    Char(CharLabel, usize),
    Accept,
}

#[derive(Clone, Debug)]
enum CharLabel {
    Literal(char),
    Any,
    Class(CharClass),
}

impl Nfa {
    fn build(expr: &Expr) -> Self {
        let mut builder = NfaBuilder { states: Vec::new() };
        let accept = builder.push(NfaState::Accept);
        let start = builder.compile(expr, accept);
        Self {
            states: builder.states,
            start,
        }
    }

    fn closure(&self, seeds: Vec<usize>) -> Vec<usize> {
        let mut visited = vec![false; self.states.len()];
        let mut stack = seeds;
        let mut out = Vec::new();
        while let Some(state) = stack.pop() {
            if visited[state] {
                continue;
            }
            visited[state] = true;
            match &self.states[state] {
                NfaState::Split(targets) => stack.extend(targets.iter().copied()),
                NfaState::Char(_, _) | NfaState::Accept => out.push(state),
            }
        }
        out
    }
}

impl CharLabel {
    fn matches(&self, character: char) -> bool {
        match self {
            Self::Literal(expected) => *expected == character,
            Self::Any => true,
            Self::Class(class) => class.matches(character),
        }
    }
}

#[derive(Debug)]
struct Dfa {
    nfa: Arc<Nfa>,
    states: Vec<Vec<usize>>,
    transitions: Vec<std::collections::HashMap<char, Option<usize>>>,
}

impl Dfa {
    fn new(nfa: Arc<Nfa>) -> Self {
        let start = nfa.closure(vec![nfa.start]);
        Self {
            nfa,
            states: vec![start],
            transitions: vec![std::collections::HashMap::new()],
        }
    }

    fn search(&mut self, text: &str, start: usize) -> (Vec<PatternMatch>, usize) {
        let mut state = 0usize;
        let mut ends = Vec::new();
        // The scan reads one byte past the accepted end before it can know the
        // match stopped there, so the exam is the end of the byte that rejected
        // the transition (or the end of the text).
        let mut examined = start;
        if self.accepting(state) {
            ends.push(start);
        }
        for (offset, character) in text[start..].char_indices() {
            let end = start + offset + character.len_utf8();
            examined = examined.max(end);
            let Some(next) = self.transition(state, character) else {
                break;
            };
            state = next;
            if self.accepting(state) {
                ends.push(end);
            }
        }
        ends.sort_unstable_by(|a, b| b.cmp(a));
        ends.dedup();
        ends.truncate(MAX_RESULTS);
        let matches = ends
            .into_iter()
            .map(|end| PatternMatch {
                end,
                captures: Vec::new(),
                examined: examined.max(end),
            })
            .collect();
        (matches, examined)
    }

    fn accepting(&self, state: usize) -> bool {
        self.states[state]
            .iter()
            .any(|state| matches!(self.nfa.states[*state], NfaState::Accept))
    }

    fn transition(&mut self, state: usize, character: char) -> Option<usize> {
        if let Some(cached) = self.transitions[state].get(&character) {
            return *cached;
        }
        let mut seeds = Vec::new();
        for node in &self.states[state] {
            if let NfaState::Char(label, target) = &self.nfa.states[*node] {
                if label.matches(character) {
                    seeds.push(*target);
                }
            }
        }
        let next_set = self.nfa.closure(seeds);
        let next = if next_set.is_empty() {
            None
        } else if let Some(existing) = self.states.iter().position(|set| *set == next_set) {
            Some(existing)
        } else {
            let id = self.states.len();
            self.states.push(next_set);
            self.transitions.push(std::collections::HashMap::new());
            Some(id)
        };
        self.transitions[state].insert(character, next);
        next
    }
}

struct NfaBuilder {
    states: Vec<NfaState>,
}

impl NfaBuilder {
    fn push(&mut self, state: NfaState) -> usize {
        let id = self.states.len();
        self.states.push(state);
        id
    }

    fn compile(&mut self, expr: &Expr, next: usize) -> usize {
        match expr {
            Expr::Empty => next,
            Expr::Literal(character) => {
                self.push(NfaState::Char(CharLabel::Literal(*character), next))
            }
            Expr::Any => self.push(NfaState::Char(CharLabel::Any, next)),
            Expr::Class(class) => self.push(NfaState::Char(CharLabel::Class(class.clone()), next)),
            Expr::Concat(items) => {
                let mut next = next;
                for item in items.iter().rev() {
                    next = self.compile(item, next);
                }
                next
            }
            Expr::Alt(items) => {
                let starts = items.iter().map(|item| self.compile(item, next)).collect();
                self.push(NfaState::Split(starts))
            }
            Expr::Repeat { expr, min, max } => {
                if let Some(max) = max {
                    let max = (*max).max(*min);
                    let mut next = next;
                    for _ in *min..max {
                        let body = self.compile(expr, next);
                        next = self.push(NfaState::Split(vec![body, next]));
                    }
                    for _ in 0..*min {
                        next = self.compile(expr, next);
                    }
                    next
                } else {
                    let star = self.push(NfaState::Split(Vec::new()));
                    let body = self.compile(expr, star);
                    self.states[star] = NfaState::Split(vec![body, next]);
                    let mut result = star;
                    for _ in 0..*min {
                        result = self.compile(expr, result);
                    }
                    result
                }
            }
            Expr::Capture { expr, .. } => self.compile(expr, next),
        }
    }
}

fn first_ascii(expr: &Expr) -> (bool, Option<u128>) {
    match expr {
        Expr::Empty => (true, Some(0)),
        Expr::Literal(character) => {
            if character.is_ascii() {
                (false, Some(1u128 << (*character as u32)))
            } else {
                (false, None)
            }
        }
        Expr::Any => (false, Some(u128::MAX)),
        Expr::Class(class) => {
            let mut mask = 0u128;
            for item in &class.items {
                match item {
                    ClassItem::Range(start, end) if start.is_ascii() && end.is_ascii() => {
                        for byte in (*start as u32)..=(*end as u32).min(127) {
                            mask |= 1u128 << byte;
                        }
                    }
                    ClassItem::Kind(kind, negated) => {
                        // These predicates must answer exactly what `CharClass::matches` answers
                        // for the same character: this mask is a *filter*, so a byte it omits is a
                        // rule the lexer never tries. `is_ascii_whitespace` and `is_whitespace`
                        // disagree on `0x0B` (vertical tab), which Unicode counts as whitespace, so
                        // a space class filtered the tab out and the matcher never saw it.
                        for byte in 0u8..=127 {
                            let character = byte as char;
                            let positive = match kind {
                                ClassKind::Digit => character.is_numeric(),
                                ClassKind::Word => character.is_alphanumeric() || byte == b'_',
                                ClassKind::Space => character.is_whitespace(),
                            };
                            if positive != *negated {
                                mask |= 1u128 << byte;
                            }
                        }
                    }
                    _ => return (false, None),
                }
            }
            if class.negated {
                mask = !mask;
            }
            (false, Some(mask))
        }
        Expr::Concat(items) => {
            let mut mask = 0u128;
            for item in items {
                let (nullable, item_mask) = first_ascii(item);
                match item_mask {
                    Some(item_mask) => mask |= item_mask,
                    None => return (false, None),
                }
                if !nullable {
                    return (false, Some(mask));
                }
            }
            (true, Some(mask))
        }
        Expr::Alt(items) => {
            let mut nullable = false;
            let mut mask = 0u128;
            for item in items {
                let (item_nullable, item_mask) = first_ascii(item);
                nullable |= item_nullable;
                match item_mask {
                    Some(item_mask) => mask |= item_mask,
                    None => return (nullable, None),
                }
            }
            (nullable, Some(mask))
        }
        Expr::Repeat { expr, min, .. } => {
            let (expr_nullable, mask) = first_ascii(expr);
            (*min == 0 || expr_nullable, mask)
        }
        Expr::Capture { expr, .. } => first_ascii(expr),
    }
}

#[derive(Clone, Debug)]
struct CharClass {
    negated: bool,
    items: Vec<ClassItem>,
}

#[derive(Clone, Debug)]
enum ClassItem {
    Range(char, char),
    Kind(ClassKind, bool),
}

#[derive(Clone, Copy, Debug)]
enum ClassKind {
    Digit,
    Word,
    Space,
}

impl CharClass {
    fn matches(&self, c: char) -> bool {
        let hit = self.items.iter().any(|item| match item {
            ClassItem::Range(start, end) => *start <= c && c <= *end,
            ClassItem::Kind(kind, negated) => {
                let positive = match kind {
                    ClassKind::Digit => c.is_numeric(),
                    ClassKind::Word => c.is_alphanumeric() || c == '_',
                    ClassKind::Space => c.is_whitespace(),
                };
                positive != *negated
            }
        });
        hit != self.negated
    }
}

/// One partial match: where it ended, and what each capture group holds.
type State = (usize, Vec<Option<(usize, usize)>>);

fn dedupe(states: &mut Vec<State>) {
    states.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    states.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    if states.len() > MAX_RESULTS {
        states.truncate(MAX_RESULTS);
    }
}

struct Matcher<'a> {
    text: &'a str,
    steps: usize,
    /// Furthest byte any branch of this attempt has looked at.
    furthest: usize,
}

impl Matcher<'_> {
    /// Reads one byte position, recording how far the attempt has looked.
    ///
    /// A failed probe counts: the matcher had to read the position to learn
    /// that nothing was there, and text inserted at that position can change
    /// what this match accepts.
    fn char_at(&mut self, pos: usize) -> Option<(char, usize)> {
        match char_at(self.text, pos) {
            Some((c, next)) => {
                self.furthest = self.furthest.max(next);
                Some((c, next))
            }
            None => {
                self.furthest = self.furthest.max(pos.min(self.text.len()));
                None
            }
        }
    }

    fn eval(&mut self, expr: &Expr, pos: usize, caps: &[Option<(usize, usize)>]) -> Vec<State> {
        if self.steps == 0 {
            return Vec::new();
        }
        self.steps -= 1;
        match expr {
            Expr::Empty => vec![(pos, caps.to_vec())],
            Expr::Literal(expected) => match self.char_at(pos) {
                Some((c, next)) if c == *expected => vec![(next, caps.to_vec())],
                _ => Vec::new(),
            },
            Expr::Any => match self.char_at(pos) {
                Some((c, next)) if c != '\n' => vec![(next, caps.to_vec())],
                _ => Vec::new(),
            },
            Expr::Class(class) => match self.char_at(pos) {
                Some((c, next)) if class.matches(c) => vec![(next, caps.to_vec())],
                _ => Vec::new(),
            },
            Expr::Concat(parts) => {
                let mut states: Vec<State> = vec![(pos, caps.to_vec())];
                for part in parts {
                    let mut next = Vec::new();
                    for (at, caps) in states {
                        next.extend(self.eval(part, at, &caps));
                        if next.len() >= MAX_RESULTS {
                            break;
                        }
                    }
                    dedupe(&mut next);
                    states = next;
                    if states.is_empty() || self.steps == 0 {
                        break;
                    }
                }
                states
            }
            Expr::Alt(parts) => {
                let mut states = Vec::new();
                for part in parts {
                    states.extend(self.eval(part, pos, caps));
                    if states.len() >= MAX_RESULTS {
                        break;
                    }
                }
                dedupe(&mut states);
                states
            }
            Expr::Repeat { expr, min, max } => {
                let mut out = Vec::new();
                self.repeat(expr, pos, caps, 0, *min, *max, &mut out);
                dedupe(&mut out);
                out
            }
            Expr::Capture { index, expr } => {
                let mut out = self.eval(expr, pos, caps);
                for (end, captures) in &mut out {
                    if *index < captures.len() {
                        captures[*index] = Some((pos, *end));
                    }
                }
                out
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &mut self,
        expr: &Expr,
        pos: usize,
        caps: &[Option<(usize, usize)>],
        count: u32,
        min: u32,
        max: Option<u32>,
        out: &mut Vec<State>,
    ) {
        if self.steps == 0 || out.len() >= MAX_RESULTS {
            return;
        }
        if count >= min {
            out.push((pos, caps.to_vec()));
        }
        if max.is_some_and(|max| count >= max) {
            return;
        }
        let next_states = self.eval(expr, pos, caps);
        for (next, next_caps) in next_states {
            if next == pos {
                // A nullable repeated expression cannot make progress; stop
                // this branch instead of looping forever.
                continue;
            }
            self.repeat(expr, next, &next_caps, count + 1, min, max, out);
            if out.len() >= MAX_RESULTS || self.steps == 0 {
                return;
            }
        }
    }
}

fn char_at(text: &str, pos: usize) -> Option<(char, usize)> {
    let tail = text.get(pos..)?;
    let c = tail.chars().next()?;
    Some((c, pos + c.len_utf8()))
}

struct Parser<'a> {
    source: &'a str,
    chars: Vec<(usize, char)>,
    index: usize,
    capture_names: Vec<String>,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            chars: source.char_indices().collect(),
            index: 0,
            capture_names: Vec::new(),
        }
    }

    fn parse(mut self) -> Result<Pattern, PatternError> {
        let expr = self.parse_alt()?;
        if self.index != self.chars.len() {
            return Err(self.error("unexpected trailing pattern characters"));
        }
        let nfa = self
            .capture_names
            .is_empty()
            .then(|| Arc::new(Nfa::build(&expr)));
        let dfa = nfa
            .as_ref()
            .map(|nfa| Arc::new(Mutex::new(Dfa::new(Arc::clone(nfa)))));
        Ok(Pattern {
            expr,
            capture_names: self.capture_names,
            dfa,
        })
    }

    fn parse_alt(&mut self) -> Result<Expr, PatternError> {
        let mut parts = vec![self.parse_seq()?];
        while self.eat('|') {
            parts.push(self.parse_seq()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().expect("one part")
        } else {
            Expr::Alt(parts)
        })
    }

    fn parse_seq(&mut self) -> Result<Expr, PatternError> {
        let mut parts = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            parts.push(self.parse_repeat()?);
        }
        Ok(match parts.len() {
            0 => Expr::Empty,
            1 => parts.pop().expect("one part"),
            _ => Expr::Concat(parts),
        })
    }

    fn parse_repeat(&mut self) -> Result<Expr, PatternError> {
        let mut expr = self.parse_atom()?;
        loop {
            match self.peek() {
                Some('*') => {
                    self.bump();
                    expr = Expr::Repeat {
                        expr: Box::new(expr),
                        min: 0,
                        max: None,
                    };
                }
                Some('+') => {
                    self.bump();
                    expr = Expr::Repeat {
                        expr: Box::new(expr),
                        min: 1,
                        max: None,
                    };
                }
                Some('?') => {
                    self.bump();
                    expr = Expr::Repeat {
                        expr: Box::new(expr),
                        min: 0,
                        max: Some(1),
                    };
                }
                Some('{') => {
                    self.bump();
                    let min = self.parse_number()?;
                    let max = if self.eat(',') {
                        if self.peek() == Some('}') {
                            None
                        } else {
                            Some(self.parse_number()?)
                        }
                    } else {
                        Some(min)
                    };
                    if !self.eat('}') {
                        return Err(self.error("expected `}` in repetition"));
                    }
                    expr = Expr::Repeat {
                        expr: Box::new(expr),
                        min,
                        max,
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_number(&mut self) -> Result<u32, PatternError> {
        let mut digits = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                digits.push(c);
                self.bump();
            } else {
                break;
            }
        }
        digits
            .parse()
            .map_err(|_| self.error("expected a repetition count"))
    }

    fn parse_atom(&mut self) -> Result<Expr, PatternError> {
        let Some(c) = self.peek() else {
            return Ok(Expr::Empty);
        };
        match c {
            '(' => {
                self.bump();
                if self.eat('?') {
                    if self.eat(':') {
                        let inner = self.parse_alt()?;
                        if !self.eat(')') {
                            return Err(self.error("expected `)`"));
                        }
                        return Ok(inner);
                    }
                    if self.eat('P') {
                        if !self.eat('<') {
                            return Err(self.error("expected `(?P<name>`"));
                        }
                        let name = self.parse_ident()?;
                        if !self.eat('>') {
                            return Err(self.error("expected `>` after capture name"));
                        }
                        if self.capture_names.iter().any(|item| item == &name) {
                            return Err(self.error("duplicate capture name"));
                        }
                        let index = self.capture_names.len();
                        self.capture_names.push(name);
                        let inner = self.parse_alt()?;
                        if !self.eat(')') {
                            return Err(self.error("expected `)`"));
                        }
                        return Ok(Expr::Capture {
                            index,
                            expr: Box::new(inner),
                        });
                    }
                    return Err(self.error("unsupported `(?` group"));
                }
                let inner = self.parse_alt()?;
                if !self.eat(')') {
                    return Err(self.error("expected `)`"));
                }
                Ok(inner)
            }
            '[' => self.parse_class(),
            '.' => {
                self.bump();
                Ok(Expr::Any)
            }
            '\\' => {
                self.bump();
                self.parse_escape()
            }
            '^' | '$' => Err(self.error("anchors are implicit and `^`/`$` are not supported")),
            '*' | '+' | '?' => Err(self.error("quantifier without an atom")),
            ')' | '|' => Err(self.error("unexpected pattern character")),
            _ => {
                self.bump();
                Ok(Expr::Literal(c))
            }
        }
    }

    fn parse_class(&mut self) -> Result<Expr, PatternError> {
        if !self.eat('[') {
            return Err(self.error("expected `[`"));
        }
        let negated = self.eat('^');
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return Err(self.error("unterminated character class"));
            };
            if c == ']' && !first {
                self.bump();
                break;
            }
            first = false;
            let left = if c == '\\' {
                self.bump();
                match self.peek() {
                    Some('d') => {
                        self.bump();
                        items.push(ClassItem::Kind(ClassKind::Digit, false));
                        continue;
                    }
                    Some('D') => {
                        self.bump();
                        items.push(ClassItem::Kind(ClassKind::Digit, true));
                        continue;
                    }
                    Some('w') => {
                        self.bump();
                        items.push(ClassItem::Kind(ClassKind::Word, false));
                        continue;
                    }
                    Some('W') => {
                        self.bump();
                        items.push(ClassItem::Kind(ClassKind::Word, true));
                        continue;
                    }
                    Some('s') => {
                        self.bump();
                        items.push(ClassItem::Kind(ClassKind::Space, false));
                        continue;
                    }
                    Some('S') => {
                        self.bump();
                        items.push(ClassItem::Kind(ClassKind::Space, true));
                        continue;
                    }
                    Some(_) => self.parse_escaped_char()?,
                    None => return Err(self.error("unterminated escape in class")),
                }
            } else {
                self.bump();
                c
            };

            if self.peek() == Some('-') && self.peek_n(1) != Some(']') {
                self.bump();
                let right = if self.peek() == Some('\\') {
                    self.bump();
                    self.parse_escaped_char()?
                } else {
                    let Some(right) = self.peek() else {
                        return Err(self.error("unterminated range in character class"));
                    };
                    self.bump();
                    right
                };
                if right < left {
                    return Err(self.error("character class range is reversed"));
                }
                items.push(ClassItem::Range(left, right));
            } else {
                items.push(ClassItem::Range(left, left));
            }
        }
        Ok(Expr::Class(CharClass { negated, items }))
    }

    fn parse_escape(&mut self) -> Result<Expr, PatternError> {
        let Some(c) = self.peek() else {
            return Err(self.error("trailing escape"));
        };
        match c {
            'd' => {
                self.bump();
                Ok(Expr::Class(CharClass {
                    negated: false,
                    items: vec![ClassItem::Kind(ClassKind::Digit, false)],
                }))
            }
            'D' => {
                self.bump();
                Ok(Expr::Class(CharClass {
                    negated: false,
                    items: vec![ClassItem::Kind(ClassKind::Digit, true)],
                }))
            }
            'w' => {
                self.bump();
                Ok(Expr::Class(CharClass {
                    negated: false,
                    items: vec![ClassItem::Kind(ClassKind::Word, false)],
                }))
            }
            'W' => {
                self.bump();
                Ok(Expr::Class(CharClass {
                    negated: false,
                    items: vec![ClassItem::Kind(ClassKind::Word, true)],
                }))
            }
            's' => {
                self.bump();
                Ok(Expr::Class(CharClass {
                    negated: false,
                    items: vec![ClassItem::Kind(ClassKind::Space, false)],
                }))
            }
            'S' => {
                self.bump();
                Ok(Expr::Class(CharClass {
                    negated: false,
                    items: vec![ClassItem::Kind(ClassKind::Space, true)],
                }))
            }
            'b' | 'B' | 'A' | 'z' => Err(self.error("zero-width assertions are not supported")),
            _ => Ok(Expr::Literal(self.parse_escaped_char()?)),
        }
    }

    fn parse_escaped_char(&mut self) -> Result<char, PatternError> {
        let Some(c) = self.peek() else {
            return Err(self.error("trailing escape"));
        };
        self.bump();
        match c {
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            't' => Ok('\t'),
            '0' => Ok('\0'),
            '\\' => Ok('\\'),
            '/' => Ok('/'),
            '.' => Ok('.'),
            '+' => Ok('+'),
            '*' => Ok('*'),
            '?' => Ok('?'),
            '(' => Ok('('),
            ')' => Ok(')'),
            '[' => Ok('['),
            ']' => Ok(']'),
            '{' => Ok('{'),
            '}' => Ok('}'),
            '|' => Ok('|'),
            '^' => Ok('^'),
            '$' => Ok('$'),
            '-' => Ok('-'),
            'x' => {
                let a = self.next_hex()?;
                let b = self.next_hex()?;
                let value = (a << 4) | b;
                char::from_u32(value).ok_or_else(|| self.error("invalid hex escape"))
            }
            'u' => {
                if !self.eat('{') {
                    return Err(self.error("expected `{` after `\\u`"));
                }
                let mut value = 0u32;
                let mut digits = 0;
                while let Some(c) = self.peek() {
                    if c == '}' {
                        break;
                    }
                    let digit = c
                        .to_digit(16)
                        .ok_or_else(|| self.error("invalid unicode escape"))?;
                    value = value
                        .checked_mul(16)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or_else(|| self.error("unicode escape overflow"))?;
                    digits += 1;
                    self.bump();
                }
                if digits == 0 || !self.eat('}') {
                    return Err(self.error("invalid unicode escape"));
                }
                char::from_u32(value).ok_or_else(|| self.error("invalid unicode scalar"))
            }
            other => Ok(other),
        }
    }

    fn next_hex(&mut self) -> Result<u32, PatternError> {
        let Some(c) = self.peek() else {
            return Err(self.error("truncated hex escape"));
        };
        let digit = c
            .to_digit(16)
            .ok_or_else(|| self.error("invalid hex escape"))?;
        self.bump();
        Ok(digit)
    }

    fn parse_ident(&mut self) -> Result<String, PatternError> {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c == '_' || c.is_alphanumeric() {
                name.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if name.is_empty() {
            return Err(self.error("expected a capture name"));
        }
        Ok(name)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).map(|(_, c)| *c)
    }

    fn peek_n(&self, n: usize) -> Option<char> {
        self.chars.get(self.index + n).map(|(_, c)| *c)
    }

    fn bump(&mut self) {
        self.index += 1;
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn error(&self, message: impl Into<String>) -> PatternError {
        PatternError {
            message: message.into(),
            offset: self
                .chars
                .get(self.index)
                .map_or(self.source.len(), |(offset, _)| *offset),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/lexer/pattern/tests.rs"]
mod tests;
