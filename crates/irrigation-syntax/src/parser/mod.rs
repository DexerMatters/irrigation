//! The AST parser: grammar, tables, the reducing engine, and retained state.

pub mod ast;
pub(crate) mod engine;
pub mod grammar;
pub mod lr;
pub(crate) mod state;

pub use ast::{
    AstNodeRecord, AstValue, BuildStats, ErrorCategoryId, RuleId, ast_node_id, ast_token_id,
    literal_value, mix, rule_id,
};
pub use engine::{AstParser, EngineOptions, ParseToken, SpliceEntry, SpliceSource};
pub use grammar::{
    Associativity, AstAction, Field, FieldId as GrammarFieldId, Grammar, GrammarError, Nonterminal,
    NonterminalId, PrecedenceLevel, Production, ProductionId, Symbol, Terminal, TerminalId,
    TreeTarget,
};
pub use lr::{Action, Item, State, Table, TableError, TableMode};
pub use state::{AstState, AstTable, AstUpdate, NodeMeta, significant_tokens};

/// Lower-case alias for the LR table submodule.
pub use lr as table;
