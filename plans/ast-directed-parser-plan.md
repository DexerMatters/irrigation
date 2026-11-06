# AST-directed parser and maximal-granularity syntax views

**Status:** historical. Superseded by
[`ast-incremental-granularity-plan.md`](ast-incremental-granularity-plan.md): the
`AstReducer`/`AstChild`/`AstMapComponent`/`AstMap` layer described here no longer
exists — the parser now builds the tree directly and publishes `AstTree` plus
`AstMeta`. The grammar still carries `@ast` fit metadata; reductions build ASTs
during deterministic and GLR parses; GLR AST recovery covers token skip, missing-token insertion, hypothetical reductions, and stack-pop recovery; AST candidates are ranked through `AstReducer::rank`; AST map/meta/tree units are diff-published and unchanged root identity is preserved across updates. Batch parser entry points are retained as an explicitly documented compatibility baseline.

**Companion audit:** [`reactive-syntax-granularity-audit.md`](reactive-syntax-granularity-audit.md)

---

## 1. Decision

The parser should be parameterized by an AST schema and build AST nodes during
reductions. It should not publish a CST and then run a separate CST-to-AST
refactor.

The LR table remains grammar-driven. The AST layer is a semantic reduction
layer:

```text
SourceFile
  -> TextComponent
  -> LexerComponent
  -> Tokens
  -> ParserComponent<A>
       : LR/GLR reduction
       : AstReducer<A> constructs AST nodes and fits children
  -> AstTree<A>
  + AstMeta (tokens, spans, diagnostics, error sets)
```

The parser may keep internal structural parse nodes for GLR, recovery,
checkpoints, and subtree reuse. Those are **internal parse metadata**, not a
published CST. The published tree is the AST.

---

## 2. Grammar and AST fit

### 2.1 AST schema

An AST schema describes node variants and their child fields:

```text
ast Expr {
    Binary {
        lhs: Expr,
        op: Token,
        rhs: Expr,
    }
    Number(i64)
    Error { message: String }
}
```

Types are either:

- another AST type;
- a token reference;
- a scalar semantic value;
- `Option<T>` / `Vec<T>` for optional and repeated children;
- an explicit error/recovery wrapper.

### 2.2 Grammar annotations

Each production alternative declares the action and where RHS children fit:

```text
parser {
    Expr = Expr "+" Expr
        @ast(Binary {
            lhs: 0,
            op: 1,
            rhs: 2,
        });

    Expr = NUMBER
        @ast(Number {
            value: 0,
        });
}
```

For lists and optionals:

```text
Args = "(" Expr,* ")"
    @ast(Args {
        values: 1,
    });
```

The mapping is positional against the production RHS. The grammar compiler
validates:

- RHS index bounds;
- duplicate field mappings;
- required child fields;
- token/scalar/child field compatibility;
- AST variant existence (when the AST is generated or known at build time);
- wrapper compatibility (`Box`, `Vec`, `Option`).

### 2.3 Reducer interface

```rust
pub trait AstReducer {
    type Node;

    fn reduce(
        &mut self,
        production: ProductionId,
        action: AstActionId,
        span: TextRange,
        children: &[AstChild<'_>],
    ) -> Self::Node;
}

pub struct Parser<A: AstReducer> {
    grammar: Arc<Grammar>,
    ast: A,
    options: ParserOptions,
}
```

The parser table remains `Grammar`; the AST reducer is the parameter. This
avoids making the whole LR machinery generic while still making semantic
construction AST-directed.

### 2.4 Generated versus dynamic AST

Two supported modes:

1. **Generated typed AST**
   - Build-time proc macro generates AST enums, `#[tree_view]`, and the reducer
     implementation from the grammar.
   - Best type safety and typed `TreeRef` consumers.

2. **Generic AST**
   - A generic node type:
     ```rust
     enum AstNode {
         Nonterminal { kind: u64, children: Vec<AstNode> },
         Error { category: u64, children: Vec<AstNode> },
     }
     ```
   - `@ast` actions store kind IDs and fit children at runtime.
   - Required for fully runtime-defined grammars.

The plan recommends generated typed AST for known grammars, generic AST as the
fallback.

---

## 3. AST View and metadata

### 3.1 AST tree

The AST should be a framework tree View:

```rust
#[tree_view(root = Expr, view = AstTree)]
pub mod ast {
    pub enum Expr { /* generated variants */ }
}
```

Benefits:

- per-node tag units;
- per-leaf-field units;
- per-child-slot units;
- stable node identity;
- subtree moves without identity churn;
- typed `TreeRef` consumers;
- no post-parse refactor.

### 3.2 AstMeta

ASTs usually drop source detail. Preserve it in auxiliary framework-shaped
Views:

```rust
#[view(derived)]
pub struct AstMeta {
    pub spans: Map<NodeId, (usize, usize)>,
    pub node_tokens: Map<(NodeId, u16), TokenId>,
    pub diagnostics: Map<DiagnosticId, Diagnostic>,
    pub error_nodes: Set<NodeId>,
    pub missing_tokens: Set<TokenId>,
}
```

The AST is semantic; `AstMeta` is source correlation and diagnostics.

### 3.3 Error representation

Choose one consistent representation:

- an `Error` AST variant;
- `Option`/`Result`-like wrappers around AST fields;
- an error node set in `AstMeta`.

The grammar’s `errors` policies must apply to AST construction, not only CST
node flags. `ignore` still builds a representable AST node/set membership but
publishes no diagnostic and charges no recovery cost.

---

## 4. Internal parse structure is not a published CST

The parser still needs:

- LR/GLR stacks and packed GSS nodes;
- checkpoints;
- right-context and reuse certificates;
- recovery candidates;
- token spans and trivia attachments;
- diagnostics origins.

Those structures may be retained in `ParserState` and may be as CST-like as
needed. They are not exposed as a semantic tree.

The AST reducer is called during reduction. For GLR:

- build candidate AST values per branch;
- discard losing branches;
- publish only the selected root;
- retain AST node IDs for reuse where semantic equality and structural
  identity permit it.

No second pass walks a published CST to create AST nodes.

---

## 5. Incremental semantics

- AST node IDs are stable across edits when the corresponding semantic node is
  reused.
- Reuse decisions use both parse-level checkpoints and AST semantic equality.
- A trivia/comment edit updates `AstMeta` but may leave the AST tree unchanged.
- A token record edit that does not change AST structure updates `AstMeta` and
  diagnostics only.
- Error nodes are fragile; touched error regions rebuild.
- AST tree deltas are per-node/per-field, not a single scalar blob.

---

## 6. Granularity contract

A View used between components must expose the smallest unit a consumer can
reasonably observe. A monolithic `Vec`/record/delta field is a compaction
barrier unless it is explicitly a convenience projection.

Rules:

1. Prefer per-entity Map entries over `Map<Key, Vec<Entity>>`.
2. Prefer per-slot child units over one `Vec<ChildId>`.
3. Prefer one token field per unit over `Arc<Token>` in one Map value.
4. Prefer per-diagnostic Map entries over `Vec<Diagnostic>`.
5. Do not make a whole-tree delta the only observable delta.
6. Keep coarse convenience views, but never on the primary dependency path.
7. Deltas must be per-unit/field where a consumer can reuse prior state.

The concrete audit is in
[`reactive-syntax-granularity-audit.md`](reactive-syntax-granularity-audit.md).

---

## 7. Implementation phases

### Phase A — AST reduction hooks

- Add `AstActionId` and production AST mapping metadata.
- Add `AstReducer` and `AstChild`.
- Make parser reduction call the reducer.
- Keep CST structural metadata internal.

### Phase B — AST views

- Generate `#[tree_view]` AST from typed ASTs.
- Add generic tree output support for `GeneralComponent` if runtime ASTs are
  needed.
- Publish AST tree units and per-field deltas.

### Phase C — AstMeta

- Move spans, token attachments, diagnostics, and error sets to granular Views.
- Keep source and token views independent.

### Phase D — Granularity refactor

- Split `Tokens.tokens` into token field units.
- Replace `Tokens.diagnostics: Vec` with a diagnostic Map.
- Replace Vec-valued Syntax child maps with slot units or AST tree slots.
- Split scalar delta blobs where downstream consumers need partial observation.

### Phase E — Correctness fixes from audit

- Fix same-version `TextComponent` delta.
- Fix `@message` on non-error lexer rules.
- Add version-gap and stale-delta tests.
- Enforce component identity uniqueness and non-panicking definition errors
  where feasible.

### Phase F — Compatibility

- Keep a CST View for tooling if required.
- Keep batch parser APIs as a deterministic baseline.
- Add AST migration examples and consumer tests.

---

## 8. Verification

| Gate | Proves |
|---|---|
| `ast_reduction_builds_nodes_during_parse` | no CST-to-AST refactor |
| `grammar_ast_mapping_validates_indices` | fit mapping checked |
| `ast_tree_matches_expected_shape` | AST tree units correct |
| `ast_meta_tracks_tokens_and_spans` | source correlation preserved |
| `ast_update_is_incremental` | unchanged AST subtree reused |
| `trivia_edit_does_not_change_ast` | semantic cutoff |
| `error_ast_is_representable` | recovery AST |
| `ast_delta_is_per_field` | granular delta |
| `token_delta_order_round_trips` | token order edits |
| `view_granularity_*` | audit findings closed |

Current baseline to preserve:

- 109 `reactive-syntax` tests;
- 7 framework producer tests;
- 4 end-to-end tests;
- 10 runtime kernel tests;
- format check.

---

## 9. Open questions

1. Should AST generation be a proc macro over an `.ast` DSL, or a Rust macro
   that consumes an existing AST definition?
2. Are token leaves AST nodes, or only IDs in `AstMeta`?
3. Does AST semantic equality ignore spans/tokens by default?
4. Should the framework expose a generic `TreeOutputTxn` for runtime-defined
   trees, or should ASTs always be statically typed `#[tree_view]`?
5. What is the compatibility story for `SyntaxRef` consumers during migration?
