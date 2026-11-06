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

use std::sync::Arc;

use crate::diagnostic::Diagnostic;
use crate::lexer::TokenId;
use crate::text::TextAnchor;

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
/// round-trip; `AstMeta::rules` maps the identity back to the name.
#[must_use]
pub fn rule_id(name: &str) -> u64 {
    reactive::const_hash(name)
}

/// Stable identity of a token leaf.
#[must_use]
pub fn ast_token_id(token: TokenId) -> u64 {
    mix(0x51ED_2701_9E37_79B9, token.0)
}

/// Stable identity of a rule node: its rule and the tokens it covers.
///
/// The *extent* is the identity, not the structure: a node that still starts and
/// ends at the same tokens is the same node even when a child in the middle
/// changed, so its ancestors keep their names and only the changed path is
/// rewritten. Nodes that only moved keep their identity too, because token
/// identities survive a shift.
///
/// A zero-width node has no extent; the tokens around the insertion point
/// distinguish it from its siblings.
#[must_use]
pub fn ast_rule_id(
    rule: u64,
    first: Option<TokenId>,
    last: Option<TokenId>,
    prev: Option<TokenId>,
    next: Option<TokenId>,
) -> u64 {
    let id = mix(0x2545_F491_4F6C_DD1D, rule);
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
    /// Stable rule identity for rule nodes, `0` for token/error nodes.
    pub rule: u64,
    /// Start anchor in the version the node's source was produced from.
    pub span_start: TextAnchor,
    /// End anchor in the version the node's source was produced from.
    pub span_end: TextAnchor,
    /// Child AST node IDs, in source order.
    pub children: Vec<u64>,
    /// Token identity for token and recovery leaves.
    pub token: Option<TokenId>,
    /// Recovery category, for recovery nodes.
    pub category: Option<ErrorCategoryId>,
    /// Whether the grammar ignores this recovery category.
    pub ignored: bool,
    /// Semantic value of this node, when it denotes a literal.
    pub value: Option<AstValue>,
    /// Named field-to-child IDs.
    pub fields: Vec<(String, u64)>,
}

impl AstNodeRecord {
    /// Resolves this node's span against a snapshot.
    #[must_use]
    pub fn resolve_span(
        &self,
        snapshot: &Arc<crate::TextSnapshot>,
    ) -> Option<std::ops::Range<usize>> {
        let start = snapshot.resolve(self.span_start)?;
        let end = snapshot.resolve(self.span_end)?;
        (start <= end).then_some(start..end)
    }

    /// Whether this record describes a recovery node.
    #[must_use]
    pub fn is_recovery(&self) -> bool {
        self.category.is_some()
    }
}

/// A parsed AST: the root plus the nodes an edit added and removed.
///
/// The build is a delta against the previous AST, so publication touches only
/// the edited region. `added` holds the nodes this parse created or replaced,
/// `removed` holds identities that no longer exist, and a node in neither is
/// untouched.
#[derive(Clone, Debug, Default)]
pub struct AstBuild {
    /// Root identity.
    pub root: u64,
    /// Nodes created or replaced by this parse.
    pub added: Vec<AstNodeRecord>,
    /// Identities that no longer exist.
    pub removed: Vec<u64>,
    /// Parser diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Recovery cost of the accepted parse.
    pub recovery_cost: u32,
    /// Locality measurements.
    pub stats: BuildStats,
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
