//! Stateful incremental lexical analysis.

pub mod guard;
pub mod pattern;
pub mod runtime;
pub mod spec;

use std::fmt;
use std::sync::Arc;

use crate::diagnostic::Diagnostic;
use crate::text::{TextAnchor, TextRange, TextSnapshot};

pub use guard::{Action, EvalContext, ExprError, GuardExpr, LexValue};
pub use pattern::{Pattern, PatternError, PatternMatch};
pub use runtime::Lexer;
pub use spec::{LexerSpec, Rule, RuleKind};

/// Identity of a token kind within one lexer specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenKindId(pub u32);

/// Metadata for one token kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenKind {
    /// Stable id in declaration order.
    pub id: TokenKindId,
    /// Source name (`ident`, or an exact literal such as `{`).
    pub name: String,
    /// Whether the token is trivia and must not participate in parsing.
    pub trivia: bool,
    /// Whether the token is word-like for keyword extraction.
    pub word: bool,
    /// Whether this kind is a declared lexical error kind.
    pub error: bool,
}

/// Identity of a token. Reused tokens keep their id across revisions.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenId(pub u64);

impl std::fmt::Debug for TokenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Token flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TokenFlags(u8);

impl TokenFlags {
    /// The token is trivia (whitespace, comments, synthetic layout tokens).
    pub const TRIVIA: Self = Self(1 << 0);
    /// The token was produced by lexical error recovery.
    pub const ERROR: Self = Self(1 << 1);
    /// The token was inserted by parser error recovery.
    pub const MISSING: Self = Self(1 << 2);
    /// The token reaches EOF in an unterminated state.
    pub const UNTERMINATED: Self = Self(1 << 3);

    /// Returns `true` when all bits in `other` are set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns the union.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns the intersection.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Returns `true` when no flag is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for TokenFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for TokenFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

/// One token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// Stable identity.
    pub id: TokenId,
    /// Token kind.
    pub kind: TokenKindId,
    /// Lexeme.
    pub text: Arc<str>,
    /// Source range.
    pub range: TextRange,
    /// Lexer state at the token start.
    pub start_state: LexConfig,
    /// Lexer state after the token and its actions.
    pub end_state: LexConfig,
    /// Furthest source position examined while recognizing the token.
    pub lookahead_end: TextAnchor,
    /// Flags.
    pub flags: TokenFlags,
    /// Lexical error message, if any.
    pub error: Option<Arc<str>>,
}

/// A persistent token sequence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenStream {
    tokens: Arc<[Arc<Token>]>,
}

impl TokenStream {
    /// Builds a token stream.
    #[must_use]
    pub fn new(tokens: Vec<Arc<Token>>) -> Self {
        Self {
            tokens: tokens.into(),
        }
    }

    /// The empty stream.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Number of tokens.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Returns `true` when there are no tokens.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Tokens in source order.
    #[must_use]
    pub fn as_slice(&self) -> &[Arc<Token>] {
        &self.tokens
    }

    /// Iterates over tokens.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Token>> {
        self.tokens.iter()
    }

    /// Returns one token.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Arc<Token>> {
        self.tokens.get(index)
    }
}

impl<'a> IntoIterator for &'a TokenStream {
    type Item = &'a Arc<Token>;
    type IntoIter = std::slice::Iter<'a, Arc<Token>>;

    fn into_iter(self) -> Self::IntoIter {
        self.tokens.iter()
    }
}

impl IntoIterator for TokenStream {
    type Item = Arc<Token>;
    type IntoIter = std::vec::IntoIter<Arc<Token>>;

    fn into_iter(self) -> Self::IntoIter {
        // The stream owns its slice; cloning the handles is the only way to hand
        // out owned items, because `Arc<[T]>` cannot be moved out of.
        self.tokens.iter().cloned().collect::<Vec<_>>().into_iter()
    }
}

/// A lexer configuration at a token boundary.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LexConfig {
    /// Current mode.
    pub mode: ModeId,
    /// Stack of modes to restore on `pop`.
    pub mode_stack: Vec<ModeId>,
    /// Auxiliary value stack.
    pub values: Vec<LexValue>,
    /// Zero-based column in Unicode scalar values.
    pub column: u32,
}

impl LexConfig {
    /// Builds a state.
    #[must_use]
    pub fn new(mode: ModeId, values: Vec<LexValue>, column: u32) -> Self {
        Self {
            mode,
            mode_stack: Vec::new(),
            values,
            column,
        }
    }

    /// The top value, if any.
    #[must_use]
    pub fn top(&self) -> Option<&LexValue> {
        self.values.last()
    }
}

/// Identity of a lexer mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModeId(pub u32);

impl fmt::Display for ModeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mode{}", self.0)
    }
}

/// Lexer runtime options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexerOptions {
    /// Maximum value-stack depth.
    pub max_value_depth: usize,
    /// Maximum consecutive zero-width tokens at one position.
    pub max_zero_width_run: usize,
}

impl Default for LexerOptions {
    fn default() -> Self {
        Self {
            max_value_depth: 256,
            max_zero_width_run: 64,
        }
    }
}

/// Result of lexing a document.
#[derive(Clone, Debug)]
pub struct LexOutput {
    /// Tokens covering the document.
    pub tokens: Arc<TokenStream>,
    /// Coordinate space for token anchors.
    pub text: Arc<TextSnapshot>,
    /// Diagnostics from lexical analysis.
    pub diagnostics: Vec<Diagnostic>,
    /// Number of characters relexed (for tests and benchmarks).
    pub relexed_chars: usize,
    /// Token index range that was rebuilt; everything else is carried over.
    pub relexed_tokens: std::ops::Range<usize>,
    /// Token index range of the previous stream that was replaced.
    pub replaced_tokens: std::ops::Range<usize>,
}

impl PartialEq for LexOutput {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.tokens, &other.tokens)
            && self.text == other.text
            && self.diagnostics == other.diagnostics
    }
}

impl Eq for LexOutput {}

impl LexOutput {
    /// Returns the token stream.
    #[must_use]
    pub fn tokens(&self) -> &TokenStream {
        &self.tokens
    }

    /// Returns diagnostics.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

/// Retained state of a lexer invocation.
#[derive(Clone, Debug)]
pub struct LexState {
    /// Last token stream.
    pub tokens: Arc<TokenStream>,
    /// Coordinate space of the tokens.
    pub text: Arc<TextSnapshot>,
    /// Diagnostics of the last token stream.
    ///
    /// Retained exactly like the tokens: a revision only relexes an edit window
    /// and reuses the rest of the stream, so the diagnostics that belong to the
    /// reused part can only be carried from here.
    pub diagnostics: Vec<Diagnostic>,
}

impl LexState {
    /// Builds retained lexer state without diagnostics.
    #[must_use]
    pub fn new(tokens: Arc<TokenStream>, text: Arc<TextSnapshot>) -> Self {
        Self {
            tokens,
            text,
            diagnostics: Vec::new(),
        }
    }

    /// Builds retained lexer state from a lex result.
    #[must_use]
    pub fn from_output(output: &LexOutput) -> Self {
        Self {
            tokens: Arc::clone(&output.tokens),
            text: Arc::clone(&output.text),
            diagnostics: output.diagnostics.clone(),
        }
    }
}

/// Lexer specification/runtime error.
#[derive(Debug, thiserror::Error)]
pub enum LexError {
    /// The specification is invalid.
    #[error("lexer specification error at byte {offset}: {message}")]
    Spec {
        /// Human-readable message.
        message: String,
        /// Byte offset in the grammar source.
        offset: usize,
    },
    /// A pattern is invalid.
    #[error(transparent)]
    Pattern(#[from] PatternError),
    /// A guard or action expression is invalid.
    #[error(transparent)]
    Expr(#[from] ExprError),
    /// A text edit was invalid.
    #[error(transparent)]
    Edit(#[from] crate::text::TextEditError),
}
