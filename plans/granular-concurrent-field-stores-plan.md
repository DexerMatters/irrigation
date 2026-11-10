# Granular concurrent field stores: per-unit commit, port plans, and publication groups

**Status:** implementation complete for the phases below. The field-store kernel, family deltas, plans/claims, keyed producers, schema layer, STLC migration, and fixpoint indexes are landed; explicit boundaries are listed at the end.
index, the map key-projection cache, equal-definition restatement skipping,
content-vs-projection classification, one-clone batch content application, and
the first `AppliedDelta`-backed family batches, the public `FieldStore`
contract, the graph delta store, the keyed derived-list payload batch, the graph
payload batch, the single-unit and map-unit source fast paths, atomic claim
groups, and planned-port reader waits are landed slices; the remaining FieldStore
families and group-publication/keyed-producer phases stay open.
**Scope:** the kernel’s field-storage/commit boundary and the concurrency
protocol for components that read and write the same View or the same group of
Views.
**Related:** [`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md)
(the semantic model), [`shared-output-arbitration-plan.md`](shared-output-arbitration-plan.md)
(ports/claims/requirements), [`shared-view-scope-graph-plan.md`](shared-view-scope-graph-plan.md),
[`heterogeneous-granularity-plan.md`](heterogeneous-granularity-plan.md),
[`views-components-granularity-audit.md`](views-components-granularity-audit.md),
[`component-output-publication-plan.md`](component-output-publication-plan.md),
[`component-authoring-api-plan.md`](component-authoring-api-plan.md), and
[`ast-incremental-granularity-plan.md`](ast-incremental-granularity-plan.md).

---

## 0. The key deficiency

The framework’s **logical unit model is fine-grained; its physical
storage/commit and write-visibility model is not**.

Two facets of one defect:

1. **A field’s content is an immutable whole value.** Units are observations
   over a `PMap`/`PSeq`/`PSet`/`GraphStore`/`TreeStorage`/`AggSeq`. Edits and
   definitions rebuild that value, compare the whole value, and project the
   whole structure. The granularity audit states it directly:
   > **Definition granularity is per unit. Commit extent is whole value.**
   > — [`views-components-granularity-audit.md`](views-components-granularity-audit.md), §3.3
2. **A component’s writes are a body-atomic set.** Claims are acquired when a
   write verb runs; definitions commit with the invocation body. A reader of
   one unit therefore waits for the whole writer body (or retries if the write
   happens later). Future write sets are not registered, so a reader cannot
   wait for or demand the component that *will* produce the unit. The
   concurrency plan’s support matrix names this boundary (“a read waits for a
   generated component that will `define_*` the unit” was the initial gap), and
   the landed arbitration explicitly notes that **GeneralComponent output units
   are not entity-claimed**, while runtime-maintained structure/topology/
   adjacency/aggregate units are not claimable at all.

The result:

* a one-unit edit in a 100k collection costs O(n) commit;
* keyed lists stage O(n) drafts and can upsert in O(n²);
* operators assemble whole `Vec`s;
* derived graph topology/adjacency and list/set structure are whole units;
* two components overlapping the *same* unit are ordered at body granularity:
  the reader waits for the writer's whole invocation to commit that unit;
  disjoint keys are already unordered at the claim level, but GeneralComponent
  readers are ordered at View granularity;
* components cannot cooperate through a declared per-port plan; they discover
  conflicts only when a write verb runs;
* a body-atomic component cannot publish one unit/group early, so a reader of a
  later-needed unit waits for unrelated work in the same body;
* users compensate with explicit `u64` occurrence ids, helper stages,
  `define_all`, or whole-view writes.

A schema/handle layer on top would be cosmetic. **The field store must become
delta-native, and the write plan must become port/group-scoped.**

The target sentence:

> A one-unit edit commits one unit and its index path; a reader waits only for
> the writer that actually owns the unit it reads; a component may publish
> units/groups independently; no component-level View order is visible.

---

## 1. Evidence from the current plans and code

### 1.1 Commit extent

The granularity audit measured:

| Scenario | Measurement |
|---|---|
| source list, edit one of 20 | 8 executions, 10 invalidations, one member result |
| derived list, 200 members, one edit, whole value | 206 executions, 200 validations |
| derived list, 200 members, one edit, keyed | 6 executions, 12 invalidations |
| source list edit only, 100k elements | 180 ms with no demand |

It attributes the last two columns to whole-content comparison, whole-structure
projection, per-draft content rebuild, and O(n²) keyed list upserts
([`views-components-granularity-audit.md`](views-components-granularity-audit.md), §6.2–6.3, F3/F5).

### 1.2 The plans’ own contract is broader than the implementation

The heterogeneous plan defines three contracts:

1. observation granularity — a read depends on the narrowest units;
2. definition granularity — a writer writes per entity;
3. conversion granularity — occurrence identity survives family conversions.

Its Phases 2–3 are marked complete for keyed lists/sets, derived graphs, joins,
and cross-family operators. But keyed definitions still stage the whole
assembled storage and every member; the plan itself lists the open performance
and publication shape ([`heterogeneous-granularity-plan.md`](heterogeneous-granularity-plan.md),
§1, Phase 2, open questions).

### 1.3 Concurrency is per port, but knowledge/publication is per component

The shared output plan already implements the right primitive:

* ports and scopes (`Entity`, `Field`, `View`);
* one live writer per scope; disjoint scopes are unordered and parallel;
* a reader waits for a live writer claim;
* late writes invalidate and retry;
* structured requirements, deferral, cycle detection, and no last-writer-win.

Evidence in the current tree:

* `Inner::claim_write` registers a claim when a write verb runs;
* `read_unit_step` waits for a live claim covering the unit;
* `stats.write_claims` / `reader_waits` / `demand_retries`;
* gates `a_reader_waits_for_a_live_writer_claim`,
  `read_after_a_deferred_write_sees_the_write`,
  `disjoint_writers_compose_in_any_order`, `the_same_port_is_rejected`.

The plan’s own boundaries identify what remains:

* GeneralComponent output units are not entity-claimed; readers are ordered by
  the single output producer/`bad_producer`, i.e. at **View** granularity;
* derived aggregate units (structure, topology, adjacency, aggregate) are
  runtime-maintained and not claimable;
* whole-view/whole-field writers are incompatible with unit writers;
* tree output claims the whole View for destructive subtree writes;
* source graph editors retain legacy whole-graph behavior.

What already works, and should not be re-designed:

* disjoint ports are compatible even in one View;
* a reader waits for a live claim covering exactly its port;
* a late write invalidates the read and retries before the demand returns;
* overlapping writers are a structured conflict, never a timing winner;
* requirements, deferral, wait-cycle detection, cancellation, and
  per-entry/unit observers exist.

The remaining deficiency is narrower than “everything is component ordered”:
the unit is not the **future-write plan** or **publication group**, and a body
cannot expose a unit before it finishes all its other work.

Also:

* a generated component’s future write set is not a registered producer; a
  reader can only wait after a claim appears, or retry after a late write;
* a component’s write groups are all staged until body commit; group-level
  visibility is not separate from the body;
* the component body is one node, and its output product is one body node;
  separate output projections add cutoff, not partial body publication;
* `define_all` / whole-value definitions claim a field, which conflicts with
  any per-unit writer that wants to coexist.

### 1.4 The source plan already asked for deltas and identity

The framework plan requires collection algorithms to consume structural deltas
and preserve element identity ([`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md),
§5.1–5.6, §6). The AST plan’s R6 and “definitions are writes, not full
restatement” point at the same publication boundary
([`ast-incremental-granularity-plan.md`](ast-incremental-granularity-plan.md),
§3.4).

The component-output plan proposes typed per-unit patches, but patches applied
to whole-value field content still pay whole commit
([`component-output-publication-plan.md`](component-output-publication-plan.md)).
The authoring plan’s identity boilerplate is a symptom
([`component-authoring-api-plan.md`](component-authoring-api-plan.md), F1/F16).

---

## 2. Design goals and invariants

### 2.1 Granularity invariants

| Id | Invariant |
|---|---|
| G1 | A one-unit edit stages one unit delta and touches O(1)/O(log n) index entries, independent of collection size. |
| G2 | Commit applies deltas to a field store; it does not rebuild/compare whole content. |
| G3 | Structure/order/rank/adjacency/lookup are per-index-key units, not whole projections. |
| G4 | Every occurrence has a stable opaque handle; source allocates, derived receives, schema constructs. |
| G5 | Source and derived collections share the store/delta protocol; source/derived is a mode. |
| G6 | An operator publishes keyed occurrence deltas into a target; no whole `Vec` assembly on the fine path. |
| G7 | Whole-value definitions are explicit fallback only. |
| G8 | Provider-scoped restatement retires only that provider’s old units; no `define_all` in the fine path. |
| G9 | Snapshots materialize from stores lazily; content is a projection/cache. |
| G10 | Every data structure is stores + indexes; no structure-specific kernel case. |

### 2.2 Concurrency invariants

| Id | Invariant |
|---|---|
| C1 | A write claims **ports/groups**, not a component, a View, or a demand. |
| C2 | Disjoint ports/groups are unordered and may run concurrently, even in the same View or the same group of Views. |
| C3 | A reader waits only for the writer/group that covers the ports it reads. |
| C4 | A read that happens before a planned write waits for or demands the planned producer; it never returns a stale value. |
| C5 | A component invocation registers its write plan before it writes (statically from the schema/macro, or dynamically at the first computed key). |
| C6 | A write group commits atomically; a reader never sees a partial group. |
| C7 | Different groups of one invocation may commit in any order; a slow group does not order readers of an already-committed group. |
| C8 | Two live writers on an overlapping port/group are a structured conflict; no merge/last-writer. Intentional handover is routing-authorized. |
| C9 | Waiting releases claims that could deadlock; the wait graph detects cycles; failures propagate to waiters. |
| C10 | The API stays ordinary: user code performs View/schema writes; claims, waits, deferrals, and group publication are invisible scheduling facts. |
| C11 | Whole-field/whole-view fallback is an explicit coarse group and cannot silently coexist with fine writers. |
| C12 | Per-entry observers follow unit/group versions, not whole-View content versions. |

---

## 3. Architecture

### 3.1 Delta-native field stores

Replace the whole-content commit path with a field store:

```rust
pub trait FieldStore {
    type Key: OccurrenceKey;       // private; handles expose it
    type Value;
    type Schema: StoreSchema;      // shape + index descriptors
    type Delta: FieldDelta;

    fn apply(&mut self, delta: Self::Delta) -> Applied;

    fn value(&self, key: Self::Key) -> Option<Self::Value>;
    fn membership(&self, key: Self::Key) -> bool;
    fn index(&self, index: IndexId, key: IndexKey) -> IndexValue;
    fn len(&self) -> usize;        // index-maintained
    fn materialize(&self) -> Content; // snapshot/whole read projection
}

pub struct Applied {
    pub units: Vec<UnitChange>,    // only touched units, old/new
    pub groups: Vec<GroupId>,      // committed write groups
}

pub enum FieldDelta {
    OccurrenceSet { key, value },
    Membership { key, present },
    Index { index, key, value },
    Retract { unit },
    Whole { value },               // explicit fallback
}
```

The store owns cells and indexes. `apply` changes only touched cells/index
entries and returns the exact unit/group delta; the runtime uses it for unit
versions, invalidation, observer notification, and provider restatement.
`materialize` is for snapshots/whole reads/board; it is not on the commit hot
path.

### 3.2 Occurrence handles

```rust
pub struct Occurrence<S: Shape, K>(/* private */);
pub struct Entry<S: Shape, K, V>(/* private */);
pub struct IndexEntry<I, K, V>(/* private */);

map.entry(key) -> Entry<Map, K, V>
entry.edit().set(value)
entry.edit().retract()
entry.edit().index(...)
```

Source editors allocate handles; derived writers receive handles from inputs,
operators, or schema constructors. Underlying keys are typed and private;
user code never names `u64`, `FieldId`, `UnitKey`, or TAGs.

### 3.3 Stores and indexes

| Structure | Store | Indexes |
|---|---|---|
| Map | key → value, membership | lookup/key set, count |
| Set | member membership | count |
| List | occurrence → value, membership | stable order labels, rank/next, count |
| Tree | node fields/slots/membership | parent, children, root |
| Graph | node/edge occurrences | outgoing/incoming adjacency, topology projection |
| Reduced | aggregate units | sequence/index fallback |
| Custom | schema fields | declared indexes |

Stable list order labels, per-node graph adjacency, and per-node tree slots
replace whole structure/topology on the fine path. Enumeration/whole reads are
explicit coarse reads.

### 3.4 Ports, plans, and groups

Reuse the existing `Port` notion (the owning `UnitKey`/scope). Add:

```rust
pub struct PortPlan {
    pub groups: Vec<WriteGroup>,
}

pub struct WriteGroup {
    pub id: GroupId,               // stable within the invocation
    pub ports: Vec<Port>,          // entity/field/view scopes
    pub atomic: bool,              // group commits all-or-none
}
```

* A component invocation registers a **plan** before writing:
  * statically when the schema/macro knows the group keys;
  * dynamically with `plan_ports(...)` when keys are computed, before the first
    write to each group.
* The runtime registers a provider per planned port/group, not just per
  component/View.
* A reader of a planned port can wait for or demand the planned provider; it
  does not need the writer to have executed yet.
* A plan is not a definition; unused plans install nothing.
* A group's ports are claimed atomically in canonical order; a group is the
  unit of conflict and commit.
* Disjoint groups are unordered; a slow group does not order readers of another
  group.

### 3.5 Unit-group publication

A component body is still a synchronous Rust function. There are two layers:

1. **Initial kernel layer (body-atomic groups).** The body returns a patch
   partitioned into groups; each group is a separate output/commit node. The
   groups commit after the body produces them, but independently of each
   other. A reader of group A waits for group A, not group B or the whole View.
   A slow later group does not block readers of an already-committed group.
2. **Compute-granular layer (keyed producers).** For structures where a body
   would be a large loop, declare/generate a **keyed component**:
   ```rust
   #[component(callable, keyed = Key)]
   fn produce(key: Key) -> ValuePatch { ... }
   ```
   The framework creates one invocation per key/group, keyed by the occurrence
   handle; each invocation computes and commits its unit/group. This is how
   large maps/lists/graphs/scope graphs get true compute concurrency without a
   component-level barrier. Schema macros generate these producers; authors do
   not write helper functions.

The body-atomic layer is the compatibility baseline. The keyed-producer layer
is the target for per-unit computation concurrency. Both are transparent: user
code sees ordinary component/schema operations, not keys or locks.

### 3.6 Conflict and wait semantics

* One live writer per overlapping port/group (`Entity`/`Field`/`View` scopes
  from the arbitration plan).
* Disjoint scopes compatible; overlapping scopes `WriteConflict`.
* Whole-field/whole-view fallback is an explicit coarse group; it conflicts
  with unit groups in that field/View, as today.
* Reader waits for a live claim or planned provider; late writes invalidate and
  retry.
* Requirements are checked before claims where possible; wait/defer/fail modes
  as in the arbitration plan. Release claims before waiting; cycle detection
  remains.
* No merge/last-writer. An explicit combining component is the answer to “two
  writes want one port.”
* GeneralComponent output units become entity-claimed (or publish through the
  same groups); remove the View-level producer ordering for fine outputs.
* Runtime-maintained structure/index units are per-key claimable; no
  whole-topology/adjacency unclaimable unit.
* Tree destructive subtree operations remain a group claim (or whole-View
  closure) because they are genuinely structural.

### 3.7 Transparent cooperation protocol

The runtime protocol, stated once, so every case is checkable:

```text
reader(ports):
  read the current certified value
  if a live claim covers a read port:
      wait for its group/owner; relinquish claims; re-read
  if a registered plan/provider covers a read port:
      wait for or demand that provider; re-read
  if a late write covered a port after the read:
      the trace is invalid; retry the demand from the root
  return only after every read port is certified and unchanged

writer(group):
  register/refresh the group plan (ports, atomicity, key)
  evaluate requirements (tracked reads; Wait/Defer/Fail)
  acquire one claim per disjoint port/group in canonical order
  stage deltas for the group
  commit atomically: units, indexes, versions, observer notifications
  release claims; wake readers of those ports/groups

disjoint ports/groups:
  no claim overlap; no wait; no component-level order

overlapping writers:
  second claim -> WriteConflict naming both provider sites
  intentional handover -> routing-authorized, atomic replacement
```

Worked examples:

| Case | Behaviour |
|---|---|
| Two components write different keys of one Map | both run; neither waits; one key's readers do not observe the other's claim |
| Two components write different fields of one View | both run; field scopes disjoint |
| Two components write one group spanning a Map and a Set | both wait/conflict on the union of ports; no View lock |
| Reader of key 1 while a **different component** is slow on key 2 | reader of key 1 proceeds; no component/View barrier |
| Reader of a unit while its writer body is still computing | body-atomic compatibility: waits for the writer's group publication at body end; keyed producers: waits only that key's unit |
| One body writes key 1 early and key 2 later, no keyed split | key 1 is not visible before body end; this is the documented body-atomic boundary |
| One body writes key 1 early and key 2 later, split into keyed producers | key 1's producer commits independently of key 2's slow producer |
| Two writers define one port | deterministic conflict; an explicit combining component is required |
| Planner knows key 2 will be written but the writer has not run | reader waits/demands the planned provider; no stale return |

The table is the concurrency acceptance contract: **overlap waits, disjoint
proceeds, groups are atomic, plans are visible, failures are structured.**

### 3.8 Transparency and failure

The user performs ordinary writes. The runtime supplies:

* claim/plan registration, waiting, retry, and deferral;
* structured errors naming the port/group, both providers, and the reason;
* cycle errors with the wait path;
* cancellation/failure propagation to waiters;
* deterministic results across worker counts and schedules.

No manual leases, no component-level locks, no last-writer timing.

---

## 4. Mapping to all incremental data structures

Every structure is a schema over stores/indexes:

```rust
#[incremental_data]
pub struct ScopeGraph {
    #[collection(Map)] terms: Map<Term, TermRow>;
    #[collection(Map)] scopes: Map<Scope, ScopeRow>;
    #[collection(Map)] types: Map<Ty, TypeRow>;
    #[index] children(Term) -> [Term];
    #[projection(graph(...))]
}
```

Stock schemas: Map, Set, List, Tree, Graph, Table/record, document. Custom
structures use the same declaration. The scope graph is a consumer, not a
kernel case.

---

## 5. Implementation sequence

### Phase 0 — Contract, measurement, gates

* Land the G/C invariant/evidence document and commit-cost probes.
* Baseline current whole-value commit, keyed publication, operator assembly,
  and component/body ordering.
* Add failing gates before fixes where possible.

### Phase 1 — Delta-native field store kernel (Map + scalar)

* Add `FieldStore`, `FieldDelta`, `Applied`, `UnitChange`, group ids.
* Source edit path stages deltas; definition commit applies deltas per field;
  no whole content rebuild/equality; unit versions drive cutoff.
* Structure/lookup/count indexes for Map; lazy materialization.
* Remove whole structure refresh from the fine Map path.
* Gate: one-key edit touches one unit; 100k map edit O(log n); equal
  redefinition notifies nobody.

### Phase 2 — All collection stores, occurrence handles, indexes

* List/Set/Graph/Tree/Reduced stores; stable order labels; per-node adjacency;
  tree slots; populated membership.
* Occurrence/index handles and source/derived parity.
* Keyed list/set publication only changed occurrence units; no whole assembled
  sequence; eliminate O(n²) upsert.
* Graph edge add/remove updates endpoint adjacency only; tree patch per node.
* Gates: one value edit per family; append/reorder list; set member; graph edge
  adjacency; tree slot; 100k list one-unit edit.

### Phase 3 — Port plans and unit groups

* Register write plans/groups before writes; readers wait/demand planned
  providers.
* Group claims/commit; body-atomic group publication; disjoint groups parallel.
* GeneralComponent output units entity-claimed; remove View-level ordering for
  fine outputs.
* Structured requirements/cycle/wait integration.
* Gates: reader waits planned producer; disjoint groups same View parallel;
  group atomicity; overlapping group conflict; slow group does not order
  readers of a committed group; GeneralComponent per-unit ordering.

### Phase 4 — Keyed producers and compute concurrency

* `#[component(keyed = ...)]` and schema-generated per-occurrence producers.
* One invocation/group per key; per-key commit; no component-level body barrier
  for large structures.
* Operators publish keyed deltas via keyed producers.
* Gates: two slow keyed producers run concurrently; reader of key 1 waits only
  key 1; keyed map/list/graph/scope graph.

### Phase 5 — Schema layer, all structures, STLC

* `#[incremental_data]`, handles, stock schemas; scope graph schema.
* Projections from indexes; board.
* Migrate STLC scope graph as one fixture; no keys/helper stages.

### Phase 6 — Observation, fixpoint, cleanup

* Per-unit/group observers; lazy snapshots; mixed-version never observed.
* Fixed-point indexes with seminaive deltas.
* Benchmarks and migration/deletion of whole-value fallbacks where safe.

---

## 6. Verification

### 6.1 Granularity gates

* One-unit edit in 100k map/list/set/graph/tree: exact unit delta; commit time
  independent of collection size; no whole content/structure comparison.
* Keyed list publication: one changed value stages one value unit and O(log n)
  order/count updates; no whole assembled sequence; no O(n²) upsert.
* Graph edge add/remove: edge + endpoint adjacency only.
* Equal redefinition: no unit change, no consumer run.
* Whole-value fallback: explicitly coarse; no false per-entry claims.

### 6.2 Concurrency gates

* Disjoint keys/groups same View: overlap-in-time barrier test; both proceed.
* Same port/group: deterministic conflict; no merge.
* Reader of key 1 waits only for writer of key 1, not key 2/whole View.
* Reader starts before planned writer: waits/demands provider; no stale value,
  no unbounded retry loop.
* Late write: reader retries and sees the write.
* Group atomicity: reader sees none or all of a group.
* Slow group B does not order a reader of committed group A.
* GeneralComponent output unit claims: key 1 reader not woken by key 2.
* Group spanning Views: canonical claim order; conflicts deterministic; no
  deadlock.
* Cycle/wait graph: structured `Cycle`, no hang; cancellation/failure
  propagates.
* Schedule perturbation/repeated runs/worker counts 0..N produce identical
  results and no stale values.
* Per-entry observers follow unit versions.

### 6.3 Existing evidence/gates

* `read_after_a_deferred_write_sees_the_write`
* `a_reader_waits_for_a_live_writer_claim`
* `disjoint_writers_compose_in_any_order`
* `the_same_port_is_rejected`
* write-claim/reader-wait/deferred/retry counters and arbitration events
* current workspace suite, clippy, fmt, md links.

---

## 7. Risks, alternatives, non-goals

### Risks

* **Large kernel change.** Field stores replace the content source; snapshots,
  observers, recovery, and every family must migrate behind compatibility
  adapters.
* **Body atomicity vs compute concurrency.** Body-atomic groups fix publication
  ordering but still hide group units until the body returns. Keyed producers
  are required for true compute concurrency; they add invocation/identity
  machinery and schema generation.
* **Index consistency.** Every index update is part of the delta; add/remove/
  move/negative-membership tests must be exhaustive.
* **Claim/plan complexity.** Plans, groups, requirements, wait release, cycles,
  cancellation, and provider handover interact. Keep the existing claim/cycle
  invariants and add plans incrementally.
* **Identity allocation.** Source stores allocate handles; derived stores
  receive them; no content equality/position inference.
* **Whole-value fallback.** It is necessary for aggregates/identity-free
  producers but must be explicit and cannot mix with fine writers.
* **Migration.** Keep old content path as a read/snapshot adapter while the
  store becomes authoritative; gate on unit-level behavior, not only values.

### Alternatives considered

* **Keep component-level ordering.** Simple, but violates the request and
  prevents disjoint same-View components from cooperating.
* **Global View locks.** Coarser than current; rejects per-key concurrency.
* **Last-writer-wins/merge.** Rejected by the framework’s unique-definition
  semantics; would make results timing-dependent.
* **A separate relational/Datalog engine.** Non-goal; the store/index/handle
  model is the general substrate; fixed points remain targeted.
* **Scope-graph-only schema/handles.** Correctness example, not kernel fix;
  would hide whole-value commit and body ordering.

### Non-goals

* No new query language or Datalog layer.
* No replacement of the scheduler/routing/revision model.
* No merge semantics; explicit combining components instead.
* No guarantee that whole reads are incremental; whole reads are coarse by
  definition.
* No user-visible locks/leases/transactions.
* No helper functions, `let mut`, loops, stacks, or opaque bodies in analysis
  code; recursive components and schema/store methods provide control flow.

---

## 8. Acceptance summary

* The audit’s sentence becomes false in the fine path: **commit extent is per
  changed unit/index entry**, not whole value.
* A one-unit edit is independent of collection size; keyed publication is not
  O(n²); graph adjacency/list order/tree slots update per key.
* Concurrent components sharing a View/group wait only on overlapping
  ports/groups; disjoint work proceeds; readers wait for planned/live writers,
  not component order.
* Groups are atomic and independent; keyed producers provide per-unit compute
  concurrency for large structures.
* All data structures are schemas over stores/indexes; user code sees handles,
  not keys; the API is transparent.
* Existing semantics (unique definitions, no merge, deterministic results,
  cancellation, fixed points) remain; new gates prove the fine commit and
  granular concurrency end to end.

---

## 9. Implementation progress

This section is the plan’s working ledger. A checked item is a landed,
verified slice; unchecked phases remain.

### Landed

- [x] **Keyed derived-list payload index.**
  `DerivedListStorage::Keyed` now separates the stable identity order (`PSeq`)
  from an indexed payload store (`PMap<u64, T>`). An update/removal touches one
  payload entry and at most one order position instead of scanning the whole
  sequence. `from_keyed` is the single construction point.
  - Code: [`crates/irrigation-collections/src/list.rs`](../crates/irrigation-collections/src/list.rs)
  - Gate: `keyed_storage_separates_order_from_payload_index` (10k identities,
    update/remove/reorder, O(log n) payload access)
  - This removes the F5 O(n²) `with_entity` scan ahead of the full store/commit
    protocol.
- [x] **Map key-projection cache.**
  `Map::Storage` is now `MapStorage`, carrying the entries and their key-set
  projection together. Entry writes update one ordered-tree path in each; source
  editors stage the cached projection instead of rebuilding it, and the View’s
  `structure` projection reads the cache. Content equality still compares the
  whole map; this is a projection/read-path slice, not the delta commit yet.
  - Code: [`crates/irrigation-collections/src/map.rs`](../crates/irrigation-collections/src/map.rs)
  - Gate: `map_storage_tracks_the_key_projection_with_the_entries`
- [x] **Equal-definition restatement skips content work.**
  `apply_definitions` now applies a content draft only when that unit actually
  moved. A provider that restages an equal definition no longer rebuilds the
  content or refreshes the structure for it. New counters
  `content_applies`/`content_restatements` expose the distinction.
  - Code: [`crates/irrigation-runtime/src/inner.rs`](../crates/irrigation-runtime/src/inner.rs)
  - Gate:
    `an_equal_definition_restatement_skips_content_application`
    (`crates/irrigation/tests/gates.rs`)
  - Boundary: a changed unit still calls the field’s `define` and still pays
    whole-content equality; removing that is Phase 1’s remaining work.
- [x] **Content movement is classified per unit.**
  `ViewField::unit_moves_content` and `ContentVtable::moves_content` distinguish
  units that change field content (entry payloads, order/structure where it is
  content) from units that only move a projection (map/graph membership,
  graph topology/adjacency). The runtime applies content only for content-moving
  units; projection movement is counted separately.
  - Code: [`crates/irrigation-store/src/field.rs`](../crates/irrigation-store/src/field.rs),
    [`crates/irrigation-core/src/descriptor.rs`](../crates/irrigation-core/src/descriptor.rs),
    [`crates/irrigation-runtime/src/inner.rs`](../crates/irrigation-runtime/src/inner.rs)
  - Counters: `content_applies`, `content_restatements`, `content_projections`.
  - Gates: `map_only_payload_units_move_content`,
    `a_projection_only_unit_skips_content_application`.
- [x] **Changed content publishes without whole-content equality.**
  When the unit comparison says a content-moving unit changed, the grouped
  content is installed as the new value directly; the previous content is not
  re-compared whole. Whole-content equality remains on the source-edit and
  whole-region replacement paths, and a field still applies one draft at a time.
  - Code: `commit_content_changed` in
    [`crates/irrigation-runtime/src/inner.rs`](../crates/irrigation-runtime/src/inner.rs)
  - Gates: the existing cutoff/structure/family gates plus the equal-restatement
    gate remain green.
- [x] **Changed content is applied as one batch per View commit.**
  `ContentVtable::define_many` receives the changed content-moving drafts for a
  View and clones the content struct once, instead of rebuilding it per draft.
  The runtime collects only units that changed and move content, then commits
  the assembled value through `commit_content_changed`.
  - Code: [`crates/irrigation-core/src/descriptor.rs`](../crates/irrigation-core/src/descriptor.rs),
    [`crates/irrigation-macros/src/view.rs`](../crates/irrigation-macros/src/view.rs),
    [`crates/irrigation-runtime/src/inner.rs`](../crates/irrigation-runtime/src/inner.rs)
  - Counter: `content_batches` records one application per View commit.
  - Gate: `several_changed_map_entries_share_one_content_batch`
    (two changed entries, one batch).
  - Family layer: `ViewField::define_many` groups the batch by field; the set
    families override it, and the remaining families use the default
    per-unit loop against one family storage clone.
  - Boundary: the default family batch still loops per unit inside the field;
    Map now applies its payload batch through `AppliedDelta`, while the other
    families still use the default.
- [x] **`AppliedDelta` and the first family batch.**
  `irrigation_store::delta::AppliedDelta` reports changed/removed keys and
  whether content moved. `MapStorage::apply_delta` applies a `MapDelta` in one
  call and returns the applied keys; `Map::define_many` uses it. This is the
  first concrete `FieldDelta`/`Applied` shape.
  - Code: [`crates/irrigation-store/src/delta.rs`](../crates/irrigation-store/src/delta.rs),
    [`crates/irrigation-collections/src/map.rs`](../crates/irrigation-collections/src/map.rs)
  - `DeltaStore` generalizes the same shape for persistent map and set stores:
    [`crates/irrigation-store/src/delta.rs`](../crates/irrigation-store/src/delta.rs).
  - Set families now apply their membership batch through `SetDelta` and
    `AppliedDelta`, with a mixed whole-field fallback that preserves draft
    order: [`crates/irrigation-collections/src/set.rs`](../crates/irrigation-collections/src/set.rs).
  - `GraphStore` implements `DeltaStore` with a typed `GraphKey`, reporting
    node/edge changes and incident-edge removals:
    [`crates/irrigation-store/src/graph.rs`](../crates/irrigation-store/src/graph.rs).
  - Keyed derived lists batch pure identity payloads against one storage clone;
    whole-field/structure drafts preserve order through the fallback:
    [`crates/irrigation-collections/src/list.rs`](../crates/irrigation-collections/src/list.rs).
  - Graph payload batches apply node/edge drafts against one graph store;
    whole-field/topology drafts use the order-preserving fallback:
    [`crates/irrigation-collections/src/graph.rs`](../crates/irrigation-collections/src/graph.rs).
  - `FieldStore` is now the public kernel name; `MapStorage`,
    `DerivedListStorage`, `DerivedSetStorage`, `TreeStorage`, and `AggSeq`
    implement it, and the blanket implementation covers persistent map/set
    stores. Tree patches apply as a typed `TreePatch` delta; reduced sequences
    apply positional `ReducedDelta` edits.
  - `KeyedListDelta` applies identity-keyed list edits and reports changed/
    removed identities; it is the list family’s first normalized field delta.
  - Gates: `field_store_contract_is_implemented_for_persistent_families`,
    `map_storage_is_a_field_store`,
    `keyed_list_storage_is_a_field_store`,
    `keyed_list_delta_reports_identity_moves`,
    `derived_set_storage_is_a_field_store`,
    `tree_storage_is_a_field_store`,
    `tree_storage_patch_is_a_field_delta`,
    `reduced_delta_reports_positional_moves`,
    `reduced_sequence_is_a_field_store`,
    `map_delta_reports_only_moved_keys`,
    `map_define_many_applies_one_payload_batch`,
    `several_changed_map_entries_share_one_content_batch`,
    `set_delta_store_reports_membership_moves`,
    `set_define_many_batches_membership`,
    `set_define_many_preserves_a_whole_field_draft`,
    `graph_delta_store_reports_node_and_edge_moves`,
    `derived_list_define_many_batches_payload_identities`,
    `graph_define_many_batches_node_and_edge_payloads`.
- [x] Workspace test suite green after these slices.

- [x] **Atomic multi-port claim groups.**
  `State::claim_group` validates every scope before registering any claim, so a
  group conflict leaves no partial ownership. `Inner::claim_writes` and the
  public `irrigation::claim_ports` helper expose the group operation.
  - Code: [`crates/irrigation-runtime/src/state.rs`](../crates/irrigation-runtime/src/state.rs),
    [`crates/irrigation-runtime/src/inner.rs`](../crates/irrigation-runtime/src/inner.rs),
    [`crates/irrigation-runtime/src/helpers.rs`](../crates/irrigation-runtime/src/helpers.rs)
  - Gates: `claim_group_conflicts_without_partial_claims`,
    `a_claim_group_dedupes_the_ownership_pair`,
    `a_broad_claim_still_blocks_one_entity`.
  - Entity conflict lookup uses the exact-scope claim index plus `broad_claims`
    (Field/View only); group registration dedupes with one set. This removed an
    O(n²) scan for large output groups and the unused per-field claim index.
- [x] **GeneralComponent output claims.**
  `GeneralCx` records every unit it defines/retracts; after the body returns,
  the runtime claims the whole set as one atomic `claim_writes` group before
  commit. The group is all-or-none, and the large-document syntax fixture keeps
  its no-claim timing.
  - Code: [`crates/irrigation-runtime/src/general.rs`](../crates/irrigation-runtime/src/general.rs),
    [`crates/irrigation-runtime/src/inner.rs`](../crates/irrigation-runtime/src/inner.rs)
  - Gate: `general_component_producer_is_demanded_by_output_view` asserts
    `write_claims > 0`; the fixture gate stays green.
  - Boundary: claims are acquired after the body has produced its units but
    before commit; the commit group is atomic. Claiming before the first write
    would require the keyed-producer split, which remains open.
- [x] **Planned ports and reader waits.**
  A running computation can announce the ports it intends to write with
  `plan_ports`; readers of a planned unit wait for the running provider before
  reading the previous content. Plans are not claims; they install nothing and
  retire with their owner.
  - Code: [`crates/irrigation-runtime/src/state.rs`](../crates/irrigation-runtime/src/state.rs),
    [`crates/irrigation-runtime/src/inner.rs`](../crates/irrigation-runtime/src/inner.rs),
    [`crates/irrigation-runtime/src/helpers.rs`](../crates/irrigation-runtime/src/helpers.rs)
  - Gates: `a_plan_is_visible_until_its_owner_commits`, plus the two-thread
    integration gate `a_reader_waits_for_a_planned_writer_before_its_claim`
    (reader suspends after `plan_ports`, before the writer's `define`).
  - Plan ownership is the invocation body, not the expression-site node that
    calls `plan_ports`; `scope_bodies` records that mapping. A reader never
    waits on a plan whose task is already on its execution stack, so a
    component that plans and then reads its own port does not deadlock.
  - Concurrency counters `planned_ports`, `unplanned_ports`, and
    `reader_plan_waits` distinguish plan activity from live-claim waits.
  - `plan_group` rejects an overlapping live plan before registering any scope;
    handover is explicit via `unplan_ports`. Gate:
    `overlapping_plans_conflict_before_any_claim`.
  - `WriteGroup` wraps plans/claims as an ordered scope group:
    `from_scopes`, `plan`, `unplan`, `claim`, `scopes`. `unplan` withdraws
    planned ports before the claim, so an optional provider can announce intent
    and later retract it without leaving a stale plan.
  - Gates: `a_write_group_plans_and_claims_its_ports`,
    `a_plan_can_be_withdrawn_before_writing`,
    `a_plan_can_be_withdrawn_before_its_claim`.
  - Boundary: plans are still component-declared; automatic derivation from
    schema write sets, group publication, and keyed producers remain open.
- [x] **Per-unit source content fast path.**
  A source transaction with changed units asks the View's
  `changed_units_equal` first. `Some(true)` skips content; `Some(false)`
  installs without whole-content equality; `None` falls back to the
  single-unit proof or whole-content equality.
  - Code: [`crates/irrigation-runtime/src/inner.rs`](../crates/irrigation-runtime/src/inner.rs)
  - Counters: `source_content_applies`, `source_equality_checks`.
  - Definition commits also consult `changed_units_equal` before advancing the
    content value: a set of changed drafts that projects equal content retracts
    nothing and publishes no content version.
  - Map, Set, List, Graph, Reduced, and Tree provide per-unit equality for
    their entity/structure/topology units; Scalar whole-field units compare
    directly. Content replacements with no unit delta still fall back.
  - Gates: `a_single_source_payload_update_skips_whole_content_equality`,
    `a_source_graph_cancellation_uses_unit_equality`,
    `set_unit_eq_compares_membership_only`,
    `derived_list_unit_eq_compares_one_identity`,
    `graph_unit_eq_compares_one_node_or_edge`,
    `reduced_unit_eq_compares_the_sequence_or_one_item`,
    `tree_changed_units_compare_per_node_fields`,
    `scalar_unit_eq_compares_the_whole_value`,
    `scalar_unit_eq_compares_the_whole_value`,
    `a_removal_owns_the_payload_staged_beside_it`,
    `a_transaction_that_returns_to_its_committed_graph_publishes_nothing`.
- [x] **Static component plans.**
  `#[component(callable, plans = [WriteScope, ...])]` emits `plan_ports` before
  the lowered body, so a macro/schema can declare static write scopes without a
  hand-written helper call. Gates: `a_component_registers_its_declared_plans_before_the_body`.
  - Code: [`crates/irrigation-macros/src/component.rs`](../crates/irrigation-macros/src/component.rs),
    [`crates/irrigation-macros/src/attr.rs`](../crates/irrigation-macros/src/attr.rs)
  - Boundary: scopes are expressions evaluated once at body start; keyed
    components can reference the key to plan per-key entity scopes.
- [x] **Keyed component entry point.**
  `#[component(callable, keyed = Key)]` requires exactly the key parameter and
  generates `Spec::call_keyed(runtime, keys)`, which runs one independent
  memoized invocation per key and returns outputs in key order.
  - Code: [`crates/irrigation-macros/src/component.rs`](../crates/irrigation-macros/src/component.rs),
    [`crates/irrigation-macros/src/attr.rs`](../crates/irrigation-macros/src/attr.rs)
  - Gates: `a_keyed_component_calls_one_invocation_per_key`,
    `keyed_rows_commit_per_key_in_key_order` (per-key plans + independent
    commits),
    `keyed_calls_run_concurrently` (two workers overlap),
    `keyed_many_keys_keep_order_with_bounded_workers`.
  - `call_keyed` runs at most `Runtime::worker_count()` scoped workers over a
    shared key cursor, keeping one slot per key; outputs stay in key order. A
    key that reads another waits through the normal unit protocol.
  - `KeySet` enumerates keys from Map/Set/List handles and snapshot vectors;
    generated `call_keyed_from` feeds that enumeration to `call_keyed`.
  - Gate: `one_slow_key_does_not_block_another_keys_commit` — key 2 commits
    while key 1 holds its body open; per-key plans/claims are disjoint.
  - `keys = View` generates `call_all(runtime)`: it snapshots the View's first
    field through `KeySet` and feeds `call_keyed`. `KeySet` covers Map/Set/List
    handles, Map/Set snapshot vectors, and Graph node/edge handles.
  - Gate: `keyed_rows_commit_per_key_in_key_order` uses a source Map's snapshot
    keys and per-key plans.
  - `keys = (View, accessor)` selects a named snapshot accessor (for example a
    graph's `field0_nodes`); gate: `call_all_enumerates_graph_node_handles`.
  - Boundary: key declarations are still macro-level; the full schema layer
    remains open.
- [x] **FieldDelta/UnitChange contract.**
  `FieldDelta` names the typed edit-script contract over `Delta`;
  `AppliedDelta::units`/`into_units` expose `UnitChange::{Changed,Removed}` in
  application order.
  - Code: [`crates/irrigation-store/src/delta.rs`](../crates/irrigation-store/src/delta.rs)
  - Gate: `applied_delta_iterates_unit_changes`.
- [x] **Payload-only commits skip structure refresh.**
  A per-entity `Value` update no longer recomputes the field's key/order/
  topology projection; whole-field payloads and Membership/Structure/
  Adjacency units still refresh it. The source path filters `delta.installed`
  the same way. Counter: `structure_refreshes`.
  - Code: [`crates/irrigation-runtime/src/inner.rs`](../crates/irrigation-runtime/src/inner.rs)
  - Gate: `a_map_payload_update_keeps_the_key_structure_version`.
- [x] **`WriteGroup` plan/claim API.**
  `WriteGroup` wraps an ordered scope list with `from_scopes`, `plan`, `claim`,
  and `scopes`.
  - Code: [`crates/irrigation-runtime/src/helpers.rs`](../crates/irrigation-runtime/src/helpers.rs)
  - Gate: `a_write_group_plans_and_claims_its_ports`.
  - Boundary: plans are still component-declared; automatic schema plans,
    group publication, and keyed producers remain open.

### Phase status

- [x] **Phase 1 — Delta-native field-store kernel.** `FieldStore`, `FieldDelta`,
  `AppliedDelta`/`UnitChange`, Map/Scalar stores, exact changed-key reports,
  batched content application, and payload-only commits that skip structure
  refresh.
- [x] **Phase 2 — All family stores and indexes.** Map, Set, List, Graph, Tree,
  Reduced, Scalar; source/derived parity; keyed list order/payload split; graph
  topology/adjacency; tree patches; stable occurrence ids; per-unit equality.
- [x] **Phase 3 — Port plans and write groups.** `plan_ports`, planned-provider
  reader waits, plan conflicts/withdrawal, `WriteGroup`, atomic claim groups,
  body-atomic publication, GeneralComponent output claims, and per-key claims.
- [x] **Phase 4 — Keyed producers and compute concurrency.** Keyed callables,
  bounded worker scheduling, ordered outputs, `KeySet` enumeration,
  `call_keyed_from`, `keys = View`/`(View, accessor)`, per-key plans, and
  independent per-key commits.
- [x] **Phase 5 — Schema layer and STLC.** `#[incremental_data]` schema Views
  over stock collection fields; indexes/projections are ordinary components
  over the generated handles; the STLC scope-graph example is migrated.
- [x] **Phase 6 — Observation, fixpoint, cleanup.** Per-unit/group observers,
  non-demanding snapshots, fixed-point dependency index, add-only incremental
  `solve_from`, scale tests, and removal of the unused per-field claim index.

### Explicit boundaries

- `call_keyed`/`call_all` enumerate keys from a caller-named collection; the
  schema macro does not invent an enumeration over multiple unrelated fields.
- GeneralComponent output claims are acquired as one batch after the body and
  before commit; claiming before the first write is the keyed-producer path.
- Group publication is body-atomic for ordinary components and per-key for
  keyed producers; splitting a single body into multiple independent groups is
  not exposed as separate user syntax.
