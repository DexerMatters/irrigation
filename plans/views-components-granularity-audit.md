# Views and components: granularity and the extent of an increment

Status: audit. This is not a plan. It records what the current kernel actually
does, measures the shape of one change, and lists the remaining gaps in priority
order so a follow-up can choose which ones matter. It was written against
`8fb3974` plus the working tree.

"Granularity" and "increment extent" are different questions:

- **Granularity** — what a read depends on, and what a definition writes. This is
  the unit matrix in §3.
- **Increment extent** — how far the work caused by one change reaches. One edit
  has four separate extents (§1), and they are not the same size.

The short version: reads and notifications are per-unit; **commit and
publication are whole-value for every collection family**. The fine granularity
is downstream of the edit, not inside it.

## 1. The four extents

| Extent | What it measures | Mechanism | Shape |
|---|---|---|---|
| **Commit** | units staged, content rebuilt, content compared, structure projected | `Edit::commit`, `Inner::apply_definitions`, editors' `Drop`, `refresh_structure` | per-unit writes, but whole-content equality and whole-structure projection |
| **Invalidation** | nodes marked suspect | reverse `dependents`, `state::invalidate` | the transitive dependency cone |
| **Execution** | node bodies that actually run, plus the loops inside them | ordered trace validation, cutoff, `Inner::eval` | data-dependent; per-expression and per-member, but a body/operator loop still runs end to end |
| **Observation** | subscribers woken | content version, unit versions | whole View, a unit set, or one entry |

A change can be narrow in one extent and wide in another. The probes in §6 show
`invalidated > executed > user-code work`, and separately show a commit that is
linear in the collection while nothing is demanded at all.

The contract in the README ("an unchanged output keeps its change version, so it
does not notify its consumers") is an *observation/execution* statement. It is
true. It does not say the edit was cheap.

## 2. The unit model in one page

- A View's content is one immutable `Arc<dyn Any>`; observations are logical
  units keyed by `(ViewId, FieldId, UnitKind, entity?)`
  (`crates/irrigation-core/src/descriptor.rs:127-184`).
- `UnitKind` is `Value`, `Membership`, `Structure`, `Rank`, `Adjacency`,
  `IncomingAdjacency`, or `Content` (`descriptor.rs:127-143`). Source lists read
  rank through the structure unit; `Rank` is declared but not used as a read.
- Every unit is a node. `read_unit` records `(node, changed_at, routing)` in the
  current trace and creates an absence cell for a negative membership read
  (`inner.rs:423-535`).
- A definition is staged in the executing frame (`stage(DefinitionDraft)`) and
  commits atomically with the node's value and trace. Each draft carries a
  `PartialEq` for cutoff.
- Structure is a runtime-maintained projection of content:
  `refresh_structure` projects a field's key set/ids/members/topology/sequence,
  compares it with the previous unit value, and bumps only on change
  (`inner.rs:1421-1466`).
- A component body is one node. Every value-producing expression is an
  `eval(site, ...)` node; a component call is an invocation keyed by argument
  equality; every output is a projection node.

## 3. View granularity

### 3.1 Unit layout

"Source" is what an editor writes. "Derived" is what a component definition
writes. Membership for derived entity definitions is created by the runtime when
the first draft for that entity installs (`inner.rs:1565-1603`).

| Field | Source editor writes | Derived definition writes |
|---|---|---|
| Scalar | `Value(field)` | `Value(field)` |
| Map | `Structure`, `Membership(k)`, `Value(k)` per entry | `Value(k)` per entry; membership auto; structure refreshed |
| List (source) | `Structure(ids)`, `Membership(id)`, `Value(id)` per element | — |
| List (derived, whole) | — | one `Value(field)` holding the whole `PSeq<T>` |
| List (derived, keyed) | — | `Value(id)` per element plus one `Value(field)` holding the whole storage; structure draft marks the field keyed |
| Set (source) | `Structure`, `Membership(m)` per member | — |
| Set (derived, whole) | — | one `Value(field)` holding the whole `PSet<T>` |
| Set (derived, keyed) | — | `Membership(m)` per member plus `Structure` |
| Graph (source) | `Structure(topology)`, `Membership(n)`, `Value(n)`, `Value(e)`, `Adjacency(n)`, `IncomingAdjacency(n)`; adjacency only for dirty endpoints | — |
| Graph (derived) | — | `Value(n)`, `Value(e)`, `Structure(topology)`; membership auto; adjacency computed from topology |
| Tree | `Root`, `Membership(n)`, `Tag(n)`, `Field(n,slot)`, `Children(n,slot)`, `Parent(n)` | same layout through `TreeOutputTxn`, as one `GeneralComponent` output |
| Reduced | `Structure(sequence)` plus `Value(index)` on a point edit | one `Value(field)` holding the whole `AggSeq` |
| Join | — | `Value(k)` per joined key on its optional target View, plus the whole `BTreeMap` output |
| General output | — | whatever `OutputTxn` stages: `Value`, `Membership`, `Structure`, `Content` |

Notes that matter:

- **Map is the only family used identically as source and derived.** List and
  set split into source and derived marker types.
- **Tree has no `Structure` projection** (`tree_structure` returns `None`,
  `tree.rs:1093-1098`); identity lives in the child-slot units.
- **Reduced's structure is the whole `AggSeq`**, including the cached aggregate.
  `aggregate()` reads the structure unit (`reduce.rs:146-156`), so any point edit
  invalidates every aggregate reader, even though reading the aggregate is
  constant time.
- **Join's whole-map output is still a whole value.** The optional target map is
  the per-key path.

### 3.2 Reads

| Read | Units observed |
|---|---|
| Scalar accessor | `Value(field)` |
| `map.keys/len/is_empty` | `Structure` |
| `map.contains_key(k)` | `Membership(k)` |
| `map.get(k)` | `Membership(k)` + `Value(k)` |
| `map.to_vec` / `map_values` | `Structure` + each `Value(k)` (per-key scopes for `map_values`) |
| `list.ids/len` | `Structure` |
| `list.get(i)` (source) | `Structure` + `Membership(id)` + `Value(id)` |
| `list.to_vec` (source) | `Structure` + each `Value(id)` |
| `list.map/filter/flat_map` (source) | `Structure` + each member `Value(id)` through member nodes |
| `list.to_list/to_map` (source) | `Structure` + each `Value(id)` |
| `list.ids/len` (derived) | `Structure` |
| `list.to_vec` (derived) | the whole `Value(field)` |
| `list.get(i)` (derived) | the whole `Value(field)` (`to_vec().nth(i)`) |
| `list.get_key(id)` (derived keyed) | `Membership(id)` + `Value(id)` |
| `list.get_key(id)` (derived whole) | `Membership(id)` + `Value(id)` **read-created, not definition-owned** (see F2) |
| `list.map/filter/flat_map` (derived) | whole storage `Value(field)` + `Structure` + per-member `Membership(id)`/`Value(id)` |
| `set.contains(m)` (source or keyed derived) | `Membership(m)` |
| `set.contains(m)` (whole derived) | `Structure` |
| `set.len/to_vec/members` (any) | `Structure` |
| `graph.topology/nodes/edges/counts` | `Structure` |
| `graph.contains_node(n)` | `Membership(n)` |
| `graph.node(n)` | `Membership(n)` + `Value(n)` |
| `graph.edge(e)` | `Membership(EdgeKey)` + `Value(EdgeKey)` (see F1) |
| `graph.outgoing/incoming/neighbors` (source) | `Adjacency(n)` / `IncomingAdjacency(n)` |
| `graph.outgoing/incoming/neighbors` (derived) | the whole topology `Structure` |
| `tree.kind/parent/exists` | `Tag(n)` / `Parent(n)` / `Membership(n)` |
| `tree.field(slot)` | `Field(n,slot)` |
| `tree.child(slot)` | `Children(n,slot)` |
| `reduced.aggregate/len/get/to_vec` | the whole sequence `Structure` |

The negative cells are part of the read set: `map.get(k) == None` observes
`Membership(k)`, and `set.contains(m) == false` observes the same cell. This is
why inserting a key wakes a reader that previously saw it absent.

### 3.3 Definition and commit

Source editor: on `Drop`, each editor stages the per-entity units it touched,
then the whole structure projection, then `set_content` stages the whole content
(`map.rs:372-390`, `list.rs:502-514`, `set.rs:249-262`,
`graph.rs:648-668`). A map `insert` stages structure even for a value-only edit.

Derived definitions: a lowered component restates every definition on every run
(`definition_mode = (Vec::new(), true)`, `inner.rs:751-764`); a unit the run does
not restage is retracted. A `GeneralComponent` defaults to delta semantics: a
unit keeps its committed value unless the component retracts or `replace_all`s
it (`general.rs:519-533`).

`apply_definitions` does three whole-field things for every commit that touches a
collection field:

1. it calls `descriptor.content.define` once per draft, rebuilding the immutable
   content;
2. it compares the whole content against the previous content to decide the
   content version (`inner.rs:1629-1643`);
3. `refresh_structure` projects the whole field and compares the projection
   (`inner.rs:1645-1647`).

`Edit::commit` has the same whole-content comparison for source edits
(`edit.rs:166-190`, `216-233`): the editor's `Drop` stages `Content`, and the
commit compares it with `content.eq`, which compares the whole struct.

This is the central distinction of the audit:

> **Definition granularity is per unit. Commit extent is whole value.**

The unit comparison keeps `changed_at` narrow, so consumers are isolated. It
does not keep the write path narrow.

## 4. Component granularity

### 4.1 Lowering

| Construct | Lowered identity | What is reused |
|---|---|---|
| literal, path, field access, index, binary/unary/cast, tuple/array/struct | `eval((scope, site), ...)` | that expression only |
| `let name = expr` | the initializer's node; uses observe it | the expression and its observation |
| `if` / `match` | plain Rust control flow; condition/scrutinee is a node | the condition/scrutinee; the taken branch's nodes |
| call to a `#[component]` function | invocation `(component, arguments)`; body + outputs | the whole invocation when its args are equal |
| call to a plain function | `eval((scope, site), ...)` around the call | the whole call expression |
| `handle.map/filter/flat_map` | operator node + one member scope per identity; `Param` records the member read | the member closure by identity |
| `vec.iter().map(...)` (standard iterator) | lazy; no per-item nodes | nothing per item (it runs inside the enclosing node) |
| closure | inline; no per-expression nodes inside | the enclosing operator's per-member nodes, if any |
| loops, `return`, effect macros, async, unsafe | rejected with a diagnostic | — |

Two consequences are easy to miss:

- **A root-style component called as a plain function is not an invocation.**
  It runs its body; its inner expression nodes are reused, but the function's
  orchestration is not a node. An owned-argument component call *is* a node and
  can be skipped entirely when its arguments are unchanged.
- **A closure is inline.** Per-call identity only exists because `map`, `filter`
  and `flat_map` create a member scope keyed by the element id (or the position
  for a whole-value list) and two site nodes inside it (`list.rs:1088-1119`).

### 4.2 Invocation and outputs

`invocation_node` interns by `(component, hash(args))` and then compares the
argument tuple with `PartialEq` (`inner.rs:2803-2855`). Equal arguments from two
call sites share one invocation. A changed argument creates a *new* invocation;
the old one stays cached until reachability/`max_invocations` reclaims it.

The body node computes the whole output product. Each output is its own node
(`component.rs:148-209`), so an equal projection keeps its `changed_at` and does
not invalidate its consumers. But:

- `call_component` evaluates the body and **every** projection, and the call site
  observes the body. A consumer of the whole tuple re-executes when any output
  changes.
- `call_component_projection` / `Runtime::read_projection` evaluate the body and
  one projection. The body still runs; the sibling projection is not evaluated.
- The body is one node, so a change in any observed input re-runs the whole Rust
  body (see F7). Projections add cutoff, not partial body execution.

### 4.3 What re-executes

`Inner::eval` (`inner.rs:566-794`):

1. returns a certified value if `verified_at == revision` and a value exists;
2. claims single-flight ownership otherwise;
3. if the node has a value, certifies the observed Views' producers and validates
   the ordered trace (`validate_trace`, `inner.rs:1006-1042`);
4. on a valid trace, re-certifies without running the closure;
5. otherwise executes the closure with a fresh recorder and commits value, trace,
   and definitions atomically.

Ordered validation matters: the trace is in source order, so a branch is checked
before a branch that may have been deleted. A body whose changed dependency is
late in the trace validates the prefix, then re-executes from the start; the
prefix expressions are `eval` calls that return cached values. The *orchestration*
is repeated; the *prefix work* is not.

A branch switch replaces the trace. Definitions a retired branch owned are
retracted by `retire_unreachable` (`inner.rs:1189-1244`), so a switched-off branch
does not keep publishing.

### 4.4 Collection operators

Per-member operators create a child scope per element identity
(`child_scope_keyed` for maps/keyed lists, a positional scope for whole lists) and
two site nodes: the parameter read and the result (`list.rs:252-398`,
`list.rs:831-1029`, `map.rs:209-248`). The result node memoizes the closure by
identity. That is the per-member granularity.

The operator itself is still one node, and its result is a plain `Vec`:

- the assembly node reads the structure (and, for a derived list, the whole
  storage unit) and loops over every member;
- with a changed member, the loop runs end to end, rebuilding the output `Vec`;
- the closure is skipped for unchanged members, but the loop is not.

This is why "primitive executions are constant" and "wall time grows with the
collection" are both true (`examples/scaling.rs`). The execution counter counts
node bodies; it does not count the loop inside the operator node.

## 5. One edit, end to end

Take a source list with one value changed:

1. **Commit.** The editor stages `Value(id)`, the whole id sequence, and the
   whole content. `Edit::commit` drops duplicate writes, compares each unit, then
   compares the whole content. The `Value(id)` unit changes; the structure is
   equal and does not change version; the content changes.
2. **Invalidation.** `Value(id)`'s reverse dependents are marked suspect
   transitively: the member parameter node, the member result node, the `map`
   assembly, the body, the root outputs. `RuntimeStats.invalidations` counts the
   cone (10 in the 20-element probe).
3. **Demand.** The body is suspect. Its trace validation fails at the changed
   observation, so it re-executes. The unchanged member nodes are clean and
   return immediately; the changed member executes; the assembly loops over all
   ids and rebuilds its `Vec`.
4. **Cutoff.** The member result for an unchanged element is equal, so its
   `changed_at` does not move. The assembly's output `Vec` changes, so the body
   and root outputs execute; an output whose value is equal keeps its version.
5. **Observation.** The whole-View observer compares the content version and
   fires. A `observe_entry`/`observe_units` subscriber fires only if one of its
   units moved.

Step 1 is the part that is linear in the collection. Steps 2–4 are the part the
design is about.

## 6. Measurements

All numbers below are from a scratch example, debug build, one machine, with
`Runtime::new()` (default workers). Execution/validation/invalidation counts are
deterministic for these scenarios; wall times are indicative, not a benchmark.

### 6.1 Node extents

| Scenario | executions | validations | invalidations | user-code work |
|---|---:|---:|---:|---|
| Source list, edit one of 20 values | 8 | 0 | 10 | one member result |
| Source map, edit one of 3 values | 5 | 0 | 7 | one value consumer |
| Source map, insert a fourth key | 5 | 0 | 9 | structure consumer re-runs; unrelated value consumer does not |
| Derived list, 200 members, edit one — whole value (`from`) | 206 | 200 | 410 | 1 closure call |
| Derived list, 200 members, edit one — keyed (`define_keyed`) | 6 | 1 | 12 | 1 closure call |
| Component chain, edit the second independent field | 10 | 0 | 8 | 1 of 2 `expensive` calls |
| Two expression sites, edit the second | 6 | 0 | 8 | expression A 0, expression B 1 |
| Two independent invocations, edit one | 10 | 0 | 8 | left body 0, right body 1 |
| Derived map, change key 1, reader of key 2 | 13 | 0 | 11 | reader body 0; key 2's units unchanged |
| Output projections, body changes, output 0 equal | 9 | 0 | 11 | projection 0 executed, `changed_at` unchanged |

The derived-list row is the clearest granularity measurement:

- **Whole value.** Every member's parameter node observes the whole storage unit.
  All 200 parameter nodes execute; only the edited member's result and closure
  run, and the unchanged result nodes are cutoff after their trace replay (the
  stats report 200 validations for 200 members). The per-member work is O(1), the
  per-member *checking* is O(n).
- **Keyed.** Each member's parameter node observes its own membership/value
  units. One parameter node and one result node run; the rest are untouched.

### 6.2 Commit extent

Source-only runtime (no root, no observers), one list value changed, measured
around `runtime.edit` only:

| list size | edit only (debug) |
|---:|---:|
| 200 | 180 µs |
| 2,000 | 2.4 ms |
| 20,000 | 30.6 ms |
| 100,000 | 180 ms |

The growth is not validation: nothing is demanded. It is the editor
rebuilding the whole id sequence and `Edit::commit` comparing the whole content.
The same shape applies to a derived definition commit: `apply_definitions`
rebuilds content and `refresh_structure` projects the field.

### 6.3 Keyed publication extent

Keyed derived list, one source value changed, edit + read:

| members | edit + read (debug) |
|---:|---:|
| 500 | 7.0 ms |
| 1,000 | 19.4 ms |
| 2,000 | 55.4 ms |
| 3,000 | 110.8 ms |

The provider restages every member on every run. `DerivedList::define` applies a
keyed value draft with `DerivedListStorage::with_entity`, which finds the id by
scanning the sequence (`list.rs:569-585`, `630-635`); n drafts therefore cost
O(n²) per restage. The keyed protocol buys per-member *downstream* reuse, not a
per-member *publication*.

## 7. Findings

Severity: correctness first, then cost, then diagnostics/docs.

### F1 — Source graph edge membership is never written (high, correctness)

**Evidence.** `GraphEditor::add_edge` stages only `Value(EdgeKey)` and marks the
endpoints dirty (`graph.rs:614-622`); `remove_edge` stages only
`Value(EdgeKey) = None` (`graph.rs:625-635`); `remove_node` does the same for
incident edges (`graph.rs:579-611`). `GraphRef::edge` first reads
`Membership(EdgeKey)` through `contains_edge` (`graph.rs:247-251`, `325-337`).
That membership unit is initialized from content on first read. If the read
happened while the edge was absent, the cell is `false` and nothing ever writes
it to `true`.

**Reproducer.**

```rust
#[view(source)]
pub struct Network { links: Graph<u64, u64> }

#[component]
pub fn root(network: &Network) -> (Option<u64>, bool, usize) {
    let links = network.links();
    (links.edge(0).map(|e| e.value), links.contains_node(1), links.edge_count())
}
```

Read with two nodes and no edge, add the first edge, read again:

```
before edge: (None, true, 0)
after edge:  (None, true, 1)
```

`edge_count` comes from topology and is correct; `edge(0)` is still `None`
because its membership cell says absent.

**Effect.** A negative edge read is not a dependency that wakes when the edge
appears. A per-edge observer registered before the edge adds never fires.
Removal leaves a `true` membership cell behind, so an observer of the membership
does not fire on removal either (the public `edge()` happens to return `None`
because the value unit is `None`). This is the same class of defect as a stale
negative map cell, but for graph edges.

**Fix direction.** Set `Membership(EdgeKey) = true` in `add_edge`, and `false` in
`remove_edge` and in `remove_node` for each incident edge. Gate: read a missing
edge, add it, assert `edge(0) == Some`; register a per-edge observer and assert
it fires on add and remove. Add a graph-edge negative cell to the family gate
that already covers map/set negatives.

### F2 — `get_key` and `to_map` on a whole-value derived list read stale cells (high, correctness hazard)

**Evidence.** A whole-value list definition stages exactly one
`Value(field)` (`define_all`, `list.rs:821-829`). It never writes per-id
`Membership`/`Value` units. `DerivedListRef::get_key` reads those cells directly
(`list.rs:803-819`); the cells are created on first read from content
(`read_unit_value`/`unit_node_init`), and the next whole-value definition does
not retract or update them because the provider does not own them. The doc
comment says `get_key` is "meaningful for keyed definitions" but returns a value
for whole lists instead of rejecting. `to_map` uses `get_key` for every id
(`list.rs:1031-1047`), so it inherits the staleness.

**Reproducer.**

```rust
#[view(derived)]
pub struct PayloadList(List<u64>);

#[component]
pub fn root(mode: &Mode) -> (Option<u64>, Option<u64>) {
    if mode.mode() == 0 { PayloadList.define_all([10u64, 20, 30]); }
    else { PayloadList.define_all([10u64]); }
    (PayloadList.get_key(1), PayloadList.get_key(2))
}
```

Observed:

```
before: (Some(20), Some(30))
after:  (Some(20), Some(30))
```

The list is `[10]` after the edit. An independent component that calls
`get_key(2)` is not even invalidated: the whole-value definition changes the
storage unit and the structure, not the per-id cells it observes, so it keeps
returning `Some(30)` without running its body.

**Effect.** Silent wrong values for any whole-value derived list that is read by
identity. It also corrupts the Observatory's granularity report: `granularity_of`
counts read-created entity units (`inner/inspect.rs:639-703`), so a whole-value
field flips from `whole_value` to `per_entity` after a `get_key` call even though
no per-entity definitions exist. That is exactly the false-granularity case the
board's G14 was meant to prevent.

**Fix direction.** In `get_key`, branch on the stored mode: for `Keyed`, keep the
per-unit read; for `Whole`, read the whole storage unit and index it (coarse but
correct), or reject with a clear diagnostic. Do the same for `to_map`. Gate:
shrink a whole list and assert `get_key` follows it; assert an independent
`get_key` reader re-runs when the whole list changes; assert `view_facts`
granularity stays `whole_value` after a `get_key` read (under
`--features observe`). The alternative — have
whole definitions maintain per-id units — turns `from` into a keyed definition
and gives up the whole-value boundary.

### F3 — Commit extent is whole-value O(n) (high, structural cost)

**Evidence.** Every source editor's `Drop` stages the full structure projection
and `set_content` stages the full content (`map.rs:372-390`, `list.rs:502-514`,
`set.rs:249-262`, `graph.rs:648-668`). `Edit::commit` compares the content with
`content.eq` before deciding whether the content version moved (`edit.rs:166-190`,
`216-233`). On the derived side, `apply_definitions` rebuilds content per draft
and compares the whole content (`inner.rs:1629-1643`); `refresh_structure`
projects the whole field (`inner.rs:1421-1466`).

**Effect.** An edit that changes one scalar in a large collection grows with the
collection (at least linear here) even with no consumers (table in §6.2). The unit-level cutoff keeps
the *published* revision narrow; it does not keep the *commit* cheap. For a
derived map, the same is true per provider run: every defined key is applied and
the whole content is compared.

**Fix direction.** These are separable optimizations:

- avoid the full content equality when the per-unit comparisons already establish
  which units moved (a dirty-field flag is enough for content equality in the
  common case);
- update source structure incrementally from the edit (insert/remove/move are
  known deltas) instead of rebuilding the projection;
- let a definition commit that stages a whole field (list storage, topology,
  reduced sequence) use that draft as the content update instead of re-applying
  every per-entity draft first.

The third also fixes F5. Measure before/after on the 100k list and a 10k map.

### F4 — Operator results are whole `Vec`s; the assembly loop is O(n) (structural)

**Evidence.** `SourceListRef::map/filter/flat_map` and the derived variants
return `Vec` (`list.rs:259-398`, `837-1029`). The operator node reads the
structure (and whole storage for a derived list), then loops over every member
even when the member results are cached (`list.rs:849-895`). The same is true
for `keys()`, `to_vec()`, and `map_values()`: they materialize a whole result.

**Effect.** Keyed identity removes the per-member *computation*, not the
per-change *assembly*. The gate
`derived_list_map_reuses_members_after_reorder_and_value_edit` says it precisely:
"A reorder re-runs the map call but reuses both member results"
(`crates/irrigation/tests/gates.rs:3514-3526`). A 10k keyed list still rebuilds a
10k-element `Vec` on every edit, and the next component sees that `Vec` as one
value. There is no `map`/`filter`/`flat_map` form that publishes its result as a keyed
View; the publishers that do exist (`to_list`, `to_map`, `node_map`,
`flatten_map`, `define_keyed`) restage the whole target.

**Fix direction.** If operator outputs need to be incremental across steps, add
a target form (`map_to`, `filter_to`, `flat_map_to`) that stages a keyed
definition into a declared derived list, so the sequence itself is a View with
per-identity units. Keep the `Vec` form as the convenience API. Document the
assembly boundary either way.

### F5 — Keyed publication is O(n) drafts and O(n²) for lists (high, cost)

**Evidence.** `define_keyed` loops every item and calls `define_unit` for its
value, then defines the whole storage (`list.rs:1049-1061`). `apply_definitions`
calls the content vtable for each draft (`inner.rs:1605`). For `DerivedList`, a
keyed value draft goes through `with_entity` → `upsert`, which finds the id with
`items.iter().position` (`list.rs:569-585`, `630-635`). n drafts therefore cost
O(n²) per restage. Measured edit+read in §6.3 is consistent with that shape.

**Effect.** Keyed lists are a granularity win downstream and a publication
regression at scale. At 10k members the provider's own commit does tens of
millions of sequence steps even when one value changed. Sets are better
(`PSet::insert` is O(log n)), maps are O(n log n), but all of them still stage n
drafts.

**Fix direction.** Apply the whole-field draft first (or once), then write only
the per-entity unit values; skip rebuilding the assembled storage for each
value draft. For lists, index by id for the staging pass or stage the ordered
sequence once and reuse it. Add a draft-count/iteration counter (F13) so the
cost is visible in tests.

### F6 — Whole-value derived lists are O(n) per member edit (structural)

**Evidence.** `DerivedListRef::map/filter/flat_map` read the whole storage to
choose the mode (`list.rs:846-848`, `911-912`, `979-980`), and in `Whole` mode
every parameter node reads the whole unit (`list.rs:878-888`, `941-951`,
`1008-1018`). Any value edit changes the whole storage, so all parameter nodes
execute; their results are cutoff, so only the changed result node and closure
run. Measured: 206 executions / 200 validations for 200 members, versus 6 / 1
keyed (§6.1).

**Effect.** `from(iter)` is a correctness-preserving convenience with a known
coarse boundary; the README says so. The practical consequence is that whole
lists are unsuitable for large collections that are edited often, and the cost
is *checking*, not user code. `define_keyed` is the fix, at the price of F5 and
of the producer supplying ids.

**Fix direction.** Keep the boundary; make it visible in docs and diagnostics.
If a producer has stable ids, `define_keyed` (or a source list + `to_list`) is
the right path. Do not infer identity from content equality.

### F7 — A component body is the re-execution unit (by design, document)

**Evidence.** `#[component]` lowers the body and evaluates it as one invocation
body node (`component.rs:118-121`, `302-304`); every expression inside is a site
node (`lower.rs:78-97`). If any observation moved, the body closure runs from the
start (`inner.rs:713-793`). Prefix expressions return cached values, but the
orchestration and any non-node Rust work rerun. The probe (two expression sites,
edit the second) shows 6 executions, expression A 0 calls, expression B 1 call.

**Effect.** "Fine logical nodes" means the *work* is per expression, not that
execution resumes mid-body. Long straight-line bodies pay a lookup per expression
and repeat their control flow. A pure helper called as a plain function is one
expression node; it is not an invocation with its own body and output identity.
Only an owned-argument component call gives the callee its own memoized body and
outputs.

**Fix direction.** None required; this is the lowering contract. State it in
`docs/architecture.md`'s component-lowering section in exactly these terms. When
a body is large, split it into owned-argument components so unchanged calls can be
skipped.

### F8 — Projection cutoff is downstream-only (by design, document)

**Evidence.** `call_component` evaluates the body and every projection
(`component.rs:176-207`), and the projection node observes the body
(`component.rs:189-197`). A body change marks every projection suspect; each
projection re-executes and cuts off if its value is equal. Probe: projection 0
`executed_at = revision`, `changed_at = 1` (old), projection 1 changed.
`read_projection` evaluates the body plus one projection (`component.rs:86-141`).

**Effect.** Separate projections prevent *downstream* invalidation of consumers
of the unchanged output; they do not prevent the body or the projection node
from running, and `call_component`'s call site observes the body, so a whole-tuple
consumer re-executes anyway. `component.rs`'s comment "Consumers of output 0 are
therefore not invalidated when only output 1 changes" is true for a consumer of
the projection node, not for a consumer of the call site. In practice the useful
case is `read_projection`/`call_projection` and equal-projection cutoff.

**Fix direction.** Qualify the comment and the README. If per-output call sites
are needed, the API would have to return projection-scoped values, not a tuple.

### F9 — Rank reads are structure reads (by design)

`SourceListRef::get(i)` reads the structure, then the membership and value of
the id at rank i (`list.rs:166-181`). Any insertion, removal, or move changes the
structure and invalidates every rank reader, even when the element that lands at
that rank is unchanged. This is correct: a rank is not an entity. Per-identity
reads on a keyed list (`get_key`) avoid it. Document the distinction next to the
list API; it is currently only implicit.

### F10 — Derived graph adjacency is topology-wide (by design)

Source graphs maintain per-node `Adjacency`/`IncomingAdjacency` units
(`graph.rs:648-668`). Derived graphs do not; `outgoing`/`incoming` compute from
the topology unit when `field_is_keyed` is true (`graph.rs:362-395`). Any
topology change therefore invalidates every adjacency reader of a derived graph,
while a source graph's payload edit does not. This is a deliberate asymmetry;
state it in the matrix so users choose source graphs when per-node adjacency
matters.

### F11 — Join structural updates re-read every joined key (cost)

Value-only changes are per-key: `update` walks the change batch, refreshes only
those keys, and re-observes the rest (`join.rs:349-373`). A structural change
(the key set moved) re-reads both structure units and loops over every existing
joined key, re-reading both sides to see whether it moved
(`join.rs:302-347`). Publishing the target map uses `replace_all`
(`join.rs:62-82`), so every key is restaged even if only one changed. The
per-key unit cutoff still keeps notifications narrow. Gate
`join_view_updates_only_changed_keys` covers the value-only path; a structural
gate should state the O(keys) path.

### F12 — General components default to `Reset` (by design)

`DependencyUpdates::Reset` reinitializes when any observed dependency changes;
`Incremental` is opt-in and receives the lazy `ChangeBatch` (`general.rs:37-49`,
`64-125`, `544-587`). The batch is materialized after the body's own reads, so it
describes what this run is about to see, not what the previous run saw. The
component's algorithm decides the true extent; the runtime only supplies the
changed units and scopes. Correct, but a coarse default: a stateful algorithm
that does not opt in pays a full reset on every input edit. Document the choice
where the trait is introduced.

### F13 — `RuntimeStats::cutoffs` is never incremented (low, diagnostics)

`cutoffs` is declared and read out (`runtime.rs:1379`, `924`) but no code
increments it (a repository-wide search finds only the declaration and the
snapshot read). The audit cannot report how many executions ended in an equal
value from the stable stats. The observatory's `no_change_runs` covers only
general components.

**Fix direction.** Increment `cutoffs` in `Inner::commit` when `!changed` (it
already computes that flag), and add staged-draft/operator-member counters if F5
is to be gated by counts rather than wall time. These are the measurements a
reviewer needs to see the extent.

### F14 — Observation granularity is three-tiered (document)

`observe` watches the View content version (`runtime.rs:666-685`); a value edit
to any key wakes it. `observe_units` watches an explicit set of unit versions
(`runtime.rs:710-749`). `observe_entry` builds the membership+value pair for one
key (`runtime.rs:751-785`) and is documented as a map entry; its membership+value
pair also names a keyed list member, but a set has only a membership unit, so
`observe_units` is the narrow form there. The
content version and unit versions are independent: a restaged equal value moves
neither; changing one unit moves that unit's version and, when the content also
differs, the View's content version once. Choose the subscription by the wake
scope, not by the View.

## 8. Boundaries that are exact today

These hold in the code and are covered by gates; they are the parts a follow-up
should not regress:

- Map value edits do not touch the structure unit; key insertion does not run
  value consumers of other keys (`value_only_edits_do_not_run_structure_consumers`,
  `structure_changes_do_not_run_value_consumers`).
- Keyed list identity survives reorder/removal; earlier members' value units are
  not rewritten; rank reads observe structure
  (`keyed_list_identity_survives_reorder_and_removal`,
  `derived_list_map_reuses_members_after_reorder_and_value_edit`). Source list
  identity is exercised by `examples/scaling.rs`.
- Source set membership is per member; a keyed derived set keeps it per member
  (`keyed_set_membership_is_per_member`).
- Source graph topology, payloads, and per-node adjacency are separate; node
  deletion retracts incident edges (`graph_topology_and_payloads_are_independent`).
  F1 is the exception at the negative edge-membership boundary.
- Tree reads are per node/tag/field/child; replacement preserves identity
  (`tree_reads_walk_kinds_and_fields`, `a_replaced_node_keeps_its_identity_and_repairs_consumers`).
- Derived maps maintain their structure unit; a value-only edit does not move it
  (`derived_map_structure_follows_definition_changes`,
  `derived_map_value_edits_do_not_touch_structure`).
- Keyed derived lists reuse per-member `map`/`filter`/`flat_map` results across
  reorder and unrelated edits (`derived_list_filter_and_flat_map_memoize_per_identity`).
- Reduced fields expose a cached lawful aggregate with logarithmic recombine
  (`point_edits_recombine_only_a_logarithmic_path`).
- The join's value-only path touches only changed keys; the target View is
  per-key (`join_view_updates_only_changed_keys`).
- Component invocations are keyed by arguments; equal output cuts off
  downstream; output projections are separate nodes
  (`equal_component_output_stops_downstream_execution`,
  `changing_one_output_does_not_change_the_other`).
- Definitions install atomically (two-phase validation) and a retired branch
  restages if it returns (`a_failed_definition_install_is_atomic`,
  `a_definition_inside_an_untaken_branch_is_retracted`).

## 9. Suggested order

1. **F1** — write edge membership in the source graph editor, plus the negative
   read and per-edge observer gates. Small and user-visible.
2. **F2** — make `get_key`/`to_map` correct (or explicitly reject) on whole-value
   lists; add the shrink/reopen gate and the granularity-report assertion.
   Choose "coarse but correct" over "fast but stale".
3. **F13** — increment `cutoffs`; add staged-draft and operator-member counters.
   Without these, F3–F5 can only be measured by wall clock.
4. **F3/F5** — optimize publication: avoid the full content compare when the
   per-unit comparison already decided the change; apply a whole-field draft
   once; avoid the per-draft linear scan for keyed lists. Gate by counts and by
   the 100k/10k timings, not by a microbenchmark.
5. **F4** — decide whether operator outputs need to be Views. If yes, add
   target-form operators; if not, document the assembly boundary clearly.
6. **F7/F8/F9/F10/F12/F14** — documentation: state the body-atomic rule, qualify
   projection cutoff, and put the rank/adjacency/reset/observation boundaries in
   the architecture notes next to the existing "honest boundaries".

The first two are correctness; the rest are honesty about cost and scope. None of
them require changing the revision, validation, or cutoff protocol.

## Appendix A — Reproducers

The two correctness findings are reproducible with the public API and no
instrumentation. The measured scenarios used a scratch example that was removed
after the run; the shapes below are sufficient to recreate them.

### A.1 Negative edge membership

```rust
#[view(source)]
pub struct Network { links: Graph<u64, u64> }

#[component]
pub fn root(network: &Network) -> (Option<u64>, bool, usize) {
    let links = network.links();
    (links.edge(0).map(|e| e.value), links.contains_node(1), links.edge_count())
}

// seed two nodes, read (edge absent), then add an edge with `add_edge`, read again
```

Observed `(None, true, 0)`, then `(None, true, 1)`; `edge(0)` should be `Some(5)`.

### A.2 Whole-value list identity read

```rust
#[view(derived)]
pub struct PayloadList(List<u64>);

#[component]
pub fn root(flag: &Flag) -> (Option<u64>, Option<u64>) {
    if flag.on() { PayloadList.define_all([10u64, 20, 30]); }
    else { PayloadList.define_all([10u64]); }
    (PayloadList.get_key(1), PayloadList.get_key(2))
}
```

Observed `(Some(20), Some(30))` before and after the list shrinks to `[10]`. A
separate component that reads only `get_key(2)` never runs its body when the
list is redefined.

### A.3 How the counts were taken

`runtime.stats()` before and after the edit+read gives executions, validations,
and invalidations. Per-node executions come from `runtime.explain_node(id)` and
`executed_at == revision`; `runtime.dump()` enumerates the nodes. User-code
counters were process statics incremented inside the relevant closures. The
edit-only timings came from a source-only runtime (no root installed, no
observers) around `runtime.edit`, so they exclude demand.

## Appendix B — Inventory

Read end to end or in the relevant sections:

- `crates/irrigation-core/src/{descriptor,ops,ids,frame}.rs`
- `crates/irrigation-runtime/src/{inner,edit,state,runtime,component,general,helpers,field}.rs`
- `crates/irrigation-runtime/src/inner/inspect.rs` (granularity derivation)
- `crates/irrigation-collections/src/{list,map,set,graph,reduce,join,tree}.rs`
- `crates/irrigation-macros/src/{view,component,lower,tree}.rs`
- `crates/irrigation/tests/gates.rs` (the family, keyed, structure, observer and
  projection gates cited above)
- `crates/irrigation/examples/scaling.rs`
- `docs/architecture.md`, `README.md`, `plans/implementation-status.md`

Commands: `cargo test --workspace --offline` (baseline green), the scratch
example for §6, and the two reproducers in Appendix A. The audit is static plus
targeted reproduction; it did not include `loom`, fuzzing, or release-mode
benchmarking.
