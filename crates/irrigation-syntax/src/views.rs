//! The published Views of the syntax pipeline.
//!
//! The pipeline publishes three things and nothing else:
//!
//! * [`Tokens`] — the token stream plus one metadata record per token (kind,
//!   text, range, flags, error) and the latest delta;
//! * `AstTree` (the generated View in [`ast_tree`]) — the AST *structure*: what
//!   each node is and which children it has;
//! * [`AstMeta`] — everything about an AST node that is not structure: spans,
//!   token identity, semantic value, named fields, diagnostics.
//!
//! Splitting structure from metadata is what makes an edit local: an insertion
//! rewrites the structure of one path and leaves every span unit alone, while a
//! value change rewrites one unit and leaves the tree alone.

#![allow(missing_docs)]

use std::sync::Arc;

use crate::diagnostic::Diagnostic;
use crate::lexer::{TokenFlags, TokenId, TokenKindId};
use crate::text::{TextAnchor, TextDocument, TextRange};

/// Stable diagnostic identity: code plus source span.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiagnosticId {
    /// Diagnostic code.
    pub code: String,
    /// Start offset.
    pub start: usize,
    /// End offset.
    pub end: usize,
}

impl DiagnosticId {
    /// Derives the identity of one diagnostic.
    #[must_use]
    pub fn from_diagnostic(diagnostic: &Diagnostic) -> Self {
        Self {
            code: diagnostic.code.clone(),
            start: diagnostic.range.start.offset,
            end: diagnostic.range.end.offset,
        }
    }
}

/// Source text View consumed by the lexer component.
#[irrigation::view(source)]
pub struct SourceFile {
    /// Current immutable document and its edit ancestry.
    pub document: TextDocument,
}

/// Source-text View emitted by the text component.
#[irrigation::view(derived)]
pub struct Text {
    /// Current immutable document.
    pub document: TextDocument,
    /// Document version.
    pub version: u64,
    /// Per-edit units, so a consumer can see what changed without diffing.
    pub edits: Map<u64, crate::TextEdit>,
}

impl Text {
    /// The document unit.
    #[must_use]
    pub fn document_unit() -> irrigation::UnitKey {
        Self::DOCUMENT.unit()
    }
}

/// Everything the parser needs to know about one token.
///
/// The token stream is the only place token structure lives: there is no second
/// tree of token kinds and ranges, just one record per token.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenPayload {
    /// Token kind.
    pub kind: TokenKindId,
    /// Lexeme.
    pub text: Arc<str>,
    /// Source range.
    pub range: TextRange,
    /// Flags.
    pub flags: TokenFlags,
    /// Lexical error message, if any.
    pub error: Option<Arc<str>>,
}

impl std::fmt::Debug for TokenPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)?;
        if let Some(error) = &self.error {
            write!(f, " · {error}")?;
        }
        Ok(())
    }
}

/// Token stream View emitted by the lexer component.
#[irrigation::view(derived)]
pub struct Tokens {
    /// Coordinate space for token anchors.
    pub text: TextDocument,
    /// Significant and trivia tokens in source order.
    pub order: List<TokenId>,
    /// Latest token delta.
    pub delta: TokenDelta,
    /// Lexical diagnostics keyed by stable identity.
    pub diagnostics: Map<DiagnosticId, Diagnostic>,
    /// Stable identities of current diagnostics.
    pub diagnostic_ids: Set<DiagnosticId>,
    /// Per-token metadata.
    pub payloads: Map<TokenId, Arc<TokenPayload>>,
    /// Tokens changed in the latest revision.
    pub changed: Set<TokenId>,
    /// Trivia tokens.
    pub trivia: Set<TokenId>,
    /// Tokens carrying lexical error flags.
    pub errors: Set<TokenId>,
}

/// The token delta of one revision.
///
/// Downstream components read this one unit to learn what moved, then read
/// payloads only for the identities it names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenDelta {
    /// Previous text version.
    pub from: u64,
    /// Current text version.
    pub to: u64,
    /// Ordered token-ID edits.
    pub order: irrigation::ListDelta<TokenId>,
    /// Token identities whose payload is new or changed.
    pub changed: Vec<TokenId>,
    /// Current lexical diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Whether every changed token is trivia.
    pub trivia_only: bool,
    /// Characters the lexer re-examined.
    pub bytes_relexed: usize,
    /// Tokens the lexer rebuilt.
    pub tokens_relexed: usize,
}

impl TokenDelta {
    /// The changed identities as a set.
    #[must_use]
    pub fn changed_set(&self) -> std::collections::HashSet<TokenId> {
        self.changed.iter().copied().collect()
    }

    /// Whether the delta describes no change at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.edits().is_empty()
            && self.changed.is_empty()
            && self.diagnostics.is_empty()
            && self.from == self.to
    }
}

/// Structural AST View.
///
/// The tree answers "what is this node?" only. Positions, token text, and
/// semantic values live in [`AstMeta`] units so an insertion does not
/// invalidate thousands of span units.
#[irrigation::tree_view(root = AstNode, view = AstTree, kind = derived)]
pub mod ast_tree {
    /// One structural AST node.
    #[derive(Clone, Debug, PartialEq)]
    pub enum AstNode {
        /// A reduced grammar node.
        Rule {
            /// Stable rule identity: [`rule_id`](crate::rule_id) of the name.
            rule: u64,
            /// The rule's spelling, for a generic drawing that has no renderer.
            name: String,
            /// Children in source order.
            children: Vec<AstNode>,
        },
        /// A token leaf.
        Token {
            /// Token identity.
            token_id: u64,
            /// The lexeme, so a generic tree can show the token it stands for.
            text: String,
        },
        /// A recovery node.
        Error {
            /// Recovery category.
            category: u64,
            /// Whether the grammar ignores this category.
            ignored: bool,
            /// Recovered children.
            recovered: Vec<AstNode>,
        },
    }
}

/// Source-correlation and semantic metadata for the AST.
#[irrigation::view(derived)]
pub struct AstMeta {
    /// Start anchor per node.
    pub span_starts: Map<u64, TextAnchor>,
    /// End anchor per node.
    pub span_ends: Map<u64, TextAnchor>,
    /// Token identity attached to a node.
    pub tokens: Map<u64, TokenId>,
    /// Semantic value per node.
    pub values: Map<u64, crate::AstValue>,
    /// Node IDs that represent recovery placeholders.
    pub errors: Set<u64>,
    /// AST parse diagnostics.
    pub diagnostics: Map<DiagnosticId, Diagnostic>,
    /// Named AST field mappings.
    pub fields: Map<(u64, String), u64>,
    /// Rule identity to rule name.
    pub rules: Map<u64, Arc<str>>,
}

/// One pipeline counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StatId {
    /// Bytes the lexer re-examined.
    BytesRelexed,
    /// Tokens the lexer relexed.
    TokensRelexed,
    /// Significant tokens the parser consumed.
    TokensReparsed,
    /// Bytes the parser rebuilt.
    BytesReparsed,
    /// AST nodes the parser built.
    NodesBuilt,
    /// AST nodes whose identity is new.
    NodesAdded,
    /// AST nodes retracted.
    NodesRemoved,
    /// Metadata units written for the AST.
    MetaUnitsWritten,
    /// Token units the parser read.
    TokenUnitsRead,
    /// Times the parser grew its window.
    Escalations,
    /// Whether the last parse adopted the recorded suffix.
    AdoptedSuffix,
}

impl StatId {
    /// Every counter, in display order.
    pub const ALL: [Self; 11] = [
        Self::BytesRelexed,
        Self::TokensRelexed,
        Self::TokensReparsed,
        Self::BytesReparsed,
        Self::NodesBuilt,
        Self::NodesAdded,
        Self::NodesRemoved,
        Self::MetaUnitsWritten,
        Self::TokenUnitsRead,
        Self::Escalations,
        Self::AdoptedSuffix,
    ];

    /// Stable name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::BytesRelexed => "bytes_relexed",
            Self::TokensRelexed => "tokens_relexed",
            Self::TokensReparsed => "tokens_reparsed",
            Self::BytesReparsed => "bytes_reparsed",
            Self::NodesBuilt => "nodes_built",
            Self::NodesAdded => "nodes_added",
            Self::NodesRemoved => "nodes_removed",
            Self::MetaUnitsWritten => "meta_units_written",
            Self::TokenUnitsRead => "token_units_read",
            Self::Escalations => "escalations",
            Self::AdoptedSuffix => "adopted_suffix",
        }
    }
}

/// Work counters for the latest revision.
///
/// One unit per counter, so a HUD can read just the numbers it displays and is
/// only invalidated when one of those moves.
#[irrigation::view(derived)]
pub struct PipelineStats {
    /// Counter values.
    pub counters: Map<StatId, u64>,
}

/// The counters of one revision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    /// Bytes the lexer re-examined.
    pub bytes_relexed: u64,
    /// Tokens the lexer relexed.
    pub tokens_relexed: u64,
    /// Significant tokens the parser consumed.
    pub tokens_reparsed: u64,
    /// Bytes the parser rebuilt.
    pub bytes_reparsed: u64,
    /// AST nodes the parser built.
    pub nodes_built: u64,
    /// AST nodes whose identity is new.
    pub nodes_added: u64,
    /// AST nodes retracted.
    pub nodes_removed: u64,
    /// Metadata units written for the AST.
    pub meta_units_written: u64,
    /// Token units the parser read.
    pub token_units_read: u64,
    /// Times the parser grew its window.
    pub escalations: u64,
    /// Whether the last parse adopted the recorded suffix.
    pub adopted_suffix: u64,
}

impl StatsSnapshot {
    /// Reads one counter.
    #[must_use]
    pub fn get(&self, stat: StatId) -> u64 {
        match stat {
            StatId::BytesRelexed => self.bytes_relexed,
            StatId::TokensRelexed => self.tokens_relexed,
            StatId::TokensReparsed => self.tokens_reparsed,
            StatId::BytesReparsed => self.bytes_reparsed,
            StatId::NodesBuilt => self.nodes_built,
            StatId::NodesAdded => self.nodes_added,
            StatId::NodesRemoved => self.nodes_removed,
            StatId::MetaUnitsWritten => self.meta_units_written,
            StatId::TokenUnitsRead => self.token_units_read,
            StatId::Escalations => self.escalations,
            StatId::AdoptedSuffix => self.adopted_suffix,
        }
    }

    /// Writes one counter.
    pub fn set(&mut self, stat: StatId, value: u64) {
        match stat {
            StatId::BytesRelexed => self.bytes_relexed = value,
            StatId::TokensRelexed => self.tokens_relexed = value,
            StatId::TokensReparsed => self.tokens_reparsed = value,
            StatId::BytesReparsed => self.bytes_reparsed = value,
            StatId::NodesBuilt => self.nodes_built = value,
            StatId::NodesAdded => self.nodes_added = value,
            StatId::NodesRemoved => self.nodes_removed = value,
            StatId::MetaUnitsWritten => self.meta_units_written = value,
            StatId::TokenUnitsRead => self.token_units_read = value,
            StatId::Escalations => self.escalations = value,
            StatId::AdoptedSuffix => self.adopted_suffix = value,
        }
    }

    /// Publishes every counter.
    pub fn publish(&self, cx: &mut irrigation::GeneralCx) {
        for stat in StatId::ALL {
            cx.write(PipelineStats::COUNTERS)
                .entry(stat)
                .set(self.get(stat));
        }
    }
}
