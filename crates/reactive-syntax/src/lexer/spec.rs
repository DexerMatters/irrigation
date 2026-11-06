//! Lexer specification and the `.grammar` lexer-block parser.

use std::fmt;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use super::guard::{Action, GuardExpr};
use super::pattern::Pattern;
use super::{LexError, ModeId, TokenKind, TokenKindId};

/// What a rule produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleKind {
    /// A normal token.
    Token(TokenKindId),
    /// A trivia token (`skip` rule).
    Skip(TokenKindId),
}

/// Compiled action for one rule.
#[derive(Clone, Debug)]
pub(crate) enum CompiledAction {
    Stay,
    Switch(ModeId),
    Push(ModeId),
    Pop,
    PushValue(GuardExpr),
    SetValue(GuardExpr),
    PopValue,
    Error(Arc<str>),
}

/// One compiled lexer rule.
#[derive(Clone, Debug)]
pub struct Rule {
    /// Output kind.
    pub kind: RuleKind,
    /// Compiled pattern.
    pub pattern: Pattern,
    /// Optional guard.
    pub guard: Option<GuardExpr>,
    /// Lexical priority, higher wins on equal match length.
    pub priority: i32,
    /// Whether the token is word-like.
    pub word: bool,
    /// Whether the token is trivia by attribute.
    pub extra: bool,
    /// Whether this rule declares a lexical error kind.
    pub error: bool,
    /// Default error message for this rule.
    pub error_message: Option<std::sync::Arc<str>>,
    /// Source offset of the rule in the grammar.
    pub offset: usize,
    pub(crate) actions: Vec<CompiledAction>,
}

/// One lexer mode.
#[derive(Clone, Debug)]
pub struct Mode {
    /// Source name.
    pub name: String,
    pub(crate) rules: Vec<Rule>,
}

impl Mode {
    /// Rules in declaration order.
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }
}

/// Compiled lexer specification.
#[derive(Clone, Debug)]
pub struct LexerSpec {
    pub(crate) initial: ModeId,
    pub(crate) modes: Vec<Mode>,
    pub(crate) kinds: Vec<TokenKind>,
    pub(crate) kind_by_name: FxHashMap<String, TokenKindId>,
    pub(crate) error_kind: TokenKindId,
    pub(crate) uses_column: bool,
}

impl LexerSpec {
    /// Initial mode.
    #[must_use]
    pub fn initial(&self) -> ModeId {
        self.initial
    }

    /// All modes.
    #[must_use]
    pub fn modes(&self) -> &[Mode] {
        &self.modes
    }

    /// All token kinds.
    #[must_use]
    pub fn kinds(&self) -> &[TokenKind] {
        &self.kinds
    }

    /// The reserved error token kind.
    #[must_use]
    pub fn error_kind(&self) -> TokenKindId {
        self.error_kind
    }

    /// Whether any guard or action observes the current column.
    #[must_use]
    pub fn uses_column(&self) -> bool {
        self.uses_column
    }

    /// Looks up a token kind by source name.
    #[must_use]
    pub fn kind_by_name(&self, name: &str) -> Option<TokenKindId> {
        self.kind_by_name.get(name).copied()
    }

    /// Rules of one mode.
    #[must_use]
    pub fn rules(&self, mode: ModeId) -> &[Rule] {
        &self.modes[mode.0 as usize].rules
    }

    /// Parses a grammar source containing a `lexer` block.
    ///
    /// The whole `grammar { ... }` wrapper is accepted; non-lexer blocks are
    /// skipped by balanced-brace scanning so this parser can be used while the
    /// parser layer is still being built.
    ///
    /// # Errors
    ///
    /// Returns a structured specification error.
    pub fn parse(source: &str) -> Result<Self, LexError> {
        let tokens = tokenize(source)?;
        SpecParser {
            source,
            tokens,
            index: 0,
            states: Vec::new(),
            initial: None,
        }
        .parse()
    }
}

struct RuleDraft {
    emit: bool,
    kind_name: String,
    pattern: PatternSource,
    priority: i32,
    word: bool,
    extra: bool,
    error: bool,
    error_message: Option<String>,
    guard: Option<GuardExpr>,
    actions: Vec<Action>,
    offset: usize,
}

enum PatternSource {
    Literal(String),
    Regex(String),
}

struct StateDraft {
    name: String,
    rules: Vec<RuleDraft>,
}

struct SpecParser<'a> {
    source: &'a str,
    tokens: Vec<DslToken>,
    index: usize,
    states: Vec<StateDraft>,
    initial: Option<String>,
}

impl SpecParser<'_> {
    fn parse(mut self) -> Result<LexerSpec, LexError> {
        let mut saw_lexer = false;
        if self.eat_ident("grammar") {
            if matches!(
                self.peek(),
                Some(DslToken {
                    kind: DslKind::Ident(_),
                    ..
                })
            ) {
                self.bump();
            }
            if !self.eat_symbol('{') {
                return Err(self.error("expected `{` after `grammar`"));
            }
            while !self.at_end() {
                if self.eat_symbol('}') {
                    break;
                }
                if self.eat_ident("lexer") {
                    saw_lexer = true;
                    self.parse_lexer_body()?;
                } else if self.eat_ident("initial") {
                    self.parse_initial()?;
                } else if self.eat_ident("state") {
                    self.parse_state()?;
                } else {
                    self.skip_item()?;
                }
            }
        } else if self.eat_ident("lexer") {
            saw_lexer = true;
            self.parse_lexer_body()?;
        } else {
            return Err(self.error("expected `grammar` or `lexer`"));
        }
        if !saw_lexer && self.states.is_empty() {
            return Err(self.error("grammar contains no lexer block"));
        }
        if !self.at_end() {
            return Err(self.error("unexpected tokens after grammar"));
        }
        self.compile()
    }

    fn parse_lexer_body(&mut self) -> Result<(), LexError> {
        if !self.eat_symbol('{') {
            return Err(self.error("expected `{` after `lexer`"));
        }
        while !self.at_end() {
            if self.eat_symbol('}') {
                return Ok(());
            }
            if self.eat_ident("initial") {
                self.parse_initial()?;
            } else if self.eat_ident("state") {
                self.parse_state()?;
            } else {
                return Err(self.error("expected `initial` or `state` in lexer block"));
            }
        }
        Err(self.error("unterminated lexer block"))
    }

    fn parse_initial(&mut self) -> Result<(), LexError> {
        let name = self.expect_ident("mode name")?;
        if !self.eat_symbol(';') {
            return Err(self.error("expected `;` after `initial`"));
        }
        if let Some(existing) = &self.initial {
            if existing != &name {
                return Err(self.error("duplicate `initial` declaration"));
            }
        }
        self.initial = Some(name);
        Ok(())
    }

    fn parse_state(&mut self) -> Result<(), LexError> {
        let name = self.expect_ident("state name")?;
        if self.states.iter().any(|state| state.name == name) {
            return Err(self.error(format!("duplicate state `{name}`")));
        }
        if !self.eat_symbol('{') {
            return Err(self.error("expected `{` after state name"));
        }
        let mut rules = Vec::new();
        while !self.at_end() {
            if self.eat_symbol('}') {
                self.states.push(StateDraft { name, rules });
                return Ok(());
            }
            rules.push(self.parse_rule(&name)?);
        }
        Err(self.error("unterminated state block"))
    }

    fn parse_rule(&mut self, _state: &str) -> Result<RuleDraft, LexError> {
        let offset = self.peek().map_or(self.source.len(), |token| token.start);
        let (emit, error_rule) = if self.eat_ident("token") {
            (true, false)
        } else if self.eat_ident("skip") {
            (false, false)
        } else if self.eat_ident("error") {
            (true, true)
        } else {
            return Err(self.error("expected `token`, `skip`, or `error`"));
        };

        let (kind_name, inline_literal) = match self.peek().cloned() {
            Some(DslToken {
                kind: DslKind::Ident(name),
                ..
            }) => {
                self.bump();
                (name, None)
            }
            Some(DslToken {
                kind: DslKind::Str(value),
                ..
            }) => {
                self.bump();
                (value.clone(), Some(value))
            }
            _ => return Err(self.error("expected a token name or string literal")),
        };

        let pattern = if self.eat_symbol('=') {
            match self.peek().cloned() {
                Some(DslToken {
                    kind: DslKind::Regex(value),
                    ..
                }) => {
                    self.bump();
                    PatternSource::Regex(value)
                }
                Some(DslToken {
                    kind: DslKind::Str(value),
                    ..
                }) => {
                    self.bump();
                    PatternSource::Literal(value)
                }
                Some(DslToken {
                    kind: DslKind::Ident(value),
                    ..
                }) => {
                    self.bump();
                    PatternSource::Literal(value)
                }
                _ => return Err(self.error("expected a pattern after `=`")),
            }
        } else if let Some(value) = inline_literal {
            PatternSource::Literal(value)
        } else {
            return Err(self.error("expected `=` and a pattern"));
        };

        let mut priority = 0i32;
        let mut word = false;
        let mut extra = false;
        let mut error_message = None;
        while self.eat_symbol('@') {
            let attribute = self.expect_ident("attribute name")?;
            match attribute.as_str() {
                "word" => word = true,
                "extra" => extra = true,
                "message" => {
                    if !self.eat_symbol('(') {
                        return Err(self.error("expected `(` after `@message`"));
                    }
                    match self.peek().cloned() {
                        Some(DslToken {
                            kind: DslKind::Str(value),
                            ..
                        }) => {
                            self.bump();
                            error_message = Some(value);
                        }
                        _ => return Err(self.error("expected a string after `@message(`")),
                    }
                    if !self.eat_symbol(')') {
                        return Err(self.error("expected `)` after message"));
                    }
                }
                "priority" => {
                    if !self.eat_symbol('(') {
                        return Err(self.error("expected `(` after `@priority`"));
                    }
                    let value = self.expect_number()?;
                    if !self.eat_symbol(')') {
                        return Err(self.error("expected `)` after priority"));
                    }
                    priority = value;
                }
                other => return Err(self.error(format!("unknown lexer attribute `@{other}`"))),
            }
        }

        if error_message.is_some() && !error_rule {
            return Err(self.error("`@message` requires an `error` rule"));
        }

        let guard = if self.eat_ident("when") {
            let start = self.peek().map_or(self.source.len(), |token| token.start);
            while !self.at_end() {
                match self.peek().map(|token| &token.kind) {
                    Some(DslKind::Arrow | DslKind::Symbol(';')) => break,
                    _ => self.bump(),
                }
            }
            let end = self.peek().map_or(self.source.len(), |token| token.start);
            let text = self.source[start..end].trim();
            if text.is_empty() {
                return Err(self.error("empty `when` guard"));
            }
            Some(GuardExpr::parse(text).map_err(|error| LexError::Spec {
                message: error.message,
                offset: start + error.offset,
            })?)
        } else {
            None
        };

        let actions = if self.eat_arrow() {
            let start = self.peek().map_or(self.source.len(), |token| token.start);
            while !self.at_end() {
                match self.peek().map(|token| &token.kind) {
                    Some(DslKind::Symbol(';')) => break,
                    _ => self.bump(),
                }
            }
            let end = self.peek().map_or(self.source.len(), |token| token.start);
            let text = self.source[start..end].trim();
            if text.is_empty() {
                return Err(self.error("empty action list"));
            }
            Action::parse_list(text).map_err(|error| LexError::Spec {
                message: error.message,
                offset: start + error.offset,
            })?
        } else {
            Vec::new()
        };

        if !self.eat_symbol(';') {
            return Err(self.error("expected `;` after lexer rule"));
        }

        Ok(RuleDraft {
            emit,
            kind_name,
            pattern,
            priority,
            word,
            extra,
            error: error_rule,
            error_message,
            guard,
            actions,
            offset,
        })
    }

    fn compile(mut self) -> Result<LexerSpec, LexError> {
        if self.states.is_empty() {
            return Err(self.error("no lexer states"));
        }
        let mode_ids: FxHashMap<String, ModeId> = self
            .states
            .iter()
            .enumerate()
            .map(|(index, state)| (state.name.clone(), ModeId(index as u32)))
            .collect();
        let initial_name = self
            .initial
            .clone()
            .or_else(|| self.states.first().map(|state| state.name.clone()))
            .ok_or_else(|| self.error("no initial state"))?;
        let initial = *mode_ids
            .get(&initial_name)
            .ok_or_else(|| self.error(format!("unknown initial state `{initial_name}`")))?;

        let mut modes: Vec<Mode> = self
            .states
            .iter()
            .map(|state| Mode {
                name: state.name.clone(),
                rules: Vec::new(),
            })
            .collect();

        let error_kind = TokenKindId(0);
        let mut kinds = vec![TokenKind {
            id: error_kind,
            name: "$error".to_string(),
            trivia: false,
            word: false,
            error: true,
        }];
        let mut kind_by_name: FxHashMap<String, TokenKindId> =
            FxHashMap::from_iter([("$error".to_string(), error_kind)]);
        let mut uses_column = false;

        let states = std::mem::take(&mut self.states);
        for state in states {
            let mode = mode_ids[&state.name];
            for draft in state.rules {
                if draft.guard.as_ref().is_some_and(GuardExpr::uses_column)
                    || draft.actions.iter().any(action_uses_column)
                {
                    uses_column = true;
                }
                let trivia = !draft.emit || draft.extra;
                let kind = if let Some(existing) = kind_by_name.get(&draft.kind_name).copied() {
                    let kind = kinds[existing.0 as usize].clone();
                    if kind.trivia != trivia || kind.word != draft.word || kind.error != draft.error
                    {
                        return Err(LexError::Spec {
                            message: format!(
                                "token `{}` is declared with conflicting trivia/word flags",
                                draft.kind_name
                            ),
                            offset: draft.offset,
                        });
                    }
                    existing
                } else {
                    let id = TokenKindId(kinds.len() as u32);
                    let kind = TokenKind {
                        id,
                        name: draft.kind_name.clone(),
                        trivia,
                        word: draft.word,
                        error: draft.error,
                    };
                    kinds.push(kind);
                    kind_by_name.insert(draft.kind_name.clone(), id);
                    id
                };
                let pattern = match draft.pattern {
                    PatternSource::Literal(text) => Pattern::literal(&text),
                    PatternSource::Regex(text) => {
                        Pattern::parse(&text).map_err(|error| LexError::Spec {
                            message: error.message,
                            offset: draft.offset + error.offset,
                        })?
                    }
                };
                let actions = draft
                    .actions
                    .into_iter()
                    .map(|action| match action {
                        Action::Stay => Ok(CompiledAction::Stay),
                        Action::Pop => Ok(CompiledAction::Pop),
                        Action::PopValue => Ok(CompiledAction::PopValue),
                        Action::PushValue(expr) => Ok(CompiledAction::PushValue(expr)),
                        Action::SetValue(expr) => Ok(CompiledAction::SetValue(expr)),
                        Action::Error(message) => Ok(CompiledAction::Error(Arc::from(message))),
                        Action::Switch(name) => mode_ids
                            .get(&name)
                            .copied()
                            .map(CompiledAction::Switch)
                            .ok_or_else(|| LexError::Spec {
                                message: format!("unknown mode `{name}` in `switch`"),
                                offset: draft.offset,
                            }),
                        Action::Push(name) => mode_ids
                            .get(&name)
                            .copied()
                            .map(CompiledAction::Push)
                            .ok_or_else(|| LexError::Spec {
                                message: format!("unknown mode `{name}` in `push`"),
                                offset: draft.offset,
                            }),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                modes[mode.0 as usize].rules.push(Rule {
                    kind: if draft.emit {
                        RuleKind::Token(kind)
                    } else {
                        RuleKind::Skip(kind)
                    },
                    pattern,
                    guard: draft.guard,
                    priority: draft.priority,
                    word: draft.word,
                    extra: draft.extra,
                    error: draft.error,
                    error_message: draft.error_message.map(std::sync::Arc::from),
                    offset: draft.offset,
                    actions,
                });
            }
        }

        for mode in &modes {
            let mut seen: FxHashMap<String, (i32, usize)> = FxHashMap::default();
            for rule in &mode.rules {
                if rule.guard.is_some() {
                    continue;
                }
                let Some(literal) = rule.pattern.exact_literal() else {
                    continue;
                };
                if let Some((priority, _)) = seen.get(&literal) {
                    if *priority == rule.priority {
                        return Err(LexError::Spec {
                            message: format!(
                                "conflicting lexer rules for literal `{literal}` in state `{}`",
                                mode.name
                            ),
                            offset: rule.offset,
                        });
                    }
                } else {
                    seen.insert(literal, (rule.priority, rule.offset));
                }
            }
        }

        Ok(LexerSpec {
            initial,
            modes,
            kinds,
            kind_by_name,
            error_kind,
            uses_column,
        })
    }

    fn skip_item(&mut self) -> Result<(), LexError> {
        if self.at_end() {
            return Ok(());
        }
        if self.eat_symbol('{') {
            self.skip_balanced('{', '}')?;
            return Ok(());
        }
        while !self.at_end() {
            match self.peek().map(|token| &token.kind) {
                Some(DslKind::Symbol(';')) => {
                    self.bump();
                    return Ok(());
                }
                Some(DslKind::Symbol('}')) => return Ok(()),
                Some(DslKind::Symbol('{')) => {
                    self.bump();
                    self.skip_balanced('{', '}')?;
                }
                _ => self.bump(),
            }
        }
        Ok(())
    }

    fn skip_balanced(&mut self, open: char, close: char) -> Result<(), LexError> {
        let mut depth = 1usize;
        while !self.at_end() {
            let Some(token) = self.peek().cloned() else {
                break;
            };
            self.bump();
            if token.kind == DslKind::Symbol(open) {
                depth += 1;
            } else if token.kind == DslKind::Symbol(close) {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
        }
        Err(self.error("unterminated block"))
    }

    fn peek(&self) -> Option<&DslToken> {
        self.tokens.get(self.index)
    }

    fn bump(&mut self) {
        self.index += 1;
    }

    fn at_end(&self) -> bool {
        self.index >= self.tokens.len()
    }

    fn eat_ident(&mut self, expected: &str) -> bool {
        if expected.is_empty() {
            return false;
        }
        match self.peek() {
            Some(DslToken {
                kind: DslKind::Ident(name),
                ..
            }) if name == expected => {
                self.bump();
                true
            }
            _ => false,
        }
    }

    fn expect_ident(&mut self, what: &str) -> Result<String, LexError> {
        match self.peek().cloned() {
            Some(DslToken {
                kind: DslKind::Ident(name),
                ..
            }) => {
                self.bump();
                Ok(name)
            }
            _ => Err(self.error(format!("expected {what}"))),
        }
    }

    fn expect_number(&mut self) -> Result<i32, LexError> {
        match self.peek().cloned() {
            Some(DslToken {
                kind: DslKind::Number(value),
                ..
            }) => {
                self.bump();
                i32::try_from(value).map_err(|_| self.error("priority is out of range"))
            }
            _ => Err(self.error("expected a number")),
        }
    }

    fn eat_symbol(&mut self, symbol: char) -> bool {
        match self.peek() {
            Some(DslToken {
                kind: DslKind::Symbol(found),
                ..
            }) if *found == symbol => {
                self.bump();
                true
            }
            _ => false,
        }
    }

    fn eat_arrow(&mut self) -> bool {
        match self.peek() {
            Some(DslToken {
                kind: DslKind::Arrow,
                ..
            }) => {
                self.bump();
                true
            }
            _ => false,
        }
    }

    fn error(&self, message: impl Into<String>) -> LexError {
        LexError::Spec {
            message: message.into(),
            offset: self.peek().map_or(self.source.len(), |token| token.start),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DslKind {
    Ident(String),
    Str(String),
    Regex(String),
    Number(i64),
    Symbol(char),
    Arrow,
}

#[derive(Clone, Debug)]
pub(crate) struct DslToken {
    pub(crate) kind: DslKind,
    pub(crate) start: usize,
}

pub(crate) fn tokenize(source: &str) -> Result<Vec<DslToken>, LexError> {
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < source.len() {
        let tail = &source[index..];
        let Some(c) = tail.chars().next() else {
            break;
        };
        if c.is_whitespace() {
            index += c.len_utf8();
            continue;
        }
        if tail.starts_with("//") {
            index += 2;
            while index < source.len() {
                let c = source[index..].chars().next().expect("char");
                index += c.len_utf8();
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        if tail.starts_with("/*") {
            index += 2;
            let mut depth = 1usize;
            while index < source.len() && depth > 0 {
                if source[index..].starts_with("/*") {
                    depth += 1;
                    index += 2;
                } else if source[index..].starts_with("*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    let c = source[index..].chars().next().expect("char");
                    index += c.len_utf8();
                }
            }
            if depth != 0 {
                return Err(spec_error(source, "unterminated block comment", index));
            }
            continue;
        }
        if tail.starts_with("->") {
            tokens.push(DslToken {
                kind: DslKind::Arrow,
                start: index,
            });
            index += 2;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = index;
            index += c.len_utf8();
            while index < source.len() {
                let c = source[index..].chars().next().expect("char");
                if c.is_alphanumeric() || c == '_' {
                    index += c.len_utf8();
                } else {
                    break;
                }
            }
            tokens.push(DslToken {
                kind: DslKind::Ident(source[start..index].to_string()),
                start,
            });
            continue;
        }
        if c.is_ascii_digit() {
            let start = index;
            index += c.len_utf8();
            while index < source.len() {
                let c = source[index..].chars().next().expect("char");
                if c.is_ascii_digit() || c == '_' {
                    index += c.len_utf8();
                } else {
                    break;
                }
            }
            let value = source[start..index]
                .replace('_', "")
                .parse::<i64>()
                .map_err(|_| spec_error(source, "invalid number", start))?;
            tokens.push(DslToken {
                kind: DslKind::Number(value),
                start,
            });
            continue;
        }
        if c == '"' {
            let start = index;
            let (value, next) = scan_string(source, index)?;
            tokens.push(DslToken {
                kind: DslKind::Str(value),
                start,
            });
            index = next;
            continue;
        }
        if c == '/' {
            let start = index;
            let (value, next) = scan_regex(source, index)?;
            tokens.push(DslToken {
                kind: DslKind::Regex(value),
                start,
            });
            index = next;
            continue;
        }
        if "{}();,=@:".contains(c) {
            tokens.push(DslToken {
                kind: DslKind::Symbol(c),
                start: index,
            });
            index += c.len_utf8();
            continue;
        }
        if c.is_ascii_punctuation() {
            tokens.push(DslToken {
                kind: DslKind::Symbol(c),
                start: index,
            });
            index += c.len_utf8();
            continue;
        }
        return Err(spec_error(
            source,
            format!("unexpected character `{c}` in grammar"),
            index,
        ));
    }
    Ok(tokens)
}

fn scan_string(source: &str, index: usize) -> Result<(String, usize), LexError> {
    let mut out = String::new();
    let mut cursor = index + 1;
    while cursor < source.len() {
        let c = source[cursor..].chars().next().expect("char");
        cursor += c.len_utf8();
        match c {
            '"' => return Ok((out, cursor)),
            '\\' => {
                let Some(escaped) = source[cursor..].chars().next() else {
                    break;
                };
                cursor += escaped.len_utf8();
                out.push(match escaped {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '\\' => '\\',
                    '"' => '"',
                    other => other,
                });
            }
            other => out.push(other),
        }
    }
    Err(spec_error(source, "unterminated string literal", index))
}

fn scan_regex(source: &str, index: usize) -> Result<(String, usize), LexError> {
    let mut out = String::new();
    let mut cursor = index + 1;
    let mut in_class = false;
    let mut escaped = false;
    while cursor < source.len() {
        let c = source[cursor..].chars().next().expect("char");
        cursor += c.len_utf8();
        if escaped {
            out.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' => {
                out.push(c);
                escaped = true;
            }
            '[' => {
                in_class = true;
                out.push(c);
            }
            ']' => {
                in_class = false;
                out.push(c);
            }
            '/' if !in_class => return Ok((out, cursor)),
            other => out.push(other),
        }
    }
    Err(spec_error(source, "unterminated regex literal", index))
}

fn spec_error(source: &str, message: impl Into<String>, offset: usize) -> LexError {
    let _ = (source, offset);
    LexError::Spec {
        message: message.into(),
        offset,
    }
}

fn action_uses_column(action: &Action) -> bool {
    match action {
        Action::PushValue(expr) | Action::SetValue(expr) => expr.uses_column(),
        _ => false,
    }
}

impl fmt::Display for RuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Token(kind) => write!(f, "token({})", kind.0),
            Self::Skip(kind) => write!(f, "skip({})", kind.0),
        }
    }
}
