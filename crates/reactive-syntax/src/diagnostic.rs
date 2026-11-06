//! Structured diagnostics shared by the text, lexer, and parser layers.

use crate::lexer::TokenKindId;
use crate::text::TextRange;

/// Severity of a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Severity {
    /// The input or grammar is invalid.
    Error,
    /// The input or grammar is accepted but suspicious.
    Warning,
}

/// One diagnostic with a stable code and a source range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Stable machine-readable code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Source range.
    pub range: TextRange,
    /// Severity.
    pub severity: Severity,
    /// Optional actionable note.
    pub notes: Vec<String>,
    /// Originating token kind, when the diagnostic came from one.
    pub token_kind: Option<TokenKindId>,
}

impl Diagnostic {
    /// Builds an error diagnostic.
    #[must_use]
    pub fn error(code: impl Into<String>, message: impl Into<String>, range: TextRange) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            range,
            severity: Severity::Error,
            notes: Vec::new(),
            token_kind: None,
        }
    }

    /// Builds a warning diagnostic.
    #[must_use]
    pub fn warning(code: impl Into<String>, message: impl Into<String>, range: TextRange) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            range,
            severity: Severity::Warning,
            notes: Vec::new(),
            token_kind: None,
        }
    }

    /// Attaches the originating token kind.
    #[must_use]
    pub fn with_token_kind(mut self, kind: TokenKindId) -> Self {
        self.token_kind = Some(kind);
        self
    }
}
