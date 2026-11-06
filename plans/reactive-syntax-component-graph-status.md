# Reactive syntax component graph: implementation status

**Plan:** [`reactive-syntax-component-graph-plan.md`](reactive-syntax-component-graph-plan.md)

The plan is implemented. The old monolithic `Parser`/`Lexer`
`GeneralComponent` adapters have been removed. Source text, tokens, and syntax
are now produced by separate general components around Views.

## Implemented

### Framework

- `GeneralComponent::OUTPUT_VIEWS`.
- `Runtime::install_general_component`: installs a non-root producer and
  registers its output Views.
- Provider-by-View demand:
  - generated View reads (`unit_node_init`) demand a producer;
  - `GeneralCx` unit reads (`ensure_unit`) demand a producer;
  - producer evaluation uses the normal `eval`/`run_general` atomic path.
- One producer per derived output View; duplicate/source/no-output installs are
  rejected.
- Output definition restriction: a general component with `OUTPUT_VIEWS` may
  define units only in those Views.
- `OutputTxn::list_entry` and `OutputTxn::list_structure`.
- Ordinary generated root components can consume producer Views.
- Framework tests in `crates/reactive/tests/general_producer.rs`.

### Syntax components

- `SourceFile` carries a `TextDocument` (immutable snapshot plus edit
  ancestry).
- `TextComponent` reads `SourceFile` and owns the derived `Text` View with
  `Text.delta: TextDelta`.
- `LexerComponent` reads `Text`, not `SourceFile`.
- `Tokens` derived View:
  - `text: TextDocument`
  - `order: List<TokenId>`
  - `tokens: Map<TokenId, Arc<Token>>`
  - `diagnostics`
  - `delta: TokenDelta`
- `LexerComponent`: reads `Text`, owns `Tokens`, incremental relex via
  `LexState`, publishes changed token units and a token delta.
- `ParserComponent`: reads `Tokens` only, owns `Syntax`, has no lexer instance
  (`Parser::token_core`), applies token deltas for changed token records, and
  uses incremental token-stream parsing.
- `Syntax` is now a derived View.
- `SyntaxDelta` and `Syntax.delta`.
- `Syntax.error_policy` units for recovery nodes.
- `Syntax.diagnostics` units keyed by stable `DiagnosticId`.
- `SyntaxRef` accessors for diagnostics and error policy.
- Ordinary generated root component test over `Syntax.root`.
- Component tests in `crates/reactive-syntax/tests/reactive_components.rs` and
  `component_errors.rs`.

### Grammar and error policy

- Named lexical error rules:
  `error unexpected = /./ @message("...");` creates its own token kind with
  `TokenFlags::ERROR` and `lex.error` diagnostics.
- `Grammar::error_terminal()` and lexical error terminal classification.
  Lexical error tokens are no longer silently mapped to EOF.
- Parser grammar surface:
  - `recovery { error_token = "$error"; }`
  - `errors { token "$error" ignore; node Error report; }`
- `ErrorPolicy::{Report, Ignore}`.
- Ignored lexical/recovery diagnostics are filtered consistently from the
  parser output.
- Ignored error nodes remain in the CST and expose `ErrorPolicy::Ignore` in the
  `Syntax` View.
- Error tests in `crates/reactive-syntax/tests/component_errors.rs`.

## Removed legacy logic

- Removed `impl GeneralComponent for Parser` and `impl GeneralComponent for
  Lexer`.
- Removed the old monolithic adapter tests.
- Renamed `reactive_adapter` to `reactive_components`.
- Removed `SourceFile { text: String }` and the adapter-side `document_after`
  text diff.
- Parser components construct `Parser::token_core`, which has no lexer.
- `Parser` batch methods remain as the deterministic batch baseline, but the
  component path never invokes them for lexing.

## Intentional compatibility / remaining details

1. **Node categories.** Recovery nodes carry
   `ErrorCategoryId::RECOVERY`; there is currently one recovery category rather
   than a user-extensible table of node categories.
2. **Batch parser API.** `Parser::parse`/`parse_with_state` remain as the
   deterministic batch baseline. They allocate a lexer per call; the parser
   struct stores no lexer, and the component path uses token streams only.

## Verification currently run

```text
cargo test -p reactive-syntax
# 20 unit + 4 component-errors + 11 lexer + 1 nullable-oracle
# + 51 parser + 2 parser-oracle + 7 reactive-components + 1 ast-observer = 97 tests

cargo test -p reactive --test general_producer --test end_to_end
# 5 producer tests + 4 end-to-end tests

cargo test -p reactive-runtime --test kernel
# 10 tests
```

The component chain test proves:

- an ordinary generated root reads `Syntax.root`;
- the parser component is demanded by that unit read;
- the parser reads `Tokens`, not `SourceFile`;
- the lexer component is demanded through token units;
- source edits flow through to updated syntax units.
