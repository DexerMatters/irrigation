# Heterogeneous View granularity: audit and plan

Status: audit complete; phase 0 of this plan is complete. F1 and F2 are fixed
with gates, as are F3–F5: derived collection structure units are maintained
from committed content, derived `Reduced`/`Graph` fields are rejected at
compile time, observer refresh no longer fails edits, retirement skips live
claims, and definition installs are two-phase. The structural F6–F13 items
remain open. Commit `fea9055` (transaction normalization) came out of the first
audit and is merged.

Relation to the other documents:

- [`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md)
  is the source plan. Sections referenced below: §5.1–§5.7 (View decomposition),
  §6.3 (routing), §8.6 (publication), §9.2 (transient parameters), §11.3
  (storage), §12.4 (memory).
- [`implementation-status.md`](implementation-status.md) records current
  coverage.
- [`../docs/architecture.md`](../docs/architecture.md) explains the node and
  unit model this plan extends.
- [`plan-conformance-audit.md`](plan-conformance-audit.md) is the second audit
  pass: a section-by-section comparison with the source plan and the defects
  found after this document was written (derived-list live reads, whole-field
  retraction, `call` conflicts, hash identities, and others).

## 1. What "perfect" means here

Granularity is exact when three separate contracts hold.

1. **Observation granularity** — a read depends on the narrowest units that
   determine its answer. Reading a map's key set must not depend on any value;
   reading one key must not depend on another key; `to_vec` may depend on
   everything, because its answer does.
2. **Definition granularity** — a provider that writes a collection writes
   units per entity, so redefining one entity with an equal value notifies
   nobody, and changing one entity's value notifies only that entity's
   consumers.
3. **Conversion granularity** — when a value moves between families
   (list→map, map→list, graph→map, list→list through an operator), occurrence
   identity survives, so per-entity work done downstream survives an upstream
   edit that does not change that occurrence.

Today contract 1 holds for `get`/`contains` reads in every family and for
structure reads in source views; contract 2 holds for source collections and for
map definitions, but derived lists and sets are whole-value and derived maps
never write their structure; contract 3 does not exist. The asymmetry is the
subject of this plan.

The invariants the finished system should state and test:

| Id | Invariant |
|---|---|
| I1 | Every collection field has a structure unit that mirrors the committed content: key set for maps, id sequence for lists, member set for sets, topology for graphs, sequence for reduced fields. |
| I2 | Every present entity has a membership unit; every absent entity has one set to `false`, so negative reads wake. |
| I3 | A per-entity value unit changes only when that entity's value changes. |
| I4 | Redefining an entity with an equal value leaves its `changed_at` unchanged and notifies no consumer. |
| I5 | A read observes only the units that determine it: keys→structure, contains→membership, get→membership+value, rank→structure, `to_vec`→structure+values. |
| I6 | When a producer supplies a stable key for an occurrence, that occurrence keeps its identity across edits and reorderings, so downstream per-member work is reused. |
| I7 | Every family has a definition path, or the macro rejects it with a diagnostic. No generated code fails with `rustc` errors that point inside `#[view]`. |
| I8 | Joins and general components can publish per-key units; consumers of an unchanged key do not re-execute. |
| I9 | An observer can subscribe per View or per entity; a per-entity observer completes without waiting for unrelated entities. |
| I10 | `collect` reclaims unreachable scopes and arena slots, and retention is bounded by a policy rather than by lifetime history. |

## 2. Audit method

- Read the unit plumbing: `reactive-runtime::{inner, edit, state, helpers,
  field}`, `reactive-store::{field, aggregate, graph, tree}`, and every handle
  in `reactive-collections`.
- Read every generated accessor in `reactive-macros::{view, tree}` and compared
  what the macro exposes with what the handle reads.
- Ran the workspace suite (`cargo test --workspace`, 127 passing) and built
  small throwaway examples against the public API to confirm or refute each
  suspicion. Confirmed reproducers are in Appendix A; the examples were removed
  after measuring.
- Cross-checked claims in `README.md`, `docs/architecture.md`, and
  `plans/implementation-status.md` against the code.

The audit was static plus targeted reproduction. It did not include model
checking under `loom`, fuzzing, or the external baselines discussed in
`implementation-status.md`.

## 3. Current granularity matrix

"Structure" below is the unit that carries identity, "membership" the per-entity
presence unit, "value" the per-entity payload.

| Family | Source edit | Derived definition | Per-entity reads | Whole-value reads | Per-member operators |
|---|---|---|---|---|---|
| Scalar | value unit | value unit | value | — | — |
| List | structure + membership + value per element | **one value unit** | — (rank reads only) | `len/get/to_vec` (structure + elements) | `map/filter/flat_map` on source lists only |
| Set | structure + membership per member | **one value unit** | `contains` | `len/to_vec` (structure) | none |
| Map | structure + membership + value per key | membership + value per key, **structure never written** | `contains_key`, `get` | `keys/len/is_empty/to_vec/map_values` (structure) | `map_values` |
| Graph | topology + membership + payload + adjacency | **no definition path (macro error)** | `contains_node/node/edge/outgoing/neighbors` | `topology/nodes/edges` | none |
| Tree | per-node membership/tag/fields/children | **no definition path (source-only View)** | `kind/fields/children` | — | — |
| Reduced | structure (the `AggSeq`) | **no definition path (macro error)** | `aggregate/len/get` (structure) | `to_vec` | — |
| Join | — | whole `BTreeMap` output | — | whole output | internal per-key deltas only |

Two structural asymmetries fall out of the table:

1. **Map is one type for source and derived views; list and set split.** The
   split gives `DerivedList`/`DerivedSet` a whole-value unit and their own
   handle; `Map` keeps the source handle, whose structural reads assume a
   source editor wrote the structure unit.
2. **Per-entity work is only possible for source list members and map values.**
   Every derived collection is an assembly: a component that builds a list loses
   identity at the moment it defines it, so nothing downstream can reuse the
   members.

## 4. Findings

Severity is about user-visible correctness first, then contract violations,
then cost.

### F0 — Transaction normalization (fixed during the audit)

`Edit::commit` applied staged writes in order, so a transaction that wrote one
unit twice (a tree editor clears a node and reinstalls it) published a change
even when the final value equaled the committed one. Fixed in `fea9055` by
keeping the last write per unit; gate
`a_transaction_is_normalized_to_its_final_values` fails without the fix.
Measured effect on the resolver example: an edit to a payload nobody reads went
from 7 executions to 0.

### F1 — Derived maps have a stale structure unit (critical)

**Evidence.** `crates/reactive-collections/src/map.rs:132-180`: `keys()` reads
`structure_unit(view, field)`, `len()` is `keys().len()`, `to_vec()` is
`keys()` + `get()`. `crates/reactive-runtime/src/inner.rs:839-1010`:
`apply_definitions` and `retract_unit` write per-key value/membership units and
the region content, but never the structure unit. The source `MapEditor` writes
it (`map.rs:331`); definitions have no equivalent. `single_field_forwarding`
in `view.rs` forwards `keys/len/is_empty` to the handle, so derived map Views
expose the broken reads.

**Status.** Confirmed by reproducer (Appendix A.1). Switching a derived map
from `{3: 30}` to `{1: 10, 2: 20}` yields:

```
len=1  keys=[3]  to_vec=[]  content=[(1, 10), (2, 20)]
```

The structure unit is lazily initialized from content on first read and never
updated afterwards, so the stale set depends on when that first read happened.

**Effect.** `keys/len/is_empty/to_vec/map_values` return wrong results for
derived maps, and `to_vec` can silently return an empty list for a non-empty
map. `EquiJoin` reads `structure_unit` for both inputs
(`crates/reactive-collections/src/join.rs:218-250`), so a join over derived maps
is wrong for the same reason. This contradicts the README row "Derived Map (per
key)" and the architecture table's "structure, per-key membership, per-key
value".

**Fix.** Make structure a runtime-maintained invariant for every collection
field (Phase 0.1): add a structure projection to the content vtable, recompute
the structure unit from the committed content after `apply_definitions` and
`retract_unit`, compare with the previous structure for cutoff, and notify only
on change. Source editors may keep writing it until Phase 1.1 unifies both
paths; a debug assertion can compare the two.

**Status: fixed.** `MapRef::len`/`is_empty` read the structure map directly (no
`keys()` allocation). `ViewField::structure` projects a field's canonical structure;
the content vtable carries the projection and its equality; the runtime
recomputes the structure unit after definition installs and membership
retraction, with cutoff on value-only edits. A statement-position `if`/`match`
is now its own node, so a switched-off branch retires its definitions before the
next statement reads the affected units. Gates:
`derived_map_structure_follows_definition_changes` (key add, key remove,
whole-map replace, read-before-first-define ordering, statement-position
definitions) and `derived_map_value_edits_do_not_touch_structure`.
`join_over_derived_maps_tracks_key_sets` is still missing: `EquiJoin` cannot
demand a provider component because the runtime has one root. Structure refresh
is per-unit on retraction; batching is part of the Phase 1.1 unification.

### F2 — Derived `Reduced` and `Graph` fields fail to compile with macro-internal errors (high)

**Evidence.** `crates/reactive-macros/src/view.rs:503-537` is the
`define_accessors` block for derived views. The `Reduced` arm emits read
methods (`fn <field>()`, `fn <field>_aggregate()`) that reference
`self.content`, which does not exist on the handle, and duplicates the live
`field0()` method. The `Graph` arm does the same for `nodes/edges`;
`shape_of` still classifies both shapes as definable (`view.rs:659`).

**Status.** Confirmed. `#[view(derived)] struct Aggregate(Reduced<Total>);`
compiles to:

```
error: implementation is not supported in `trait`s or `impl`s
error[E0592]: duplicate definitions with name `field0`
```

and `#[view(derived)] struct Topology(Graph<u64, u64>);` to:

```
error[E0609]: no field `content` on type `&Topology`
```

**Effect.** Two families cannot be used as derived views at all, and the error
comes from inside the attribute macro rather than from a diagnostic. This is
also why the matrix has no definition path for them.

**Fix.** Phase 0.2: reject both shapes in a derived `#[view]` with a clear
`compile_error!` message. Phase 3 implements the real paths (`Reduced` defines
its sequence unit; `Graph` stages topology, node payload, and edge drafts).

**Gate.** A compile-failure harness for the macro diagnostics. The workspace
has no `trybuild` dependency and cannot add one offline, so the gate is a
documented `tests/ui` pair plus a note that it needs the harness crate; until
then, the macro rejection itself is the deliverable.

### F3 — A committed edit returns an error and observers never fire on a source-only runtime (high)

**Evidence.** `crates/reactive-runtime/src/runtime.rs:548`:
`notify_subscribers` starts with `let snapshot = self.read()?;`. `read()`
demands the installed root and fails with "no root component installed". `edit`
commits the transaction first and calls `notify_subscribers` afterwards
(`runtime.rs:305-317`), so the transaction is durable but the call reports
failure.

**Status.** Confirmed by reproducer (Appendix A.2):

```
source-only edit: Err(Internal { message: "no root component installed; ..." }), callbacks seen: 0
rooted edit:      Ok(()),                                                       callbacks seen: 1
```

**Effect.** Source-only hosts (the `snapshot()`-style usage documented in the
README) cannot use observers at all, and every edit appears to fail after it has
already committed. This breaks the observer contract "runs after every change to
`V`".

**Fix.** Phase 0.3: demand the root only when one is installed; otherwise use
`capture_snapshot()`. Keep `read()` when a root exists so observation roots stay
active.

**Gate.** `a_source_only_edit_notifies_its_observer` and an assertion that the
edit returns `Ok`.

### F4 — Retirement can clear a node another task is executing (high, inspection)

**Evidence.** `crates/reactive-runtime/src/inner.rs:694-735`: `commit` retires
old trace entries through `retire_unreachable`, which clears value, trace, and
definitions and sets `state = Vacant` after checking only that the node has no
dependents and is not a unit/content node. It does not check `is_running()` and
does not consider waiters.

**Status.** Confirmed by inspection; a deterministic reproducer needs a
schedule-perturbation test.

**Effect.** A node claimed by task A can be retired by task B's commit when B
drops it from its trace at the same moment A demands it. B leaves the node
`Vacant` while A still owns it; A's waiters see "not running" and re-claim, so
two tasks execute one node in one revision — a single-flight violation. Values
are deterministic, so results usually survive, but the claim protocol is the
core invariant, duplicate execution is counted in cost gates, and a non-pure
computation (already unsupported) would corrupt state.

**Fix.** Phase 0.4: `retire_unreachable` returns early for `is_running()` nodes
and nodes with waiters; a later commit or `collect` retires them once the claim
is released.

**Gate.** A two-worker branch-switch stress test that repeatedly (a) demands a
node from one branch, (b) commits the other branch dropping it, and (c) asserts
no node commits twice in a revision (add a per-revision execution counter or
detect two commits for one node/revision).

### F5 — A failed definition install can leave partial region state (medium, inspection)

**Evidence.** `crates/reactive-runtime/src/inner.rs:765-837`:
`install_definitions` retracts old definitions, then installs drafts view by
view. If `apply_definitions` fails for a later view (for example
`DefinitionDenied` for an unregistered view), the function returns `Err` after
earlier retractions/installs are already applied; `commit` calls `fail` without
rollback and the provider's `definitions` list still describes the old set.

**Status.** Confirmed by inspection; the reachable path is narrow because the
macros call `ensure_view` before defining, so it needs a hand-built `UnitKey`
into an unregistered view.

**Effect.** A failed commit can leave a region partially retracted or installed
— visible to every other consumer — contradicting "definitions commit
atomically" and "a failed scope cannot leave a partial output".

**Fix.** Phase 0.5: two-phase install. Validate every draft (views registered,
unique definitions, owner rules) before mutating, then apply; or build the new
region/content privately and swap it under one lock. The persistent content
makes the swap cheap (`Arc`).

**Gate.** `a_failed_definition_install_leaves_the_region_unchanged`: define one
valid unit and one into an unregistered view in the same commit, assert the
error and that the valid unit is still undefined.

### F6 — Derived lists and sets are whole-value definitions, and per-member operators are source-only (structural)

**Evidence.** `crates/reactive-collections/src/list.rs:521-575`:
`DerivedListRef` has one value unit and no `map/filter/flat_map`.
`set.rs:324-375`: `DerivedSetRef` is the same. The per-member operators live in
`impl SourceListRef` (`list.rs:114`, methods at 200/258/296).
`map_values` is the only cross-view per-entity operator, and it returns a
`Vec`, not a View.

**Status.** Documented boundary (`list.rs` module docs, README), not a bug. It
is the main granularity gap this plan targets.

**Effect.** A component that builds a list or set from a computation loses the
identity of everything in it. `source list → map → filter` re-executes the
assembly; nothing downstream of the derived boundary can reuse per-member work;
the plan's "insertion at the front retains identity-based mapped work" holds
only inside a single `map` call, not across definitions.

**Fix.** Phase 2: keyed derived collections.

- New definition protocol: a structure draft (ordered ids for lists, member ids
  for sets) plus one value draft per entity, staged atomically by its provider.
- The runtime computes membership deltas from the old and new structure,
  retracts absent entities, and keeps present ids stable.
- API: `List::define_keyed(iter)` / `Set::define_keyed(iter)`, with explicit
  `u64` occurrence ids first (matching the list storage `(u64, T)`), and a
  persistent key→id table as a later option for producers that only have
  semantic keys.
- Per-member `map/filter/flat_map` move to all list handles; the source path
  stays byte-for-byte compatible.
- `from(iter)` keeps whole-value semantics, so no existing behavior changes.

**Gates.** `keyed_list_reorder_reuses_member_work`,
`keyed_list_removal_retracts_one_member`,
`keyed_set_membership_is_per_member`,
`derived_list_map_reuses_members_after_value_edit`.

### F7 — Derived graphs and derived trees have no path at all (structural)

**Evidence.** `crates/reactive-macros/src/tree.rs:405`: the tree descriptor
hardcodes `ViewKind::Source`; `#[tree_view]` has no derived form. Graph
definitions fail to compile (F2). Graph snapshot accessors expose only
`nodes/edges` (`view.rs:392-420`) while the live handle exposes topology,
payloads, edges, and adjacency — another source/derived asymmetry.

**Status.** Confirmed.

**Effect.** Components can read graphs and trees but cannot define them; the
family matrix is not closed. Users reconstruct a whole tree against a source
View or drop to `GeneralComponent`, which loses the typed tree API.

**Fix.** Phase 0.2 rejects derived `Graph`/`Reduced`/`Tree` fields with a
diagnostic. Phase 3 adds graph definitions (topology draft + node/edge payload
drafts + adjacency maintenance). Derived trees stay out of scope for this plan
and should be rejected explicitly; the tree representation is source-only by
design today.

**Gates.** Macro diagnostics for all three shapes; `derived_graph_payload_and_topology_are_separate`,
`derived_graph_node_removal_retracts_edges`.

### F8 — The join publishes one whole map value (structural)

**Evidence.** `crates/reactive-collections/src/join.rs:196`:
`type Output = BTreeMap<Key, Joined>`. `initialize`/`update` read
`structure_unit` of both inputs (`join.rs:218-250`). Nothing stages per-key
units for the joined result.

**Status.** Confirmed. The internal update is per-key; the published value is
whole.

**Effect.** A consumer of the join cannot reuse the result of an unchanged key;
every joined-value change is a whole-output change, and the join inherits F1
when an input is a derived map.

**Fix.** Phase 3: publish the joined map into a derived Map View with the keyed
definition protocol, so each joined key is a unit (and F1's structure fix
applies). Keep `BTreeMap` as the `GeneralComponent::Output` for compatibility,
but add an optional View target.

**Gates.** `join_view_updates_only_changed_keys`,
`join_view_retracts_keys_that_leave_either_side`.

### F9 — Observation granularity is the whole View (structural)

**Evidence.** `crates/reactive-runtime/src/runtime.rs` (`observe`,
`observe_with_changes`) subscribes per View; `notify_subscribers` fires when the
View's content version changes; there is no per-unit or per-entry subscription.

**Status.** Confirmed; plan §8.6 explicitly allows a per-entry observer to
complete sooner.

**Effect.** An observer interested in one map key is woken for every key change
(and, with F1, may be woken for structural reads that are stale).

**Fix.** Phase 4: per-entry subscriptions (`observe_entry::<V>(key)` for maps /
`observe_member` for lists/sets) driven by unit versions instead of the content
version. The existing whole-View observer stays.

**Gates.** `a_per_key_observer_ignores_other_keys`,
`a_per_key_observer_completes_before_the_whole_view_is_rematerialized`.

### F10 — Retention is lifetime-shaped, not policy-shaped (structural)

**Evidence.** `crates/reactive-runtime/src/state.rs:273`: `ScopeTable` only
grows; `State::nodes` is an append-only `Vec` (`state.rs:396`); `collect`
(`inner.rs`) clears records but never frees slots or prunes scopes. Component
invocations are retained per distinct argument tuple with no bound; plan §9.2
asks for bounded caching to prevent unbounded history. `collect` is manual.

**Status.** Confirmed by inspection; no test asserts memory stability under
churn.

**Effect.** A long-lived runtime with high argument churn grows the arena, the
scope table, and the invocation map even after `collect`. `memory_report` does
not expose scopes or task state, so the growth is invisible.

**Fix.** Phase 4: prune unreachable scopes during `collect` (walk reachable
nodes' scopes), add arena slot reuse with generation checks (the plan's §11.3
generational arena), add a bounded invocation-cache policy, and extend
`memory_report` with scopes, tasks, and structure units.

**Gates.** `collect_reclaims_scopes_and_reuses_slots`,
`argument_churn_stays_within_the_retention_policy`.

### F11 — Documentation and claims lag the code (medium)

README says "Derived Map (per key)" and the architecture table says maps have a
structure unit, but derived maps do not (F1). The status document does not
mention that derived lists/sets are whole-value while derived maps are per-key,
nor that derived `Reduced`/`Graph` are unimplemented. `docs/architecture.md`
should state the granularity contract from §1 above and mark whole-value reads
as intentional.

**Fix.** Phase 0.7: update `README.md`, `docs/architecture.md`,
`implementation-status.md`, and `evidence.md` together with the Phase 0 fixes;
add a "granularity contract" section to the architecture doc that the invariants
in §1 can be checked against.

### F12 — Minor: `TaskStatus::Done` is never constructed (low)

`crates/reactive-runtime/src/task.rs:37-46` defines it, `runtime.rs:415` matches
it, and `finish_attempt` removes the task without assigning it. Either remove
the variant or set it before removal so diagnostics can distinguish "finished"
from "never existed".

### F13 — Minor: `MapRef::len()` is O(n) (low)

`map.rs:142` computes `keys().len()`, materializing the full key set to count
it. A size unit maintained with the structure (Phase 1) makes `len` O(1) and
keeps the narrow observation (structure only). Same for `SetRef::len`, which
goes through `to_vec`.

## 5. Plan

### Phase 0 — Correctness (target: one short milestone)

| Task | Finding | Deliverable | Gate |
|---|---|---|---|
| 0.1 | F1 | Runtime-maintained structure units for collection fields (content-vtable projection; cutoff; retraction) | derived-map and join gates above |
| 0.2 | F2, F7 | Macro diagnostics for derived `Reduced`/`Graph`/`Tree` fields; no generated `self.content` in handle impls | compile-fail harness (or documented pair) |
| 0.3 | F3 | `notify_subscribers` snapshots without a root; edit reports the commit outcome | `a_source_only_edit_notifies_its_observer` |
| 0.4 | F4 | `retire_unreachable` skips running nodes and nodes with waiters | branch-switch stress gate |
| 0.5 | F5 | Two-phase definition install (validate, then apply/swap) | `a_failed_definition_install_leaves_the_region_unchanged` |
| 0.6 | F12 | Remove or use `TaskStatus::Done` | — |
| 0.7 | F11 | README/architecture/status/evidence updated with the contract and known limits | doc review |

Acceptance for the phase: workspace tests green, clippy clean, each gate above
present and demonstrably failing before its fix (where a reproducer exists).

### Phase 1 — One structure contract for every family

1. **Structure projection.** Extend the content vtable (or `FieldDescriptor`)
   with a per-field structure value and equality: map keys, list id sequence,
   set members, graph topology, reduced sequence. This is the one runtime change
   that makes structure universal. It touches the hand-built descriptors in
   `crates/reactive-runtime/tests/kernel.rs`; update them.
2. **Runtime maintenance.** Every definition commit recomputes structure for
   the fields it touched, compares against the previous unit value, and bumps
   only on change. Retraction recomputes too. Source editors keep writing
   structure; a debug build compares editor-written and recomputed values and
   panics on divergence until 1.3 makes the runtime the single writer.
3. **Reads.** `keys/len/is_empty` read only structure; `to_vec` reads structure
   plus values; `len` uses a size projection or the structure's size. Add
   `structure_version` to diagnostics if useful for tests.
4. **Coverage.** Lists and sets keep their whole-value derived behavior in this
   phase; the structure unit exists for them and is the foundation for Phase 2.

**Status: complete.** Every collection marker has a structure projection
(map keys, list ids, set members, graph topology, reduced sequence); the runtime
recomputes the structure unit after whole-value definitions and retractions as
well as membership changes, with equality cutoff. Derived lists and sets still
have whole-value semantics; their structure unit is the positional id sequence
or member set that Phase 2 will key identities with. Source editors remain the
writers for source views. Gates:
`structure_is_maintained_for_every_family`,
`value_only_edits_do_not_run_structure_consumers`,
`structure_changes_do_not_run_value_consumers`,
`graph_structure_projects_topology`, `reduced_structure_projects_the_sequence`.

### Phase 2 — Identity-preserving derived collections

**Status: complete.** Derived lists now have two storage modes: a whole-value
definition (`from`, positional identities, unchanged behavior) and a keyed
definition (`define_keyed`, explicit identities). The keyed protocol stages one
value draft per entity plus the assembled storage; the runtime retracts absent
entities and keeps surviving identities. `ids`, `get_key` and per-member `map`
are available on derived lists. Gates:
`keyed_list_identity_survives_reorder_and_removal`,
`derived_list_map_reuses_members_after_reorder_and_value_edit`. Derived sets
gain the same protocol: `define_keyed` stages one membership unit per member
plus the structure draft, `contains` reads the per-member membership unit, and
retraction certifies the false value instead of clearing it (gate
`keyed_set_membership_is_per_member`). Per-member `map`, `filter` and
`flat_map` all memoize by identity on keyed lists
(`derived_list_filter_and_flat_map_memoize_per_identity`); rank reads stay
structure-sensitive by design, and `get_key` is the per-entity read.

1. **Protocol.** A provider stages a structure draft (ordered ids / member ids)
   plus one value draft per entity; the runtime diffs old and new structure,
   retracts absent entities, and reuses ids for surviving ones.
2. **API.** `List::define_keyed`, `Set::define_keyed` (explicit `u64` ids
   first), and per-member operators on all list handles. `from(iter)` keeps
   whole-value semantics.
3. **Reads.** `get`/`contains` become per-entity for keyed collections; rank
   reads stay structure-sensitive by design (I5).
4. **Chains.** `map`/`filter`/`flat_map` on a keyed derived list reuse members
   across edits and reorderings; removal retracts exactly one member.

Gates: `keyed_list_reorder_reuses_member_work`,
`keyed_list_removal_retracts_one_member`,
`keyed_set_membership_is_per_member`,
`derived_list_map_reuses_members_after_value_edit`.

### Phase 3 — Heterogeneous composition

**Status: in progress.** Derived `Reduced` fields define their whole sequence
(`a_derived_reduction_defines_and_redefines_its_sequence`). Cross-family
operators `Map::to_list` and `List::to_map` publish keyed definitions from one
family into another, preserving per-key identity
(`map_to_list_preserves_key_identity`, `list_to_map_retracts_missing_keys`).
Derived graphs are supported: `define_graph` stages node payloads, edge payloads
and a topology draft; node removal retracts incident edges; derived adjacency is
computed from the topology unit; source graphs keep per-node adjacency units
(`derived_graph_topology_and_payloads_are_separate`). The join can publish a
per-key target map through `JoinSpec::target` while keeping its whole-map output
(`join_view_updates_only_changed_keys`), and `Graph::node_map` defines one map
entry per node (`graph_node_map_updates_one_node`). Per-key `GeneralComponent`
output uses the same `GeneralCx::define` path the join exercises.
`Tree::flatten_map` remains.

1. **Join as a View.** Done: `JoinSpec::target` names an optional map View that
   receives one entry per joined key; the whole `BTreeMap` output is unchanged.
   The target View must be registered before the join is demanded.
2. **Derived graphs.** Topology draft plus node/edge payload drafts, adjacency
   maintenance, and edge retraction on node removal. Derived `Reduced` defines
   its sequence unit. Derived trees remain rejected (documented).
3. **Cross-family operators with identity:** `Map::to_list(key_fn)`,
   `List::to_map(key_fn)`, `Graph::node_map()` are done; `Tree::flatten_map()`, each
   defining a keyed View so downstream per-key work survives an upstream
   value edit. These are the first APIs that make I6 checkable across families.
4. **GeneralComponent outputs.** Done in part: `GeneralCx::output` returns a
   typed `OutputTxn` (`define`/`field`/`entry`/`membership`) that stages
   per-unit definitions; `GeneralCx` also supports dependency scopes with
   retired-scope notification filtering. Gate:
   `retired_dependency_scopes_filter_notifications`.

Gates: `join_view_updates_only_changed_keys`,
`map_to_list_preserves_key_identity`, `graph_node_map_updates_one_node`,
`general_component_publishes_per_key_units`.

### Phase 4 — Observation, retention, evidence

**Status: in progress.** Per-entry observers are implemented:
`Runtime::observe_units` watches a set of logical units and
`Runtime::observe_entry` watches one map entry's membership and value units;
the callback fires only when a watched unit's version moves
(`a_per_key_observer_ignores_other_keys`). `collect` now prunes interned
scopes that no live node references, `ReclaimStats` reports them, and
`memory_report` gains `scopes` and `tasks`
(`collect_reclaims_unreachable_scopes`). Argument churn is bounded by
reachability: old invocation records are reclaimed
(`argument_churn_stays_within_the_retention_policy`). Arena slot reuse
(generational `NodeId`s) remains.

1. Per-entry observers (F9).
2. `collect` prunes scopes, reuses arena slots with generation checks, and
   enforces a bounded invocation policy (F10); `memory_report` gains scopes,
   tasks, structure units, and invocation count.
3. Benchmark matrix for cross-family workloads: `workloads.rs` now reports
   map→list, list→map, join-view, and graph-node-map rows with executions and
   wall time at 100 and 1000 entities (tree flatten still missing).
4. Docs: the granularity contract in `docs/architecture.md`, the evidence index
   updated with every new gate.

Gates: `a_per_key_observer_ignores_other_keys`,
`collect_reclaims_scopes_and_reuses_slots`,
`argument_churn_stays_within_the_retention_policy`.

## 6. Ordering and risk

- F1 and F3 are user-visible correctness issues and should land first and
  independently of the rest; F1's gate is cheap and the fix is contained to the
  definition commit.
- F4/F5 are protocol fixes; F4 is a race, so its gate must be a stress test with
  a deterministic failure mode if possible (per-revision commit counting),
  otherwise it should be marked as "needs a perturbation harness" rather than
  claimed covered.
- Phase 2 is the largest change: it adds a definition protocol and an id
  allocation policy. Keep `from(iter)` untouched so the change is additive.
- Structure units add one unit and one comparison per touched field per commit.
  Every collection field already has a structure unit (source views), so the
  cost is the content projection and the comparison. Measure before Phase 1
  lands: the map benchmark and the tree workload are the sensitive cases.
- Retention work (F10) interacts with `collect`'s current gate acquisition and
  with suspended tasks; do it after the keyed protocol so structure units are
  not being reclaimed differently than other units.
- Non-goals: incremental `to_vec`/whole-collection reads; rank-addressed list
  observations (a position is not an entity); derived trees; replacing the
  dispatcher model described in `routing-generations-plan.md`.

Open questions to settle at the start of each phase:

1. Does structure belong in the content vtable, the field descriptor, or the
   region? The vtable is the only place that can see typed content per field.
2. Explicit `u64` ids or a persistent key→id table for keyed lists? Start with
   explicit ids; a key table is a follow-up if producers cannot supply ids.
3. Should derived lists/sets get structure + membership units in Phase 2 while
   keeping a whole-value unit as a compatibility read? Recommended: the value
   unit stays for `to_vec`, membership becomes per entity.
4. How is a "node committed twice in one revision" detected in a gate without
   changing production semantics? A test-only counter behind a feature flag, or
   a debug assertion in `commit` that no node in `Ready` state is executed
   again at the same revision.

## Appendix A — Confirmed reproducers

### A.1 Derived map structure (F1)

```rust
#[view(source)]
pub struct Flag { on: bool }

#[view(derived)]
pub struct Registry(Map<u32, u32>);

#[reactive]
mod m {
    use super::{Flag, Registry};

    #[component]
    pub fn provider(flag: &Flag) -> Registry {
        if flag.on() {
            Registry::from([(1u32, 10u32), (2u32, 20u32)])
        } else {
            Registry::from([(3u32, 30u32)])
        }
    }

    #[component]
    pub fn root(flag: &Flag) -> (Registry, usize, Option<u32>, Vec<(u32, u32)>) {
        let registry = provider(flag);
        (registry, registry.field0().len(), registry.field0().get(&3), registry.field0().to_vec())
    }
}
```

Observed, alternating the flag:

```
on=false len=1 get(3)=Some(30) to_vec=[(3, 30)] content=[(3, 30)]
on=true  len=1 get(3)=None     to_vec=[]        content=[(1, 10), (2, 20)]
on=false len=1 get(3)=Some(30) to_vec=[(3, 30)] content=[(3, 30)]
```

`get` is correct because it reads membership and value units; `len`/`keys`/
`to_vec` read the structure unit, which was initialized on the first read and
never updated.

### A.2 Source-only observer (F3)

```rust
let runtime = Runtime::new();
runtime.edit(|edit| edit.view::<Counter>().set_value(1)).expect("seed");
let sub = runtime.observe::<Counter>(move |_snapshot| { /* count */ }).expect("observe");
let result = runtime.edit(|edit| edit.view::<Counter>().set_value(2));
```

Observed:

```
source-only edit: Err(Internal { message: "no root component installed; call `runtime.install` first" }), callbacks seen: 0
rooted edit:      Ok(()),                                                       callbacks seen: 1
```

The edit committed (the view content is new); only the notification read failed.

### A.3 Derived `Reduced` and `Graph` (F2)

```rust
#[view(derived)]
pub struct Aggregate(Reduced<Total>);

#[view(derived)]
pub struct Topology(Graph<u64, u64>);
```

Observed `cargo build` errors:

```
error: implementation is not supported in `trait`s or `impl`s
error[E0592]: duplicate definitions with name `field0`
error[E0609]: no field `content` on type `&Topology`
```

## Appendix B — Audit inventory

Files read end to end or in the relevant sections:

- `crates/reactive-runtime/src/{inner,edit,state,runtime,task,demand,gate,general,helpers}.rs`
- `crates/reactive-runtime/tests/kernel.rs`
- `crates/reactive-collections/src/{list,map,set,graph,tree,reduce,join}.rs`
- `crates/reactive-macros/src/{view,tree,component,lower}.rs`
- `crates/reactive/tests/{gates,suspension,tree_view}.rs`, `crates/reactive/examples/{workloads,scaling,parallel,name_resolver}.rs`
- `README.md`, `docs/architecture.md`, `plans/{implementation-status,evidence,routing-generations-plan}.md`

Commands: `cargo test --workspace --offline` (128 passing), `cargo test
--release --workspace --offline`, `cargo clippy --workspace --all-targets
--offline -- -D warnings`, plus the throwaway reproducers in Appendix A (run
from `crates/reactive/examples/` and deleted afterwards).
