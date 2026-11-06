# Framework-native syntax Views: one coherent granularity model

**Status:** superseded by [`ast-only-parser-tree-metadata-plan.md`](ast-only-parser-tree-metadata-plan.md). The parser now publishes only `AstTree` plus the granular `AstMeta` metadata View; `CstTree` is no longer a published View.

**Scope:** replace the current hybrid syntax/token Views with Views built
directly from the framework’s primitive field families: `Map`, `List`, `Set`,
`Tree`, and scalars. The goal is not to add more fine-grained fields next to
coarse compatibility fields. The goal is to make the framework-native path the
primary path, then remove the bespoke aggregate logic.

**Related audit:** [`reactive-syntax-granularity-audit.md`](reactive-syntax-granularity-audit.md)

---

## 1. Decision

The current system has a sound unit model but a hybrid syntax implementation:

- some fields are fine-grained units;
- some fields are coarse compatibility records;
- diff publication can leave stale changed sets or old child/field slots;
- the parser and lexer still have hand-written `define_syntax`/token
  publication logic that bypasses the framework’s field-family abstractions.

The replacement design is:

```text
Text Component
  owns: Text View
     - document / version scalars
     - Map<EditId, TextEdit>
     - List<EditId> for edit order

Lexer Component
  reads: Text View
  owns: Tokens View
     - Map<TokenId, field> for every token fact
     - Set<TokenId> for errors/trivia/changed/gaps
     - Map<TokenId, Option<TokenId>> for previous/next links

Parser Component
  reads: Tokens View
  owns: AstTree View (framework Tree)
     - per-node tag
     - per-node parent
     - per-node leaf fields
     - per-child-slot child relation
  owns: AstMeta View
     - Map for spans, token attachments, diagnostics, named fields
```

The central rule:

> Every independently observable fact is one logical unit in a primitive View
> family. No Map value is a heterogeneous record. No List value is a whole
> child list. No Tree is rewritten through an opaque side channel.

---

## 2. What is wrong with the current model

### 2.1 Composite records

Current examples:

```rust
Tokens.tokens: Map<TokenId, Arc<Token>>
Syntax.children: Map<NodeId, Vec<NodeId>>
Syntax.token_children: Map<NodeId, Vec<TokenId>>
Syntax.fields: Map<(NodeId, usize), String>
AstMap.nodes: Map<u64, AstNodeRecord>
```

A change to one token range invalidates readers of token text if they read the
same `Arc<Token>`. A change to one child slot invalidates a whole parent’s
child vector. These are not finest-grained observations.

### 2.2 Opaque publication

`define_syntax` and the lexer `publish` function manually construct unit keys,
memberships, structures, and stale retractions. That duplicates framework
policy and makes it easy to forget a retraction.

Wanted:

```rust
txn.tree(view, field).set_root(&build);
txn.list(view, field).replace(index, value);
txn.set(view, field).remove(member);
```

with the framework maintaining unit keys, structures, memberships, and
retractions.

### 2.3 Stale fine units

The audit found:

- `Tokens.changed`, `Syntax.changed_nodes`, `Syntax.removed_nodes` can retain
  previous memberships under `preserve_definitions`;
- removed `child_slots`/`token_slots` can remain;
- `AstMeta.fields` can retain removed field mappings;
- `AstTree` can retain removed extra child slots.

The fix belongs in the primitive patch layer, not ad hoc cleanup in syntax
code.

### 2.4 Whole-tree deltas

`TokenDelta`, `SyntaxDelta`, and `TextDelta` are convenient scalar summaries,
but they are coarse primary units. They should become optional or be replaced
by per-node/per-field changes and changed sets.

---

## 3. Framework primitives to add

### 3.1 Typed Tree output

A general component needs a first-class way to publish a Tree View without
manually touching `TreeStorage`, `tree_slots`, and content.

Proposed API:

```rust
impl GeneralCx {
    pub fn tree_output(
        &mut self,
        view: ViewId,
        field: FieldId,
    ) -> TreeOutputTxn<'_>;
}
```

```rust
impl TreeOutputTxn<'_> {
    pub fn set_root(&mut self, build: &TreeBuild);
    pub fn replace(&mut self, node: TreeNodeId, build: &TreeBuild);
    pub fn remove_subtree(&mut self, node: TreeNodeId);
    pub fn move_subtree(
        &mut self,
        node: TreeNodeId,
        parent: TreeNodeId,
        slot: u16,
        index: usize,
    );
}
```

It should stage:

- Tree content (`TreeStorage`);
- root unit;
- membership units;
- tag units;
- parent units;
- leaf field units;
- child-slot structure units.

It should commit atomically with the component state and other output Views.
It should share the same implementation as `TreeEditor`, but operate on a
GeneralCx output transaction instead of a source `Edit`.

### 3.2 List patch operations

Current list output helpers only install entries. Add explicit diff patches:

```rust
impl ListOutputTxn<'_> {
    pub fn insert(&mut self, index: usize, id: u64, value: T);
    pub fn replace(&mut self, id: u64, value: T);
    pub fn remove(&mut self, id: u64);
    pub fn set_structure(&mut self, ids: impl IntoIterator<Item = u64>);
}
```

The transaction must maintain the list structure, membership, and values as
one coordinated operation.

### 3.3 Set membership patches

Add explicit membership operations:

```rust
impl OutputTxn<'_> {
    pub fn insert_set<T>(&mut self, view, field, value: T);
    pub fn remove_set<T>(&mut self, view, field, value: T);
}
```

A previous changed set must be retracted explicitly when it no longer applies.

### 3.4 Map patch operations

For keyed facts:

```rust
impl MapOutputTxn<'_> {
    pub fn insert<K, V>(&mut self, key: K, value: V);
    pub fn update<K, V>(&mut self, key: K, value: V);
    pub fn remove<K>(&mut self, key: K);
}
```

Removal must remove membership and value units and update the map structure.

### 3.5 Tree View schema support

- `#[tree_view(..., kind = derived)]` is already needed and present.
- Add a generic tree schema usable by runtime grammars:
  - `CstNode`/`AstNode` variants;
  - `TreeValue::UInt` for grammar IDs/offsets;
  - token leaves via `Token { token_id: UInt }` or sidecar token Maps.

### 3.6 Diff commit contract

The primitive transactions should make the following hard to get wrong:

- restaged units are replaced;
- explicitly removed units are retracted;
- unchanged units are preserved only when the producer opts into
  `preserve_definitions`;
- changed sets are retracted as part of the same commit;
- structure units are refreshed exactly once.

---

## 4. Target Token View

Replace the opaque `Arc<Token>` map and coarse delta with field families.

```rust
#[view(derived)]
pub struct Tokens {
    // Coordinate space.
    pub text: TextDocument,
    pub text_version: u64,

    // Local ordering. The List is an iteration convenience; links are the
    // granular order facts.
    pub order: List<TokenId>,
    pub previous: Map<TokenId, Option<TokenId>>,
    pub next: Map<TokenId, Option<TokenId>>,
    pub head: Option<TokenId>,
    pub tail: Option<TokenId>,

    // Token facts, one unit family each.
    pub kind: Map<TokenId, TokenKindId>,
    pub text_value: Map<TokenId, Arc<str>>,
    pub range_start: Map<TokenId, usize>,
    pub range_end: Map<TokenId, usize>,
    pub flags: Map<TokenId, TokenFlags>,
    pub start_state: Map<TokenId, LexConfig>,
    pub end_state: Map<TokenId, LexConfig>,
    pub lookahead_end: Map<TokenId, usize>,

    // Membership sets.
    pub errors: Set<TokenId>,
    pub trivia: Set<TokenId>,
    pub changed: Set<TokenId>,
    pub removed: Set<TokenId>,

    // Diagnostics are independent facts, not a Vec blob.
    pub diagnostics: Map<DiagnosticId, Diagnostic>,
    pub diagnostic_owner: Map<DiagnosticId, TokenId>,
}
```

Rules:

- No `Arc<Token>` in the primary View.
- Ranges are start/end units, not a tuple.
- Lexer states are per field map values.
- A token's error message is a separate Map value or an error metadata value,
  not embedded in a record.
- `changed` and `removed` must be fully retracted each revision; they are not
  accumulated sets.
- `order` is documented as a whole-sequence convenience. Components needing
  local order read `previous`/`next`.

If a consumer needs one “record” for convenience, it can compose these fields
locally; the View does not store the composition.

---

## 5. Target Text View

```rust
#[view(derived)]
pub struct Text {
    pub document: TextDocument,
    pub version: u64,
    pub edits: Map<EditId, TextEdit>,
    pub first_edit: Option<EditId>,
    pub last_edit: Option<EditId>,
}
```

- `document` is a whole-coordinate scalar; this is intentional.
- Edits are per-edit Map entries.
- Do not use `TextDelta { edits: Vec<TextEdit> }` as the primary unit.
- The lexer can read the document and the edits it needs; a whole-batch
  consumer can read the Map structure.

---

## 6. Target Tree Views

### 6.1 Generic CST Tree

For runtime grammars, use a derived Tree schema rather than a CST-shaped Map:

```rust
#[tree_view(root = CstNode, view = CstTree, kind = derived)]
pub mod cst {
    #[derive(Clone, Debug, PartialEq)]
    pub enum CstNode {
        Nonterminal {
            grammar_kind: u64,
            start: u64,
            end: u64,
            children: Vec<CstNode>,
        },
        Token {
            token_id: u64,
        },
        Error {
            category: u64,
            ignored: bool,
            start: u64,
            end: u64,
            children: Vec<CstNode>,
        },
    }
}
```

Tree facts:

- tag: variant + type;
- leaf fields: grammar kind, offsets, error category/ignored, token ID;
- child slots: child nodes;
- parent: framework unit;
- membership: framework unit.

Tokens remain in the `Tokens` View; CST token nodes carry only token IDs.
This avoids embedding mutable token records into tree nodes.

### 6.2 AST Tree

The published AST should use a generated typed Tree schema:

```rust
#[tree_view(root = Expr, view = AstTree, kind = derived)]
pub mod ast {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Expr {
        Binary {
            op: u64,
            lhs: Box<Expr>,
            rhs: Box<Expr>,
        },
        Number(i64),
        Error {
            category: u64,
            ignored: bool,
        },
    }
}
```

For fully runtime-defined ASTs, use a generic node variant with
`kind/start/end/children` fields. Typed ASTs are generated from the grammar
and AST definition; they are preferable for downstream typed consumers.

### 6.3 AST metadata

AST trees should not be forced to carry every source detail. Publish an
`AstMeta`-style View:

```rust
#[view(derived)]
pub struct AstMeta {
    pub spans: Map<NodeId, (usize, usize)>, // or start/end maps
    pub node_tokens: Map<(NodeId, u16), TokenId>,
    pub diagnostics: Map<DiagnosticId, Diagnostic>,
    pub errors: Set<NodeId>,
    pub fields: Map<(NodeId, String), NodeId>,
}
```

Use Map entries for independently observable metadata. Do not collapse all
metadata into one AST node record.

---

## 7. Component ownership

```text
TextComponent
  reads SourceFile
  owns Text

LexerComponent
  reads Text
  owns Tokens

ParserComponent<A>
  reads Tokens
  owns AstTree, AstMeta
  optionally owns CstTree for tooling

Downstream component
  reads AstTree/AstMeta
```

The parser component:

- never owns a lexer;
- never publishes a CST-shaped Map View;
- publishes AST Tree units through `TreeOutputTxn`;
- publishes AST metadata through normal Map/Set transactions;
- can retain CstTree for diagnostics/tooling, but the semantic tree is AST.

---

## 8. Granularity and diff rules

1. A View field must be one of the standard field families. No composite
   `Vec<T>` payload unless it is an explicit ordered-list field.
2. A Map value must be a scalar fact or a small immutable value, not a
   heterogeneous record.
3. A Tree child relationship is a child slot, not `Map<NodeId, Vec<NodeId>>`.
4. A Set membership change is an explicit insert/remove transaction.
5. A changed/removed set is cleared or retracted between revisions.
6. A List removal/reorder updates membership, value, and structure together.
7. Deltas are optional summaries. The primary incremental signal is the unit
   version and changed sets.
8. `content` units are for snapshots/whole-view consumers; fine consumers must
   read field units.
9. No field is both a coarse compatibility record and the primary path. Keep
   compatibility views separate or delete them at migration end.
10. Diff publication must include explicit retractions for:
    - removed Map entries;
    - removed Set members;
    - removed List entries;
    - removed Tree nodes/slots;
    - changed/removed sets that no longer apply.

---

## 9. Incremental algorithm

### Lexer

- Retain previous token IDs and facts.
- Relex the edited window.
- Emit:
  - insert/update/delete token facts;
  - order link updates;
  - error/trivia set changes;
  - changed/removed set changes;
  - diagnostics map changes.
- Do not publish a whole `Arc<Token>` per changed token.

### Parser

- Read token facts and links.
- Build parse state.
- Emit AST Tree via `TreeOutputTxn`:
  - reuse stable AST node IDs;
  - patch changed tags/fields/slots;
  - retract removed nodes/slots;
  - publish per-node spans/token attachments/diagnostics in AstMeta.
- Publish CstTree only if tooling requests it.

### Downstream

- Typed `TreeRef` reads tag/field/slot units.
- Metadata consumers read specific AST node maps.
- No downstream component reads a monolithic delta unless it explicitly wants
  whole-tree patching.

---

## 10. Migration phases

### Phase A — primitive transactions

- `TreeOutputTxn` for General components.
- List insert/replace/remove/structure patches.
- Set insert/remove.
- Map insert/update/remove.
- Tests for unit retraction and structure refresh.

### Phase B — Text View

- Replace `TextDelta` Vec with per-edit Map.
- Migrate TextComponent to diff publication.
- Add same-version replacement tests.

### Phase C — Tokens View

- Replace `Arc<Token>` Map with per-field Maps.
- Replace diagnostics Vec with diagnostic Map.
- Replace whole-delta primary with changed/removed sets.
- Keep List order as convenience; make links primary for local order.
- Migrate parser token reading.

### Phase D — CstTree

- Introduce generic derived Tree schema.
- Parser emits CstTree through TreeOutputTxn.
- Remove `Syntax` custom View from the primary path.
- Keep compatibility view only if required.

### Phase E — AstTree

- AST reducer builds typed/generic TreeBuild values.
- Parser emits AstTree directly.
- AstMeta uses granular Maps/Sets.
- Remove AstMap from the primary path.

### Phase F — cleanup

- Delete opaque `define_syntax`.
- Delete coarse compatibility fields from primary Views.
- Delete scalar delta fields unless they remain explicit optional projections.
- Update all consumers and tests.
- Add a static audit test/gate that rejects Vec-valued Map fields and
  record-valued Map values in primary syntax Views.

---

## 11. Verification gates

| Gate | Requirement |
|---|---|
| `no_composite_map_values` | primary syntax maps use scalar fields, not records |
| `tree_output_is_atomic` | Tree content + all units commit together |
| `tree_patch_retracts_removed_slots` | removed child slots are gone |
| `changed_set_is_retracted` | changed/removed sets do not accumulate |
| `token_field_edit_isolated` | range edit does not invalidate text readers |
| `child_slot_edit_isolated` | one child slot does not invalidate siblings |
| `list_remove_updates_structure` | list units remain consistent |
| `set_remove_updates_structure` | set structure updates |
| `ast_tree_identity_preserved` | unchanged AST nodes keep identity |
| `full_snapshot_equals_tree_units` | TreeRefs and content agree |
| `random_edit_oracle` | full parse equals incremental output |
| `no_internal_lexer_in_parser` | parser reads Tokens only |
| `no_manual_unit_keys_in_syntax` | syntax components use transactions, not tree_slots |

---

## 12. Acceptance

The design is accepted when:

- every primary syntax/token View is expressed only with scalar, Map, List,
  Set, or Tree fields;
- the lexer and parser use framework output transactions rather than manual
  unit-key construction;
- no changed/removed set or removed tree slot can be observed after it no
  longer applies;
- Tree/AST/Token component consumers observe the smallest applicable unit;
- full snapshots and fine units agree under randomized edits;
- the old custom Syntax/Tokens schema is removed from the primary path.
