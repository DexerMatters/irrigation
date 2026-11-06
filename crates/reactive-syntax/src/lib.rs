//! Incremental lexical and syntax analysis.
//!
//! The crate publishes an AST and token metadata, and nothing else:
//!
//! * [`Text`] — the immutable document and its edit ancestry;
//! * [`Tokens`] — the token stream plus one metadata record per token;
//! * [`AstTree`] — the AST structure;
//! * [`AstMeta`] — spans, token identity, values, fields, and diagnostics;
//! * [`PipelineStats`] — work counters per revision.
//!
//! Each layer consumes the previous layer's delta: the lexer relexes the edit
//! window, the parser resumes from a checkpoint and splices untouched subtrees,
//! and publication is a patch. [`LexerComponent`] and [`ParserComponent`] are
//! ordinary reactive components; their incremental algorithms are internal.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod components;
pub mod diagnostic;
pub mod lexer;
pub mod parser;
pub mod text;
pub mod views;

pub use components::{AstConsumer, LexerComponent, ParserComponent, TextComponent};
pub use diagnostic::{Diagnostic, Severity};
pub use lexer::{
    Action, GuardExpr, LexError, LexOutput, LexState, LexValue, Lexer, LexerOptions, LexerSpec,
    ModeId, Rule, RuleKind, Token, TokenFlags, TokenId, TokenKind, TokenKindId, TokenStream,
};
pub use parser::{
    AstNodeRecord, AstParser, AstState, AstTable, AstUpdate, AstValue, BuildStats, ErrorCategoryId,
    Grammar, GrammarError, ast_rule_id, ast_token_id, literal_value, rule_id, significant_tokens,
};
pub use text::{TextAnchor, TextDocument, TextEdit, TextRange, TextSnapshot, TextVersion};
pub use views::{
    AstMeta, AstMetaField, DiagnosticId, PipelineStats, SourceFile, StatId, StatsSnapshot, Text,
    TokenDelta, TokenPayload, Tokens,
    ast_tree::{AstNode, AstNodeRef, AstTree},
};

/// Lower-case alias for the LR table submodule.
pub use parser::lr as table;
