# The AST is a `tree_view`, and the grammar says how to build it

**Status:** implemented, with the deviations at the end of this file.
**Scope:** `crates/irrigation-syntax` (grammar mapping, publication, generic-tree
readers), `crates/irrigation-collections` and `crates/irrigation-macros` (the
`tree_view` layout), and the three AST consumers (`stlc_scope_graphs`,
`observatory_json`, `observatory/demo.rs`).
**Out of scope:** the kernel authoring surface (covered by
[`component-authoring-api-plan.md`](component-authoring-api-plan.md)),
scheduler and revision policy, and pipeline installation ergonomics.
**Related:** [`identity-plan.md`](identity-plan.md) (node identity must not
move), [`emission-site-plan.md`](emission-site-plan.md) (the STLC host this
plan simplifies),
[`ast-incremental-granularity-plan.md`](ast-incremental-granularity-plan.md).

---

## 0. Recommendation in one page

`shape()` exists because there is no way to get a typed AST out of a generic
parser. `tree_view` already *is* the typed-tree declaration the workspace uses
(`name_resolver.rs`), and its generated accessors are already unit-granular. What
was missing is the bridge: nothing could build a declared tree from a parse, so
every host hand-wrote an enum, a classifier, and a pile of index and token
lookups.

The plan builds that bridge in three moves:

1. **The grammar's declarations became load-bearing.** `name: Element` positions
   and `@ast(Name { f: i })` lists are merged and validated; `@wrap(field)` names
   a pass-through rule; the parse publishes per-position child ranges. The dead
   `AstMeta::fields` publication (zero readers) was removed rather than fed:
   names are compiled into the tree, not published as metadata.
2. **`#[tree_view]` describes itself.** The macro emits a
   `const LAYOUT: TreeLayout` — types, variants, leaf fields with their slot and
   kind, child fields with their slot and cardinality. That is general IR
   reflection: a parser, a desugarer, a serializer, or the board's inspector can
   use it, and no grammar concept appears in it.
3. **The grammar maps onto that tree; the parser publishes it.**
   `@tree(Expr::Lam)` on a production, `Grammar::map_tree(&TreeLayout)` to
   resolve and validate the mapping, and
   `ParserComponent::with_tree::<Syntax>(grammar)` to publish the declared tree
   **from the same records, with the same node ids**, alongside the generic
   `AstTree` that keeps the raw parse and the recovery surface.

Consumers then read `ExprRef`, `ExprKind`, `expr.body()`, `expr.name()` — the
accessors `tree_view` already generates — and `shape()`, `TermShape`, the index
table, the token-text lookups and the literal re-parsers all disappear.

---

## 1. The bar

| Property | Test |
|---|---|
| **The tree is the only declaration** | No second schema type, no classifier function, no macro attribute that names a grammar rule inside `tree_view`. |
| **`tree_view` stays general** | The layout machinery mentions no rule, no grammar, no parser. A test builds a non-parser IR tree through the same layout. |
| **Identity is the parser's** | A typed node keeps the parser's extent id; the STLC gates stay green **unmodified** (a re-spelling re-identifies nothing, a rename re-identifies nothing, a structural edit keeps the untouched subtree, a warm read executes nothing). |
| **Granularity is not coarsened** | Accessors read the same per-slot units they read today; the typed publication is one extra patch of per-node entries, not a whole-tree rewrite. |
| **Failure is loud** | A grammar that cannot produce the declared tree is a construction error listing every mismatch, not a `None` at read time. |

---

## 2. Evidence

Counts are `grep` on the tree at the time of writing.

**The framework computed the answer and threw it away**

| Fact | Where | Readers before |
|---|---|---|
| `name: Element` field positions are lowered into `Production.fields` | `parser/grammar.rs`, `lower_sequence` | **none** — the engine published only `ast_action.fields` |
| `@ast(Name { f: i })` fields are published per node as `AstMeta.fields: Map<(u64, String), u64>` | `views.rs`, `components/parser.rs` | **none** (only the writer) |
| literal payloads are published per token leaf | `AstMeta.values` | **none** — hosts re-parsed the text |
| rule names are published once per rule | `AstMeta.rules` | **one**, `demo.rs`, as a whole-map `to_vec()` |
| only the last child of a repeating position survived | `engine.rs` `positions` | the repetition boundary was lost |

Because `AstMeta.fields` had no reader, its publication was pure cost — and it
carried a latent defect: `publish()` retracted fields only for *removed* ids, so
a rebuilt node whose field disappeared (a field on an optional child, e.g.
`values: Values?`, when `print(x)` becomes `print()`) left an entry pointing at a
retracted node. Removing the publication removed the defect.

**What every host re-implemented**

| Demand | Occurrences |
|---|---|
| semantic enum + classifier (`TermShape` + `shape`) | 1, and one per future host |
| rule-name → id comparisons | 13 in `syntax.rs`; `demo.rs` matched on `&str` |
| positional child reads | 20 in `syntax.rs`, 2 in `observatory_json.rs`, heuristics in `demo.rs` |
| token-text lookup | 3 (`syntax::token_text`, `Source::text`, test `leaf_text`) |
| literal re-parsing | 3 (`syntax::integer`, `syntax::boolean`, `observatory_json::token_kind`) |
| wrapper skipping | 3 (`syntax::term`, `Source::unwrap`, the `_ =>` arm in `walk`) |
| whole-map materialization inside a component | 2 (`demo.rs` `rules()`, `payloads()`) |

Consumer size before: `syntax.rs` 211 lines, `observatory_json.rs` 425,
`demo.rs` 305.

**Feasibility facts (checked, not assumed)**

- `TreeOutputTxn` is keyed by a runtime `ViewId` and takes raw
  `TreePatchNode { id, type_index, variant, fields, children }`, so an arbitrary
  declared tree can be published **without generics**.
- `GeneralComponent` documents and supports a construction-chosen output View:
  `component_id()` and `output_views()` are overridable methods, used by `Join`.
- `TreeNodeId = u64`, so the parser's extent ids are the typed tree's ids.
- The `tree_view` macro already knows, per variant, the ordered leaf slots, the
  ordered child slots and their cardinalities, and the supported leaf types.
- `AstNodeRef` accepts hand-written inherent impls in `views.rs` (same crate as
  the generated module), and View reads are ambient, exactly like
  `AstTree::root()`.

---

## 3. Design

```text
grammar:   Lam = "\\" name: ident ":" ty: Type "." body: Term  @tree(Expr::Lam)
                     └── field names ──┘                            │
                                                                    │ maps to a variant
#[tree_view] enum Expr { Lam { name: String, ty: Ty, body: Expr } }  │
                     └── TreeLayout: leaf slots, child slots ───────┘
                                                                    │
Grammar::map_tree(&Expr::LAYOUT) -> TreeMapping   (validated once, loud errors)

parse records (rule, children, child ranges, token payloads)
   ├─ AstTree + AstMeta            generic tree: raw parse, recovery, the board
   └─ Syntax (declared tree)       same node ids, variant/leaf/child slots from TreeMapping
```

### 3.1 Grammar: one merged field declaration, mapping annotations

- **Merge.** `f: Element` and `@ast(Name { f: i })` feed one
  `Production.fields: Vec<(FieldId, usize)>`. Both spellings of the same field
  must agree on position, else `GrammarError::Spec`.
- **Positions become ranges.** The reduce path records, for productions that
  carry fields or a tree mapping, one `Range<usize>` per RHS position into the
  published child list. That is what lets a `Vec` tree field take *all* the
  children of a repetition (`items: Item*`) instead of the last one, without
  guessing from a flat list.
- **`@wrap(field)`** names a declared field at a nonterminal position: the rule
  builds no node, and its payload child stands in for it, in both trees
  (`AstMeta.wraps`). Validated at compile time (the field exists, the position is
  singular and a nonterminal). `@wrap` and `@tree` are mutually exclusive on one
  production.
- **`@tree(Type::Variant)`** names the declared tree node this production
  builds. Type and variant are resolved against the tree at mapping time, not
  compile time, because the tree is a Rust type.

### 3.2 `tree_view` gains general reflection (no grammar concepts)

```rust
pub struct TreeLayout { pub types: &'static [TypeLayout], pub root: u16 }
pub struct TypeLayout { pub name: &'static str, pub variants: &'static [VariantLayout] }
pub struct VariantLayout {
    pub name: &'static str,
    pub leaves: &'static [LeafLayout],
    pub children: &'static [ChildLayout],
}
pub struct LeafLayout { pub name: &'static str, pub slot: u16, pub kind: LeafKind }
pub struct ChildLayout {
    pub name: &'static str,
    pub slot: u16,
    pub cardinality: Cardinality,
    pub target: u16,
}
pub enum Cardinality { One, Optional, Many }
pub enum LeafKind { Text, Int, UInt, Float, Bool }   // exactly TreeValue's payloads

pub trait TreeSchema: ViewSchema { const LAYOUT: &'static TreeLayout; }
```

`#[tree_view]` emits `impl TreeSchema for View { const LAYOUT = … }` from the
same `EnumSpec`/`FieldSpec` data the accessors are generated from, so a layout
and an accessor cannot disagree (a test asserts this by writing through the
layout and reading through the accessors). Short type names are unique inside one
`tree_view` module, so `Expr::Lam` resolves without a path.

### 3.3 The grammar → tree mapping

```rust
pub struct TreeMapping { /* rule -> build slots, root rule, layout */ }
pub enum TreeMappingError {
    UnknownType { rule: String, ty: String },
    UnknownVariant { rule: String, ty: String, variant: String },
    UnknownField { rule: String, variant: String, field: String },
    MissingField { rule: String, variant: String, field: String },
    Cardinality { rule: String, field: String, expected: Cardinality, found: &'static str },
    PositionKind { rule: String, field: String, expected: &'static str, found: &'static str },
    UnmappedChildRule { rule: String, field: String, child_rule: String },
    WrapAndTree { rule: String },
    UnmappedRoot { rule: String },
}
impl Grammar {
    pub fn map_tree(&self, layout: &TreeLayout) -> Result<TreeMapping, Vec<TreeMappingError>>;
}
```

Resolution: for each production with `@tree(T::V)`, find the type and variant by
name, then match every declared field to the variant's `LeafLayout`/`ChildLayout`:

- a leaf field reads a terminal position; `LeafKind::Text` takes the token text,
  the numeric and bool kinds take the token's published `AstValue` with a
  text-parse fallback;
- a `One`/`Optional`/`Many` child field reads a position whose range length is
  exactly 1 / 0-or-1 / any;
- a declared tree field with no grammar field is `MissingField`; a grammar field
  with no tree field is `UnknownField`;
- every rule reachable through a declared child position must itself be mapped,
  or be `@wrap` (which is followed to its payload);
- the start production must yield the tree's root type, directly or through
  `@wrap`.

Unmapped, unreachable rules (separator rules, recovery productions, a `Program`
wrapper when the root is declared through `@wrap`) simply produce no typed node.

### 3.4 The parser publishes the declared tree

```rust
impl ParserComponent {
    pub fn new(grammar: Grammar) -> Self;
    pub fn with_tree<S: TreeSchema>(grammar: Grammar) -> Result<Self>;
}
```

- `with_tree` calls `grammar.map_tree(S::LAYOUT)` and fails with every mismatch;
  `output_views()` returns `[AstMeta, AstTree, PipelineStats, S::DESCRIPTOR]`,
  and `component_id()` is mixed with the tree's view id (the `Join` pattern), so
  a plain and a typed parser coexist.
- Publication is one extra pass over the same `added`/`removed` records:
  `TreePatchNode { id: record.id, parent: <nearest mapped ancestor>, type_index,
  variant, fields: <leaves in slot order>, children: <ChildSlot per declared
  child slot> }`, applied through `cx.tree_output(S::view_id())` into a
  `TreeStorage` held in the component state. Wrapper children are substituted by
  following `@wrap` on the record (bounded).
- The generic `AstTree` and `AstMeta` are still published: they are the raw
  parse, the recovery surface, and what the board draws while text is broken. A
  rule with no mapping and a recovery node are absent from the typed tree; the
  parse diagnostics already explain the latter.
- An unconvertible leaf publishes `TreeValue::Unit`, so its accessor reads `None`
  and the nearby lexer or parse diagnostic explains it.

### 3.5 The generic-tree reader set

```rust
impl AstNodeRef {
    pub fn rule_id(&self) -> Option<RuleId>;
    pub fn token(&self) -> Option<TokenId>;
    pub fn text(&self) -> Option<Arc<str>>;
    pub fn value(&self) -> Option<AstValue>;
    pub fn integer(&self) -> Option<i64>;
    pub fn boolean(&self) -> Option<bool>;
    pub fn wrapped(&self) -> Option<AstNodeRef>;
    pub fn semantic(&self) -> AstNodeRef;
    pub fn range_in(&self, document: &TextDocument) -> Option<Range<usize>>;
}
```

`RuleId` (a `u64` newtype, with `const rule_id(name)`) keeps rule ids from being
confused with node ids and `TokenId`s; `ast_rule_id` became `ast_node_id` because
it builds a *node* id from a rule.

---

## 4. Granularity and identity

| Edit | Typed tree | Generic tree |
|---|---|---|
| literal re-spelling | the literal node keeps its id; one leaf slot changes | one `values` unit changes |
| rename | one leaf slot changes on the binder/reference nodes | one token payload unit |
| parens inserted/deleted | the wrapped node keeps its id (its payload child stands in); the enclosing spine re-keys as it does today | a `Parens` node appears or retracts; consumers of `semantic()` are unaffected |
| structural | changed extents re-key, untouched typed subtrees keep every node id | unchanged |
| no edit | nothing executes | nothing executes |

The typed publication adds per-node entries for changed nodes only — no
whole-tree rebuild, no second walk of the document, no duplicated identity logic.
The typed tree is a *derived* View, so it is drawn on the board with real variant
names and is not host-editable.

---

## 5. Phases

**P0 — grammar declarations became load-bearing, dead publication went.**
1. Merge `name:` and `@ast { f: i }` into `Production.fields` with agreement and
   range validation.
2. `AstNodeRecord.positions: Vec<Range<usize>>` emitted for productions with
   fields or `@tree`; replaced the lossy `positions` local in the engine.
3. `@wrap(field)` parsing, validation, `AstMeta.wraps`, and retraction on
   rebuild (the `values`/`tokens`/`wraps` entries a rebuilt record no longer
   has).
4. Deleted `AstMeta.fields` and `AstNodeRecord.fields`; the stale-entry defect
   and one unit per field per node went with them.
5. `RuleId`, `const rule_id`, the `ast_node_id` rename, and
   `AstMeta.rules: Map<RuleId, Arc<str>>`.
6. The §3.5 readers; mechanical updates in `observatory_json.rs`, `demo.rs`,
   `tests/identity.rs`, `tests/support`.

**P1 — `tree_view` reflection.** The `TreeLayout` family in
`irrigation-collections` plus the facade re-export; the `TreeSchema` trait; the
macro emits `LAYOUT`.

**P2 — grammar mapping, typed publication, host migration.** `@tree` in the DSL;
`Grammar::map_tree` with one test per error class; `ParserComponent::with_tree`;
the STLC tree plus its three stages on `ExprRef`; the JSON and demo trees.

**P3 — documents.** `README.md`, the STLC `README.md`, `docs/architecture.md`,
`plans/implementation-status.md`, and the `SUITE_FLOOR` raise in
`scripts/verify.py`.

---

## 6. Acceptance criteria

1. `cargo test --workspace` and `scripts/verify.py` green; every pre-existing
   STLC gate green without edits beyond mechanical renames.
2. `grep -rn "fn shape" crates/` is empty; `syntax.rs` has no rule-id comparison,
   no child index, and no token or literal plumbing.
3. `tree_view` reflection passes a test with a non-parser IR tree (proves it is
   general).
4. `Grammar::map_tree` reports each error class by name, and
   `ParserComponent::with_tree` fails with them rather than publishing a partial
   tree.
5. `AstMeta.fields` is gone; no metadata field is published without a reader.
6. The fixture baseline is unchanged or re-recorded with a reason, and the typed
   publication never raises `meta_units_written` for a grammar without `@tree`
   (the fixture grammar has none).

---

## 7. Edge cases and failure modes

- **Recovery and error nodes**: no variant, so no typed node; the parent's
  required child field is then absent, which the accessor reports as `None`. Raw
  structure, spans and diagnostics remain in `AstTree`/`AstMeta`.
- **Unmapped reachable rule**: construction error, not silence. Unmapped
  *unreachable* rules are how `Program`-style wrappers and separators stay out of
  the typed tree.
- **Optional child**: `Optional` reads a `?` position (0 or 1). `Many` reads a
  repetition or group position (0..n). Anything else is `Cardinality`.
- **Two productions → one variant** (e.g. `Boolean = "true" | "false"`): legal;
  the mapping is keyed by production.
- **Wrapper whose payload is itself a wrapper**: substitution follows the chain,
  bounded at 64 against a corrupted publication.
- **Same node id, different production**: a rebuild that changes the wrap relation
  retracts the stale `wraps` entry.
- **Type-name collisions**: short names are unique inside one `tree_view` module;
  a grammar mapping a type the tree does not declare is `UnknownType`.
- **Leaf conversion**: `AstValue` first, token text second, `TreeValue::Unit`
  last, so a malformed literal degrades to `None` rather than panicking.
- **Reads outside a computation** panic, as every View read already does;
  documented on each reader.

---

## 8. Breaking changes (pre-1.0, in-tree, mechanical)

- `AstMeta.fields` and `AstNodeRecord.fields` are removed (zero readers; the
  defect path went with them).
- `AstMeta.rules` key `u64` → `RuleId`; `rule_id` became `const fn`;
  `ast_rule_id` became `ast_node_id`.
- `AstNodeRef::token_id()` → `token() -> Option<TokenId>`.
- `@ast(Name { f: i })` still parses but no longer publishes; a field declared in
  both spellings must agree.
- `Grammar`/`Production` gained fields (`wrap`, `tree`);
  `TreeMapping`/`TreeMappingError` are new.
- `ParserComponent::with_tree` and the `TreeSchema`/`TreeLayout` items are
  additive.

---

## 9. Rejected alternatives

1. **A separate `#[ast_schema]` + typed `TermRef` + a schema validator**: a
   second way to declare a typed tree next to `tree_view`, and a macro that knows
   grammar concepts. Replaced by §3.2–3.4.
2. **A `SyntaxPipeline` install helper**: convenience for repeated call sites, but
   it hides the lexer-before-parser ordering behind a bundle whose errors then
   need their own taxonomy. The three installs stay visible.
3. **A lowering `GeneralComponent` from `AstTree` to the typed tree**: a second
   full walk plus a patch algorithm that would have to re-derive the parser's
   extent identity. Publishing from the records is the same output with no second
   identity model and no second tree in memory.
4. **Generating the tree from the grammar at compile time**: `Grammar::compile`
   builds an LALR table at runtime and the DSL is a runtime string; a macro over
   it is a second language implementation.
5. **Inferring the tree type from the parent field instead of
   `@tree(Type::Variant)`**: needs a fixpoint over the whole grammar and produces
   worse diagnostics; the qualified form is explicit.
6. **Publishing `AstMeta.fields` and giving it a reader**: names would be
   compiled into the tree *and* published per node; the map is cost with no
   consumer, which this repository had already removed once (`06f8f6b`).

---

## 10. Reading list

- [`../crates/irrigation-syntax/src/parser/grammar.rs`](../crates/irrigation-syntax/src/parser/grammar.rs)
  — the DSL, `Production.fields`, `@ast`, and the new `@tree`/`@wrap`.
- [`../crates/irrigation-syntax/src/parser/engine.rs`](../crates/irrigation-syntax/src/parser/engine.rs)
  — record construction and the per-position child ranges.
- [`../crates/irrigation-syntax/src/components/parser.rs`](../crates/irrigation-syntax/src/components/parser.rs)
  — publication of `AstTree`, `AstMeta`, and the declared tree.
- [`../crates/irrigation-syntax/src/views.rs`](../crates/irrigation-syntax/src/views.rs)
  — the published Views and the generic-tree readers.
- [`../crates/irrigation-macros/src/tree.rs`](../crates/irrigation-macros/src/tree.rs)
  — `#[tree_view]` expansion and the emitted `TreeLayout`.
- [`../crates/irrigation-collections/src/tree.rs`](../crates/irrigation-collections/src/tree.rs)
  — `TreeStorage`, `TreePatch`, `ChildSlot`, `TreeValue`, and the layout types.
- [`../crates/irrigation/examples/stlc_scope_graphs/main.rs`](../crates/irrigation/examples/stlc_scope_graphs/main.rs)
  — the three stages over the declared tree.

---

## 11. Deviations from the written plan

The phases landed with the following differences. Each one is a decision the
implementation forced; none of them leaves a claim in this file untrue.

1. **`AstMeta::fields` was removed instead of republished under an interned
   key.** The plan's P0 proposed publishing the named fields per node. Nothing
   reads them (the typed tree is built from the parse records, not from the
   metadata), so publishing was cost without a consumer — the class of unit this
   repository has deleted before. The names live in `Production.fields`, which
   `@tree` mapping and `@wrap` resolve; the stale-entry defect the plan found went
   away with the publication it lived in. The fixture baseline was re-recorded
   because the deletion removed ten to a hundred and forty units per script
   (`meta_units_written`, one pair per published field, eight scripts).

2. **`tree_view` now merges compatible accessors across variants.** The plan
   assumed each field name had to be unique inside one enum, and that the STLC
   would have to rename `Lam::body` versus `Let::body`. Requiring that is the
   macro's own limitation, and it makes an AST declaration read badly, so two
   variants may now share an accessor name when the type and the read agree; the
   accessor dispatches on the discriminant. A shared name with two types is still
   a compile error naming both variants.

3. **Leaf payload readers name a field, and the names must be distinct per
   enum.** `#[value]`-style payload fields are not part of the tree macro; a
   literal's payload is a leaf field of its variant (`Int { int: i64 }`,
   `Bool { flag: bool }`). Since accessors merge by name and type, `value: Expr`
   (a `Let` child) and `value: i64` (an `Int` leaf) cannot coexist, so the STLC
   spells the literals `int` and `flag`. This is the price of one accessor
   namespace per enum, and it is checked at compile time.

4. **The readers stay ambient.** `value`, `integer`, `boolean`, `wrapped`,
   `semantic`, and `range_in` read `AstMeta`, so like every View read they only
   work inside a computation. `rule_id`, `token`, `text`, `children`, and the typed
   accessors also work on snapshot refs, because they read the tree. The tests
   that exercise the ambient readers do so inside a probe component, which is how
   a host uses them.

5. **`Grammar::map_tree` takes a `&'static TreeLayout`.** `TreeSchema::LAYOUT` is
   a constant, so a mapping cannot outlive the declaration it was resolved
   against. This also lets `TreeMapping` keep the variant layouts without
   cloning them.

6. **The demo language lost its `Values` wrapper rule.** `Print` and `Call` take
   their arguments as a `Vec` field fed by a repeated position
   (`args: (Expression ("," Expression)*)?`), which is the shape the declared tree
   declares. `Values` had no node of its own and no field could name it, so it
   would have been an unmapped child rule — a construction error rather than a
   silent hole.

7. **A grammar rule whose name collides with a lexer token literal is a trap.**
   `Atom = Int` in a grammar that also declares `token "Int"` names the *token*,
   not the rule, so the production builds a rule node with no fields. The mapping
   reports it (`UnmappedChildRule`), which is how the test grammar found it. The
   DSL does not diagnose the collision itself; that is left as a grammar
   diagnostic improvement.

8. **A component's emissions cannot be moved into helper functions.** An
   entity's identity is the expression that emitted it, and a helper called from a
   lowered body is one expression: two `emit_*` calls inside it would give two
   entities one name (the runtime reports it as a re-entrant demand). The STLC's
   `resolve` therefore keeps every emission inline and extracts only the pure
   parts (`binding_of`, `label_of`); the constraint is stated in its doc comment
   so the next reader does not rediscover it. The graph also stores the syntax
   tree's own `TermKind`, which removed the translation table between a node and
   its payload.

What the plan predicted and the implementation confirms: no consumer classifies
a node, `stlc_scope_graphs/syntax.rs` is the tree plus one conversion, the STLC's
22 gates pass unmodified, the JSON and demo sinks read typed refs instead of
materializing `AstMeta::rules` and `Tokens::payloads`, and the typed tree keeps
the parser's node identities.
