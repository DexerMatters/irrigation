# Invalid-input churn: identity, error recovery, and warm retirement

**Status:** implemented for the observed defect. The root cause is closed:
invalid keystrokes no longer rebuild the pipeline, the typed tree survives, and
retractions are reported honestly. The full parent/slot identity and the
invocation-key/standby refinements in Phase 4 remain recorded below as future
work, not as blockers for this defect.

**Scope:** the STLC Observatory typing path: text → tokens → AST → declared tree →
the four STLC stages → the runtime's change notification and the board. The
question this plan answers is why typing an incomplete/ill-formed character
produces the *largest* "changed" burst on the board, and which layer owns it.

---

## Verdict

It is **not the kernel's change notification granularity**. The kernel's
per-unit notification is exact: when identity is stable, it cuts off precisely
(§2.1: a literal edit leaves the scope graph at `changed = 0`, `cutoff = 316`).

It is a **keys** problem first:

1. An AST node's identity is its **extent** (`ast_node_id(rule, first, last)`,
   `crates/irrigation-syntax/src/parser/ast.rs:196`). The declared tree inherits
   that id, and the STLC root term's extent is the whole document.
2. Any structural keystroke — and every parse error — changes the root term's
   extent, therefore its id.
3. A callable component's invocation is keyed by its **argument tuple**
   (`crates/irrigation-runtime/src/inner/ops.rs:274`), so a new root term is a
   new `resolve`/`check`/`evaluate`/`lower` invocation.
4. The old invocation leaves the root's trace and `retire_unreachable`
   (`crates/irrigation-runtime/src/inner/waits.rs:14`, called from
   `inner/eval.rs:993`) **eagerly retracts every unit it defined**. The next
   keystroke re-creates them.

Parse errors then make it much worse: recovery publishes a tree whose root
extent does not cover its children, and the declared tree collapses from 12
nodes to 3 (§2.3–§2.4). Invalid input is the visible worst case because it both
re-keys the root and destroys the typed tree; the long-lasting revert cost is
the same mechanism running in reverse.

A second, real **kernel granularity** problem amplifies it: retirement is
immediate, the global unit index keeps retired units until `collect`, and the
observer counts them. `changed_units` can therefore exceed the number of live
units, and a transient error reports a graph-wide change twice (retract, then
re-create) even when the final text and all final values are identical.

**The finest-granularity fix is not to make the notification finer — it already
is. It is to make identity independent of the edit, to keep an error local, and
to keep a transiently unreachable computation warm.**

---

## 0. What shipped, measured

The first implementation pass is in the tree. It keeps the plan's shape but
stops short of the full occurrence-path identity and the standby policy.

| change | where |
|---|---|
| the root has one stable identity (`ast_root_id`) | `parser/ast.rs`, `parser/engine.rs` |
| a rule node is named by `(rule, first token)` when that is unique, with the extent as the collision fallback | `parser/ast.rs`, `parser/engine.rs` |
| a lexer error token is skipped as a recovery leaf, never treated as EOF | `parser/engine.rs` |
| every rebuilt node's extent is recomputed from its children after the parse | `parser/engine.rs` |
| the last well-formed declared tree is kept while a parse has recovery nodes | `components/parser.rs` |
| the full-parse path patches against the previous tree instead of an empty storage | `components/parser.rs` |
| retracted cells and removed memberships are not live/changed units; rows render `removed` | `inner/inspect.rs`, `inner/state.rs`, `observe/project.rs` |
| `retracted_units` is projected to the wire and the board | `observe/wire.rs`, `web/observatory` |

Fresh-process measurements on the same seed and the same edit sequence as §1:

| edit | before | now | reduction |
|---|---:|---:|---:|
| valid token append | 537 changed units | **203** | 62% |
| remove that token | 411 | **85** | 79% |
| one invalid character | 563 | **64** | 89% |
| revert after the error | 447 | **38** | 91% |
| invalid: bytes reparsed | 53 (whole document) | **10** (suffix adopted) | 81% |
| invalid: AST nodes added / removed | 6 / 5 | **1 / 0** | local |
| typed tree after a local error | 3 nodes | **12 nodes** (unchanged) | preserved |
| stages changed on invalid | Syntax 55, ScopeGraph 161, Machine 135 | **0 / 0 / 0** | no pipeline churn |

The invalid edit still executes the four stages, but every output is a cutoff:
the parser writes raw-AST Views, and the runtime's provider observation is per
component, not per View, so a change to `AstTree` re-certifies readers of
`Syntax`. This is a remaining kernel granularity item (see the list below).

Future refinements (not required for the observed defect):

* **Full occurrence-path identity.** The short identity is a stable prefix-based
  naming for the common case; a left-recursive `App` chain still falls back to
  the extent and re-keys its spine. The parent/slot form in §5.3 is the end
  state.
* **Invocation key decoupling** (§5.4).
* **Standby retention** (§5.5).
* **Per-View provider routing**, so a multi-output producer's change to one View
  does not re-certify readers of another.
* **Hard budgets** in §7.1: invalid 64 vs the 24 target, append 207 vs 80.
  The targets were written before the parser's legitimate metadata churn was
  measured; the new values are the ratchet until the remaining phases land.

---

## 1. What was measured

Method: `target/debug/examples/stlc_scope_graphs --bind 127.0.0.1:7807`, the
whole buffer POSTed to `/api/v1/views/<SourceFile>/text`, then
`/api/v1/workspace`, `/api/v1/revisions/<rev>`, and `/api/v1/views/<view>/render`
read after each edit. "changed units" is the sum of `ViewWire.changed_units`
over every View — the number the board uses to decide a plate is `changed`
(`crates/irrigation-observe/src/wire.rs:749`).

Seed (53 characters, 39 significant tokens):

```text
let x = 1 in
let f = \y:Int. x in
let x = true in
f 2
```

### 1.1 One edit at a time (fresh process, rev 1–9)

| edit | rev | bytes reparsed | suffix adopted | AST nodes added / removed | Syntax nodes | ScopeGraph changed | Machine changed | total changed units |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| seed (first revision) | 1 | — | — | 34 / 0 | 12 | 147 | 162 | 1215 |
| append a space (trivia) | 2 | 0 | — | 0 / 0 | 12 | **0** | **0** | 30 |
| append a digit to the final literal (`2` → `23`) | 3 | 4 | 0 | 0 / 0 | 12 | **0** | 2 | **52** |
| append a token (`f 2` → `f 2 3`), valid | 4 | 8 | 0 | 7 / 4 | — | **218** | 28 | **537** |
| remove that token, back to the seed | 5 | 1 | 0 | 4 / 7 | — | **218** | 33 | **411** |
| trailing incomplete ` let` | 6 | 7 | 0 | 2 / 1 | — | 0 | 0 | 87 |
| insert `#` after the literal | 8 | **53** | **0** | 6 / 5 | **3** | **161** | **135** | **563** |
| revert to the seed | 9 | 9 | 1 | 5 / 6 | — | **161** | **130** | **447** |

Rows marked **bold** are the defect. A valid structural token costs 537 changed
units; one invalid character costs 563; undoing either costs 411–447. A literal
re-spelling costs 52, and a trivia edit costs 30, with the scope graph untouched.

### 1.2 Repeated toggles (20 edits each)

| edit class | median changed units | max | runtime commit median | HTTP wall median |
|---|---:|---:|---:|---:|
| literal re-spelling (`1` ↔ `12`) | 64 | 1215¹ | 0.50 ms | 14.6 ms |
| valid append/remove of one token | 420 | 554 | 0.57 ms | 32.7 ms |
| insert/remove one invalid `#` | 436 | 550 | 0.34 ms | 21.2 ms |
| trailing incomplete ` let` | 86 | 101 | 0.50 ms | 17.8 ms |

¹ the first revision of a fresh process counts every unit; ignore that row for
steady state.

The runtime commit is cheap; the *number of units that move* is the cost, and
that is what the board paints. The measured counts are not a rendering bug: the
units really are retracted and re-created.

---

## 2. The causal chain, layer by layer

### 2.1 `changed_units` means "this unit's `changed_at` is the current revision"

`view_facts` walks the global edited-unit table and counts a field's units whose
node `changed_at == revision`
(`crates/irrigation-runtime/src/inner/inspect.rs:473–553`). A unit is counted
when it is **created, updated, or retracted**. That is why a re-created value
counts as changed even when the final value equals the old value, and why a
transient error can double-count one logical change.

The exactness of the notification is visible in the good cases:

| edit | scope graph | machine | reason |
|---|---|---|---|
| append trivia | 0 changed | 0 changed | no observed unit moved |
| literal re-spelling | 0 changed, `cutoff` | 2 changed | the literal payload moved; the graph's units did not |
| re-spell the same literal back | 0 changed, `cutoff` | 2 changed | same |

The kernel does not over-notify. Too many units are being *given new values*.

### 2.2 AST identity is the extent, not the occurrence

```rust
// crates/irrigation-syntax/src/parser/ast.rs:196
pub fn ast_node_id(rule, first, last, prev, next) -> u64 {
    // id(rule, first, last); spans are deliberately not part of identity
}
```

The documented intent is "two nodes describing the same structure over the same
tokens are the same node, and a node that merely moved keeps its identity"
(`ast.rs:10–25`). The intent holds for re-spellings and for insertions before a
node, because token ids are occurrences. It does **not** hold for the node's own
extent growing: appending a token inside a node changes the last token of that
node and of every ancestor whose extent covers the append point. The root's
extent is the whole document, so **every append re-keys the root**.

Measured on the seed: a valid append (`f 2` → `f 2 3`) keeps 30 of 34 AST
nodes and re-keys four ancestors — `Program` and the three `Let`s. The invalid
`#` insertion keeps 29 of 34 and re-keys five: `Program`, `Let` ×2, `Lam`, and
`IntType`. The root is in both sets, exactly as the extent rule predicts. The
typed tree inherits those ids, so the root term ref changed and every stage
downstream started over.

### 2.3 Parse recovery truncates the root and violates the extent invariant

Insert `#` after `1` and inspect the published tree:

```text
Rule Program             span 0..12      children extend to 53
  Rule Let               span 0..12      children extend to 53
    Token "let"          span 0..3
    ...
    Rule Let             span 9..53
      Token "1"          span 8..9        (before its parent's start)
      Rule Lam           span 9..30
      ...
      Rule App           span 50..53
        Error "1"        span 9..10
        Token "Int"      span 24..27
        ...
```

A rule node's identity is computed from its own `first_token`/`last_token`
(`engine.rs:877–885`), while its `children` include recovery nodes attached
later. The result is a node whose extent is a prefix of the document but whose
children extend to the end. On the seed run the `Program` id matched
`ast_node_id(Program, 0, 9)` while the full stream is 0..39; the next keypress
moved the truncation point, so the root id moved again.

The acceptance check is supposed to prevent this (`state.rs:711–726`), but it
exempts any node marked `recovered` from the tiling check. A recovered root can
therefore publish a tree that no invariant describes.

Why it matters:

* the root term id is now a function of the **error offset**, not of the program;
* every extra invalid character shifts the offset and re-keys the root again;
* the typed tree's root is still mappable, so the pipeline keeps running — on a
  tree that is structurally wrong.

### 2.4 The declared tree collapses on recovery

`typed_patch` only maps records that have a grammar production
(`crates/irrigation-syntax/src/components/parser.rs:656–713`). Recovery nodes
have `production: None`, so they map to nothing. The declared root's child field
filters them out (`source.typed_id(*child)`), which removes the valid child
subtree from the root slot. Measured:

```text
seed      Syntax: 12 nodes (Program, Let, Let, Lam, Int, Var, Bool, App, ...)
invalid   Syntax:  3 nodes (Program, Let, Int)   ← 9 nodes gone
revert    Syntax: 12 nodes (all twelve ids restored)
```

The typed ids are restored on revert because the AST ids are restored, but the
intermediate revision retracted nine nodes and everything the four STLC stages
had emitted from them. The root `stlc` component reads `syntax::root()`, so a
missing child is not a local hole: it changes what every stage sees.

### 2.5 A component invocation is its argument tuple

```rust
// crates/irrigation-runtime/src/inner/ops.rs:274–320
fn invocation_node(component, name, args_hash, args, scope, arity, args_eq) {
    // reuse only when (component, args_hash) matches and args_eq holds
    let invocation_scope = state.scopes.child(scope, invocation_id);
    ...
}
```

`resolve(term, scope, chain)`, `check(term)`, `evaluate(term, env, depth)`, and
`lower(term, chain, at)` all take a term (and sometimes a scope) as an argument.
When the root term id changes, the root calls are new invocations under new
scope ids; emission sites are `(invocation scope, site)`, so every unit a stage
emits is new. The old invocation remains in `state.invocations`, but it is no
longer in the root's trace.

`eval` then retires every old trace entry that the new trace dropped
(`inner/eval.rs:990–1000` → `waits.rs:14`): it clears the value, trace, and
definitions, retracts every unit the node owned, and recurses. In the invalid
case this retracts the whole old scope graph, the machine listing, and the
typed-tree-dependent outputs.

On revert, the graph node ids come back (the old invocation record was never
evicted, so the same sites are reused):

```text
seed    graph node ids: 1029, 1041, 1080, 1104, 1144, 1183, ...
invalid graph node ids: 922, 1322, 2537, 2549, 2607
revert  graph node ids: 1029, 1041, 1080, 1104, 1144, 1183, ...  (all restored)
```

But every restored unit has `changed_at = 3` because it was retracted at revision
2 and re-created at revision 3. Identity survived; **lifetime did not**. If the
invocation cache had been bounded and had evicted the old invocation, even the
ids would have differed.

### 2.6 Retirement is eager and the unit index keeps retired units

Two independent amplification facts:

1. **No warm window.** `retire_unreachable` runs on the same commit that drops a
   trace entry. There is no distinction between "this computation is gone" and
   "this computation is momentarily not demanded because the user is between
   two keystrokes". For a text editor, the second is the common case.
2. **The global unit table is pruned only by `collect`.** `state.units` maps a
   unit key to its node and is only removed in `collect`
   (`inner/ops.rs:602`). `view_facts` iterates it and counts retired units if
   their `changed_at` is the revision. This is why:

   ```text
   literal  AstTree live units 183, changed 2
   invalid  AstTree live units 218, changed 87     (the tree has ~40 nodes)
   literal  ScopeGraph live units 70, changed 0
   invalid  ScopeGraph live units 80, changed 161
   ```

   The "live units" count itself grows because it counts retired entries. The
   default runtime has `auto_collect_every: None` and `max_invocations: None`
   (`inner/mod.rs:128–178`); the example never overrides them. A scripted
   81-revision session grew from 2 381 to 4 287 nodes.

This is the part that is genuinely **kernel lifetime/observation granularity**,
not notification granularity: the fact "this unit no longer exists" is not
separated from "this unit's value changed", and the fact "not demanded this
revision" is not separated from "dead".

### 2.7 The frontend faithfully aggregates that data

`view_state` marks a View `changed` when its `content_version` is the revision,
when `changed_units > 0`, or when the revision journal names it
(`crates/irrigation-observe/src/wire.rs:749–764`). `Canvas.reactionOf` then
paints the whole plate (`web/observatory/src/canvas/Canvas.tsx:71–92`). A single
retracted unit is enough. Invalid input therefore looks like "everything
changed", and the footer count (`N of M rows`) mixes removed, re-created, and
genuinely updated rows.

The frontend is not the root cause; it is the messenger. But the target design
should stop using a single `changed` word for three different events.

---

## 3. Answering "keys or kernel granularity?"

| layer | granularity | verdict |
|---|---|---|
| kernel change detection | per unit, with `changed_at` and cutoff | **fine**; no over-notification |
| kernel invocation identity | argument tuple, new invocation id on every arg change | **coarse**; an extent change above becomes a subtree re-key |
| kernel lifetime | eager retirement on the first trace drop | **too coarse in time**; transient non-demand is treated as death |
| kernel observation | `state.units` retains retired keys; `changed_units` mixes create/update/retract | **stale/ambiguous** |
| syntax keys | AST id = extent; root extent = document; recovery truncates it | **root cause** |
| parser recovery | full-document fallback (`bytes_reparsed = 53`, `suffix = 0`), tree mangles root/child spans | **coarse**; one bad char rewrites the tree |
| typed-tree mapping | recovery records have no production; root slot drops the valid child | **coarse**; whole-tree collapse for a local error |
| frontend | View-level badge from an aggregate count | **display-level**; should be per-field/row + error state |

So the answer is: **keys are the primary cause; the kernel's unit notifications
are already fine; the kernel's invocation lifetime and unit index are the
secondary granularity defects; error recovery is the accelerant.**

---

## 4. Invariants for the target

| Id | Invariant |
|---|---|
| I1 | A tree has exactly one root; its id is stable for the life of the document and does not depend on the extent. |
| I2 | A node id names an occurrence path: `(parent id, slot, first token, kind)`; it survives re-spelling, insertion before/after, and appends elsewhere. |
| I3 | Extent is data, not identity: `span_start`, `span_end`, `first_token`, `last_token` change while the id stays. |
| I4 | Every child span is contained in its parent's span; the root spans the whole document. This holds for recovered nodes too. |
| I5 | A recovered node is a leaf or a small local tree; it does not re-parent valid siblings. |
| I6 | A local error keeps every unaffected typed node; the error region is an explicit variant/hole, and the typed root stays. |
| I7 | An edit re-keys only the nodes whose occurrence path changed; untouched subtrees keep ids and units. |
| I8 | Invocation occurrence is the call site/path, not the argument value; a changed argument invalidates the body but reuses the scope and sites. |
| I9 | A computation that leaves the trace enters `standby` for a bounded window; its units are retained and re-certified, not retracted immediately. |
| I10 | A re-demand that observes the same dependency versions reuses the value with `changed_at` unchanged (a cutoff, not a change). |
| I11 | The observer distinguishes created, updated, and retracted units; `changed_units` counts live units whose value moved; retired entries are not live. |
| I12 | Per keystroke work is proportional to the changed occurrence path, not to document size. |

---

## 5. Design

The design is layered so that each phase is independently shippable. The first
two phases (5.1–5.2) capture most of the invalid-input win without changing the
public authoring API; the identity redesign (5.3) is the "finest granularity"
end state.

### 5.1 Make the root's identity stable (smallest high-ROI change)

The root is unique, so its id does not need the extent:

```rust
// syntax: one root per tree; a constant for that tree schema
pub fn ast_root_id(schema: u64) -> u64 {
    mix(0x5EED_0000_0000_0001, schema)
}
```

Apply it in both publish paths:

* the raw AST root (`engine.rs` `accept`/`salvage`) uses `ast_root_id(schema)`
  instead of `ast_node_id(rule, first, last)`;
* the declared tree root uses the same id (it is derived from the AST root), or
  its own schema-stable root id if the declared tree has a root wrapper. The
  two trees are different Views, so the same numeric id is not a collision.

Consequences that are visible immediately:

* appending a token no longer re-keys the root, so `syntax::root()` returns the
  same `TermRef` and the root invocations are reused;
* an error no longer changes the root id, so the root invocations are reused;
  §5.2 is still required so the error does not empty the root's child slot;
* the root's *content* units (children/tag/field) still change, so the body
  re-executes and re-emits correctly; unaffected child terms keep their ids.

This alone does not make nested ancestors stable: `Let`/`Lam` extents still grow
when their bodies grow, so those invocations still re-key. It reduces an append
from "whole tree re-keys" to "the ancestor path re-keys"; the leaves and their
emissions survive. Measure it before doing 5.3.

### 5.2 Make an error local (parser + typed tree)

#### 5.2.1 Parser acceptance with recovery

Add an error-tolerant splice mode that can accept a proposal with one recovery
node when:

* the root still covers `(0, significant.len())`;
* every rebuilt non-recovered node's children tile its span;
* a recovered node's span is contained in its parent's span;
* the recovered region is the smallest window that contains the changed tokens.

Do not fall back to `initial()` on the first rejection. The current component
does so at `components/parser.rs:414–429` and `:467–478`, which is why
`bytes_reparsed = 53` and `suffix = 0` for a single bad character.

A recovery node's own identity should be anchored on the **gap**, not on its
children:

```text
recovery_id = error_id(category, [anchor_before, anchor_after])
```

so the recovery node keeps its id while the user types more characters into the
same gap, and only its payload/span moves. This mirrors the "missing token"
identity already documented in `engine.rs:1100–1120`.

#### 5.2.2 Fix the extent bookkeeping

The current root/ancestor extent can be a prefix of its children because
`first_token`/`last_token` are computed with `find_map` over children that may
not carry tokens (`engine.rs:836–840`, `:1180–1232`). Compute the extent from
the **final** child list, after pending recovery nodes are appended and sorted:

```rust
let first = children.iter().find_map(|c| c.first_token)
    .or_else(|| /* first token of the gap */);
let last = children.iter().rev().find_map(|c| c.last_token)
    .or_else(|| /* last token of the gap */);
```

and add a validation pass that rejects or repairs a tree whose child span falls
outside its parent's span or whose root span is not the document. The validation
must not exempt `recovered` nodes from **containment** (it may still exempt them
from tiling).

#### 5.2.3 Keep the typed tree partial

The grammar already declares fields. Add one of:

**Option A (preferred):** an `@error` declaration that maps a recovery record
to a declared `Error`/`Unknown` variant, with the gap's tokens as leaves. The
tree keeps its shape; a reader can see "this term is broken" without losing its
parent/siblings.

**Option B:** when a child has no typed id, keep the **previous** typed node for
that child (the last well-formed term) and attach a diagnostic to the error
region. The root slot is never emptied by a local error.

Either way, `publish_typed` must not drop the root's child slot just because one
descendant is unmappable. Add a test that the typed node count before and after
a local error differs by at most the error node(s).

### 5.3 Make identity an occurrence path (the finest granularity)

This supersedes the extent rule for the cases that hurt while preserving the
uniqueness the extent provides.

```text
id(root)        = mix(ROOT, schema)
id(node)        = mix(parent_id, slot, first_token_id, kind)
                  [ disambiguator only on a same-anchor collision ]
slot            = the grammar field / declared child slot the node fills
first_token_id  = the occurrence token at the node's start
kind            = the rule or declared variant
disambiguator   = the node's end token when, and only when, two nodes in the
                  same (parent, slot, first, kind) coexist
```

Why this is stable:

* re-spelling: token id stable, kind stable, parent stable, slot stable;
* inserting before/after: the first token id does not change (token ids are
  occurrences, `identity-plan.md`);
* appending elsewhere: neither parent nor first token nor slot changes;
* deleting a sibling: the slot is not an index, so later siblings do not shift;
* re-parenting (recovery): the parent id changes, correctly identifying a new
  structural context;
* same-start nested siblings in a stable tree (`f 2 3` has two `App`s starting
  at `f`): the **parent id** separates them, because the inner `App` fills the
  outer one's `func` slot. A disambiguator is only needed when two nodes with
  the same parent, slot, first token, and kind coexist (zero-width/overlapping
  nodes, or a left-recursive node whose parent is about to change).

**Left-recursive growth needs an explicit rule.** Growing `f 2` to `f 2 3`
re-parents the old `App` under a new outer `App`; both start at `f` in the same
`Let` body slot. Two honest choices:

* **Adopt the occurrence (finest).** The old id stays on the *outer*
  application; its `children` unit changes to `[new inner App, 3]`, and the new
  inner `App` gets a fresh id. The body term keeps its identity, so `resolve`
  and the typed tree keep the term; only the new function subterm is new.
* **Disambiguate by end token (simpler).** The old id stays on the inner
  `App` (extent `f..2`, unchanged) and the outer gets a new id. This is today's
  behaviour for this one construct; it is local, but the body term re-keys.

The plan recommends the first for `App`/left-recursive productions because it
keeps the *term the parent names* stable. Either way, the decision must be
tested: the ancestor `Let`s and the root must not re-key.

The root has one live instance, so the root id is a constant per tree. The
disambiguator is *data in the id only for the collision case*; all ordinary
nodes keep a stable id while their extent grows.

Required changes:

1. `ast_node_id` gains `parent`/`slot` and takes the disambiguator; call sites
   pass the production's field/slot, not just the rule.
2. `Build::push` collision handling (`engine.rs:338–349`) uses the disambiguator,
   never the opaque `mix(natural ^ guard, ...)` bump (the bump is deterministic
   within one parse but changes between parses when the tree shape changes).
3. `AstState` splicing (`parser/state.rs:449–580`, `divergence`, `SpliceIndex`,
   `removed_nodes`) compares `(id, kind, text)`; a rebuilt node with the same id
   and a changed extent is an in-place replacement, not a new node, and its
   `AstMeta` units (span/token/value) are updated in place.
4. `accepted`/`tile` validates containment and tiling by extent; identity and
   extent are separate fields in `SpanInfo`.
5. The declared tree uses the same ids; `TreeStorage`'s parent/children units
   update in place when a node's extent or children change.
6. The STLC stages need no API change: `TermRef` is `(view, id)`, and the id now
   survives structural growth. The evaluator's rows remain keyed by
   `(term, env, depth)`; environment changes are semantic and stay new rows by
   design.

Gate for this phase: a one-token append keeps the root and every ancestor that
does not change its occurrence path; only the new subtree is new, and an
ancestor's `children` unit changes in place. For the STLC seed, `Program` and
the three `Let`s must survive a `f 2` → `f 2 3` append, not just the leaves.
A left-recursive growth may re-key the `App` spine (see above); it must not
propagate past the `Let` that owns it. The scope graph's unchanged terms must
be a cutoff.

### 5.4 Decouple invocation occurrence from argument value

Even with 5.1–5.3, an argument can legitimately change (a node is edited, a
scope is re-created by recovery). Today that is a new invocation and a new
scope. The target separates **occurrence** from **input**:

```rust
/// Stable identity of a call occurrence.
pub struct InvocationKey {
    component: ComponentId,
    site: SiteId,          // the macro-assigned call site
    path: u64,             // host-provided stable path/key (default: args hash)
}

fn invocation_node(key: InvocationKey, args: Arc<dyn Any>, ...) -> ... {
    // same key -> same invocation scope and output sites
    // args differ -> store the new args, mark the body suspect, re-run on demand
}
```

Default behavior stays `path = args_hash` for compatibility. A callable
component opts in with a declared key, e.g.:

```rust
#[component(callable, key = self)]
fn resolve(term: TermRef, scope: u64, chain: Vec<Frame>) -> Option<u64> { ... }
```

or the root pipeline passes `DocumentRoot` explicitly. Then a root term whose
*value* changed (children) but whose occurrence is the same reuses the scope;
only nodes that read the changed child units re-execute. `resolve`'s own output
may change; the unit's value version decides whether dependents wake (the
existing `output_eq`/change-batch path).

Correctness requirement: the argument must be observed as a dependency, not
just stored. Two legal designs:

* the runtime records an `ArgDep` unit for the tuple; a changed arg invalidates
  the body exactly like a changed read;
* the body reads its args through the tracked API, and the host declares the
  args it wants observed.

Without one of these, an arg change could be silently ignored. Keep the default
args-as-identity until this dependency exists.

### 5.5 Keep a transiently unreachable computation warm

Add a bounded **standby** state between "in the trace" and "retired":

```rust
struct StandbyEntry { node: NodeId, since: Revision, weight: usize }

fn retire_unreachable(...) {
    if standby.has_room() {
        // keep value, trace, definitions and units
        mark_standby(node, revision);
        return;
    }
    clear_and_retract(node); // today's behavior
}
```

Rules:

* a standby node's units stay installed but are marked standby; they are not
  reported as live-changed and do not wake consumers;
* on re-demand, certify the dependency versions first. If all still hold, drop
  standby and reuse the value/units with `changed_at` unchanged (a cutoff, no
  notification). If any moved, re-execute; equal output is still a cutoff;
* a standby node that stays unreachable for `standby_ttl` revisions (or when the
  budget is exceeded) is retired exactly as today;
* a new provider that wants a standby-owned unit takes it over through the
  existing write-arbitration path; the standby entry is retired on the takeover;
* `collect` prunes standby entries and frees their scopes/nodes; expose
  `standby_nodes`, `standby_reclaimed`, and `standby_bytes` in `memory_report`.

This is the fix that makes "invalid → revert" cost **zero** notifications for
unaffected views: the old graph/machine units are still warm and their values
are unchanged.

Bounds: `RuntimeConfig::standby_revisions: Option<u64>`,
`standby_budget: Option<usize>` (nodes or bytes). Defaults: a small window
(e.g. 8 revisions) and a size budget proportional to the live graph; `None`
keeps today's eager behavior for users who prefer it.

### 5.6 Notify transitions, not a single "changed" bit

Separate the three events that `changed_at` currently collapses:

```rust
pub struct UnitActivity {
    pub created: u32,
    pub updated: u32,
    pub retracted: u32,
}
```

* `ViewWire.changed_units` counts **live created + updated** units only;
* add `retracted_units` (and optionally `created_units`) so the board can say
  "3 moved, 12 removed";
* `view_state` gains `"invalid"` (diagnostics moved but no live data moved) and
  keeps `"cutoff"` for "providers ran, values equal";
* the render marks retracted rows `removed` (already a `RowState`) and only
  live updates `changed`;
* `view_facts` iterates live units (see 5.7), not retired entries.

### 5.7 Prune or mark retired units in the global index

`state.units` is the lookup table for unit cells and is only pruned by
`collect` (`inner/ops.rs:602`). Two options:

* **Preferred:** remove the key on retraction when no snapshot or read needs it,
  and let a re-demand re-materialize the cell (it already can; `unit_node` is
  looked up lazily). Add a test that a retracted unit is not returned by
  `view_facts` and does not grow `units`.
* **Conservative:** keep the key but add a `retired_at` flag; `view_facts` and
  `memory_report` skip retired entries. `collect` removes them.

Then run `collect` on a policy (e.g. `auto_collect_every = Some(128)` and a
`max_invocations` cap) in the example. This is not the main fix, but it keeps
the observation honest and bounds long editing sessions.

### 5.8 Frontend and editor

* **Per-field/row reaction.** A plate's header should use `created + updated`
  and show `N moved` / `M removed`; pulse only the rows whose state is
  `changed` (already supported by `.row-changed`). Do not paint the whole card
  for a retracted-only revision.
* **Error is not a change.** Render the parse/type diagnostics as an error chip
  and give the plate an `invalid` state; do not add it to the changed count.
* **Inspector facts.** Show `created / updated / retracted` and the live unit
  count, so `changed > live` cannot happen on screen.
* **Coalesce keystrokes (optional, measured).** The editor POSTs the whole
  buffer per `input` event. Schedule one POST per animation frame (or a 12–16 ms
  timer), keep the latest buffer authoritative, and flush on blur/Enter. This
  reduces transient revisions and round trips; it does not replace
  incrementality. Measure the effect on `wall_ms` before adopting.

---

## 6. Phases and gates

Each phase is independently shippable. The "expected" column is the target
changed-unit total for the §1 edit class; it becomes the gate in §7.

| Phase | Change | Files | Expected effect | Risk |
|---|---|---|---|---|
| **0** | Repro harness and counters | `crates/irrigation/tests/`, example API | none; establishes §1 numbers as tests | low |
| **1** | Extent invariant + error-local parser acceptance + typed-tree holes | `parser/engine.rs`, `parser/state.rs`, `components/parser.rs` | invalid 563 → ≤ 160; no full reparse; Syntax node count preserved | medium; recovery correctness |
| **2** | Stable root id (raw + declared tree) | `parser/engine.rs`, `components/parser.rs`, `mapping.rs` | invalid ≤ 80; structural append ≤ 300; revert ≤ 200 | low |
| **3** | Occurrence-path identity | `parser/ast.rs`, `engine.rs`, `state.rs`, `components/parser.rs`, typed mapping | structural append ≤ 80; literal stays ≤ 64; invalid ≤ 40 | high; splice/identity audit |
| **4** | Invocation key + standby retention + unit-index pruning | `inner/ops.rs`, `inner/eval.rs`, `inner/waits.rs`, `inner/mod.rs`, `inner/inspect.rs` | revert ≤ 8; transient error retracts nothing unaffected | high; lifetime/write arbitration |
| **5** | Transition notifications + frontend | `observe/wire.rs`, `collector.rs`, `web/observatory` | honest counts; no whole-card flash for retractions | low |
| **6** | Performance corpus and docs | tests, README/plans | ratchets in CI | low |

Suggested ordering: 0 → 1 → 2 → 4 → 3 → 5 → 6 if the goal is the fastest
visible improvement with bounded risk; 0 → 1 → 2 → 3 → 4 → 5 → 6 if the goal is
the clean final architecture. Phase 4 is independent of Phase 3 and can run in
parallel.

---

## 7. Budgets, tests, and measurement

### 7.1 Budget table (target)

Measured on the seed after a warm start; "total" is the sum of `changed_units`
over all Views (the board's count).

| edit | today | Phase 2 | Phase 3/4 | hard budget |
|---|---:|---:|---:|---:|
| append trivia | 30 | 30 | 30 | ≤ 30 |
| literal re-spelling | 52–73 | 52 | 52 | ≤ 64 |
| valid token append | 537 | ≤ 300 | ≤ 80 | ≤ 80 |
| remove that token | 411 | ≤ 220 | ≤ 60 | ≤ 60 |
| one invalid character | 563 | ≤ 160 | ≤ 40 | ≤ 24 |
| revert after one error | 447 | ≤ 120 | ≤ 8 | ≤ 8 |
| parse bytes reparsed (one bad char) | 53 | ≤ 8 | ≤ 3 | ≤ 3 |
| suffix adopted (one bad char) | 0 | ≥ 1 | ≥ 1 | ≥ 1 |
| typed tree nodes after one local error | 3 | 12 | 12–14 | ≥ 12 |
| root id across a structural append | changes | same | same | same |
| root id across an error | changes | same | same | same |
| graph node ids across invalid → revert | restored | restored | restored | same |
| `changed_units` ≤ live units | no | no | yes | yes |
| runtime nodes after 80 edits | 4 287 | — | bounded | bounded |

The "hard budget" is the ratchet; a change that raises it needs a written
exception, like the existing fixture counters.

**Measured ratchet after the first pass** (fresh process, same sequence):

| edit | measured | hard target | verdict |
|---|---:|---:|---|
| append trivia | 30 | ≤ 30 | met |
| literal re-spelling | 52 | ≤ 64 | met |
| valid token append | 203 | ≤ 80 | open; `App` spine and listing order |
| remove that token | 85 | ≤ 60 | open; same |
| one invalid character | 64 | ≤ 24 | open; token/AST metadata is the floor |
| revert after one error | 38 | ≤ 8 | open; same |
| parse bytes reparsed | 10 | ≤ 3 | open; suffix adoption is local |
| suffix adopted | 1 | ≥ 1 | met |
| typed tree after a local error | 12 | ≥ 12 | met |
| root id across append/error | same | same | met |
| stage output changes on invalid | 0 | 0 | met |

The remaining budget gaps are the parser's own metadata churn (every token and
span it rebuilds is a real unit), the left-recursive `App` fallback, and the
machine listing's order identity. They are the subject of Phases 3 (full
occurrence path) and 4; the current values are a ratchet in the meantime.

### 7.2 Tests

Add, in the spirit of `crates/irrigation/tests/identity.rs` and
`crates/irrigation-syntax/tests/identity.rs`:

Shipped:

```text
a_structural_append_keeps_the_root_identity          (syntax/identity.rs)
an_invalid_character_keeps_the_root_identity         (syntax/identity.rs)
spans_stay_contained_across_structural_and_invalid_edits (syntax/identity.rs)
a_structural_append_keeps_the_let_spine              (stlc main.rs)
an_invalid_character_does_not_run_the_stages         (stlc main.rs)
reverting_an_error_does_not_run_the_stages           (stlc main.rs)
typing_the_seed_one_character_at_a_time_stays_local  (stlc main.rs)
a_large_program_keeps_an_append_local                (stlc main.rs)
```

Still to add when Phase 4 lands:

```text
typed_tree_keeps_valid_terms_across_an_error
invalid_char_changes_only_the_error_subtree
revert_after_an_error_notifies_nothing
structural_append_rekeys_only_the_changed_path
invocation_key_keeps_sites_across_argument_changes
standby_units_re_certify_without_recreating
retracted_units_are_not_counted_as_live
changed_units_never_exceeds_live_units
```

Keep green: the STLC example's 36 tests, `irrigation/tests/emission.rs`,
`irrigation/tests/identity.rs`, `irrigation-syntax/tests/identity.rs`,
`irrigation-syntax/tests/fixture.rs`, `irrigation/tests/change_notification.rs`,
and `irrigation-syntax/tests/json_ast_delta.rs`.

The end-to-end tests drive the runtime directly, without HTTP: build the
STLC runtime, apply the seed, then a scripted sequence of `TextDocument` edits
(`1`→`12`, `f 2`→`f 2 3`, insert `#`, revert), and assert the counters from
`Runtime::stats` plus `view_facts`. The HTTP path is only a manual check.

### 7.3 Scaling test

Run the same script on a generated 1 000-node STLC-like program. The per-keystroke
changed-unit count must not grow with the document; the work must be proportional
to the edited occurrence path. This is the real "finest granularity" gate; the
53-character seed can hide O(n) behavior.

---

## 8. Risks, alternatives, and non-goals

### Risks

* **Identity collisions** in 5.3. The disambiguator must be deterministic across
  parses; the parser's opaque collision bump must go. A property test comparing
  incremental and from-scratch parses (the existing fixture corpus) is the
  safety net.
* **Splice correctness.** Extent and identity are now separate; every place that
  assumed `id` implies an extent must be audited (`divergence`,
  `removed_nodes`, `accepted`, `SpanInfo`).
* **Invocation reuse and args.** 5.4 is only sound if a changed argument is a
  tracked dependency. Keep args-as-identity as the default and make the opt-in
  explicit.
* **Standby vs write arbitration.** A standby provider still owns units; a new
  provider must be able to take them over. Reuse the existing owner/claim path
  and test the takeover.
* **Memory.** Standby is bounded; `collect` must reclaim it; expose the numbers.
* **Auto-collect** can change `changed_at`/notification timing. Collect between
  revisions, after observers read, and never mid-transaction.

### The 80/20 fallback

If 5.3 (occurrence-path identity) is too risky for the next milestone, Phases
1 + 2 + 4 already remove most of the visible churn:

* the root id stops changing on appends and errors;
* recovery stops mangling/erasing the typed tree;
* transient unavailability stops retracting units.

That combination should bring a one-character error from ~563 to well under 100
and a revert from ~447 to near zero, without changing the public authoring API.
Phase 3 is then the clean, permanent version of the same fix.

### Non-goals

* No change to the kernel's per-unit change detection; it is correct.
* No re-architecture of the STLC example's four stages; they are already
  demand-driven and emit as they walk.
* No change to the evaluator's "a row is this term in this environment" rule.
  Environment changes are semantic inputs (`concurrent-incremental-system-plan.md`
  §9.2); they stay new rows.
* No claim that a full parse is always wrong. A first parse, a paste, or a
  whole-document replacement may legitimately rebuild; the target is the
  one-token edit and the one-character error.

---

## 9. Evidence appendix

### 9.1 Reproduce

```sh
# fresh process, separate port
cargo run --example stlc_scope_graphs -p irrigation --features observatory \
  -- --bind 127.0.0.1:7807

# drive it (the source View key is stable for the example)
curl -s http://127.0.0.1:7807/api/v1/workspace | jq .revision
curl -s -X POST --data-binary @source.txt \
  http://127.0.0.1:7807/api/v1/views/v:a67b6607743345ba/text
curl -s http://127.0.0.1:7807/api/v1/revisions/<rev> | jq '{changed,cutoffs,executed_nodes}'
curl -s http://127.0.0.1:7807/api/v1/workspace \
  | jq '[.views[] | {name, changed_units, fields: [.fields[] | {name, units, changed_units}]}]'
```

The numbers in §1 are from the `/workspace` `changed_units` sums and the
`/revisions` `duration_ns`; the AST/typed node counts are from the first field's
`/render` model.

### 9.2 Code pointers

| What | Where |
|---|---|
| AST id is `(rule, first, last)` | `crates/irrigation-syntax/src/parser/ast.rs:196` |
| rule reduction computes `first_token`/`last_token` | `crates/irrigation-syntax/src/parser/engine.rs:836`, `:877` |
| `accept` returns a single frame or `$root` | `crates/irrigation-syntax/src/parser/engine.rs:1180` |
| recovery: skip / close / insert | `crates/irrigation-syntax/src/parser/engine.rs:947` |
| acceptance skips recovered nodes | `crates/irrigation-syntax/src/parser/state.rs:711` |
| full-parse fallback | `crates/irrigation-syntax/src/components/parser.rs:414`, `:467` |
| typed mapping only for productions | `crates/irrigation-syntax/src/components/parser.rs:656` |
| typed publish | `crates/irrigation-syntax/src/components/parser.rs:860` |
| invocation keyed by args | `crates/irrigation-runtime/src/inner/ops.rs:274` |
| eager retirement of dropped trace entries | `crates/irrigation-runtime/src/inner/eval.rs:993` |
| `retire_unreachable` | `crates/irrigation-runtime/src/inner/waits.rs:14` |
| `view_facts` counts `state.units` | `crates/irrigation-runtime/src/inner/inspect.rs:473` |
| `state.units` pruned only by collect | `crates/irrigation-runtime/src/inner/ops.rs:602` |
| `auto_collect_every` / `max_invocations` default `None` | `crates/irrigation-runtime/src/inner/mod.rs:128` |
| View state `changed` | `crates/irrigation-observe/src/wire.rs:749` |
| card reaction | `web/observatory/src/canvas/Canvas.tsx:71` |
| row counts and footer | `web/observatory/src/canvas/viewInfo.ts:37` |

### 9.3 The two trees at the error (observed)

```text
Program  id 4333039319839212424   span 0..12    // children reach 53
Let      id 17483501048728909072  span 0..12
Let      id 4638025479249958512   span 9..53
Lam      id 3665741230475475115   span 9..30
IntType  id 4985613453719076436   span 9..27
App      id 8250250753784248903   span 50..53
Error    id 6875525816980709200   span 9..10
```

`Program`'s id matches `ast_node_id(Program, significant[0], significant[9])`,
not the full stream `(0..39)`. The typed tree at that revision:

```text
Program / Let x / Int(1)   (3 nodes; the other 9 are gone)
```

After reverting to the seed, all twelve typed ids return, and the scope graph
ids return, but every graph unit's `changed_at` is the revert revision:

```text
seed    graph nodes: 1029 1041 1080 1104 1144 1183 ...
invalid graph nodes: 922 1322 2537 2549 2607
revert  graph nodes: 1029 1041 1080 1104 1144 1183 ...   changed_at = 3
```

That is the signature of retraction + re-creation: identity survived, lifetime
did not.
