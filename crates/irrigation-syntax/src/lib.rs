//! Incremental lexical and syntax analysis.
//!
//! The crate publishes an AST and token metadata, and nothing else:
//!
//! * [`Text`] — the immutable document and its edit ancestry;
//! * [`Tokens`] — the token stream plus one metadata record per token;
//! * [`AstTree`] — the AST structure;
//! * [`AstMeta`] — spans, token identity, values, wrappers, rule names, and
//!   diagnostics;
//! * [`PipelineStats`] — work counters per revision.
//!
//! Each layer consumes the previous layer's delta: the lexer relexes the edit
//! window, the parser resumes from a checkpoint and splices untouched subtrees,
//! and publication is a patch. [`LexerComponent`] and [`ParserComponent`] are
//! ordinary incremental components; their incremental algorithms are internal.
//!
//! # Typed trees
//!
//! [`AstTree`] is the *raw* parse: rule nodes, token leaves, and recovery nodes.
//! A host that wants a typed AST declares it with `#[tree_view]` and maps its
//! grammar onto it: a production names the variant it builds
//! (`@tree(Term::Lam)`), its fields name the RHS positions that fill them
//! (`body: Term`), and a rule that adds no meaning is a wrapper (`@wrap(inner)`).
//! [`Grammar::map_tree`] resolves that against the tree's own description and
//! reports every mismatch; [`ParserComponent::with_tree`] then publishes the tree
//! from the same parse records, with the same node identities.
//!
//! A host that does not declare a tree reads the raw one instead: [`AstNodeRef`]
//! answers `rule_id`, `token`, `text`, `value`, and `semantic` (the node a
//! wrapper stands for), each reading exactly one unit.
//!
//! # Identity
//!
//! A token id names a token *occurrence*: the token that starts at a position
//! keeps its id when the edit re-spells it, and only its payload changes. An AST
//! node id is the extent it covers, so a node keeps its id exactly while its
//! boundary tokens keep theirs; a node this revision rebuilt still keeps that
//! id, and its [`AstMeta`] record is rewritten in place.
//!
//! That contract is load-bearing above this crate: a component invocation is
//! identified by its arguments, so an argument that changed identity without
//! changing meaning re-creates every entity emitted beneath it. Keeping the
//! token and node ids stable is what makes a re-spelling a payload change, and
//! only a structural edit re-keys the constructs whose extent changed.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod components;
pub mod diagnostic;
pub mod lexer;
pub mod mapping;
pub mod parser;
pub mod text;
pub mod views;

pub use components::{AstConsumer, LexerComponent, ParserComponent, TextComponent};
pub use diagnostic::{Diagnostic, Severity};
pub use lexer::{
    Action, GuardExpr, LexError, LexOutput, LexState, LexValue, Lexer, LexerOptions, LexerSpec,
    ModeId, Rule, RuleKind, Token, TokenFlags, TokenId, TokenKind, TokenKindId, TokenStream,
};
pub use mapping::{Feed, RuleBuild, TreeMapping, TreeMappingError};
pub use parser::{
    AstNodeRecord, AstParser, AstState, AstTable, AstUpdate, AstValue, BuildStats, ErrorCategoryId,
    Grammar, GrammarError, RuleId, TreeTarget, ast_node_id, ast_token_id, literal_value, rule_id,
    significant_tokens,
};
pub use text::{TextAnchor, TextDocument, TextEdit, TextRange, TextSnapshot, TextVersion};
pub use views::{
    AstMeta, DiagnosticId, PipelineStats, SourceFile, StatId, StatsSnapshot, Text, TokenDelta,
    TokenPayload, Tokens,
    ast_tree::{AstNode, AstNodeRef, AstTree},
};

/// Lower-case alias for the LR table submodule.
pub use parser::lr as table;
