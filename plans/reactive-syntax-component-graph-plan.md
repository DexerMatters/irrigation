# Reactive Syntax Components: Lexer and Parser as view actors

**Status:** historical. The component graph it describes still exists in shape
(`Text` → `Tokens` → `AstTree` + `AstMeta`), but the CST-era internals
(`reactive_components.rs`, the `AstComponent`/`AstMap` layer) were removed by
[`ast-incremental-granularity-plan.md`](ast-incremental-granularity-plan.md);
compatibility notes and verification evidence are in
[`reactive-syntax-component-graph-status.md`](reactive-syntax-component-graph-status.md).

**Correction to the earlier pipeline sketch:** the framework is a graph of
components around Views, not a fixed pipeline with stages. A general component
is still just a component. What makes it “general” is that it supplies its own
`initialize`/`update` algorithm instead of generated component code. There is no
`GeneralStage`, no stage role, and no required component ordering. This plan is
written around that actor/view model.

**Related documents**

- [`incremental-parser-lexer-plan.md`](incremental-parser-lexer-plan.md)
- [`incremental-parser-lexer-implementation-status.md`](incremental-parser-lexer-implementation-status.md)
- [`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md)

---

## 1. Decision

Make the lexer and parser ordinary **component actors** with declared input and
output Views:

```text
SourceFile  (source View, host-owned)
    ▲ read
LexerComponent   (GeneralComponent, custom increment)
    │ owns
Tokens View
    ▲ read
ParserComponent  (GeneralComponent, custom increment)
    │ owns
Syntax View
    ▲ read
ordinary application components
```

This is not a pipeline description. It is a dependency description:

- a component reads any View units it chooses;
- a component owns/publishes output Views;
- the runtime resolves a unit to its producer and demands that producer;
- invalidation propagates along recorded unit reads;
- a component’s internal algorithm is self-specified;
- components can be demanded in any order, concurrently where independent, and
  no component is “stage 1” or “stage 2” structurally.

The lexer does not “call” the parser. The parser reads token units. The parser
does not own a lexer in this mode. The dependency edge exists because the parser
read token units, not because a pipeline says it comes second.

---

## 2. Why the current adapters do not form this graph

Current facts:

- `Parser` and `Lexer` are two independent `GeneralComponent` roots over
  `SourceFile`.
- Only one general root can be installed:
  [`runtime.rs:149`](../crates/reactive-runtime/src/runtime.rs#L149) rejects a
  second general root.
- `Parser` owns an internal lexer and performs a full internal lex in
  `parse_incremental_inner`:
  [`runtime.rs:559`](../crates/reactive-syntax/src/parser/runtime.rs#L559).
- `Parser::VIEWS` currently lists `SourceFile` and `Syntax`, but `Syntax` is an
  output view, not an input.
- `SourceFile` is a scalar `String`.
- `Syntax` is declared `#[view(source)]` although the parser defines it; it
  should be a derived/output view.
- Lexer error tokens have kind `$error` (id 0), but parser terminal lowering
  skips kind id 0; an error token can therefore be classified as EOF rather
  than as a declared error token.

The current adapter tests pass independently, but no test proves a
`Tokens -> Parser` dependency.

---

## 3. Component/view semantics

### 3.1 A component is an actor around Views

A component:

- declares the Views it reads (`INPUT_VIEWS`, currently `VIEWS`);
- declares the Views it owns/publishes (`OUTPUT_VIEWS`);
- retains `State`;
- reads units through `GeneralCx`, recording dependencies;
- defines output units atomically with state and output;
- is invalidated when any observed unit version changes;
- participates in snapshots, revisions, cancellation, and cycle detection.

“Actor” here means a demandable unit provider, not a thread or mailbox. It is
the same execution model as ordinary generated components, with a custom update
algorithm.

### 3.2 General components are not special roles

There is no root/stage distinction in the component model itself. A general
component is simply a component whose `initialize` and `update` are supplied by
the user rather than generated from view expressions.

The runtime still needs an entry demand. That entry is a **root demand**, not a
property of a general component. Options:

- an ordinary generated root component reads `Syntax` units;
- a compatibility `install_general` root remains for the current monolithic
  parser adapter;
- future runtimes may allow multiple roots/subscriptions, but that is outside
  this plan.

A general component can be installed as a producer without being the root.

### 3.3 Distinct concepts

Use precise terms:

| Term | Meaning |
|---|---|
| View | typed namespace of logical units |
| source View | populated by host edits |
| output/derived View | populated by a component producer |
| input View | View units read by a component |
| producer | component that owns an output View and defines its units |
| root demand | the entry computation requested by the host |
| component graph | dependency graph induced by unit reads and providers |
| pipeline | **not a framework concept**; avoid it for scheduling |

---

## 4. Runtime changes for producer components

### 4.1 Trait extension

Keep `GeneralComponent` as the only custom algorithm interface. Add output
declarations:

```rust
pub trait GeneralComponent: Send + Sync + 'static {
    type State: Clone + Send + Sync + 'static;
    type Output: Clone + PartialEq + Send + Sync + 'static;

    const ID: ComponentId;
    const NAME: &'static str;

    /// Views read by this component.
    const VIEWS: &'static [&'static ViewDescriptor] = &[];

    /// Views owned by this component.
    ///
    /// Empty for a compatibility root that publishes no View units.
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[];

    fn initialize(&self, cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)>;
    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        changes: &ChangeBatch,
    ) -> Result<(Self::State, Self::Output)>;
}
```

No new `GeneralStage` trait is introduced. A producer component is a
`GeneralComponent` with `OUTPUT_VIEWS`.

### 4.2 Installation as a component producer

```rust
pub fn install_general_component<T: GeneralComponent>(
    &self,
    component: T,
) -> Result<ComponentInstall>;
```

This:

- registers `VIEWS` and `OUTPUT_VIEWS`;
- requires every output View to be `ViewKind::Derived`;
- enforces one producer per output View;
- allocates a normal component node with `general = Some(index)`;
- records `producers: FxHashMap<ViewId, NodeId>`;
- does **not** set `general_root` and does not make the component a root.

Compatibility:

```rust
pub fn install_general<T: GeneralComponent>(&self, component: T) -> Result<()>;
```

continues to install a root component and keeps the current monolithic
`Parser`/`Lexer` paths alive.

### 4.3 Unit provider resolution

When a component reads a unit:

```text
cx.read(unit)
  -> ensure_unit(unit)
       if unit exists: read
       else if producers[unit.view] exists:
            demand(producer node)
            retry unit read
       else: fail undefined unit
```

`demand(producer)` uses the existing `eval`/`run_general` path. It runs outside
the state lock, with a frame and cancellation token. A producer is evaluated
when its output is demanded, not because it was installed.

The producer node records ownership of every unit it defines; `provider_of`
returns it for diagnostics and routing. A second component defining the same
View unit is a hard error.

### 4.4 Dependency recording

When a consumer reads a token unit:

```text
consumer trace: Tokens.<unit> -> lexer component node
```

When the parser component reads `Tokens.delta`, it records a dependency on the
lexer’s token delta unit. When the lexer reads `SourceFile.document`, it records
a dependency on the source View unit.

When the lexer commits changed token units, the normal invalidation machinery
notifies the parser. No new “pipeline event” or ordering API is needed.

### 4.5 Atomicity, cutoff, cancellation

- State, output value, definitions, dependency trace, and scopes commit
  together.
- Unit equality controls cutoff; an equal token delta does not invalidate the
  parser.
- A parser sees either the old token View or the fully committed new one.
- Cancellation or failure publishes nothing; the old state and units remain
  valid.
- A consumer with a stale delta resets from the full View.

### 4.6 Cycles and concurrency

- If a producer reads its own output unit, normal cycle detection fires.
- Parser and lexer are independent graph nodes; the parser is not “downstream”
  by construction, only by unit dependency.
- Components that read disjoint Views can be demanded/updated concurrently.
- Workers may schedule any ready producer; tests must be invariant across worker
  counts.

---

## 5. Views

### 5.1 SourceFile

Current compatibility form:

```rust
#[view(source)]
pub struct SourceFile {
    pub text: String,
}
```

For the actor graph, prefer a source View that carries the immutable document
and its edit ancestry:

```rust
#[view(source)]
pub struct SourceFile {
    pub document: TextDocument,
}
```

`TextDocument` must become `PartialEq + Eq + ViewField`-compatible (or be wrapped
in a view field type). `TextDocument` already contains `Arc<TextSnapshot>` with
parent/edit history, so a source edit wakes the lexer with:

- the new snapshot;
- `edits_since(previous_version)`.

This is the source-to-lexer text delta. No extra “text stage” is required.
If a system wants a separate text-delta View, that is an optional ordinary
component, not a pipeline requirement.

### 5.2 Tokens (lexer output View)

```rust
#[view(derived)]
pub struct Tokens {
    pub text: TextDocument,
    pub order: List<TokenId>,
    pub tokens: Map<TokenId, TokenRecord>,
    pub errors: Map<TokenId, TokenErrorRecord>,
    pub delta: TokenDelta,
}
```

`TokenRecord` carries everything the parser needs without reading source text:

```rust
pub struct TokenRecord {
    pub id: TokenId,
    pub kind: TokenKindId,
    pub text: Arc<str>,
    pub range: TextRange,
    pub start_state: LexConfig,
    pub end_state: LexConfig,
    pub lookahead_end: TextAnchor,
    pub flags: TokenFlags,
    pub error_category: Option<ErrorCategoryId>,
}
```

`TokenDelta` uses the framework’s collection delta algebra:

```rust
pub struct TokenDelta {
    pub from: TextVersion,
    pub to: TextVersion,
    pub order: ListDelta<TokenId>,
    pub tokens: MapDelta<TokenId, TokenRecord>,
    pub errors: MapDelta<TokenId, TokenErrorRecord>,
    pub relexed: TextRange,
    pub trivia_only: bool,
}
```

`ListDelta`/`MapDelta`/`SetDelta` are currently exported by `reactive-store`;
either re-export them through `reactive` or add a direct dependency.

### 5.3 Syntax (parser output View)

Make the existing `Syntax` View derived. Keep the current per-node/per-token
unit maps and add error/diagnostic/delta units:

```rust
#[view(derived)]
pub struct Syntax {
    pub root: Option<NodeId>,
    pub nodes: Map<NodeId, NodeRecord>,
    pub spans: Map<NodeId, TextRange>,
    pub children: Map<NodeId, Vec<NodeId>>,
    pub parents: Map<NodeId, Option<NodeId>>,
    pub token_children: Map<NodeId, Vec<TokenId>>,
    pub token_text: Map<TokenId, String>,
    pub fields: Map<(NodeId, usize), String>,

    pub error_category: Map<NodeId, ErrorCategoryId>,
    pub error_policy: Map<NodeId, ErrorPolicy>,
    pub token_error_category: Map<TokenId, ErrorCategoryId>,
    pub diagnostics: Map<DiagnosticId, DiagnosticRecord>,
    pub delta: SyntaxDelta,
}
```

`SyntaxDelta` is a compact patch using `MapDelta`/`SetDelta`:

```rust
pub struct SyntaxDelta {
    pub from: TextVersion,
    pub to: TextVersion,
    pub nodes: MapDelta<NodeId, NodeRecord>,
    pub spans: MapDelta<NodeId, TextRange>,
    pub children: MapDelta<NodeId, Vec<NodeId>>,
    pub parents: MapDelta<NodeId, Option<NodeId>>,
    pub errors: MapDelta<NodeId, ErrorCategoryId>,
    pub diagnostics: SetDelta<DiagnosticId>,
    pub changed_ranges: Vec<TextRange>,
    pub reused: Vec<NodeId>,
}
```

`DiagnosticId` is stable, derived from error category, anchor range, and recovery
origin; not from parse order.

---

## 6. Lexer component

### 6.1 Inputs/outputs

```rust
impl GeneralComponent for LexerComponent {
    type State = LexState;
    type Output = LexerSummary; // optional; View units are authoritative

    const VIEWS: &[&ViewDescriptor] =
        &[<SourceFile as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &[&ViewDescriptor] =
        &[<Tokens as ViewSchema>::DESCRIPTOR];

    ...
}
```

### 6.2 Initialize

1. read `SourceFile.document`;
2. lex from scratch;
3. publish `Tokens.text`, `order`, token records, error records, and an empty
   delta.

### 6.3 Update

1. read new `SourceFile.document`;
2. compute `edits_since(state.text.version)`; if absent, full reset;
3. call the existing incremental lexer (`lex_incremental`) with retained
   `LexState`;
4. compare old/new token order and records;
5. publish changed order/token/error units and `TokenDelta`;
6. set `trivia_only` when perfect-hash significant tokens are unchanged;
7. retain the new token/text state.

The lexer’s self-specified increment is exactly the current relex-window and
stable-token-ID algorithm; only its output channel changes from a `LexOutput`
root value to `Tokens` View units.

### 6.4 Error tokens

Lexical errors remain explicit tokens. `TokenRecord.error_category` is set for
`TokenFlags::ERROR`. The parser looks up the grammar-declared error token
terminal; it must not fall back to EOF.

---

## 7. Parser component

### 7.1 Inputs/outputs

```rust
impl GeneralComponent for ParserComponent {
    type State = ParserState;
    type Output = SyntaxSummary; // optional

    const VIEWS: &[&ViewDescriptor] = &[
        <Tokens as ViewSchema>::DESCRIPTOR,
    ];
    const OUTPUT_VIEWS: &[&ViewDescriptor] = &[
        <Syntax as ViewSchema>::DESCRIPTOR,
    ];

    ...
}
```

The parser component has no `Lexer` field and never reads `SourceFile` in this
mode. It receives the text snapshot through `Tokens.text`.

### 7.2 Initialize

1. read `Tokens.order` and token records;
2. construct a `TokenStream`;
3. parse from scratch;
4. publish syntax units and empty delta.

### 7.3 Update

1. read `Tokens.delta`;
2. if `delta.from != state.text.version`, read the full token View and reset;
3. apply order/token/error deltas to the retained token stream;
4. if `trivia_only`, update trivia attachment/ranges/diagnostics without
   re-running the grammar;
5. otherwise call `parse_incremental_with_state` with the new token stream and
   `Tokens.text`;
6. publish changed node/child/field/error/diagnostic units and `SyntaxDelta`;
7. retract units for removed nodes/tokens.

The parser’s internal increment remains GSS/stack checkpoints, visible/hidden
suffix reuse, error-region invalidation, and repair search. It no longer owns
the lexer increment.

### 7.4 Downstream consumers

Ordinary generated components can read `Syntax` units:

```rust
#[component]
pub fn outline(tree: &Syntax) -> Outline {
    let root = tree.root().expect("tree");
    ...
}
```

The runtime resolves `Syntax.root` to the parser component producer. Unchanged
node units do not invalidate the consumer. A whole-tree consumer can instead
read `Syntax.delta` and apply the collection deltas.

---

## 8. Error token and AST error specification

### 8.1 Proposed grammar syntax

Lexer:

```text
lexer {
    initial root;

    error unexpected = . @message("unexpected character");

    state root {
        token ident = /[A-Za-z_][A-Za-z0-9_]*/;
        token number = /[0-9]+/;
        token "{" ; token "}" ; token ":" ; token "," ;
    }
}
```

Parser:

```text
parser {
    recovery {
        mode = balanced;
        error_token = unexpected;
    }

    errors {
        token unexpected ignore;
        node Error report;
    }

    Document = Value;
    ...
}
```

### 8.2 Semantics

- `error <name> = ...;` creates a named lexical error token kind.
- `recovery.error_token` declares the parser-visible error terminal.
- `errors { token ... }` sets lexical error policy.
- `errors { node ... }` sets CST error-node policy.
- `report`: publish diagnostics and charge normal recovery cost.
- `ignore`: keep explicit error tokens/nodes with an `IGNORED` flag, suppress
  diagnostics, and zero the category’s recovery cost.

“Ignore” is not deletion. Byte coverage, stable identity, and incremental
diagnosis remain valid; only publication/cost are shortcut.

### 8.3 AST/token metadata

```rust
pub enum ErrorPolicy { Report, Ignore }

bitflags NodeFlags {
    FRAGILE,
    ERROR,
    IGNORED,
    MISSING_REGION,
}

pub struct NodeMeta {
    pub start_state: usize,
    pub end_state: usize,
    pub first_token: Option<TokenId>,
    pub last_token: Option<TokenId>,
    pub fragile: bool,
    pub right_context: u64,
    pub flags: NodeFlags,
    pub error_category: Option<ErrorCategoryId>,
}
```

- `NodeKind::Error` remains the structural kind.
- `NodeFlags`/`ErrorCategoryId` distinguish fragments, ignored errors, and
  recovery origins.
- Tokens retain `TokenFlags::ERROR` plus `error_category`.
- Grammar compilation assigns stable `ErrorCategoryId`s and validates
  `error_token` references.

### 8.4 Incremental diagnosis rules

1. Error categories and `DiagnosticId`s are grammar-derived and stable.
2. Error nodes and tokens inside an error region are fragile; never reuse them
   as whole subtrees.
3. An edit intersecting an error region invalidates from the error-start
   boundary to the next stable token boundary.
4. Removed error nodes retract their diagnostics in the same atomic commit.
5. Ignored categories remain in the tree/token stream with `IGNORED`; their
   diagnostics are never published and their cost is zero.
6. Adjacent same-category errors with touching ranges are merged before
   publication.
7. `TokenDelta` and `SyntaxDelta` carry error/diagnostic additions, changes, and
   removals.

---

## 9. Dependency and ordering rules

The graph is defined by unit reads:

```text
SourceFile.document
    ↑ read by LexerComponent
Tokens.delta / Tokens.order / Tokens.tokens
    ↑ read by ParserComponent
Syntax.root / Syntax.nodes / ...
    ↑ read by application components
```

There is no required syntax for ordering. The runtime may:

- demand lexer before parser when the parser first requests a token unit;
- run independent consumers concurrently;
- reuse unchanged units;
- detect cycles if a consumer reads its own producer output.

### 9.1 Data versions and stale deltas

Every delta carries `from`/`to` source versions. A consumer:

- applies only when its retained version equals `from`;
- otherwise resets from the full View;
- never attempts to merge a partial delta across a version gap.

The producer’s state and delta commit are atomic, so a consumer cannot observe a
new delta with old units or vice versa.

### 9.2 No-change behavior

- Source text equal: no text units change.
- Token stream equal: no token units change, parser not invalidated.
- Trivia-only change: token delta is marked, parser updates extras/ranges but
  does not re-run grammar.
- Syntax units equal: downstream consumers not invalidated.

---

## 10. Compatibility and migration

1. Keep existing monolithic `Parser`/`Lexer` roots working.
2. Add producer installation; do not reinterpret `install_general`.
3. Make `Syntax` derived and separate output views from input views.
4. Keep `SyntaxRef` and current map accessors stable; extend with error fields.
5. `SourceFile` may remain `String` for compatibility; add a `TextDocument`
   source form or a compatibility diff path in the lexer component.
6. No framework-wide multiple-root change is required; a root ordinary component
   can consume the producer Views.

---

## 11. Implementation phases

### Phase A — component producers

- add `OUTPUT_VIEWS`;
- add `install_general_component`;
- add provider-by-view demand and one-producer enforcement;
- extend definition capabilities with allowed output Views;
- add a toy producer/consumer test.

### Phase B — token View

- add `Tokens`, `TokenRecord`, `TokenDelta`;
- add delta type re-exports/dependency;
- add lexer component output publication and cutoff.

### Phase C — parser component

- split parser core from lexer ownership;
- add parser input `Tokens` only;
- apply token deltas and preserve checkpoints/suffix reuse;
- add trivia-only path.

### Phase D — syntax View and errors

- make `Syntax` derived;
- add error/diagnostic units and `SyntaxDelta`;
- implement grammar error declarations and error terminal classification;
- add error-region incremental behavior.

### Phase E — downstream and verification

- ordinary root consuming Syntax;
- changed-unit tests;
- cancellation, worker determinism, benchmarks, fuzz.

---

## 12. Verification gates

| Gate | Purpose |
|---|---|
| `general_component_publishes_view` | producer component defines units |
| `general_component_is_demanded_by_view` | unit read demands producer |
| `one_producer_per_view` | provider uniqueness |
| `producer_commit_is_atomic` | failure/cancel publishes nothing |
| `source_document_delta_is_exact` | edits_since source edge |
| `lexer_token_view_matches_full_lex` | token units equal full lex |
| `token_delta_applies_to_old_stream` | token delta sufficiency |
| `parser_component_reads_tokens_not_source` | no internal lexer |
| `parser_update_from_token_delta` | parser reacts to Tokens |
| `trivia_only_edit_skips_reparse` | token-to-tree shortcut |
| `syntax_units_match_tree` | per-node units equal CST |
| `syntax_delta_applies_to_old_tree` | tree delta sufficiency |
| `downstream_updates_only_changed_syntax_units` | unit cutoff reaches consumers |
| `error_token_is_declared_terminal_not_eof` | error classification |
| `error_policy_ignore_keeps_tree_flags` | ignored errors represented |
| `error_policy_report_publishes_diagnostics` | reported errors publish |
| `error_region_edit_is_local` | no error subtree reuse |
| `producer_graph_deterministic_across_workers` | scheduling determinism |
| `stale_delta_forces_full_reset` | version guards |
| `chain_benchmark_*` | performance work |

Current baseline that must remain green:

- 96 `reactive-syntax` tests;
- randomized lexer/parser oracles;
- nullable/Earley oracle;
- adapter tests;
- criterion parser/lexer benchmarks.

---

## 13. Performance expectations

- **Source edge:** O(edit length), no parse.
- **Lexer producer:** O(relexed window), stable token-ID reuse, token delta
  proportional to changed tokens.
- **Parser producer:** O(changed GSS region + error region), no internal lex.
- **Downstream:** only units read and changed; no full-tree re-publication when
  one node changes.
- **Trivia-only edit:** no grammar reparse.
- **No-change edit:** no producer invalidation.

The monolithic parser remains a benchmark baseline; the component graph should
win for local edits and fine-grained consumers, not necessarily for full-file
reparse.

---

## 14. Open questions

1. Should `GeneralComponent::Output` remain a summary value, or should custom
   components return only state and publish Views? Current compatibility favors
   keeping `Output`.
2. Should `SourceFile` carry `TextDocument` directly, or should a source
   component publish a separate `Text` View? The actor model permits either;
   `TextDocument` on the source View is simpler and avoids an artificial text
   stage.
3. Exact grammar surface for `errors { token ...; node ...; }`.
4. Whether `SyntaxDelta` should be a scalar field or derivable from changed
   per-node units. The plan recommends both: units for fine-grained observers,
   delta for whole-tree consumers.
5. Whether downstream components may install before producer components; the
   provider map should make this irrelevant, but install-time diagnostics must
   be deterministic.

---

## 15. Acceptance

The design is accepted when:

- the lexer and parser are ordinary `GeneralComponent` actors with input and
  output Views;
- the parser reads token units and never constructs a lexer;
- the runtime demands producers by View unit ownership;
- source document changes produce text deltas that reach the lexer through
  normal invalidation;
- token deltas reach the parser through normal invalidation;
- syntax units/deltas reach ordinary downstream components through normal
  dependency tracking;
- error tokens have a declared parser terminal and are never EOF;
- error categories/policies are explicit in the grammar and stable in the CST;
- ignored errors retain explicit token/tree representation without diagnostics;
- cancellation, atomic commits, cutoff, stale-delta resets, and worker
  determinism pass;
- the existing 96-test baseline remains green.
