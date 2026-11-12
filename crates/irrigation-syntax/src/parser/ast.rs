//! The AST data model.
//!
//! The parser produces [`AstNodeRecord`]s and nothing else: there is no
//! concrete tree. Token identity, text, ranges, and flags are metadata living in
//! the `Tokens` View, so an AST node only says *what* it is and which children
//! it has.
//!
//! # Identity
//!
//! A rule node is identified by its rule and its child identities:
//!
//! ```text
//! id(rule, children) = hash(rule, children)
//! ```
//!
//! Spans are deliberately *not* part of identity. Two nodes describing the same
//! structure over the same tokens are the same node, and a node that merely
//! moved because text was inserted before it keeps its identity. That is what
//! lets a local reparse leave every untouched subtree (and its metadata units)
//! alone.
//!
//! Token and recovery leaves are identified by the token they cover, so a
//! subtree containing a changed token gets a new identity along the ancestor
//! chain and nowhere else.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use crate::lexer::TokenId;
use crate::text::TextAnchor;

use super::grammar::{FieldId as GrammarFieldId, ProductionId};

/// Error category attached to a recovery node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ErrorCategoryId(pub u16);

impl ErrorCategoryId {
    /// Category for ordinary parser recovery nodes.
    pub const RECOVERY: Self = Self(1);
    /// Category for a parser-inserted missing token.
    pub const MISSING: Self = Self(2);
}

/// One semantic value attached to an AST node.
///
/// Values are metadata, never tree fields: a changed literal is one unit.
#[derive(Clone, Debug, PartialEq)]
pub enum AstValue {
    /// No payload.
    Unit,
    /// A boolean literal.
    Bool(bool),
    /// A signed integer literal.
    Int(i64),
    /// An unsigned integer literal that does not fit in `i64`.
    UInt(u64),
    /// A floating-point literal.
    Float(f64),
    /// A text literal.
    Text(Arc<str>),
}

impl AstValue {
    /// The signed integer, when the payload is one (or an unsigned value that fits).
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            Self::UInt(value) => i64::try_from(*value).ok(),
            _ => None,
        }
    }

    /// The unsigned integer, when the payload is one.
    #[must_use]
    pub fn as_uint(&self) -> Option<u64> {
        match self {
            Self::UInt(value) => Some(*value),
            Self::Int(value) => u64::try_from(*value).ok(),
            _ => None,
        }
    }

    /// The float, when the payload is one.
    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(value) => Some(*value),
            _ => None,
        }
    }

    /// The boolean, when the payload is one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// The text, when the payload is text.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }
}

/// Literal denotation for one token's text.
///
/// Language-agnostic default: integers, floats, and booleans are recognized by
/// their text; everything else stays text. A typed AST schema can override this
/// by publishing its own `values` units.
#[must_use]
pub fn literal_value(text: &str) -> AstValue {
    if let Ok(value) = text.parse::<i64>() {
        return AstValue::Int(value);
    }
    if let Ok(value) = text.parse::<u64>() {
        return AstValue::UInt(value);
    }
    if let Ok(value) = text.parse::<f64>() {
        return AstValue::Float(value);
    }
    match text {
        "true" => AstValue::Bool(true),
        "false" => AstValue::Bool(false),
        "null" => AstValue::Unit,
        _ => AstValue::Text(Arc::from(text)),
    }
}

/// Stable identity of an AST rule name.
///
/// Hashing the name keeps identities stable across reparses without a registry
/// round-trip; `AstMeta::rules` maps the identity back to the name. It is a
/// `const fn`, so a consumer may name a rule in a `const` instead of hashing its
/// spelling on every comparison.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleId(pub u64);

impl RuleId {
    /// The identity of `name`.
    #[must_use]
    pub const fn of(name: &str) -> Self {
        Self(irrigation::const_hash(name))
    }

    /// Returns the raw identity word.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Stable identity of an AST rule name, as a plain word.
///
/// [`RuleId`] is the typed spelling and is what readers should use.
#[must_use]
pub const fn rule_id(name: &str) -> RuleId {
    RuleId::of(name)
}

/// Stable identity of a token leaf.
#[must_use]
pub fn ast_token_id(token: TokenId) -> u64 {
    mix(0x51ED_2701_9E37_79B9, token.0)
}

/// Stable identity of a rule node that is unique at its first token.
///
/// Most rule occurrences are named by the token that starts them: there is one
/// `Let` per `let`, one `Lam` per `\\`, one `Document` per first token. Those
/// nodes used the extent, so growing a body at the end re-keyed every ancestor.
/// The parser offers this short identity first and falls back to the extent when
/// a second node with the same rule and first token exists (a left-recursive
/// `App` chain is the real case).
#[must_use]
pub fn ast_short_id(rule: RuleId, first: TokenId) -> u64 {
    mix(mix(0x4F43_4355_5252_454E, rule.0), first.0)
}

/// Stable identity of a tree root.
///
/// A tree has one root. Its identity is the tree's, not its extent's: a root
/// whose span grows must stay the same node, or every consumer of the tree
/// restarts its invocation scope on every edit. The parser owns one tree per
/// AST View, so one constant per schema is enough; a root that a failed parse
/// has to synthesize (`$error`) keeps the same identity as the root it
/// replaced, so an error is an edit to the tree rather than a new tree.
#[must_use]
pub const fn ast_root_id() -> u64 {
    0x5452_4545_524F_4F54
}

/// Stable identity of a rule node: its rule and the tokens it covers.
///
/// The *extent* is the identity, not the structure: a node that still starts and
/// ends at the same tokens is the same node even when a child in the middle
/// changed, so its ancestors keep their names and only the changed path is
/// rewritten. Nodes that only moved keep their identity too, because token
/// identities survive a shift, and a token that is re-spelled in place keeps its
/// identity as well, so re-spelling a leaf does not re-key the nodes above it.
///
/// The full extent, rather than one boundary, is what separates nested siblings:
/// `f 2 3` nests an `App` that starts at the same token as its parent.
///
/// A zero-width node has no extent; the tokens around the insertion point
/// distinguish it from its siblings.
#[must_use]
pub fn ast_node_id(
    rule: RuleId,
    first: Option<TokenId>,
    last: Option<TokenId>,
    prev: Option<TokenId>,
    next: Option<TokenId>,
) -> u64 {
    let id = mix(0x2545_F491_4F6C_DD1D, rule.0);
    match (first, last) {
        (Some(first), Some(last)) => mix(mix(id, first.0), last.0),
        _ => mix(
            mix(id, prev.map_or(0, |token| token.0)),
            next.map_or(u64::MAX, |token| token.0),
        ),
    }
}

/// One generic AST node, as published.
///
/// Records carry identity and structure plus the facts that become metadata
/// units. Spans are version-tagged anchors so a node whose source did not
/// change keeps a stable anchor across edits.
#[derive(Clone, Debug, PartialEq)]
pub struct AstNodeRecord {
    /// Stable node identity.
    pub id: u64,
    /// Constructor or grammar rule name.
    pub kind: String,
    /// Stable rule identity for rule nodes, [`RuleId::default`] for token and
    /// recovery nodes.
    pub rule: RuleId,
    /// The production that built this node, for a rule node.
    ///
    /// Two alternatives of one rule share a rule id, so they can build different
    /// declared tree variants; the production is the only key that tells them
    /// apart.
    pub production: Option<ProductionId>,
    /// Start anchor in the version the node's source was produced from.
    pub span_start: TextAnchor,
    /// End anchor in the version the node's source was produced from.
    pub span_end: TextAnchor,
    /// Child AST node IDs, in source order.
    pub children: Vec<u64>,
    /// Token identity for token and recovery leaves.
    pub token: Option<TokenId>,
    /// Token text for token and recovery leaves, when the parser has it.
    pub text: Option<std::sync::Arc<str>>,
    /// Recovery category, for recovery nodes.
    pub category: Option<ErrorCategoryId>,
    /// Whether the grammar ignores this recovery category.
    pub ignored: bool,
    /// Semantic value of this node, when it denotes a literal.
    pub value: Option<AstValue>,
    /// Each declared field, and the slice of [`Self::children`] it names.
    ///
    /// Recorded only for productions that declare a field or build a declared
    /// tree node. A single child is a one-element range; a repetition is the
    /// whole run, which a flat child list cannot recover on its own.
    pub fields: Vec<(GrammarFieldId, Range<usize>)>,
}

impl AstNodeRecord {
    /// Whether this record describes a recovery node.
    #[must_use]
    pub fn is_recovery(&self) -> bool {
        self.category.is_some()
    }
}

/// Where one parse did its work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuildStats {
    /// First significant token index this parse rebuilt.
    pub token_start: usize,
    /// End of the rebuilt token window.
    pub token_end: usize,
    /// Significant tokens the parser consumed (reduced or skipped).
    pub tokens_scanned: usize,
    /// Nodes this parse built.
    pub nodes_built: usize,
    /// Whether the parse adopted the previous suffix instead of finishing.
    pub adopted_suffix: bool,
    /// Subtrees adopted from the previous tree.
    pub splices: usize,
    /// Token index where the suffix was adopted.
    pub adopted_at: usize,
    /// How many times the window had to grow.
    pub escalations: u32,
    /// Whether the parse had to look at the whole token stream.
    pub full: bool,
}

/// Mixes two words into one identity word.
#[must_use]
pub fn mix(a: u64, b: u64) -> u64 {
    let mut hash = a ^ b.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    hash ^= hash >> 32;
    hash
}

/// Identity of a recovery node with no token of its own.
#[must_use]
pub(crate) fn error_id(category: ErrorCategoryId, parts: impl IntoIterator<Item = u64>) -> u64 {
    let mut id = mix(0x9F1A_2C33_4D55_6E77, u64::from(category.0));
    for part in parts {
        id = mix(id, part);
    }
    id
}
