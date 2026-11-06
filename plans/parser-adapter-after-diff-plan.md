# Parser adapter after-diff pass

**Status:** implemented, then extended by
[`ast-only-parser-tree-metadata-plan.md`](ast-only-parser-tree-metadata-plan.md).
The after-diff now runs inside the parser engine (`AstParser::update` validates
coverage and tiling before a proposal is accepted); the standalone
`parser::ast_diff` module and the CST it diffed are gone: the parser adapter publishes local `TreePatch` units via `TreeOutputTxn::apply_patch`, with a budget that returns an error instead of falling back to a whole-tree diff. CstTree is no longer published.

**Scope:** add a local after-diff pass *inside the incremental parser adapter*
so downstream Tree/View consumers receive only the finest changed units.

**Non-negotiable rules:**

1. The after-diff pass belongs to the parser adapter.
2. The pass runs after parsing and before output publication.
3. The diff is **local**.
4. A full-tree diff is forbidden — not as the primary path, not as a fallback.

---

## 1. Current problem

Today the parser adapter publishes CstTree through `TreeOutputTxn::set_root_diff`.
That helper builds two complete `TreeBuild` values and compares every node in
both trees:

```text
old tree ──build full TreeBuild──┐
                                 ├── compare every node ── emit patches
new tree ──build full TreeBuild──┘
```

This is a full diff even when the parser only reparsed a small token window.

The parser already has the information needed for a local diff:

- the first differing significant token;
- the common reused suffix;
- the checkpoint used for the incremental parse;
- the reparsed token/byte window;
- reused subtree spans;
- the old parse tree;
- the new parse tree.

The adapter should use that information directly instead of diffing the whole
tree afterwards.

---

## 2. Adapter boundary

The after-diff pass lives in the parser adapter:

```text
Tokens View
   │
   ▼
Parser core
   │  ParseOutput { tree, reparse window, reused suffix, ... }
   ▼
Parser adapter
   ├─ AfterDiff pass
   │    ├─ old tree + new tree
   │    ├─ local parse window
   │    ├─ reused suffix/prefix
   │    └─ local node patches
   ├─ TreeOutputTxn::apply_local_patch
   └─ CstTree output units
             │
             ▼
      downstream components
```

Rules:

- The parser core must not publish Views.
- The adapter must not delegate local diffing to a generic full-tree helper.
- No second component is introduced for diffing.
- The diff pass is not exposed as a new public reactive stage; it is an
  implementation detail of the parser adapter.

---

## 3. Required local data

The parser core should expose enough metadata for a local diff without walking
the whole tree.

### 3.1 Parse window

`ParseOutput.reparse_stats()` already provides:

```rust
pub struct ReparseStats {
    pub token_start: usize,
    pub token_end: usize,
    pub byte_start: usize,
    pub byte_end: usize,
    pub reused_suffix: usize,
    pub checkpoint: Option<usize>,
    pub full: bool,
}
```

Add or derive:

```rust
pub struct ParseWindow {
    /// First reparsed significant-token index.
    pub token_start: usize,
    /// End of the reparsed significant-token window.
    pub token_end: usize,
    /// First affected byte.
    pub byte_start: usize,
    /// End of affected byte range.
    pub byte_end: usize,
    /// Token index from which old suffix subtrees were reused.
    pub reused_suffix_start: usize,
    /// Checkpoint used by the parse, when present.
    pub checkpoint: Option<usize>,
}
```

### 3.2 Reused subtree metadata

The existing reuse index already knows old nodes reused as a suffix. Expose a
compact list on `ParseOutput`:

```rust
pub struct ReusedSubtree {
    pub old_id: u64,
    pub new_id: u64,
    pub old_span: (usize, usize),
    pub new_span: (usize, usize),
    pub token_start: usize,
    pub token_end: usize,
}
```

Only subtrees that intersect the parse seam need to be listed. This list must
be built during parsing, not by walking the old tree afterwards.

### 3.3 Changed child slots

The diff pass needs to know which child relations changed:

```rust
pub struct ParseReuse {
    pub window: ParseWindow,
    pub reused: Vec<ReusedSubtree>,
}
```

The adapter uses this metadata to limit every comparison to the seam and the
newly parsed region.

---

## 4. After-diff output

The pass produces a local patch:

```rust
pub struct CstPatch {
    pub root: u64,
    pub insertions: Vec<NodeInsertion>,
    pub updates: Vec<NodeUpdate>,
    pub removals: Vec<NodeRemoval>,
    pub statistics: CstPatchStats,
}
```

```rust
pub struct NodeInsertion {
    pub id: u64,
    pub parent: Option<u64>,
    pub slot: u16,
    pub index: usize,
    pub tag: (u16, u16),
    pub fields: Vec<TreeValue>,
    pub children: Vec<ChildSlot>,
}

pub struct NodeUpdate {
    pub id: u64,
    pub tag: Option<(u16, u16)>,
    pub parent: Option<Option<u64>>,
    pub fields: Vec<(u16, TreeValue)>,
    pub children: Vec<(u16, ChildSlot)>,
}

pub struct NodeRemoval {
    pub id: u64,
}
```

`CstPatch` must never contain the whole tree. Its size is bounded by the local
parse window plus reusable seam nodes.

### Statistics

```rust
pub struct CstPatchStats {
    pub window: (usize, usize),
    pub visited_old: usize,
    pub visited_new: usize,
    pub inserted: usize,
    pub updated: usize,
    pub removed: usize,
    pub slots_updated: usize,
    pub reused_subtrees: usize,
    pub full_diff: bool, // must always be false
}
```

`full_diff` exists only as an assertion/debug field. It must not become a
supported mode.

---

## 5. Local diff algorithm

### 5.1 Establish the seam

From `ParseReuse`:

```text
window.token_start .. window.token_end
window.byte_start .. window.byte_end
reused_suffix_start
checkpoint
```

Old-tree traversal starts only at nodes whose spans intersect the window.
Nodes wholly before the window and wholly after the reused seam are skipped.

### 5.2 Find local roots

Local roots are the smallest old/new subtrees that contain the window:

- the node covering `window.byte_start`;
- the node covering `window.byte_end`;
- their lowest common ancestor, constrained to the parse seam;
- reused subtrees crossing the seam.

No traversal may begin at the document root unless the edit actually covers
the document root.

### 5.3 Compare local nodes

For each old/new node pair reachable from local roots:

1. Match by stable node ID when reuse metadata says it was reused.
2. Otherwise match by parent slot and token interval.
3. Emit an update only if:
   - tag changed;
   - parent changed;
   - leaf field changed;
   - child slot changed.
4. Emit insertion for new nodes not matched.
5. Emit removal for old nodes in the local window not matched.
6. Do not descend into matched reusable subtrees.

### 5.4 Child-slot comparison

For each changed parent:

- compare only slots belonging to that parent;
- emit removed slot memberships;
- emit inserted/changed child slots;
- do not regenerate the parent’s unchanged slots.

### 5.5 Node identity

- Reused nodes keep their old IDs.
- New nodes receive IDs during parsing as today.
- Moved nodes keep their ID and emit a parent/slot patch.
- Removed nodes emit removal and retract memberships.

### 5.6 Local recovery changes

If recovery introduced/removed nodes:

- only nodes inside the reparse window may be part of the patch;
- recovery nodes outside the window reuse old IDs;
- if a recovery decision changes outside the window, the parser must widen the
  parse window explicitly and record that widening.

---

## 6. Publication

Add a framework primitive:

```rust
impl TreeOutputTxn<'_> {
    pub fn apply_patch(&mut self, patch: &CstPatch);
}
```

It must:

- update Tree content for changed nodes only;
- define tags/parents/fields/child slots for inserted/updated nodes;
- retract removed node membership and child slots;
- preserve unchanged units;
- never rebuild Tree content by walking the whole tree.

The parser adapter calls:

```rust
let patch = CstDiff::run(
    previous_tree,
    next_tree,
    previous_reuse,
    next_reuse,
);

cx.tree_output(CstTree::view_id()).apply_patch(&patch);
```

`TreeOutputTxn::set_root_diff` must not be called by the parser adapter.

---

## 7. Never-full-diff enforcement

### 7.1 API enforcement

- `set_root_diff` remains for source editors/tests, but the parser adapter has
  no access to it.
- The adapter-side tree transaction exposes only:
  - `publish_initial` for first parse;
  - `apply_patch` for updates.
- A static test greps the adapter module for `set_root_diff` and fails on use.

### 7.2 Runtime budget

`CstDiff::run` receives a budget:

```rust
pub struct DiffBudget {
    pub max_visited: usize,
    pub max_depth: usize,
}
```

If the local traversal exceeds the budget:

- return a structured `DiffTooLarge` error;
- do not fall back to a full diff;
- widen the parse window explicitly and retry;
- if still too large, publish an error diagnostic and preserve the previous
  published tree rather than silently doing a full diff.

### 7.3 Instrumented counters

Every after-diff run records:

```text
window tokens/bytes
visited old/new nodes
reused subtrees
insert/update/remove counts
```

Assertions:

- `visited_new <= local_window_nodes + seam_nodes`;
- `visited_old <= old_window_nodes + seam_nodes`;
- `visited_* << total_nodes` for point edits;
- `full_diff == false`.

### 7.4 No hidden root traversal

Forbidden in the adapter after-diff path:

- `walk(root)`;
- `collect_all_nodes(root)`;
- `build full TreeBuild`;
- `set_root_diff`;
- `diff(old_root, new_root)`.

Code review and static tests reject these in the adapter module.

---

## 8. Downstream reaction

With local patches:

- an unchanged node’s tag/parent/fields/child slots remain verified;
- only changed node units produce notifications;
- replaying the same parse produces no patch;
- a point edit in a large tree touches only the edited path and local seam.

Downstream components should observe:

```text
changed node tag units
changed child-slot units
changed leaf fields
removed node memberships
```

and not the whole CstTree.

### AST adapter impact

The AST adapter currently parses tokens independently. If it consumes CstTree
metadata, it must consume the same local patch or a local AST diff. It must not
fall back to a whole-tree diff.

---

## 9. Tests

### 9.1 Unit tests for window construction

- edit first token;
- edit middle token;
- edit last token;
- insert at start/middle/end;
- delete range;
- replace range;
- trailing trivia;
- recovery insert/delete.

Assertions:

- local roots do not include the document root unless necessary;
- window matches `ReparseStats`;
- visited counts bounded.

### 9.2 Large-tree locality

Build a JSON document with thousands of nodes. Edit one leaf near the middle.

Assertions:

```text
visited_old << total_nodes
visited_new << total_nodes
full_diff == false
```

Record before/after counts and timings in the edit-trace log.

### 9.3 Downstream spy

A test component observes CstTree units.

For a point edit, assert it observes:

- the edited token/node;
- ancestors whose child slots changed;
- the seam nodes.

It must not observe unrelated sibling subtrees.

### 9.4 No-op edits

Trailing trivia and unchanged significant-token sequences should produce:

```text
patch empty
```

### 9.5 Budget failure

Force a diff that exceeds the budget. Assert:

- no full diff happened;
- a structured error was recorded;
- the previous published tree was retained;
- the parse window was widened explicitly on retry.

### 9.6 Static enforcement

- adapter module contains no `set_root_diff`;
- no `walk`/collect-all in the after-diff path;
- no full `TreeBuild` construction for updates.

### 9.7 Trace logging

Add env-gated trace:

```text
[syntax] after-diff window=tokens 1..2 bytes=1..5
[syntax] after-diff visited=old:4 new:5 inserted=1 updated=2 removed=0
[syntax] after-diff reused_subtrees=3 full=false
```

Include durations and node counts in the existing `REACTIVE_SYNTAX_TRACE` logs.

---

## 10. Implementation phases

### Phase 1 — parser metadata

- Add `ParseReuse`/`ReusedSubtree` metadata to `ParseOutput`.
- Record local roots, seam nodes, and reused subtree spans during parsing.
- Add tests for metadata windows.

### Phase 2 — after-diff core

- Implement `CstDiff::run`.
- Implement local root selection.
- Implement local node matching and patch generation.
- Add budget/statistics.
- No adapter publication changes yet.

### Phase 3 — framework patch API

- Add `TreeOutputTxn::apply_patch`.
- Preserve unchanged units; retract removed nodes/slots.
- Add tree patch unit tests.

### Phase 4 — adapter integration

- Replace parser adapter’s `set_root_diff` call with `apply_patch`.
- Keep initial parse publication.
- Add trace logs and adapter tests.

### Phase 5 — enforcement/cleanup

- Remove parser adapter access to full-diff helpers.
- Add static/no-full-diff tests.
- Add large-tree locality tests.
- Update edit-trace documentation.

---

## 11. Acceptance criteria

The plan is implemented when:

- the parser adapter owns an after-diff pass;
- no update path performs a full-tree diff;
- point edits touch only local nodes and seam nodes;
- unchanged nodes retain their prior units;
- downstream observers see only changed tag/slot/field/membership units;
- full parse fallback still uses an edit-derived local window;
- budget failure produces an error, never a full diff;
- tests assert visited counts and full-diff=false;
- trace logs expose local windows, visit counts, and timings.

---

## 12. Explicit non-goals

- No new reactive component for diffing.
- No public “diff view” replacing canonical CstTree units.
- No whole-tree equality scan.
- No full rebuild as a fallback.
- No language-specific diffing.
- No AST-specific diff pass in this plan; AST locality is a follow-up once the
  parser adapter patch format is stable.
