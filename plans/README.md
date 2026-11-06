# Plans

The plan that describes the current state of the syntax pipeline is
[`ast-incremental-granularity-plan.md`](ast-incremental-granularity-plan.md).
Everything else here is either a framework-level plan or a record of how the
syntax layer got here.

## Current

| Plan | What it covers |
|---|---|
| [`ast-incremental-granularity-plan.md`](ast-incremental-granularity-plan.md) | The AST-only incremental pipeline: text → tokens → AST → metadata, locality, and the fixture that gates it. |
| [`kernel-change-notification-plan.md`](kernel-change-notification-plan.md) | A consumer reruns only when an observed unit changed. |
| [`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md) | The framework design: views, units, routing, suspension. |
| [`routing-generations-plan.md`](routing-generations-plan.md) | Routing generations and unreachable-subtree retirement. |
| [`heterogeneous-granularity-plan.md`](heterogeneous-granularity-plan.md) | The View granularity audit and fixes. |

## Superseded (kept as the record)

| Plan | Superseded by | Why |
|---|---|---|
| [`ast-only-parser-tree-metadata-plan.md`](ast-only-parser-tree-metadata-plan.md) | the granularity plan | It removed the CST from the published Views; the granularity plan made every layer incremental and replaced span-based identity with token-extent identity. |
| [`ast-directed-parser-plan.md`](ast-directed-parser-plan.md) | the granularity plan | The `AstReducer`/`AstMapComponent`/`AstMap` layer it built was removed; the engine builds the tree directly. |
| [`parser-adapter-after-diff-plan.md`](parser-adapter-after-diff-plan.md) | the granularity plan | The after-diff now lives inside `AstParser::update`; `parser::ast_diff` is gone. |
| [`framework-native-syntax-views-plan.md`](framework-native-syntax-views-plan.md) | `ast-only-parser-tree-metadata-plan.md` | `CstTree` stopped being a published View. |
| [`reactive-syntax-component-graph-plan.md`](reactive-syntax-component-graph-plan.md) | the granularity plan | The component graph survives in shape; its CST-era internals were removed. |
| [`incremental-parser-lexer-plan.md`](incremental-parser-lexer-plan.md) | the granularity plan | The CST lexer/parser it describes was replaced by the AST-only one. |

## Records

`implementation-status.md` and `evidence.md` map claims to tests.
`plan-conformance-audit.md` and `reactive-syntax-granularity-audit.md` are the
audits that motivated the rewrites. Status files belong to the plan they name.
