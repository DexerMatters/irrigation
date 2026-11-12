# Identity is the occurrence, not the spelling

**Status: implemented.** The syntax layer keeps token and node identity across
an in-place edit, the parser rebuilds the affected leaves in place, and the STLC
host passes binder occurrences instead of names. A one-character edit now
changes zero scope-graph entities. Evidence: `crates/irrigation-syntax/tests/identity.rs`,
`crates/irrigation/tests/identity.rs`, the STLC example gates, and the fixture
corpus in `crates/irrigation-syntax/tests/fixture.rs`.

## The problem

An edit that only re-spells a token re-created the graph. Measured on the STLC
seed, editing the last token (`f 2` → `f 3`):

```
AST   34 nodes, 27 kept   (re-keyed: Program, Let×3, App, Integer, the token)
graph 18 nodes,  5 kept
graph 29 edges,  5 kept   (+592 executions, +478 expression sites)
```

Two layers compounded. The lexer minted a fresh `TokenId` for any token whose
text changed, so every AST node whose extent ended at that token re-keyed
(`ast_rule_id(rule, first, last)`); and a component invocation is identified by
its arguments, so a re-keyed `term` or an emitted `scope` re-created every
invocation beneath it, and with it every emission site and entity. A second,
independent defect: `Frame.name` carried a *spelling*, so a rename changed an
argument even when the binder occurrence was the same.

## The rule

Identity names the occurrence; content is payload.

* A token is the occurrence at a position. An edit that re-spells it keeps the
  id and changes the payload.
* An AST node is the extent it covers over those token occurrences. A rebuilt
  node keeps its id; its `AstMeta` record is rewritten in place.
* A component invocation **is** its argument tuple, so an argument must be an
  occurrence — a token, an AST node, an entity — never a spelling or a value
  that is merely data.
* An entity emitted from a component body is the expression site that emitted it
  (`emission_id`); a keyed collection entry is its key; a shared node is its
  content.

Content-addressed identity is correct exactly when the content *is* the name (a
canonical type node shared by `emit_shared`, a JSON path), and wrong when the
content is only a spelling: keying by a name re-creates the entity on every
rename.

## What changed

### `irrigation-syntax`

* **`lexer/runtime.rs`** — `Continuation` replaces lexeme comparison with
  occurrence identity: a token takes the id of the old token that starts at the
  same mapped position and has the same kind; when an edit collapses two old
  starts onto one position, the kind selects the survivor and the lexeme breaks
  a tie between two candidates of that kind. Synthesized tokens are always
  fresh, zero-width tokens never claim an identity, and fresh ids are minted
  above every old and provisional id so a reused id cannot collide. It runs on
  the relexed region of both return paths of `try_incremental` (the
  "suffix reused" path never reconciled before) and on `relex_all` when the edit
  history is available. `reconcile_tokens` remains the textual fallback for a
  version gap with no edit history.
* **`parser/state.rs`** — `divergence` compares `(id, kind, text, flags)`. The
  id half is what the earlier "divergence is by identity" fix needed (a token the
  lexer re-identifies with the same lexeme must rebuild); the content half is
  what a re-spelling needs (same id, new text). They are complementary. The
  changed token can never be spliced (candidates are position-filtered to the
  textually identical suffix), so the leaf and its ancestors are rebuilt with the
  same extent ids; `commit` already keeps a same-identity rebuild out of
  `removed`, and `publish` rewrites its `AstMeta`.
* **`components/parser.rs`** — `nodes_added` counts ids absent from the previous
  table, so "AST nodes whose identity is new" means what it says; a rebuilt node
  is no longer reported as added.
* **`lib.rs`, `lexer/mod.rs`, `parser/ast.rs`** — the identity contract is
  written down, including the ladder above and the note that a component above
  this crate inherits the identity it passes as an argument.

### `irrigation` (facade and runtime docs)

`crates/irrigation/src/lib.rs` gains an **Identity** section: the ladder, the
"arguments are identity" rule, the three authoring styles (root, callable,
general producer), and the two legitimate content-addressed identities.
`general.rs` states what a producer owes its output identity and why
`ChangeBatch` makes keying by occurrence cheap.

### `irrigation` macros

Derived `List` snapshots now expose `{field}_ids()` and `{field}_key(id)`,
matching the live handle's `ids`/`get_key`, so a test (or an observer) can assert
per-entity identity instead of guessing from payloads.

### The example

* `Frame { scope, binder }` — the name is read with `syntax::name(&frame.binder)`
  where it is needed. A rename therefore changes no argument, so no invocation,
  site, node, or edge id changes; labels and `D`/`R` payloads update in place.
* `examples/observatory_json.rs` — the sink keys each row by the leaf's AST
  occurrence instead of a hash of its path. A value edit updates one row, a key
  rename keeps the row and moves its path payload, and a deleted leaf retracts
  its row.

### Gates

* `crates/irrigation-syntax/tests/identity.rs` (7): token identity survives a
  re-spelling of any length; a kind change replaces it; a collapsed start keeps
  the surviving occurrence; every AST identity survives a leaf edit with the
  value unit and span refreshed; a middle edit in a large document is local and
  fresh; a deletion retracts only its subtree.
* `crates/irrigation/tests/identity.rs` (4): a callable component recursing over
  the AST keeps every entity across a re-spelling; a structural insert retains
  the surviving entities and lowers the new ones; a `GeneralComponent` producer
  keyed by the token occurrence keeps its identities while one payload moves; and
  the negative control pins that a component taking a *spelling* as an argument
  is content-addressed (a new invocation).
* STLC example (22): a literal re-spelling changes **no** scope-graph node, edge,
  or payload (18/18, 29/29) and keeps the literal's evaluation row; a rename
  keeps every identity and updates labels; a structural append keeps the
  untouched `1` binding, the module scope, and the shared type scopes.
* `observatory_json` (3): the sink's three identity cases.
* The syntax fixture corpus (incremental tree equals a from-scratch parse at
  every revision, with work bounds) stayed green; `nodes_added` bounds may only
  go down.

## Consequences

* A re-spelling and a rename are payload-only edits at every layer.
* A structural edit re-keys exactly the constructs whose extent changed. That is
  the honest limit of an extent identity that stays unique without a host key: a
  path or one-sided anchor identity would collide for nesting or break on
  insertion.
* A row of `Evaluation.outcomes` is "this term in this environment"; a changed
  environment is a different row. The scope graph is untouched by such an edit.
* Known limitation, documented in `irrigation-macros/src/lower.rs`: a value that
  goes through an iterator adapter (`cloned`, `rev`, ...) carries no node, so a
  local bound from one is untracked and a later `if let`/`match` on it keeps its
  first answer. Write the branch where the read is. (A future fix can classify
  `cloned`/`copied` by receiver, so `Option::cloned` is not treated as an
  iterator adapter.)
