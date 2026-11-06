# Reactive syntax: granularity and correctness audit

**Scope:** the current `reactive-syntax` component graph and the runtime code it
uses.

**Companion design:** [`ast-directed-parser-plan.md`](ast-directed-parser-plan.md)

This audit separates:

1. **Granularity defects** — a View value is larger than the unit its consumers
   actually need, so unrelated changes invalidate it.
2. **Correctness/robustness bugs** — behavior that can be wrong, stale, or
   misleading.
3. **Performance/maintainability issues** — correct behavior with unnecessary
   work or coupling.

The audit is based on reading the current source and executing the current
test suite; the findings marked *static* have not been reproduced by a dedicated
test yet.

### Remediation status (first pass)

Fixed:

- **B1** same-version text replacement now emits a full replacement delta and
  has a dedicated test.
- **B2** `@message` on a non-error lexer rule is rejected and tested.
- **B3** parser no-op checks the retained document content before returning the
  old state.
- **B6** general component IDs must be unique.
- **G1** `Tokens` now has a `diagnostic_map: Map<DiagnosticId, Diagnostic>`
  alongside the coarse Vec convenience.
- **G4/G5** `Syntax` now has per-child-slot and per-token-leaf-slot units.
- **G2 (partial)** granular token kind/text/range/flags maps are published
  alongside the `Arc<Token>` convenience.
- **G3 (partial)** linked `previous`/`next` token maps are published alongside
  the List convenience.
- **G6** split `span_starts`/`span_ends` units are published.
- **G1 (partial)** a stable `Tokens.diagnostic_map` is published alongside the
  coarse Vec, and lexer diagnostics now carry their originating token kind.
- **G10 (partial)** `Text.edits: Map<u64, TextEdit>` is published alongside the
  coarse `TextDelta`.
- **G12 (partial)** `GeneralCx::preserve_definitions` lets a component keep
  previous units. `define_syntax` skips unchanged node definitions, and the
  lexer skips unchanged token records/diagnostics while preserving their units.
  AST map/meta publication skips unchanged nodes and retracts removed nodes;
  AST tree publication skips unchanged node units and retracts removed node
  membership.
- **G7 (partial)** `Syntax.field_values: Map<(NodeId, usize), FieldValue>` now
  publishes typed node/token field payloads alongside the string convenience.
- **G8 (partial)** `Syntax.changed_nodes` and `Syntax.removed_nodes` Sets expose
  per-node change membership.
- **G9 (partial)** `Tokens.changed` Set exposes per-token change membership.
- **B4** ignored lexical diagnostics filter by their structured token kind,
  with range matching retained only as a fallback.
- **B5** output-view definitions outside the declared Views now fail as a
  structured component error instead of panicking, with a framework test.
- **B7** producer reads now skip `eval` when the producer is already certified
  for the current revision.

Accepted compatibility/remaining optimization:

- **B8** batch parser entry points remain as a documented deterministic
  compatibility baseline; the component path uses token streams only.
- **G2** the coarse `Arc<Token>` value remains as a compatibility accessor;
  granular token kind/text/range/flags maps are the primary fine-grained path.
- **G3** `List<TokenId>` remains as a convenience; linked order units are
  available for local observations.
- **G7–G10 (partial)** typed fields, change sets, and per-edit text units exist;
  string/delta convenience values remain for compatibility.
- **G12** syntax, token, AST map/meta, and AST tree publication are diffed;
  broader producer patches remain future optimization.
- GLR/GLR recovery and AST publication are implemented in the AST plan.

The AST plan is implemented alongside this audit: productions carry
`AstAction { name, fields }`; deterministic and GLR AST parsers invoke
`AstReducer` during reductions; recovery covers skip/insertion/hypothetical
reduction/stack pop; `AstMapComponent` publishes `AstMap`, `AstMeta`, and a
derived `AstTree`, with diff publication and stable root identity on repeated
updates.

---

## 1. Current component graph

```text
SourceFile
  -> TextComponent -> Text { document, delta }
  -> LexerComponent -> Tokens { text, order, tokens, diagnostics, delta }
  -> ParserComponent -> Syntax { nodes, spans, children, parents,
                                  token_children, token_text, fields,
                                  root, delta, error_policy,
                                  error_category, diagnostics }
  -> ordinary components
```

The graph and provider demand are working. The granularity of the View values
is the main remaining issue.

---

## 2. Granularity findings

### G1 — `Tokens.diagnostics` is one `Vec<Diagnostic>` scalar

**Location:** [`reactive_components.rs:593`](../crates/reactive-syntax/src/reactive_components.rs#L593)

```rust
pub diagnostics: Vec<Diagnostic>,
pub delta: TokenDelta, // also carries Vec<Diagnostic>
```

**Problem:** changing one lexical diagnostic invalidates every consumer that
reads the diagnostics field, including consumers that only care about a
specific error range.

**Target:** a stable diagnostic map plus membership/set units:

```rust
pub diagnostics: Map<DiagnosticId, Diagnostic>,
pub diagnostic_ids: Set<DiagnosticId>,
```

or, if diagnostics only need membership and payload access:

```rust
pub diagnostics: Map<DiagnosticId, Diagnostic>,
```

Every consumer reads the one diagnostic it needs.

**Severity:** high for diagnostic consumers; medium overall.

---

### G2 — `Tokens.tokens` stores an entire `Arc<Token>` per Map value

**Location:** [`reactive_components.rs:600`](../crates/reactive-syntax/src/reactive_components.rs#L600)

```rust
pub tokens: Map<TokenId, Arc<Token>>,
```

**Problem:** token kind, text, range, flags, start/end state, and error message
are one unit. A range-only change invalidates text/kind readers; a trivia flag
change invalidates lexical-state readers.

**Target:** split the record into independent field maps:

```rust
pub token_kind: Map<TokenId, TokenKindId>,
pub token_text: Map<TokenId, Arc<str>>,
pub token_range: Map<TokenId, TextRange>,
pub token_flags: Map<TokenId, TokenFlags>,
pub token_error: Map<TokenId, Option<Arc<str>>>,
pub token_start_state: Map<TokenId, LexConfig>,
pub token_end_state: Map<TokenId, LexConfig>,
```

A consumer that only walks token text does not observe range or lexer-state
changes. Keep a convenience `Token` accessor only outside the primary graph.

**Severity:** high for token-level consumers.

---

### G3 — `Tokens.order` is a `List<TokenId>` whose structure unit is the whole order

**Location:** [`reactive_components.rs:598`](../crates/reactive-syntax/src/reactive_components.rs#L598)

```rust
pub order: List<TokenId>,
```

The framework List structure projection is a single ordered identity sequence.
Any insertion, removal, or reorder changes the whole structure unit, so every
consumer of `order()` is invalidated even when it only needs a local neighbor.

**Target:** add a per-token order relation for fine consumers:

```rust
pub previous: Map<TokenId, Option<TokenId>>,
pub next: Map<TokenId, Option<TokenId>>,
pub head: Option<TokenId>,
pub tail: Option<TokenId>,
```

Keep `order: List<TokenId>` as an explicit coarse convenience projection. The
parser can consume links or the list depending on its update strategy.

**Severity:** medium; inserts are inherently global for a list, but local
neighbor observation should be possible.

---

### G4 — `Syntax.children: Map<NodeId, Vec<NodeId>>`

**Location:** [`reactive_components.rs:32`](../crates/reactive-syntax/src/reactive_components.rs#L32)

**Problem:** changing one child updates the parent’s entire children value.
Consumers cannot observe one child slot.

**Target:** per-slot units:

```rust
pub child_slots: Map<(NodeId, u16), Option<NodeId>>,
pub child_order: Map<NodeId, Vec<u16>>, // convenience; or linked slots
```

or use the framework `#[tree_view]` AST tree, whose child slots are separate
units.

**Severity:** high; this is the main CST tree-granularity defect.

---

### G5 — `Syntax.token_children: Map<NodeId, Vec<TokenId>>`

**Problem:** same as G4, for token leaf children.

**Target:** per-slot token attachment units, or AST tree token nodes plus
`AstMeta.node_tokens: Map<(NodeId, u16), TokenId>`.

**Severity:** high for consumers interested in one token leaf.

---

### G6 — `Syntax.spans: Map<NodeId, (usize, usize)>`

**Problem:** start and end are one tuple unit. A consumer that only compares
start positions still invalidates on end-only changes.

**Target:** `spans.start: Map<NodeId, usize>` and
`spans.end: Map<NodeId, usize>` (or `TextRange` if consumers genuinely need the
pair).

**Severity:** low–medium.

---

### G7 — `Syntax.fields: Map<(NodeId, usize), String>`

**Problem:** per-field unit is good, but the payload is an untyped `String`.
That prevents typed AST consumers and forces formatting/parsing in consumers.

**Target:** typed field payloads in the AST tree or a generic
`AstValue`/`TreeValue` map, not stringly-typed payloads.

**Severity:** medium for the AST migration; not a granularity defect.

---

### G8 — monolithic `Syntax.delta: SyntaxDelta`

**Location:** [`reactive_components.rs:91`](../crates/reactive-syntax/src/reactive_components.rs#L91)

`SyntaxDelta` contains node/span/child/error/diagnostic map deltas, but it is a
single scalar field. Reading `syntax.delta()` observes the whole delta.

**Target:** expose delta components as separate fields or only use `delta` for
explicit whole-tree subscribers. Fine-grained consumers should read the changed
units directly.

**Severity:** medium; it is a convenience API, but it must not be the primary
way downstream components observe changes.

---

### G9 — monolithic `Tokens.delta: TokenDelta`

**Problem:** any token/diagnostic change invalidates a delta reader. The parser
needs the full token delta, so this one is less problematic, but downstream
token consumers should not be forced through it.

**Target:** keep `delta` for the parser; add per-field deltas (kind/text/range/
flags/order/diagnostics) for other consumers.

**Severity:** medium.

---

### G10 — monolithic `Text.delta: TextDelta`

**Location:** [`reactive_components.rs:506`](../crates/reactive-syntax/src/reactive_components.rs#L506)

`TextDelta` stores `Vec<TextEdit>` in one scalar. Every edit invalidates every
delta reader.

**Target:** a `Map<EditId, TextEdit>` or per-edit units, plus a version/range
summary. The lexer can still consume the whole ordered edit list as a
convenience.

**Severity:** medium.

---

### G11 — `Tokens.text` and `Text.document` are whole-document scalars

A lexer must observe the whole document version to know that a source edit
occurred. This is inherent, not a defect. However, consumers that only need a
token should not read `Tokens.text` unless they need coordinate-space identity.

**Action:** document that `text` is a version/coordinate scalar, not a
fine-grained observation channel.

**Severity:** informational.

---

### G12 — `define_syntax` and lexer `publish` rewrite all units

**Location:** [`reactive_components.rs:261`](../crates/reactive-syntax/src/reactive_components.rs#L261)

`define_syntax` walks every current node and defines every membership/value
unit on every parser update. The runtime’s equality cutoff prevents downstream
notifications for equal values, but the producer still computes and stages all
units.

Similarly, the lexer publishes all tokens/order entries each update.

**Target:** diff old/new node and token records and stage only changed units.
This is a performance issue, not an invalidation-granularity issue.

**Severity:** medium–high for large trees.

---

## 3. Correctness and robustness findings

### B1 — same-version document replacement produces an empty `TextDelta`

**Location:** [`reactive_components.rs:562`](../crates/reactive-syntax/src/reactive_components.rs#L562)

`TextComponent::update` uses:

```rust
document.snapshot().edits_since(state.snapshot().version())
```

`TextSnapshot::edits_since` returns `Some([])` when the versions are equal, even
when the text content differs. Replacing a source document with a new
`TextDocument::new(...)` at the same version therefore publishes:

- the new document;
- an empty `TextDelta`.

The lexer’s same-version fallback relexes correctly, but text-delta consumers
see a changed document with no edits.

**Fix:** if `from == to` and text differs, emit a full replacement edit. Also
check that the old snapshot is actually reachable from the new one.

**Severity:** high for text-delta correctness.

---

### B2 — `@message` on a non-error token rule marks the token as an error

**Location:** [`spec.rs:289`](../crates/reactive-syntax/src/lexer/spec.rs#L289)

The rule parser accepts `@message` on any rule and stores `error_message`.
The lexer initializes `error_message` from that field and calls:

```rust
token_flags(rule, error_message.is_some())
```

which sets `TokenFlags::ERROR` even for a normal `token` rule.

**Fix:** reject `@message` unless the rule is declared with the `error` keyword,
or only apply the message to error rules.

**Severity:** high; silent wrong token classification.

---

### B3 — parser no-op can retain an old text snapshot

`ParserComponent::update` returns the old state when the token delta is empty
and `from == to`. If the source document was replaced at the same version but
the token stream/diagnostics compare equal, the parser state can retain the old
text snapshot.

**Fix:** no-op only when the new `TextDocument`/`TextSnapshot` is the same
version and content as the retained state, or always update the state text
snapshot.

**Severity:** medium; same-version replacement is a supported case after B1.

---

### B4 — per-kind lexical ignore filtering relies on diagnostic ranges

**Location:** [`runtime.rs:550`](../crates/reactive-syntax/src/parser/runtime.rs#L550)

The parser filters `lex.error` diagnostics by finding a token whose range
contains the diagnostic start. A zero-width diagnostic at a token boundary can
fall outside the token range, leaving an ignored error visible.

**Fix:** attach the originating token kind/ID to lexer diagnostics, or filter
by a structured diagnostic key rather than range containment.

**Severity:** medium; edge case in ignored-error views.

---

### B5 — output-view ownership failure panics

`GeneralCx::define` and `define_capable` use `assert!` when a definition is
outside the declared output views. That is a hard engine panic, not a structured
component failure.

**Fix:** return a `Result` from output-definition helpers or stage a denied
definition that commit turns into `ComputeError::DefinitionDenied`.

**Severity:** medium; error reporting/robustness.

---

### B6 — component identity uniqueness is not checked

`install_general_component` and `install_general` accept components without
checking that `T::ID` is unique in the runtime. Two components with the same ID
can be installed.

**Fix:** maintain a component-ID set and reject duplicates, or document that
IDs are only diagnostic labels for general components.

**Severity:** medium; identity/routing diagnostics.

---

### B7 — `unit_node_init` demands the producer on every read

The producer-aware read path calls `eval(producer)` before checking the unit.
This is correct for invalidation, but repeated reads of many units in the same
revision repeatedly validate the producer.

**Fix:** add a fast path when the producer is already certified for the current
revision and its trace is clean, or cache the unit/version check per read scope.

**Severity:** low–medium performance.

---

### B8 — batch parser compatibility retains a separate lexer path

`Parser::parse` and `parse_with_state` allocate a lexer per call. The component
path uses token streams only, so this is compatibility, but it means two parsing
entry points have different lexer behavior.

**Fix:** document explicitly, or move the batch helper behind a feature flag /
`BatchParser` type.

**Severity:** low; compatibility design issue.

---

### B9 — `Syntax.diagnostics` and `Tokens.diagnostics` use different shapes

`Syntax` has a stable diagnostic Map; `Tokens` has a `Vec`. This inconsistency
complicates consumers and violates the granularity rule.

**Fix:** use `Map<DiagnosticId, Diagnostic>` in both, with the same
`DiagnosticId` construction.

**Severity:** medium; API consistency.

---

## 4. Improvement priorities

### P0 — correctness/granularity blockers

1. Fix B1 same-version TextDelta.
2. Reject or scope B2 `@message`.
3. Split `Syntax.children` and `token_children` into per-slot units (G4/G5).
4. Convert `Tokens.diagnostics` to a stable Map (G1/G9).

### P1 — AST migration

1. Add `AstReducer`/`@ast` production fit mapping.
2. Add AST tree output (`TreeBuild`/`TreeOutputTxn`).
3. Add `AstMeta` for spans/tokens/diagnostics.
4. Keep CST internal; publish AST only.

### P2 — finer token views

1. Split `Arc<Token>` into field maps (G2).
2. Add linked-order units or retain List as a documented coarse convenience (G3).
3. Split text edits into per-edit units (G10).
4. Split spans start/end (G6).

### P3 — performance and robustness

1. Diff-only lexer publication and AST/tree diffing (G12).
2. Structured error on output-view violations (B5).
3. Component-ID uniqueness (B6).
4. Producer fast path (B7).
5. Structured diagnostic token IDs (B4).

---

## 5. Audit evidence and test gaps

Current green baseline:

```text
cargo test -p reactive-syntax
# 109 tests across unit, lexer, parser, oracle, component,
# AST-grammar and observer suites

cargo test -p reactive --test general_producer -- --test-threads=1
# 7 tests

cargo test -p reactive --test end_to_end -- --test-threads=1
# 4 tests

cargo test -p reactive-runtime --test kernel
# 10 tests
```

The suite currently has:

- component-chain edit coverage;
- same-version document replacement coverage for lexing, but **not** for
  `TextDelta`;
- no non-error `@message` gate;
- no per-slot child invalidation gate;
- no diagnostic Map granularity gate;
- no AST reduction/tree gate.

Required new gates:

| Gate | Finding |
|---|---|
| `same_version_text_replacement_has_full_delta` | B1 |
| `message_attribute_requires_error_rule` | B2 |
| `parser_state_text_matches_document` | B3 |
| `ignored_lex_error_diagnostic_has_token_kind` | B4 |
| `invalid_output_view_is_a_component_error` | B5 |
| `component_ids_are_unique` | B6 |
| `child_slot_edit_does_not_invalidate_sibling` | G4 |
| `token_kind_edit_does_not_invalidate_token_text` | G2 |
| `lexical_diagnostic_edit_is_per_diagnostic` | G1 |
| `ast_reduction_builds_during_parse` | AST plan |
| `ast_tree_incremental_move_preserves_identity` | AST plan |

---

## 6. Audit conclusion

The component graph itself is sound, and the current Views are functionally
correct on the tested paths. The remaining problems are concentrated in three
places:

1. **Large composite values** (Vec children, Vec diagnostics, Arc<Token>,
   monolithic deltas) that defeat unit-level invalidation.
2. **A few concrete bugs**, especially same-version text replacement and
   `@message` on non-error rules.
3. **The missing AST-directed reduction layer**, which requires a tree output
   protocol and a grammar-to-AST fit mapping.

The AST migration should happen together with the granularity refactor, not as
a second tree pass. Otherwise the new AST View will inherit the same coarse
values.
