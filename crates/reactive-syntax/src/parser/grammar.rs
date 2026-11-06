//! Parser grammar specification, EBNF lowering, and compiled grammar IR.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::lexer::spec::{self, DslKind, DslToken};
use crate::lexer::{LexError, LexerSpec, TokenKindId};

use super::lr::{self, Table, TableMode};

/// Nonterminal identity.
pub type NonterminalId = usize;
/// Terminal identity.
pub type TerminalId = usize;
/// Field identity.
pub type FieldId = usize;
/// Production identity.
pub type ProductionId = usize;

/// Grammar symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Symbol {
    /// A terminal token kind.
    Terminal(TerminalId),
    /// A nonterminal.
    Nonterminal(NonterminalId),
}

/// One terminal (a non-trivia lexer token kind).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terminal {
    /// Terminal id.
    pub id: TerminalId,
    /// Lexer token kind.
    pub token_kind: TokenKindId,
    /// Human-readable name.
    pub name: String,
    /// Precedence level inferred from annotated productions.
    pub precedence: Option<u32>,
}

/// One nonterminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nonterminal {
    /// Nonterminal id.
    pub id: NonterminalId,
    /// Source/internal name.
    pub name: String,
}

/// One named field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// Field id.
    pub id: FieldId,
    /// Field name.
    pub name: String,
}

/// Associativity of a precedence level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Associativity {
    /// Left associative.
    Left,
    /// Right associative.
    Right,
    /// Non-associative.
    NonAssoc,
}

/// One precedence level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrecedenceLevel {
    /// Level number; higher binds tighter.
    pub level: u32,
    /// Associativity.
    pub associativity: Associativity,
    /// Declared name.
    pub name: String,
}

/// One BNF production.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Production {
    /// Production id.
    pub id: ProductionId,
    /// Left-hand side.
    pub lhs: NonterminalId,
    /// Right-hand side.
    pub rhs: Vec<Symbol>,
    /// Fields attached to RHS positions.
    pub fields: Vec<(FieldId, usize)>,
    /// Static precedence, if declared.
    pub precedence: Option<u32>,
    /// Dynamic precedence.
    pub dynamic: i32,
    /// Rule name for diagnostics/conflict declarations.
    pub label: String,
    /// Whether this is the augmented start production.
    pub augmented: bool,
    /// AST constructor attached to this production.
    pub ast_action: Option<AstAction>,
    /// Whether the production is an invisible wrapper whose children belong to
    /// the parent (`$groupN` and `$repeatN`).
    pub transparent: bool,
}

/// AST constructor attached to one production.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AstAction {
    /// AST constructor or variant name.
    pub name: String,
    /// Field name to RHS index mappings.
    pub fields: Vec<(String, usize)>,
}

impl Production {
    /// Right-hand-side length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rhs.len()
    }

    /// Returns `true` for an epsilon production.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rhs.is_empty()
    }
}

/// Policy for a grammar-declared error class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ErrorPolicy {
    /// Publish diagnostics and charge normal recovery cost.
    #[default]
    Report,
    /// Keep the error explicit in the tree/token stream without publishing a
    /// diagnostic or charging recovery cost.
    Ignore,
}

/// A compiled grammar plus its parse table.
#[derive(Clone, Debug)]
pub struct Grammar {
    pub(crate) lexer: LexerSpec,
    pub(crate) terminals: Vec<Terminal>,
    pub(crate) nonterminals: Vec<Nonterminal>,
    pub(crate) productions: Vec<Production>,
    pub(crate) start: NonterminalId,
    pub(crate) fields: Vec<Field>,
    pub(crate) precedence: Vec<PrecedenceLevel>,
    pub(crate) conflicts: Vec<(String, String)>,
    pub(crate) table: Table,
    pub(crate) table_mode: TableMode,
    pub(crate) eof_terminal: TerminalId,
    pub(crate) error_terminal: Option<TerminalId>,
    pub(crate) lex_error_policy: ErrorPolicy,
    pub(crate) node_error_policy: ErrorPolicy,
    pub(crate) ignored_lex_error_kinds: FxHashSet<TokenKindId>,
}

impl Grammar {
    /// Compiles a grammar source containing a lexer block and a parser block.
    ///
    /// # Errors
    ///
    /// Returns a structured grammar error.
    pub fn compile(source: &str) -> Result<Self, GrammarError> {
        let lexer = LexerSpec::parse(source)?;
        let raw = DslGrammar::parse(source)?;
        lower(lexer, raw)
    }

    /// The lexer specification.
    #[must_use]
    pub fn lexer(&self) -> &LexerSpec {
        &self.lexer
    }

    /// The start nonterminal.
    #[must_use]
    pub fn start(&self) -> NonterminalId {
        self.start
    }

    /// The terminal for one lexer token kind.
    #[must_use]
    pub fn terminal_for_kind(&self, kind: TokenKindId) -> Option<TerminalId> {
        self.terminals
            .iter()
            .find(|terminal| terminal.token_kind == kind)
            .map(|terminal| terminal.id)
    }

    /// Terminals.
    #[must_use]
    pub fn terminals(&self) -> &[Terminal] {
        &self.terminals
    }

    /// Nonterminals.
    #[must_use]
    pub fn nonterminals(&self) -> &[Nonterminal] {
        &self.nonterminals
    }

    /// Productions.
    #[must_use]
    pub fn productions(&self) -> &[Production] {
        &self.productions
    }

    /// Fields.
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// Declared precedence levels.
    #[must_use]
    pub fn precedence(&self) -> &[PrecedenceLevel] {
        &self.precedence
    }

    /// Finds a field by source name.
    #[must_use]
    pub fn field_by_name(&self, name: &str) -> Option<FieldId> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.id)
    }

    /// Declared rule-pair conflicts.
    #[must_use]
    pub fn conflicts(&self) -> &[(String, String)] {
        &self.conflicts
    }

    /// Nullable nonterminals as a fixed-point vector.
    #[must_use]
    pub fn nullable_nonterminals(&self) -> Vec<bool> {
        let mut nullable = vec![false; self.nonterminals.len()];
        loop {
            let mut changed = false;
            for production in &self.productions {
                if nullable[production.lhs] {
                    continue;
                }
                if production.rhs.iter().all(|symbol| match symbol {
                    Symbol::Terminal(_) => false,
                    Symbol::Nonterminal(id) => nullable[*id],
                }) {
                    nullable[production.lhs] = true;
                    changed = true;
                }
            }
            if !changed {
                return nullable;
            }
        }
    }

    /// All productions with at least one right-nullable reduction prefix.
    #[must_use]
    pub fn rnglr_reductions(&self) -> Vec<(ProductionId, Vec<usize>)> {
        self.productions
            .iter()
            .filter_map(|production| {
                let prefixes = self.rn_reduction_prefixes(production.id);
                (prefixes.len() > 1).then_some((production.id, prefixes))
            })
            .collect()
    }

    /// Prefix lengths whose remaining suffix is nullable.
    #[must_use]
    pub fn rn_reduction_prefixes(&self, production_id: ProductionId) -> Vec<usize> {
        let Some(production) = self.productions.get(production_id) else {
            return Vec::new();
        };
        (0..=production.rhs.len())
            .filter(|prefix| self.is_right_nullable(production_id, *prefix))
            .collect()
    }

    /// Whether the suffix after `prefix` is nullable for one production.
    #[must_use]
    pub fn is_right_nullable(&self, production_id: ProductionId, prefix: usize) -> bool {
        let Some(production) = self.productions.get(production_id) else {
            return false;
        };
        if prefix > production.rhs.len() {
            return false;
        }
        let nullable = self.nullable_nonterminals();
        production.rhs[prefix..].iter().all(|symbol| match symbol {
            Symbol::Terminal(_) => false,
            Symbol::Nonterminal(id) => nullable[*id],
        })
    }

    /// Parse table.
    #[must_use]
    pub fn table(&self) -> &Table {
        &self.table
    }

    /// Table construction mode used for this grammar.
    #[must_use]
    pub fn table_mode(&self) -> TableMode {
        self.table_mode
    }

    /// EOF terminal id.
    #[must_use]
    pub fn eof_terminal(&self) -> TerminalId {
        self.eof_terminal
    }

    /// Declared parser terminal for lexical error tokens.
    #[must_use]
    pub fn error_terminal(&self) -> Option<TerminalId> {
        self.error_terminal
    }

    /// Diagnostic policy for lexical errors.
    #[must_use]
    pub fn lex_error_policy(&self) -> ErrorPolicy {
        self.lex_error_policy
    }

    /// Whether a lexical error kind has `ignore` policy.
    #[must_use]
    pub fn is_lex_error_ignored(&self, kind: TokenKindId) -> bool {
        self.ignored_lex_error_kinds.contains(&kind)
    }

    /// Diagnostic policy for parser recovery nodes.
    #[must_use]
    pub fn node_error_policy(&self) -> ErrorPolicy {
        self.node_error_policy
    }

    /// Finds a nonterminal by source name.
    #[must_use]
    pub fn nonterminal_by_name(&self, name: &str) -> Option<NonterminalId> {
        self.nonterminals
            .iter()
            .find(|nonterminal| nonterminal.name == name)
            .map(|nonterminal| nonterminal.id)
    }
}

/// Grammar compilation error.
#[derive(Debug, thiserror::Error)]
pub enum GrammarError {
    /// Lexer specification failed.
    #[error(transparent)]
    Lex(#[from] LexError),
    /// Parser specification failed.
    #[error("parser grammar error at byte {offset}: {message}")]
    Spec {
        /// Human-readable message.
        message: String,
        /// Byte offset in the grammar source.
        offset: usize,
    },
    /// The LR table could not be built.
    #[error("parser table error: {0}")]
    Table(#[from] lr::TableError),
}

impl GrammarError {
    fn spec(message: impl Into<String>, offset: usize) -> Self {
        Self::Spec {
            message: message.into(),
            offset,
        }
    }
}

struct DslGrammar {
    rules: Vec<RawRule>,
    start: Option<String>,
    precedence: Vec<PrecedenceLevel>,
    conflicts: Vec<(String, String)>,
    table_mode: TableMode,
    error_token: Option<String>,
    lex_error_policy: ErrorPolicy,
    node_error_policy: ErrorPolicy,
    ignored_lex_errors: FxHashSet<String>,
}

struct RawRule {
    name: String,
    alternatives: Vec<RawAlt>,
}

struct RawAlt {
    elements: Vec<RawElem>,
    precedence: Option<String>,
    dynamic: i32,
    ast_action: Option<AstAction>,
}

#[allow(dead_code)]
enum RawElem {
    Ref(String, usize),
    Literal(String, usize),
    Group(Vec<RawAlt>, usize),
    Repeat(Box<RawElem>, char, usize),
    Field(String, Box<RawElem>),
}

struct DslGrammarParser<'a> {
    source: &'a str,
    tokens: Vec<DslToken>,
    index: usize,
    grammar: DslGrammar,
}

impl DslGrammar {
    fn parse(source: &str) -> Result<Self, GrammarError> {
        let tokens = spec::tokenize(source)?;
        let mut parser = DslGrammarParser {
            source,
            tokens,
            index: 0,
            grammar: DslGrammar {
                rules: Vec::new(),
                start: None,
                precedence: Vec::new(),
                conflicts: Vec::new(),
                table_mode: TableMode::Lalr,
                error_token: None,
                lex_error_policy: ErrorPolicy::Report,
                node_error_policy: ErrorPolicy::Report,
                ignored_lex_errors: FxHashSet::default(),
            },
        };
        parser.parse_top()?;
        Ok(parser.grammar)
    }
}

impl DslGrammarParser<'_> {
    fn parse_top(&mut self) -> Result<(), GrammarError> {
        while !self.at_end() {
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
                self.expect_symbol('{')?;
                self.parse_top_items()?;
                self.expect_symbol('}')?;
            } else if self.eat_ident("lexer") {
                self.skip_braced()?;
            } else if self.eat_ident("parser") {
                self.parse_parser_block()?;
            } else if self.eat_ident("start") {
                let name = self.expect_ident("start rule")?;
                self.expect_semicolon()?;
                self.grammar.start = Some(name);
            } else if self.eat_ident("precedence") {
                self.parse_precedence()?;
            } else if self.eat_ident("conflict") {
                self.parse_conflict()?;
            } else if self.eat_ident("table") {
                self.parse_table()?;
            } else if self.eat_ident("errors") {
                self.parse_errors_block()?;
            } else if self.eat_ident("recovery") {
                self.parse_recovery_block()?;
            } else if self.eat_ident("extras") {
                self.skip_item()?;
            } else if self
                .peek()
                .is_some_and(|token| matches!(token.kind, DslKind::Symbol('}')))
            {
                break;
            } else {
                self.skip_item()?;
            }
        }
        Ok(())
    }

    fn parse_top_items(&mut self) -> Result<(), GrammarError> {
        while !self.at_end() {
            if self
                .peek()
                .is_some_and(|token| matches!(token.kind, DslKind::Symbol('}')))
            {
                return Ok(());
            }
            self.parse_top()?;
        }
        Ok(())
    }

    fn parse_parser_block(&mut self) -> Result<(), GrammarError> {
        self.expect_symbol('{')?;
        while !self.at_end() {
            if self.eat_symbol('}') {
                return Ok(());
            }
            if self.eat_ident("errors") {
                self.parse_errors_block()?;
                continue;
            }
            if self.eat_ident("recovery") {
                self.parse_recovery_block()?;
                continue;
            }
            let name = self.expect_ident("rule name")?;
            self.expect_symbol('=')?;
            let alternatives = self.parse_alternatives(&[DslKind::Symbol(';')])?;
            self.expect_semicolon()?;
            if self.grammar.rules.iter().any(|rule| rule.name == name) {
                return Err(self.error(format!("duplicate parser rule `{name}`")));
            }
            self.grammar.rules.push(RawRule { name, alternatives });
        }
        Err(self.error("unterminated parser block"))
    }

    fn parse_errors_block(&mut self) -> Result<(), GrammarError> {
        self.expect_symbol('{')?;
        while !self.at_end() {
            if self.eat_symbol('}') {
                return Ok(());
            }
            let class = self.expect_ident("error class (`token` or `node`)")?;
            match class.as_str() {
                "token" => {
                    let name = self.expect_name("error token name")?;
                    self.grammar.error_token = Some(name.clone());
                    let policy = self.parse_error_policy()?;
                    if policy == ErrorPolicy::Ignore {
                        self.grammar.ignored_lex_errors.insert(name);
                    } else {
                        self.grammar.ignored_lex_errors.remove(&name);
                    }
                }
                "node" => {
                    let _ = self.expect_name("error node name")?;
                    self.grammar.node_error_policy = self.parse_error_policy()?;
                }
                other => {
                    return Err(self.error(format!("unknown error class `{other}`")));
                }
            }
            self.expect_semicolon()?;
        }
        Err(self.error("unterminated errors block"))
    }

    fn parse_recovery_block(&mut self) -> Result<(), GrammarError> {
        self.expect_symbol('{')?;
        while !self.at_end() {
            if self.eat_symbol('}') {
                return Ok(());
            }
            let key = self.expect_ident("recovery option")?;
            if key == "error_token" {
                self.expect_symbol('=')?;
                self.grammar.error_token = Some(self.expect_name("error token name")?);
                self.expect_semicolon()?;
            } else {
                self.skip_item()?;
            }
        }
        Err(self.error("unterminated recovery block"))
    }

    fn parse_error_policy(&mut self) -> Result<ErrorPolicy, GrammarError> {
        let policy = self.expect_ident("error policy (`report` or `ignore`)")?;
        match policy.as_str() {
            "report" => Ok(ErrorPolicy::Report),
            "ignore" => Ok(ErrorPolicy::Ignore),
            other => Err(self.error(format!("unknown error policy `{other}`"))),
        }
    }

    fn expect_name(&mut self, what: &str) -> Result<String, GrammarError> {
        match self.peek().cloned() {
            Some(DslToken {
                kind: DslKind::Ident(name) | DslKind::Str(name),
                ..
            }) => {
                self.bump();
                Ok(name)
            }
            _ => Err(self.error(format!("expected {what}"))),
        }
    }

    fn parse_alternatives(&mut self, stop: &[DslKind]) -> Result<Vec<RawAlt>, GrammarError> {
        let mut alternatives = vec![self.parse_sequence(stop)?];
        while self.eat_symbol('|') {
            alternatives.push(self.parse_sequence(stop)?);
        }
        Ok(alternatives)
    }

    fn parse_sequence(&mut self, stop: &[DslKind]) -> Result<RawAlt, GrammarError> {
        let mut elements = Vec::new();
        let mut precedence = None;
        let mut dynamic = 0i32;
        let mut ast_action = None;
        loop {
            if self.at_end()
                || stop.iter().any(|kind| self.peek_kind() == Some(kind))
                || self.peek_is('|')
                || self.peek_is(')')
            {
                break;
            }
            if self.eat_symbol('@') {
                let annotation = self.expect_ident("annotation")?;
                match annotation.as_str() {
                    "prec" => {
                        self.expect_symbol('(')?;
                        let level = self.expect_ident("precedence level")?;
                        if self.eat_symbol(',') {
                            let _assoc = self.expect_ident("associativity")?;
                        }
                        self.expect_symbol(')')?;
                        precedence = Some(level);
                    }
                    "dynamic" => {
                        self.expect_symbol('(')?;
                        dynamic = self.expect_number()?;
                        self.expect_symbol(')')?;
                    }
                    "ast" => {
                        ast_action = Some(self.parse_ast_action()?);
                    }
                    other => {
                        return Err(self.error(format!("unknown parser annotation `@{other}`")));
                    }
                }
                continue;
            }
            elements.push(self.parse_element()?);
        }
        Ok(RawAlt {
            elements,
            precedence,
            dynamic,
            ast_action,
        })
    }

    fn parse_ast_action(&mut self) -> Result<AstAction, GrammarError> {
        self.expect_symbol('(')?;
        let name = self.expect_ident("AST constructor name")?;
        let mut fields = Vec::new();
        if self.eat_symbol('{') {
            while !self.eat_symbol('}') {
                let field = self.expect_ident("AST field name")?;
                self.expect_symbol(':')?;
                let index = self.expect_number()?;
                let index = usize::try_from(index)
                    .map_err(|_| self.error("AST field index must be non-negative"))?;
                fields.push((field, index));
                if !self.eat_symbol(',') && !self.peek_is('}') {
                    return Err(self.error("expected `,` or `}` in AST field list"));
                }
            }
        }
        self.expect_symbol(')')?;
        Ok(AstAction { name, fields })
    }

    fn parse_element(&mut self) -> Result<RawElem, GrammarError> {
        let offset = self.peek_offset();
        let mut element = if let (
            Some(DslToken {
                kind: DslKind::Ident(name),
                ..
            }),
            true,
        ) = (self.peek().cloned(), self.peek_n_is_symbol(1, ':'))
        {
            let field = name;
            self.bump();
            self.expect_symbol(':')?;
            let inner = self.parse_primary()?;
            RawElem::Field(field, Box::new(inner))
        } else {
            self.parse_primary()?
        };
        while let Some(c) = self.peek_char() {
            if c == '*' || c == '+' || c == '?' {
                self.bump();
                element = RawElem::Repeat(Box::new(element), c, offset);
            } else {
                break;
            }
        }
        Ok(element)
    }

    fn parse_primary(&mut self) -> Result<RawElem, GrammarError> {
        let offset = self.peek_offset();
        match self.peek().cloned() {
            Some(DslToken {
                kind: DslKind::Ident(name),
                ..
            }) => {
                self.bump();
                Ok(RawElem::Ref(name, offset))
            }
            Some(DslToken {
                kind: DslKind::Str(value),
                ..
            }) => {
                self.bump();
                Ok(RawElem::Literal(value, offset))
            }
            Some(DslToken {
                kind: DslKind::Symbol('('),
                ..
            }) => {
                self.bump();
                let alternatives = self.parse_alternatives(&[DslKind::Symbol(')')])?;
                self.expect_symbol(')')?;
                Ok(RawElem::Group(alternatives, offset))
            }
            _ => Err(self.error("expected a symbol, string, or group")),
        }
    }

    fn parse_precedence(&mut self) -> Result<(), GrammarError> {
        self.expect_symbol('{')?;
        let mut level = 0u32;
        while !self.at_end() {
            if self.eat_symbol('}') {
                return Ok(());
            }
            let name = self.expect_ident("precedence level")?;
            self.expect_symbol('=')?;
            let associativity = match self.expect_ident("associativity")?.as_str() {
                "left" => Associativity::Left,
                "right" => Associativity::Right,
                "nonassoc" => Associativity::NonAssoc,
                other => return Err(self.error(format!("unknown associativity `{other}`"))),
            };
            self.expect_semicolon()?;
            self.grammar.precedence.push(PrecedenceLevel {
                level,
                associativity,
                name,
            });
            level += 1;
        }
        Err(self.error("unterminated precedence block"))
    }

    fn parse_table(&mut self) -> Result<(), GrammarError> {
        self.expect_symbol('=')?;
        let mode = self.expect_ident("table mode")?;
        self.expect_semicolon()?;
        self.grammar.table_mode = match mode.as_str() {
            "lalr" => TableMode::Lalr,
            "lr1" => TableMode::Lr1,
            "ielr" => TableMode::Ielr,
            other => return Err(self.error(format!("unknown table mode `{other}`"))),
        };
        Ok(())
    }

    fn parse_conflict(&mut self) -> Result<(), GrammarError> {
        let left = self.expect_ident("rule name")?;
        self.expect_symbol(',')?;
        let right = self.expect_ident("rule name")?;
        self.expect_semicolon()?;
        self.grammar.conflicts.push((left, right));
        Ok(())
    }

    fn skip_item(&mut self) -> Result<(), GrammarError> {
        if self.eat_symbol('{') {
            return self.skip_balanced('{', '}');
        }
        let mut depth = 0i32;
        while !self.at_end() {
            if depth == 0 && self.peek_is(';') {
                self.bump();
                return Ok(());
            }
            if self.peek_is('}') && depth == 0 {
                return Ok(());
            }
            if self.peek_is('{') {
                depth += 1;
            } else if self.peek_is('}') {
                depth -= 1;
            }
            self.bump();
        }
        Ok(())
    }

    fn skip_braced(&mut self) -> Result<(), GrammarError> {
        self.expect_symbol('{')?;
        self.skip_balanced('{', '}')
    }

    fn skip_balanced(&mut self, open: char, close: char) -> Result<(), GrammarError> {
        let mut depth = 1usize;
        while !self.at_end() {
            if self.peek_is(open) {
                depth += 1;
            } else if self.peek_is(close) {
                depth -= 1;
                if depth == 0 {
                    self.bump();
                    return Ok(());
                }
            }
            self.bump();
        }
        Err(self.error("unterminated block"))
    }

    fn peek(&self) -> Option<&DslToken> {
        self.tokens.get(self.index)
    }

    fn peek_kind(&self) -> Option<&DslKind> {
        self.peek().map(|token| &token.kind)
    }

    fn peek_char(&self) -> Option<char> {
        match self.peek_kind()? {
            DslKind::Symbol(c) => Some(*c),
            _ => None,
        }
    }

    fn peek_is(&self, symbol: char) -> bool {
        self.peek_char() == Some(symbol)
    }

    fn peek_n_is_symbol(&self, n: usize, symbol: char) -> bool {
        matches!(
            self.tokens.get(self.index + n).map(|token| &token.kind),
            Some(DslKind::Symbol(c)) if *c == symbol
        )
    }

    fn peek_offset(&self) -> usize {
        self.peek().map_or(self.source.len(), |token| token.start)
    }

    fn bump(&mut self) {
        self.index += 1;
    }

    fn at_end(&self) -> bool {
        self.index >= self.tokens.len()
    }

    fn eat_ident(&mut self, expected: &str) -> bool {
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

    fn expect_ident(&mut self, what: &str) -> Result<String, GrammarError> {
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

    fn expect_number(&mut self) -> Result<i32, GrammarError> {
        match self.peek().cloned() {
            Some(DslToken {
                kind: DslKind::Number(value),
                ..
            }) => {
                self.bump();
                i32::try_from(value).map_err(|_| self.error("number is out of range"))
            }
            _ => Err(self.error("expected a number")),
        }
    }

    fn expect_symbol(&mut self, symbol: char) -> Result<(), GrammarError> {
        if self.eat_symbol(symbol) {
            Ok(())
        } else {
            Err(self.error(format!("expected `{symbol}`")))
        }
    }

    fn eat_symbol(&mut self, symbol: char) -> bool {
        if self.peek_is(symbol) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_semicolon(&mut self) -> Result<(), GrammarError> {
        self.expect_symbol(';')
    }

    fn error(&self, message: impl Into<String>) -> GrammarError {
        GrammarError::spec(message, self.peek_offset())
    }
}

fn lower(lexer: LexerSpec, raw: DslGrammar) -> Result<Grammar, GrammarError> {
    let conflicts = raw.conflicts.clone();
    let mut nonterminals: Vec<Nonterminal> = raw
        .rules
        .iter()
        .enumerate()
        .map(|(index, rule)| Nonterminal {
            id: index,
            name: rule.name.clone(),
        })
        .collect();
    let by_name: FxHashMap<String, NonterminalId> = nonterminals
        .iter()
        .map(|nonterminal| (nonterminal.name.clone(), nonterminal.id))
        .collect();

    let mut terminals = Vec::new();
    let mut terminal_by_kind = FxHashMap::default();
    let mut error_terminal = None;
    for kind in lexer.kinds() {
        if kind.trivia {
            continue;
        }
        let id = terminals.len();
        terminals.push(Terminal {
            id,
            token_kind: kind.id,
            name: kind.name.clone(),
            precedence: None,
        });
        terminal_by_kind.insert(kind.name.clone(), id);
        terminal_by_kind.insert(kind.id.0.to_string(), id);
        if kind.id == lexer.error_kind() {
            error_terminal = Some(id);
        }
    }
    if let Some(name) = &raw.error_token {
        let resolved = if name == "error" || name == "$error" {
            Some(lexer.error_kind())
        } else {
            lexer
                .kinds()
                .iter()
                .find(|kind| kind.name == *name || kind.id.0.to_string() == *name)
                .map(|kind| kind.id)
        };
        // A declared name is an alias for the reserved lexical error kind.
        // Distinct named error rules can later split this into multiple kinds.
        let kind = resolved.unwrap_or_else(|| lexer.error_kind());
        let Some(kind_info) = lexer.kinds().iter().find(|candidate| candidate.id == kind) else {
            return Err(GrammarError::spec(
                format!("`{name}` is not a lexical error token"),
                0,
            ));
        };
        if !kind_info.error {
            return Err(GrammarError::spec(
                format!("`{name}` is not a lexical error token"),
                0,
            ));
        }
        error_terminal = lexer
            .kinds()
            .iter()
            .find(|candidate| candidate.id == kind)
            .and_then(|candidate| terminal_by_kind.get(&candidate.name).copied())
            .or(error_terminal);
    }
    let eof_terminal = terminals.len();
    terminals.push(Terminal {
        id: eof_terminal,
        token_kind: TokenKindId(u32::MAX),
        name: "$eof".to_string(),
        precedence: None,
    });

    let mut fields = Vec::new();
    let mut field_by_name: FxHashMap<String, FieldId> = FxHashMap::default();
    let mut productions = Vec::new();
    let start_name = raw
        .start
        .clone()
        .or_else(|| raw.rules.first().map(|rule| rule.name.clone()))
        .ok_or_else(|| GrammarError::spec("grammar has no parser rules", 0))?;
    let start = *by_name
        .get(&start_name)
        .ok_or_else(|| GrammarError::spec(format!("unknown start rule `{start_name}`"), 0))?;

    let accept = nonterminals.len();
    nonterminals.push(Nonterminal {
        id: accept,
        name: "$accept".to_string(),
    });
    productions.push(Production {
        id: 0,
        lhs: accept,
        rhs: vec![Symbol::Nonterminal(start)],
        fields: Vec::new(),
        precedence: None,
        dynamic: 0,
        label: "$accept".to_string(),
        augmented: true,
        ast_action: None,
        transparent: false,
    });

    let precedence_by_name: FxHashMap<String, u32> = raw
        .precedence
        .iter()
        .map(|level| (level.name.clone(), level.level))
        .collect();

    for rule in &raw.rules {
        let lhs = by_name[&rule.name];
        for alternative in &rule.alternatives {
            let (rhs, fields) = lower_sequence(
                &alternative.elements,
                &by_name,
                &terminal_by_kind,
                &mut nonterminals,
                &mut productions,
                &mut fields,
                &mut field_by_name,
            )?;
            let precedence = alternative
                .precedence
                .as_ref()
                .map(|name| {
                    precedence_by_name.get(name).copied().ok_or_else(|| {
                        GrammarError::spec(format!("unknown precedence level `{name}`"), 0)
                    })
                })
                .transpose()?;
            // A production that is nothing but another nonterminal adds no
            // structure: it is transparent, so the tree contains the rule that
            // actually produced something.
            let transparent = rhs.len() == 1
                && matches!(rhs[0], Symbol::Nonterminal(_))
                && alternative.ast_action.is_none();
            productions.push(Production {
                id: productions.len(),
                lhs,
                rhs,
                fields,
                precedence,
                dynamic: alternative.dynamic,
                label: rule.name.clone(),
                augmented: false,
                ast_action: alternative.ast_action.clone(),
                transparent,
            });
        }
    }

    for (index, production) in productions.iter_mut().enumerate() {
        production.id = index;
    }
    for production in &productions {
        if production.augmented {
            continue;
        }
        let Some(level) = production.precedence else {
            continue;
        };
        if let Some(Symbol::Terminal(terminal)) = production
            .rhs
            .iter()
            .rev()
            .find(|symbol| matches!(symbol, Symbol::Terminal(_)))
        {
            terminals[*terminal].precedence = Some(level);
        }
    }
    let mut ignored_lex_error_kinds = FxHashSet::default();
    for name in &raw.ignored_lex_errors {
        let kind = if name == "error" || name == "$error" {
            Some(lexer.error_kind())
        } else {
            lexer
                .kinds()
                .iter()
                .find(|kind| kind.name == *name || kind.id.0.to_string() == *name)
                .map(|kind| kind.id)
        }
        .unwrap_or_else(|| lexer.error_kind());
        ignored_lex_error_kinds.insert(kind);
    }
    reject_nullable_cycles(&nonterminals, &productions)?;
    let start_production = 0;
    let table = lr::build(
        &terminals,
        &nonterminals,
        &productions,
        &raw.precedence,
        &conflicts,
        start_production,
        eof_terminal,
        raw.table_mode,
    )?;

    Ok(Grammar {
        lexer,
        terminals,
        nonterminals,
        productions,
        start,
        fields,
        precedence: raw.precedence,
        conflicts,
        table,
        table_mode: raw.table_mode,
        eof_terminal,
        error_terminal,
        lex_error_policy: raw.lex_error_policy,
        node_error_policy: raw.node_error_policy,
        ignored_lex_error_kinds,
    })
}

fn reject_nullable_cycles(
    nonterminals: &[Nonterminal],
    productions: &[Production],
) -> Result<(), GrammarError> {
    let mut nullable = vec![false; nonterminals.len()];
    loop {
        let mut changed = false;
        for production in productions {
            if nullable[production.lhs] {
                continue;
            }
            if production.rhs.iter().all(|symbol| match symbol {
                Symbol::Terminal(_) => false,
                Symbol::Nonterminal(id) => nullable[*id],
            }) {
                nullable[production.lhs] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut edges = vec![Vec::new(); nonterminals.len()];
    for production in productions {
        if production.augmented {
            continue;
        }
        if production.rhs.iter().all(|symbol| match symbol {
            Symbol::Terminal(_) => false,
            Symbol::Nonterminal(id) => nullable[*id],
        }) {
            for symbol in &production.rhs {
                if let Symbol::Nonterminal(id) = symbol {
                    edges[production.lhs].push(*id);
                }
            }
        }
    }
    /// Depth-first walk of the nullable graph.
    fn visit(
        node: usize,
        edges: &[Vec<NonterminalId>],
        state: &mut [u8],
        nonterminals: &[Nonterminal],
    ) -> Result<(), GrammarError> {
        state[node] = 1;
        for next in &edges[node] {
            if state[*next] == 1 {
                return Err(GrammarError::spec(
                    format!("nullable cycle involving `{}`", nonterminals[*next].name),
                    0,
                ));
            }
            if state[*next] == 0 {
                visit(*next, edges, state, nonterminals)?;
            }
        }
        state[node] = 2;
        Ok(())
    }
    let mut state = vec![0u8; nonterminals.len()];
    for node in 0..nonterminals.len() {
        if state[node] == 0 {
            visit(node, &edges, &mut state, nonterminals)?;
        }
    }
    Ok(())
}

/// The symbols of one lowered sequence, and where its named fields sit in it.
type LoweredSequence = (Vec<Symbol>, Vec<(FieldId, usize)>);

#[allow(clippy::too_many_arguments)]
fn lower_sequence(
    elements: &[RawElem],
    by_name: &FxHashMap<String, NonterminalId>,
    terminal_by_name: &FxHashMap<String, TerminalId>,
    nonterminals: &mut Vec<Nonterminal>,
    productions: &mut Vec<Production>,
    fields: &mut Vec<Field>,
    field_by_name: &mut FxHashMap<String, FieldId>,
) -> Result<LoweredSequence, GrammarError> {
    let mut rhs = Vec::new();
    let mut field_positions = Vec::new();
    for element in elements {
        let (symbol, field, extra_productions) = lower_element(
            element,
            by_name,
            terminal_by_name,
            nonterminals,
            productions,
            fields,
            field_by_name,
        )?;
        let position = rhs.len();
        rhs.push(symbol);
        if let Some(field) = field {
            field_positions.push((field, position));
        }
        productions.extend(extra_productions);
    }
    Ok((rhs, field_positions))
}

fn lower_element(
    element: &RawElem,
    by_name: &FxHashMap<String, NonterminalId>,
    terminal_by_name: &FxHashMap<String, TerminalId>,
    nonterminals: &mut Vec<Nonterminal>,
    productions: &mut Vec<Production>,
    fields: &mut Vec<Field>,
    field_by_name: &mut FxHashMap<String, FieldId>,
) -> Result<(Symbol, Option<FieldId>, Vec<Production>), GrammarError> {
    match element {
        RawElem::Ref(name, _) => {
            if let Some(terminal) = terminal_by_name.get(name).copied() {
                Ok((Symbol::Terminal(terminal), None, Vec::new()))
            } else if let Some(nonterminal) = by_name.get(name).copied() {
                Ok((Symbol::Nonterminal(nonterminal), None, Vec::new()))
            } else {
                Err(GrammarError::spec(
                    format!("unknown grammar symbol `{name}`"),
                    0,
                ))
            }
        }
        RawElem::Literal(value, offset) => terminal_by_name
            .get(value)
            .copied()
            .map(|terminal| (Symbol::Terminal(terminal), None, Vec::new()))
            .ok_or_else(|| {
                GrammarError::spec(format!("no lexer token matches literal `{value}`"), *offset)
            }),
        RawElem::Field(name, inner) => {
            let field = if let Some(field) = field_by_name.get(name).copied() {
                field
            } else {
                let id = fields.len();
                fields.push(Field {
                    id,
                    name: name.clone(),
                });
                field_by_name.insert(name.clone(), id);
                id
            };
            let (symbol, _nested, productions) = lower_element(
                inner,
                by_name,
                terminal_by_name,
                nonterminals,
                productions,
                fields,
                field_by_name,
            )?;
            Ok((symbol, Some(field), productions))
        }
        RawElem::Group(alternatives, _) => {
            let id = nonterminals.len();
            nonterminals.push(Nonterminal {
                id,
                name: format!("$group{id}"),
            });
            let mut extra = Vec::new();
            for alternative in alternatives {
                let (rhs, field_positions) = lower_sequence(
                    &alternative.elements,
                    by_name,
                    terminal_by_name,
                    nonterminals,
                    productions,
                    fields,
                    field_by_name,
                )?;
                extra.push(Production {
                    id: 0,
                    lhs: id,
                    rhs,
                    fields: field_positions,
                    precedence: None,
                    dynamic: alternative.dynamic,
                    label: format!("$group{id}"),
                    augmented: false,
                    ast_action: None,
                    transparent: true,
                });
            }
            Ok((Symbol::Nonterminal(id), None, extra))
        }
        RawElem::Repeat(inner, kind, _) => {
            let id = nonterminals.len();
            nonterminals.push(Nonterminal {
                id,
                name: format!("$repeat{id}"),
            });
            let (symbol, field, mut extra) = lower_element(
                inner,
                by_name,
                terminal_by_name,
                nonterminals,
                productions,
                fields,
                field_by_name,
            )?;
            let mut push = |rhs: Vec<Symbol>| {
                extra.push(Production {
                    id: 0,
                    lhs: id,
                    rhs,
                    fields: Vec::new(),
                    precedence: None,
                    dynamic: 0,
                    label: format!("$repeat{id}"),
                    augmented: false,
                    ast_action: None,
                    transparent: true,
                });
            };
            match kind {
                '*' => {
                    push(Vec::new());
                    push(vec![Symbol::Nonterminal(id), symbol]);
                }
                '+' => {
                    push(vec![symbol]);
                    push(vec![Symbol::Nonterminal(id), symbol]);
                }
                '?' => {
                    push(Vec::new());
                    push(vec![symbol]);
                }
                _ => return Err(GrammarError::spec("unknown repetition operator", 0)),
            }
            let _ = field;
            Ok((Symbol::Nonterminal(id), None, extra))
        }
    }
}
