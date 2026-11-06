# AST-only parser output: framework Tree structure plus granular metadata

**Status:** implemented, then extended by
[`ast-incremental-granularity-plan.md`](ast-incremental-granularity-plan.md)
(which replaced the CST-era identity scheme with token-extent identity, made the
lexer and parser incremental, and removed the remaining CST-era modules). The
parser publishes `AstTree` (structure only) and `AstMeta` (split span anchors, per-node values, token attachments, error set, diagnostics, named fields, rule names). `CstTree`, `AstComponent`, `AstMap`, and the CST local-diff module are removed; the parser adapter runs a local AST after-diff and publishes a `TreePatch` through `TreeOutputTxn::apply_patch`, with a budget that errors instead of falling back to a whole-tree diff. AST node identities are structural hashes over the rule and the tokens the node
covers (spans are metadata, so they do not move identity), so unchanged subtrees
keep their units.

**Scope:** make the parser publish exactly one structural View — the AST Tree —
and move every non-structural fact (spans, token attachments, semantic values,
recovery markers, diagnostics, named fields) into separate granular metadata
Views built from framework `Map`/`Set`/scalar fields.

**Related plans:**

- [`parser-adapter-after-diff-plan.md`](parser-adapter-after-diff-plan.md)
- [`framework-native-syntax-views-plan.md`](framework-native-syntax-views-plan.md)
- [`ast-directed-parser-plan.md`](ast-directed-parser-plan.md)

---

## 1. Decision

Today the parser publishes `CstTree` and the AST component publishes
`AstTree` + `AstMeta`. That is two structural authorities over the same parse,
and the AST Tree still carries spans as leaf fields.

The replacement design is:

```text
Tokens
  │
  ▼
Parser core (private CST/GSS/checkpoints/recovery)
  │
  ▼
Parser adapter
  ├─ AST Tree publication (structure only)
  └─ AST metadata publication (spans, values, tokens, errors)
```

Rules:

1. The parser publishes **only** the AST Tree as structure.
2. `CstTree` is not published. A CST may exist privately inside the parser core
   for GLR, recovery, checkpoints, and ambiguity handling.
3. Spans are never Tree leaf fields.
4. Semantic values are never Tree leaf fields.
5. Every published fact is a framework field family: `Tree`, `Map`, `Set`,
   `List`, or scalar.
6. There is one structural tree and one metadata surface. No duplicate
   authority.

---

## 2. Why spans must not be Tree fields

Tree leaf fields are units keyed by `(node, slot)`. Putting `start`/`end` there
makes every position-shifting edit invalidate every descendant leaf.

Consider a 10,000-node JSON document with an insertion at offset 5:

| Representation | Units invalidated by one insertion |
|---|---|
| spans as Tree leaf fields (`node_start`, `node_end`) | 2 per node ≈ 20,000 units |
| spans as a `Map<NodeId, usize>` of raw offsets | every node after the edit |
| spans as `Map<NodeId, TextAnchor>` | only nodes whose parse changed |

The third form is correct: a `TextAnchor` is version-tagged, so a node whose
source text did not change keeps a stable anchor even when byte offsets move.

The same reasoning applies to values: a string value stored in a Tree field is
one unit that also encodes structure; moving it to `Map<(NodeId, FieldId), Value>`
makes it independent of structure and of spans.

**Rule:** Tree units answer *"what is this node?"*. Metadata units answer
*"where is it and what does it hold?"*.

---

## 3. AST Tree schema

### 3.1 Generic runtime schema

Runtime grammars cannot generate Rust enums, so the generic schema carries only
structure:

```rust
#[reactive::tree_view(root = AstNode, view = AstTree, kind = derived)]
pub mod ast_tree {
    /// Structural AST node.
    #[derive(Clone, Debug, PartialEq)]
    pub enum AstNode {
        /// A reduced grammar node.
        Rule {
            /// Grammar nonterminal identity.
            rule: u64,
            /// Children in declaration order.
            children: Vec<AstNode>,
        },
        /// A token leaf.
        Token {
            /// Token identity only; text/kind/range live in Tokens.
            token_id: u64,
        },
        /// A recovery or missing-token node.
        Error {
            /// Recovery category.
            category: u64,
            /// Whether the grammar ignores this category.
            ignored: bool,
            /// Recovered children.
            children: Vec<AstNode>,
        },
    }
}
```

Notes:

- `rule` is a `u64` grammar nonterminal id, not a string name. String names
  live in a grammar-level Map (§4.7), so renaming a rule does not touch nodes.
- No `start`, no `end`, no text, no value.
- Child order is expressed by child slots, not by vectors in metadata.

### 3.2 Typed schema for known languages

For a language with a defined AST, the same rule applies with typed variants:

```rust
#[reactive::tree_view(root = JsonNode, view = JsonAst, kind = derived)]
pub mod json_ast {
    #[derive(Clone, Debug, PartialEq)]
    pub enum JsonNode {
        Document { children: Vec<JsonNode> },
        Object { children: Vec<JsonNode> },
        Array { children: Vec<JsonNode> },
        Pair { children: Vec<JsonNode> },
        String { token_id: u64 },
        Number { token_id: u64 },
        Bool { token_id: u64 },
        Null,
        Error { category: u64, ignored: bool, children: Vec<JsonNode> },
    }
}
```

Typed variants give downstream consumers compile-time structure. They still
carry only identity and structure; values come from metadata.

### 3.3 Field naming rules

- Tree leaf names must be unique across all variants of one schema, because the
  macro generates per-field accessors.
- Use `token_id`, `rule`, `category`, `ignored`, `children` — never `start`,
  `end`, `span`, `text`, `value`.
- A schema that needs a value must reference it by identity (`token_id`) or
  read it from metadata.

---

## 4. AST metadata Views

All metadata is framework-native:

```rust
#[reactive::view(derived)]
pub struct AstMeta {
    // Position, version-safe.
    pub span_starts: Map<NodeId, TextAnchor>,
    pub span_ends: Map<NodeId, TextAnchor>,

    // Token attachment for non-token nodes (e.g. an identifier leaf hoisted).
    pub tokens: Map<NodeId, TokenId>,

    // Typed semantic values, one unit per (node, field).
    pub values: Map<(NodeId, FieldId), AstValue>,

    // Recovery.
    pub errors: Set<NodeId>,
    pub diagnostics: Map<DiagnosticId, Diagnostic>,

    // Named grammar fields, for consumers that want `value` rather than slot 1.
    pub fields: Map<(NodeId, FieldId), NodeId>,
}
```

`AstValue`:

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum AstValue {
    Unit,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Text(Arc<str>),
}
```

### 4.1 Span start/end split

```text
span_starts[node]  → one unit
span_ends[node]    → one unit
```

A consumer that needs only "where does this node start" observes one unit. A
tuple `Map<NodeId, (usize, usize)>` would couple them.

`TextAnchor` is required, not optional. A raw offset is only valid in one text
version and produces false invalidations plus stale reads after edits.

### 4.2 Token attachment

`tokens` attaches a token id to a node that is not itself a token leaf. This
covers grammars that hoist a token into a named field while the Tree stores a
`Rule` node. JSON `Pair.key` uses it only if the grammar models keys as a
non-token node; otherwise the key is a `Token` leaf and this map stays empty.

### 4.3 Values

`values` is `Map<(NodeId, FieldId), AstValue>`. Each entry is one unit.

JSON example:

```text
values[(number_node, 0)] = Int(12)
values[(string_node, 0)] = Text("a")
values[(bool_node, 0)]   = Bool(true)
```

A consumer that reads one literal observes exactly one unit. Changing a number
does not touch the string beside it.

If a grammar has no semantic payloads (pure syntax), `values` stays empty and
downstream code ignores it.

### 4.4 Errors

`errors: Set<NodeId>` holds recovery markers. Membership is one unit per node.

Do not store `ignored` as a Tree field plus a set; derive it:

```text
errors.contains(node) → recovery node
metadata category from `Error.category` Tree field
```

### 4.5 Diagnostics

`diagnostics: Map<DiagnosticId, Diagnostic>` is per-diagnostic. Retraction of a
removed diagnostic is a membership `false` write.

### 4.6 Named fields

`fields: Map<(NodeId, FieldId), NodeId>` maps a semantic grammar field to the
child node that occupies it. This keeps "`value` is child 1" out of the Tree and
lets consumers use grammar field names.

### 4.7 Grammar names

Keep rule and token names out of AST nodes:

```rust
#[reactive::view(source)]
pub struct GrammarInfo {
    pub rules: Map<u64, Arc<str>>,
    pub tokens: Map<u32, Arc<str>>,
}
```

A rule rename changes one Map entry and no AST node.

---

## 5. Component ownership

```text
TextComponent
  reads SourceFile
  owns Text

LexerComponent
  reads Text
  owns Tokens

ParserComponent
  reads Tokens
  owns AstTree, AstMeta
```

The parser component:

- never publishes `CstTree`;
- may keep a private CST internally;
- publishes AST structural units and metadata units in the same transaction;
- uses `TreeOutputTxn::apply_patch` for updates.

The separate `AstComponent` is removed once the parser publishes AST directly.
Its output Views move to `ParserComponent`.

---

## 6. Parser integration

### 6.1 Reducer contract

`AstReducer` builds a `TreeBuild` instead of an opaque record map:

```rust
pub trait AstReducer {
    type Node;

    fn reduce(
        &mut self,
        production: &Production,
        production_id: ProductionId,
        span: TextRange,
        children: &[AstChild<Self::Node>],
    ) -> Self::Node;

    fn rank(&self, node: &Self::Node) -> i64 { 0 }
    fn recover(&mut self, span: TextRange, ignored: bool) -> Option<Self::Node>;
}
```

The adapter's reducer produces:

```text
TreeBuild            structural node
AstNodeFacts         span anchors, token id, values, field names
```

`AstNodeFacts` are accumulated per node during reduction and never enter the
Tree.

### 6.2 Publication

```rust
let ast = build_ast_tree(root, &builder);        // TreeBuild
let cst_storage = ast.to_storage();              // structural content only

let _ = cx.tree_output(AstTree::view_id()).set_root(&ast);
publish_ast_meta(cx, &builder, previous_meta);
```

On update:

```rust
let (patch, stats) = ast_local_diff(old_ast, new_ast, window)?;
let storage = cx.tree_output(AstTree::view_id()).apply_patch(&state.ast, &patch);
let meta = publish_ast_meta_patch(cx, &builder, &state.meta, window);
```

### 6.3 Anchors

Every reduced node records:

```text
span_start = TextAnchor::new(snapshot.version(), start)
span_end   = TextAnchor::new(snapshot.version(), end)
```

Reused subtree nodes keep their previous anchors. Because anchors are
version-tagged, later edits resolve them correctly without rewriting them.

---

## 7. Local after-diff for structure and metadata

The existing local after-diff (`parser::after_diff`) already selects a
containing path plus a local subtree. Two changes are needed:

### 7.1 Diff the AST, not the CST

`local_diff` receives two `TreeBuild` values or two AST node collections — not
`SyntaxTree`. The window is still the parse window from `ReparseStats`.

### 7.2 Diff metadata locally

Metadata patching must use the same local node set:

```text
changed nodes  → stage span/value/token units
removed nodes  → retract span/value/token/field/error memberships
unchanged      → no writes
```

No full metadata scan. A metadata consumer must not observe unrelated nodes.

### 7.3 Budget

Keep the existing `DiffBudget`; exceeding it returns `CstDiffError::TooLarge`
(rename to `AstDiffError`). Never fall back to a whole-tree diff.

---

## 8. Granularity inventory

| Fact | Family | Unit key |
|---|---|---|
| node exists | `Tree` membership | one per node |
| node variant | `Tree` tag | one per node |
| node parent | `Tree` parent | one per node |
| child relation | `Tree` child slot | one per slot |
| token leaf identity | `Tree` field `token_id` | one per leaf |
| rule identity | `Tree` field `rule` | one per rule node |
| error category | `Tree` field `category` | one per error node |
| span start | `Map<NodeId, TextAnchor>` | one per node |
| span end | `Map<NodeId, TextAnchor>` | one per node |
| semantic value | `Map<(NodeId, FieldId), AstValue>` | one per node+field |
| token attachment | `Map<NodeId, TokenId>` | one per node |
| error membership | `Set<NodeId>` | one per node |
| diagnostic | `Map<DiagnosticId, Diagnostic>` | one per diagnostic |
| named field | `Map<(NodeId, FieldId), NodeId>` | one per node+field |
| rule name | `Map<u64, Arc<str>>` | one per rule |

Every row is a framework primitive. No bespoke record container appears.

---

## 9. Downstream reaction matrix

| Edit | Tokens | AstTree | AstMeta |
|---|---|---|---|
| number `1`→`12` | number token facts, `changed`, order | number leaf + pair/object slot path | span anchors for path, `values[(number,0)]` |
| key `"a"`→`"aa"` | key token text/range | key leaf + pair slot | span anchors for path |
| trailing whitespace | trivia token, `changed` | none | none |
| array element append | new token facts, order | new element subtree, array slot | new element spans/values, array span |
| object member insert | new token facts, order | new pair subtree, object slot | new pair spans, object/document span |
| delete value | token removed, order | error/missing node, slots | error membership, diagnostic, spans |
| whole-file replace | everything | full AST patch within window | full metadata patch within window |

Only touched nodes and ancestor slots appear. Untouched siblings keep their
previously verified units.

---

## 10. Framework support required

### 10.1 Existing, reusable

- `TreeOutputTxn::set_root` / `apply_patch`
- `TreePatch` / `TreePatchNode`
- `TreeBuild::to_storage`
- `#[tree_view(..., kind = derived)]`
- `Map`, `Set`, scalar fields
- `TextAnchor`

### 10.2 To add or confirm

1. `apply_patch` must stage membership/tag/parent/field/child-slot units and
   nothing else (already true).
2. `apply_patch` must not require a full-tree diff to compute retractions
   (already true: `TreePatch.removed` is explicit).
3. General-component `Map`/`Set` output helpers must support explicit removal
   of one entry and one member (partially present via `remove_membership`,
   `set_remove`; confirm coverage for `Map<(K1,K2), V>` keys).
4. A `Map` field whose value is `TextAnchor` must accept `TextAnchor` without a
   wrapper (`AnyKey`/`Value` bounds are satisfied).
5. A diff-budget error type for AST (`AstDiffError`).

---

## 11. Migration phases

### Phase 1 — schema split

- Add `AstNode` generic tree schema with no spans.
- Add `span_starts`, `span_ends`, `values`, `tokens`, `errors`, `fields` to
  `AstMeta`.
- Keep existing `AstTree` spans temporarily for compatibility.
- Add tests asserting no `start`/`end` fields exist on AST tree schemas.

### Phase 2 — reducer produces TreeBuild + facts

- Change the adapter reducer to emit `TreeBuild` plus `AstNodeFacts`.
- Record anchors during reduction.
- Publish `AstMeta` from facts.
- Keep `CstTree` publication temporarily.

### Phase 3 — parser owns AST

- Move `AstTree`/`AstMeta` to `ParserComponent::OUTPUT_VIEWS`.
- Publish AST from the parser adapter transaction.
- Keep `AstComponent` installed but unused, behind a temporary test.

### Phase 4 — local AST after-diff

- Point `local_diff` at AST trees.
- Apply `TreePatch` to AST content.
- Emit local metadata patches with the same node set.
- Add locality tests asserting visited counts and `full=false`.

### Phase 5 — remove CstTree

- Delete `CstTree` from parser outputs.
- Delete AST component and its Views.
- Update all tests to read AST only.
- Update `SyntaxConsumer`/`CstConsumer` equivalents to AST consumers.

### Phase 6 — cleanup

- Remove `AstMap`-era leftovers.
- Remove duplicate AST tree construction paths.
- Add static tests: no `start`/`end` in tree schemas; no `set_root_diff` in the
  parser adapter; no `CstTree` in parser outputs.

---

## 12. Tests

### 12.1 Schema tests

```text
ast_tree_schema_has_no_position_fields
ast_meta_has_split_span_maps
ast_meta_values_are_per_field_units
ast_tree_token_leaf_is_identity_only
```

### 12.2 Locality tests

```text
ast_local_diff_is_window_bounded
ast_metadata_patch_is_window_bounded
ast_local_diff_respects_budget
```

### 12.3 Behavior tests

```text
json_ast_number_edit_touches_one_value_unit
json_ast_key_edit_does_not_touch_sibling_values
json_ast_whitespace_edit_publishes_no_span_units
json_ast_array_insert_keeps_existing_element_units
json_ast_object_insert_keeps_existing_pair_units
json_ast_recovery_publishes_error_membership_and_diagnostic
```

### 12.4 Downstream spy test

A component reads one AST node's value and one span, then asserts which units
notified it for each edit class. Assertions:

```text
number edit            → value unit + span units on path
key edit               → span units on path only
whitespace edit        → no notification
array append           → new element units + array span
unrelated sibling edit → no notification
```

### 12.5 Structural integrity

```text
ast_tree_matches_metadata_nodes
ast_tree_child_slots_cover_all_children
ast_tree_has_no_orphan_metadata
```

`ast_tree_matches_metadata_nodes` asserts every Tree membership has span
entries and vice versa, catching partial patches.

---

## 13. Acceptance criteria

- The parser publishes `AstTree` and `AstMeta` only.
- No span, text, or semantic value appears in a Tree schema.
- All published facts use framework `Tree`/`Map`/`Set`/scalar fields.
- Spans use `TextAnchor`.
- Metadata updates are local to the parse window plus seam nodes.
- A downstream reader of one node's value observes only that unit.
- A trailing-whitespace edit produces no AST or metadata notification.
- Local after-diff never falls back to a full diff.
- `CstTree` is gone from published Views.
- The separate AST component is gone.

---

## 14. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Anchor maps grow with node count | anchors are one unit per node; removal is explicit on node removal |
| Metadata and Tree drift | integrity test asserts membership/span parity |
| GLR ambiguity needs a CST | CST stays private inside the parser core |
| Recovery spans overlap | `errors` set + `category` field define recovery nodes |
| Typed schemas differ per language | generic schema first; typed schemas are a later refinement |
| Values of unknown types | `AstValue` enum; extend explicitly when a language needs it |

---

## 15. Non-goals

- No CST consumers in the public API.
- No second structural tree.
- No “metadata tree” replacement — metadata stays flat Maps/Sets.
- No whole-tree diff of any kind.
- No language-specific diffing.
- No change to lexer Views.

---

## 16. Open questions

1. Should `values` be one Map per value type (`ints`, `floats`, `texts`) so a
   reader observes a typed unit without an enum match?
2. Should `fields` store `FieldId` or a string name? `FieldId` is cheaper and
   stable; names belong in `GrammarInfo`.
3. Should recovery nodes carry a diagnostic id in the Tree, or only in
   `diagnostics` plus `errors`?
4. Do typed AST schemas need generated `AstValue` accessors?
