# Plan conformance audit

Status: second audit pass, complete for the sections below. This document
compares the implementation with
[`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md)
section by section, and records defects that the previous audits did not find.
It complements [`heterogeneous-granularity-plan.md`](heterogeneous-granularity-plan.md)
(granularity findings F1–F13); those are referenced rather than repeated.

The first pass audited `reactive-runtime`. This pass added:
`reactive-collections` handles and editors, the macro-generated accessor paths,
`reactive-store` field semantics, the tree editor, `Runtime::call`, the observer
path, and the plan's own normative text for §2, §4–§12.

All findings marked **confirmed** were reproduced with a throwaway example
against the public API; the examples were removed afterwards and their code is
in Appendix A. Findings marked **inspection** are code reads without a
reproducer, usually because the trigger is a race or an adversarial name.

## 1. Summary

New defects in this pass:

| Id | Finding | Severity |
|---|---|---|
| N1 | Derived list live reads (`to_vec/len/get`) always fail with `TypeMismatch`; only snapshots work | critical |
| N2 | Whole-field definitions (scalar/list/set) cannot be retracted: the old value persists and no consumer is invalidated | high |
| N3 | `Runtime::call` bypasses the unique-definition invariant: two components can define one unit and both succeed | high |
| N4 | A committed edit reports `Err` when the observer refresh (root demand) fails, including a root that is simply uninstalled | high |
| N5 | View/site/scope identities are truncated hashes; distinct names alias silently (ViewId collision demonstrated end to end) | critical |
| N6 | The component macro accepts opaque function calls and hidden mutation; §7.2 requires rejecting them | high |
| N7 | Engine errors inside a parallel `map` become `Panicked { "parallel map worker: …" }` and print a panic message | medium |
| N8 | `commit` never checks that the committing task still owns the node | medium |
| N9 | Cancellation is not checked inside the commit critical section | medium |
| N10 | Absence-predicate unit cells are created on read and never reclaimed, contrary to §5.1 | medium |
| N11 | Tree `replace` rebuilds every descendant with fresh ids; only the replaced node keeps its identity | medium |
| N12 | Component output projections cannot be demanded separately; every call evaluates all of them | medium |
| N13 | Routing generations and the owner dispatcher of §6.2–§6.3 do not exist; provider routing is not part of any dependency | high |
| N14 | `GeneralComponent` implements a small fraction of the §10.5 design (no `request().await`, no dependency scopes, no output transaction, no suspendible futures) | high |
| N15 | Explainability answers "why did it change" only; not validated-vs-executed, identity reuse, or blockers/cycles (§11.6) | low |
| N16 | Invalidation cost is not recorded separately from recomputation (§8.4) | low |
| N17 | Duplicate-definition diagnostics carry labels, not the two source locations (§2.2) | low |
| N18 | Incoming graph adjacency is stored but has no observation unit or read API (§5.6) | low |
| N19 | Fixed-point resets are not reported (§9.5) | low |
| N20 | The `reactive-plan` crate is declared as a dependency but never used | low |
| N21 | The join has no gate for both sides changing in one transaction, and it is not the bag join of §5.5 | medium |
| N22 | Tree cross-references and whole-tree import ambiguity reporting are absent (§5.7) | medium |
| N23 | Whole-context component arguments rekey every recursive call (the plan's §13.3 counterexample) | medium |

Already reported: F1 (derived map structure unit), F2 (derived `Reduced`/`Graph`
macro errors), F3 (observer without a root; fixed with N4), F4 (retirement of
running nodes; fixed), F5 (partial definition install; fixed), F6 (derived
collections are whole-value), F7 (no derived graph/tree), F8 (join output is a
whole map), F9 (whole-View observers), F10 (retention), F11 (docs), F12/F13
(minor).

### C0 progress

Implemented and gated (commit `fd0a4ce`…`d9f…`):

| Finding | Fix | Gate |
|---|---|---|
| N1 | `DerivedListRef` reads the stored `PSeq<T>` | `derived_list_live_reads_match_the_snapshot` |
| N2 | Whole-field retraction reverts content and unit to empty and invalidates readers | `a_retracted_whole_field_definition_reverts_to_empty` |
| N3 | Conflicts resolve by "the earlier provider still claims the unit", on root and `call` demands | `two_called_components_cannot_define_one_unit` |
| N4/F3 | `edit` reports the commit; refresh failures go to `Runtime::notification_error`; source-only runtimes snapshot without a root | `a_source_only_edit_notifies_its_observer`, `an_edit_is_not_failed_by_a_failing_root` |
| N7 | Parallel members resume the original engine payload | `a_cycle_inside_a_parallel_member_stays_a_cycle` |
| N8 | `NodeState::Running` carries a claim generation; `commit` rejects stale claims | `commit_rejects_a_stale_generation` (unit) |
| N9 | `commit` checks the cancel token under the metadata lock | `commit_checks_cancellation_under_the_lock` (unit) |
| N10 | `collect` reclaims false membership cells that no live trace keeps | `absence_cells_are_reclaimed_with_their_consumers` |
| F4 | Retirement skips nodes with a live claim or waiters | `retirement_leaves_a_running_node_alone` (unit) |
| F5 | Definition installs validate all target regions before mutating | `a_failed_definition_install_is_atomic` |
| N5 | Full-width `ViewId`/`ComponentId`, registration collision rejection, counter-allocated scopes, sequential sites with per-component body scopes, equality-checked map-member keys | `full_width_ids_do_not_alias_where_truncation_did`, `keyed_child_scopes_disambiguate_equal_hashes`, `map_member_scopes_compare_keys_not_only_hashes` |

C0 is complete.

### C1 progress

| Finding | Fix | Gate |
|---|---|---|
| F1 | Derived collection structure units are projected from committed content and refreshed on membership changes, with cutoff on value-only edits | `derived_map_structure_follows_definition_changes`, `derived_map_value_edits_do_not_touch_structure` |
| F2 | Derived `Graph` fields are rejected with attribute-level diagnostics; derived `Reduced` is now supported (Phase 3) | `a_derived_graph_field_is_a_clear_diagnostic` (macro unit test), `a_derived_reduction_defines_and_redefines_its_sequence`, `a_named_derived_reduction_uses_generated_accessors` |
| N24 (new) | Statement-position `if`/`match`/block statements are nodes, so a switched-off branch retires definitions before the next statement reads their units | covered by the statement form of `derived_map_structure_follows_definition_changes` |
| N11 | Tree replacement reconciles the new build against the existing subtree and reuses children whose variant is unchanged | `replacing_a_variant_reuses_unchanged_children` |
| N12 | `Runtime::read_projection`/`call_projection` demand one projection node; siblings stay unevaluated | `reading_one_projection_does_not_evaluate_the_other`, `a_call_projection_is_independently_demandable` |
| N16 | `RuntimeStats::invalidations` records the nodes marked suspect by each invalidation cone | `invalidation_cost_is_counted` |
| F9 | Unit-scoped and per-entry observers fire only for watched units | `a_per_key_observer_ignores_other_keys` |
| F10 | `collect` prunes unreachable scopes, reuses arena slots through generation-checked `NodeId`s, and `memory_report` reports scopes, tasks, and structure units | `collect_reclaims_unreachable_scopes`, `collect_reuses_arena_slots_and_detects_stale_ids`, `argument_churn_stays_within_the_retention_policy` |
| N15 (partial) | Explanations carry `executed_at`/`validated_at`, scheduling state, waiter count, and `Explain::waiting_tasks` | `explanations_distinguish_validation_from_execution` (identity-reuse and cycle inspectors still open) |
| N20 | `reactive-plan` is published through `reactive::plan` as the lowering vocabulary; the unused runtime dependency is removed | `the_plan_vocabulary_is_public` |
| N13 (partial) | Units carry a routing generation bumped on provider changes; deps record it and validation checks it; `provider_of`/`routing_generation` expose the route; `Change` flags routing-only changes | `a_provider_handover_advances_the_routing_generation`, `trace_validation_rejects_a_routing_change_with_an_equal_value` (dispatcher/recipes/route retirement still open) |
| N6 (partial) | `#[component(strict, allows = [path, ...])]` rejects undeclared free-function calls with a diagnostic; declared primitives are allowed; the default remains permissive for migration | `strict_components_reject_undeclared_calls`, `strict_components_allow_declared_primitives`, `a_strict_component_runs_a_declared_primitive` (method calls, IO, and hidden mutation still unchecked) |
| N14/C3 (partial) | `GeneralCx` gains dependency scopes (`begin_scope`/`end_scope`/`retain`, retired-scope notification filtering), a typed `OutputTxn` (`define`/`field`/`entry`/`membership`), and `observe` to retain dependencies without re-reading values | `retired_dependency_scopes_filter_notifications` (typed Inputs/Params/Returns, `request().await`/suspendible futures, local executor, output capabilities still open) |
| §5.5 bag join | `EquiJoin` with `i64` sides is a bilinear bag join over signed weights; updates retain untouched keys' dependencies and retract removed keys from the target | `signed_weight_join_applies_the_bilinear_rule` (phase-4 progress gate and external baselines still open) |
| N15 (partial) | `Runtime::wait_edges` exposes the wait graph for blocker/cycle inspection | `argument_churn_stays_within_the_retention_policy` (identity-reuse diagnosis still open) |
| N19 (partial) | `FixedPoint::contributions` reports the work of a from-scratch reset | `fixed_point::tests::reachability_matches_a_from_scratch_traversal` (SCC localization still open) |
| Phase 4 gate | Invocation churn is bounded by reachability: `collect` drops old invocation records and scopes | `argument_churn_stays_within_the_retention_policy` |
| N21 | A two-sided join transaction is gated against the from-scratch join; the join can publish a per-key target map | `a_join_tracks_both_inputs_changing_in_one_transaction`, `join_view_updates_only_changed_keys` (bag-join semantics still open) |
| F12/F13 | `TaskStatus::Done` removed; `MapRef::len/is_empty` read the structure map directly | covered by the existing map/join gates |

N24 was found while gating F1: definitions staged in statement-position branches
were retired only at the enclosing component's commit, after later statements
had already read the stale units. The lowering now gives control-flow statements
their own `eval` node.

Phase 1 of the granularity plan (one structure contract for every family) is
also complete: map, list, set, graph and reduced fields all project structure;
the runtime maintains it for whole-value definitions and retractions;
`value_only_edits_do_not_run_structure_consumers` and
`structure_changes_do_not_run_value_consumers` show the cutoff split. Derived
lists and sets keep whole-value semantics until Phase 2.

Phase 2 of the granularity plan has started: keyed derived lists with stable
identities, `define_keyed`/`get_key`, and per-member `map` reuse across reorders
and value edits (`keyed_list_identity_survives_reorder_and_removal`,
`derived_list_map_reuses_members_after_reorder_and_value_edit`); keyed derived
sets with per-member membership units
(`keyed_set_membership_is_per_member`). The gate also found and fixed a
retraction bug: retracting a membership unit cleared its own certification in
the separate value-unit branch, so a direct membership read reported
"undefined" instead of false. Phase 2 is complete: keyed lists and sets, per-identity `map`/`filter`/`flat_map`, and the membership protocol flag.

Phase 3 of the granularity plan has started: derived `Reduced` fields and
derived graphs are supported families, and `Map::to_list`/`List::to_map`
preserve per-key identity across families. Join-as-View and the graph
cross-family operator are implemented; the benchmark matrix in `workloads.rs`
reports map→list, list→map, join-view, and graph-node-map rows. Tree flatten
remains.

Still open from C1: N6 (declared primitives). The body still computes the whole
output tuple, so per-projection demand saves the sibling projection node but not
sibling body work; splitting tuple construction into lazy projections is part of
the routing/lowering phase. The join-over-derived-maps gate is deferred: the
runtime has one root, and `EquiJoin` cannot demand a provider component.

## 2. Section-by-section conformance

Status values: **yes** (conformant), **partial**, **no** (absent or divergent),
**deviation** (documented and accepted).

| Plan | Status | Evidence / note |
|---|---|---|
| §2.1 primitive-level precision | partial | `get`/`contains_key`/`edge_weight` observe narrow units (tests); `map[k].field` observes the whole value of key `k`, not its fields; whole collection algorithms re-traverse members for assembly while bodies stay memoized (F6). |
| §2.1 reject unsupported lowering | **no** | Loops, `return`, `unsafe`, `async`, effect macros are rejected, but arbitrary function calls are accepted as opaque nodes (N6); the plan names opaque calls explicitly. |
| §2.2 one definition per unit | **partial/no** | Enforced for root demands, bypassed by `Runtime::call` (N3); diagnostics lack source locations (N17). |
| §2.2 diagnostics at construction/expansion | partial | End-of-revision reachability resolution (handover) is a different mechanism; no source spans. |
| §2.3 dynamic dependency staging | yes | Key nodes + membership/value units discovered at execution; cached and re-validated (negative-read gates). |
| §2.4 no raw mutable access | yes | `Param` implements `Deref` only; editors own mutation; no `DerefMut` in the public API. |
| §4.1 singleton View namespace | partial | Semantics hold; namespace key is a truncated hash, so two names can alias (N5). |
| §4.2 typed products and independent projections | partial | Projections are separate nodes with cutoff; there is no per-projection demand (N12), and a call returns the whole tuple. |
| §4.3 snapshot/change laws | yes | Apply/compare/normalize at commit (F0 fixed in `fea9055`); randomized oracle. Whole-field retraction is the exception (N2). |
| §4.4 three graphs distinct | yes (implicit) | Control graph is the invocation/scope tree plus ordered traces; no separate arena, documented. |
| §5.1 units and sparse negative reads | partial | Negative reads are dependencies; absence cells are never reclaimed (N10); derived maps lack a structure unit (F1). |
| §5.2 scalar equality | yes | `PartialEq` with floats documented; `Param`/value equality; no hashes as equality. |
| §5.3 lists | partial | Source lists have stable ids, per-element units, rank reads; derived list live reads are broken (N1) and whole-value (F6). |
| §5.4 maps and sets | partial | Map per-key membership/value works; derived structure missing (F1); `filter` membership not separately reactive; no set-valued operators, so the support-count rule is vacuous. |
| §5.5 joins | partial | Per-key incremental equi-join on `GeneralComponent`; output is a whole `BTreeMap` (F8); not the bag join with signed weights; cross-term gate missing (N21). |
| §5.6 graphs | partial | Per-node/edge/adjacency units, topology, node deletion retracts edges (tests), payload vs topology isolation (tests); incoming adjacency stored but not observable (N18); source-only (F7). |
| §5.7 trees | partial | Enum groups, per-variant accessors, per-node units, move preserves identity (test), replace preserves the replaced node only (N11); no cross-reference wrapper, no import ambiguity report (N22). |
| §6.1 runtime-owned identity | **no** | ViewId/ComponentId/SiteId are `const_hash(…) as u32`; scopes are `hash_combine`; `map_values` member scopes are keyed by `hash_one(key)`. The plan forbids hash-only correctness keys; a ViewId collision is demonstrated (N5). |
| §6.2 definition by demand / dispatcher | **no** | No owner dispatcher, private definition capabilities, or membership generator; the runtime is the tentative owner arbiter (F1 routing notes); uniqueness is bypassed through `call` (N3). |
| §6.3 routing generations | **no** | Definitions commit with the provider's value; there is no revisioned routing recipe, no route retirement with snapshots, and an equal-value provider replacement leaves consumers' traces unchanged (N13). |
| §7.1 IR and call projections | partial | Site/vocabulary documented in `reactive-plan` (unused, N20); `Switch` is lazy (tests); `Call` returns a reconstructed tuple, so projections are not separately demandable (N12). |
| §7.2 lowering subset | **partial/no** | `let`/`if`/`match`/tuples/`?`/`Result` lower as promised; loops/return/unsafe/async/effect macros rejected; opaque calls and hidden mutation accepted (N6). |
| §7.3 incremental construction | partial | Site nodes and member scopes are cached by identity and revalidated; a component body still re-executes top-level sites on any change (bodies are memoized, so primitives do not re-run). |
| §7.4 fine logical nodes | yes | Per-primitive nodes, no fusion, invocation interning. Nodes are a global arena, not per-invocation arenas (F10 territory). |
| §8.1 revision protocol | yes | Write gate, single active revision, atomic commit, ordered validation (kernel + oracle). |
| §8.2 per-node state | partial | Value/versions/trace/dependents/waiters/owner; no generation; no commit-time ownership check (N8); `Cancelled`/`Checking` states are not modeled. |
| §8.3 demand algorithm | yes | Claim, waiter, ordered validation, fresh recorder, atomic commit, cutoff, waiter completion (kernel tests). |
| §8.4 invalidation cost | partial | Reverse subscriptions with dedup; total cost is not recorded separately (N16); inactive caches keep edges, so the alternative is satisfied. |
| §8.5 parallel scheduling | partial/deviation | Suspended demand (gates), single-flight, helpers; shared ready queue instead of stealing (documented); no intra-region child spawning; progress counts queued/running/waiting but not expansion permits. |
| §8.6 glitch-free publication | partial | One-revision demand scope and snapshot isolation (tests); no per-entry observer (F9); routing generations absent (N13). |
| §8.7 progress/failure/cancellation | partial | Task counts; panic isolation; cancellation wakes tasks; cancellation is checked before commit, not inside it (N9); engine errors inside parallel operators lose their kind (N7). |
| §9.1 recursion | deviation | 256 MiB worker stacks + depth limit instead of heap frames; `max_depth` makes exhaustion an error. |
| §9.2 transient parameters | partial | Small `Hash + Eq` args and `Param`; no generated context records with tracked projections; whole-context args rekey recursive calls (N23). |
| §9.3 mutual recursion | yes | Module-level expansion, invocation interning; mutual-recursion gates. |
| §9.4 cross-worker cycles | yes | Task wait graph with cycle detection (gate `a_cycle_between_two_tasks_is_reported_not_hung`); cycle errors carry node/task ids, not invocation paths (N15); no transfer into a fixed-point region. |
| §9.5 fixed points | partial | Declared lattice, worklist on growth, conservative full recomputation on input change (satisfies the deletion rule); resets are not reported (N19); fixed-point is invoked explicitly, not selected from a detected cycle. |
| §10.1 Views | yes | `#[view(source|derived)]`, `#[tree_view]`, generated schemas. |
| §10.2 builder components | partial | `#[reactive] mod` + `#[component]`; helpers must be components *or supported primitives* — plain calls are accepted as opaque primitives (N6); per-output demand absent (N12). |
| §10.3 connect/edit/observe | partial | Edits, snapshots, observers; a failing root makes edits report failure (N4); tree navigation uses vec + `replace_*` rather than a uniform typed chain. |
| §10.4 multiple providers | partial | Distinct units can be defined by different providers; field-construction composition (`Report { summary, details }`) is not a supported surface; `call` bypasses uniqueness (N3). |
| §10.5 `GeneralComponent` | **no** | Implemented: retained state, unit reads, change batches, atomic state/output/definition commit, `Reset`/`Incremental`, ready-future adapter. Missing: `Inputs`/`Params`/`Outputs`/`Returns`, input snapshots, `cx.request(...).await`, dependency scopes with retain/replace/remove, output transaction/capabilities, suspendible futures, local-executor trait, retired-scope notification filtering (N14). |
| §11.1 crate split | yes | Boundaries match; `reactive-plan` is unused (N20). |
| §11.2 tuples/type erasure | yes | Generated tuple arities, descriptors, downcast checks at boundaries. |
| §11.3 storage/ownership | partial | `Arc` payloads, arena ids, reachability reclamation; no generational arena/slot reuse (F10); absence cells and scopes retained (N10, F10). |
| §11.4 synchronization rules | partial | No user code under metadata locks; one mutex; explicit wait edges; one successful claim per node *attempt*, but commit does not verify the owner/generation (N8). |
| §11.5 effects | partial/deviation | Effect macros rejected; arbitrary functions with side effects accepted (N6), documented in the README as "cached with their node". |
| §11.6 explainability | partial | `explain_node` gives dependency versions and suspect flags; no validated-vs-executed, identity-reuse failure, or blocker/cycle inspector (N15). |
| §12.1 obligations A–G | partial | A partial (whole-field retraction, N2); C, F, G hold; B partial (provider routing is not an observation, N13); D violated through `call` (N3); E partial (no routing generations). |
| §12.2 cost model | yes | Measured in the examples; validation cost is the documented linear term. |
| §12.4 memory | partial | `memory_report` covers nodes/values/edges/sites/units/invocations/regions; retention gaps (F10, N10), no invalidation counters. |
| §13 verification | partial | Randomized oracle, counterexample gates, suspension stress; no model checking/fuzzing; concurrent schedule perturbation is limited (§13.3 item 8 is covered by task status). |
| §14 phases | partial | Phase 4's strict reading ("downstream progress before unrelated upstream units finish") is only indirectly evidenced; phase 5's external baselines remain unavailable offline. |
| §15 recommendation | yes | Typed graph kernel hosted in Rust, as recommended. |

## 3. New findings in detail

### N1 — Derived list live reads fail (critical, confirmed)

**Where.** `crates/reactive-collections/src/list.rs`, `DerivedListRef::to_vec`:
it calls `read_unit_value(self.unit(), |content| Some((self.content)(content).to_vec()))`,
so the read's expected type is `Vec<T>`. `DerivedListRef::define_all` stores
`PSeq<T>` in the same unit. When the unit exists — the normal case after the
provider has defined the list — the downcast fails.

**Effect.** `to_vec`, `len`, `get`, and the forwarded `Slot::to_vec/len/get` on
a derived list View fail with `TypeMismatch { expected: "Vec<T>" }`. Any
component reading a derived list live cannot run at all; only
`snapshot.view::<V>()` works. The workspace has no test that reads a derived
list live, which is why this survived.

**Reproducer.** Appendix A.4. Observed:

```
live read failed: TypeMismatch { expected: "alloc::vec::Vec<u64>" }
snapshot read: [1, 2, 3]
```

**Fix.** Read `PSeq<T>` in `to_vec` (like `DerivedSetRef` reads `PSet<T>`), then
convert; make the initializer return the same type as `define_all`. Gate:
`derived_list_live_reads_match_snapshots` (to_vec/len/get, before and after a
redefinition).

### N2 — Whole-field definitions cannot be retracted (high, confirmed)

**Where.** `inner::retract_unit` only updates the value node and membership for
`unit.entity.is_some()`; scalar, list, set and reduced field units are
field-level (no entity). The content is retracted, but the field's unit node
keeps its value, its `verified_at`, and is never marked suspect or added to
`changed_nodes`.

**Effect.** A provider that stops defining a whole-field unit leaves the last
value readable and no consumer invalidated. The plan's definition model says a
derived unit exists only while a provider defines it; §6.3 also says routing
maintenance is independent of value equality. Two consequences:

- A consumer's trace keeps observing a definition that no longer exists, so the
  control/dependency graph is stale even though the value is the same.
- `DerivedListRef`'s unit and the region content can diverge (the content is
  retracted; the unit keeps the old sequence).

**Reproducer.** Appendix A.5 (scalar): after the provider stops defining
`Total`, the live read returns the old `42` at every revision.

**Fix.** Give whole-field definitions an explicit definition marker (a
`defined` unit or a version field), or at minimum mark the value node suspect,
clear `verified_at`, and extend `changed_nodes` on retraction so readers fail
the way an undefined unit should. The marker version is preferable because it
also distinguishes "default" from "defined" for scalars. Gate:
`a_retracted_whole_field_definition_is_not_readable`.

### N3 — `Runtime::call` bypasses unique-definition enforcement (high, confirmed)

**Where.** `Runtime::call` runs `call_component` inside a task and never calls
`resolve_conflicts`; conflicts are only resolved at the end of `demand_root`,
and even there the reachability test looks from the installed root, which does
not include a component that was only demanded through `call`.

**Effect.** Two components that define the same unit both succeed through
`call`; the unit becomes last-writer, with no error at any point. This violates
the plan's construction invariant (§2.2, §6.2, §12.1 D), which is the whole
reason definition conflicts are diagnosed rather than merged.

**Reproducer.** Appendix A.6:

```
calls: Ok(1) Ok(2)
slot after both calls: 2
first again: Ok(1)
slot now: 2
```

**Fix.** Resolve conflicts at the end of every demand root (including `call`),
with reachability computed from the demanded providers as well as the root; or
make any second live provider an immediate error and require explicit routing
(§6.3) for handover. Gate:
`two_called_components_cannot_define_one_unit`.

### N4 — A committed edit reports observer-refresh failures (high, confirmed)

**Where.** `Runtime::edit` commits, then calls `notify_subscribers()?`. That
function demands the root (`self.read()`), so any root demand failure — no root
installed, a failing `GeneralComponent`, a panicking user body — becomes the
edit's return value.

**Effect.** The edit is durable but the caller sees `Err`; a caller using `?`
will believe the transaction failed and may retry or abort. This is broader than
F3: it happens with an installed root whenever the root cannot be evaluated.
Callback panics have the same shape.

**Reproducer.** Appendix A.7: `edit` returns `Err(Internal { "deliberate
failure" })` while the committed value is `1`.

**Fix.** Separate commit outcome from observer refresh: return the commit result
and report notifications through a dedicated channel (or ignore refresh errors
with a documented policy and an observable diagnostic). Gate:
`an_edit_is_not_failed_by_a_failing_root`.

### N5 — Hash-only namespaces alias distinct names (critical, confirmed for ViewId)

**Where.** `const_hash` is FNV-1a 64; the macros truncate it to `u32`:

- `ViewId = ViewId(const_hash("<module>::<Name>") as u32)`
- `ComponentId = ComponentId(const_hash("<module>::<fn>") as u32)`
- `SiteId = SiteId((const_hash("<module>::<component>:<counter>") as u32) | 1)`
- `ScopeTable::child` uses `hash_combine(parent, next)` as the scope id;
- `map_values` keys member scopes by `hash_one(key)`.

`register_view` silently ignores a second descriptor with an existing id.

**Effect.** Two distinct View types with colliding truncated ids share one
region; the second registration is dropped and registering an editor for it
panics with "content has an unexpected type". The plan forbids this explicitly
(§6.1 "Never use only a hash as a correctness key"; §5.2 "collisions must not
establish semantic equality"). ComponentId collisions are benign for invocation
interning only because generated argument types disambiguate; site-id
collisions within one component would silently alias two expression nodes
(probability ≈ `S²/2³¹`, about 2% at 10,000 sites in one component), and the
`map_values` member-scope hash can alias two keys.

**Reproducer.** Appendix A.3: two view names chosen to collide under a fixed
prefix produce the same `ViewId(0x335f71be)`; the second edit panics.

**Fix.** Collision-free identity:
- Assign `ViewId`/`ComponentId` at registration from a registry keyed by the
  descriptor's fully qualified name (or keep 64-bit hashes and detect
  collisions by comparing names, failing loudly).
- Number sites sequentially per component (source order) rather than hashing a
  counter, and derive scopes from `(parent, index)` allocation, not a hash.
- Key `map_values` member scopes by the map key through a collision-checked
  table instead of `hash_one`.

Gate: a registration test with an injected collision must fail deterministically
instead of aliasing.

### N6 — Opaque calls and hidden mutation are accepted (high, inspection + examples)

**Where.** `reactive-macros/src/lower.rs`, `Expr::Call`: every call is lowered
to a site node; nothing distinguishes generated components from arbitrary
functions. `Expr::MethodCall` similarly accepts any method.

**Effect.** §2.1 says "Do not silently make unsupported code coarse" and §7.2
says "Reject opaque calls, arbitrary I/O, hidden mutation … in standard
components". Today a component can call `std::fs::read_to_string`,
`std::process::id`, or a helper that mutates a static; the call becomes one
memoized node whose internals are invisible, and the effect is skipped on reuse
(the README documents that consequence). The name-resolver example relies on
this: `probe::slow(...)` is an ordinary function with observable side effects.

**Fix.** A declared-primitive policy: an attribute (`#[primitive]`) or an
explicit registry of adapters with declared determinism and delta semantics;
unknown calls in a `#[component]` become a macro diagnostic pointing at the call
site. `GeneralComponent` remains the escape hatch. Gate:
a compile-failure pair for an unknown call, and a positive pair for a declared
primitive.

### N7 — Parallel operators mangle engine errors (medium, confirmed)

**Where.** `reactive-collections/src/list.rs` (and the map parallel path):
scoped-thread results are joined with `handle.join().expect("parallel map
worker")`. An engine panic payload (Cycle, Cancelled, …) is wrapped into a
string panic by `expect`.

**Effect.** The original error kind is lost; the demand returns
`Panicked { message: "parallel map worker: Any { .. }" }` and the panic hook
prints a message even though the engine caught the failure. §8.7 requires
structured failure and §9.4 wants cycle errors to survive.

**Reproducer.** Appendix A.8: a recursive component inside a parallel `map`
reports `Panicked` instead of `Cycle`.

**Fix.** Join manually and resume the original payload (`std::panic::resume_unwind`
for engine payloads), or make member evaluation return `Result` and aggregate
the first engine error. Gate:
`a_cycle_inside_a_parallel_member_stays_a_cycle`.

### N8 — Commit is owner-blind (medium, inspection)

**Where.** `inner::commit` checks the revision and then writes value, trace,
definitions and state; it never checks `record.state == Running { owner: me }`
or a generation. The `generation` field was removed as unused (commit
`8f34d76`).

**Effect.** §8.2 requires an owner task and generation; §11.4 requires one
successful claim per node/revision/generation. Any path that loses or replaces a
claim (F4 retirement, future slot reuse) lets a stale attempt publish. Today the
normal paths are careful, so this is a defense-in-depth gap, but it is also the
reason F4 is more dangerous than it looks.

**Fix.** Keep the claiming task id (and a per-node generation bumped on each
claim) and verify both at commit under the state lock; reject stale commits.

### N9 — Cancellation is not checked inside commit (medium, inspection)

**Where.** `eval` checks `frame.cancel().is_cancelled()` after execution and
before calling `commit`; `commit` itself does not receive or check the token.
`run_general` has the same shape.

**Effect.** §8.7: "Stage changes privately; check the generation at commit."
A cancellation that arrives between the check and the commit lock publishes.
The window is small but real, and the cancel token carries a generation that is
not consulted.

**Fix.** Pass the cancel token into `commit`/`fail` and check it while holding
the metadata lock, before installing definitions or values.

### N10 — Absence cells are never reclaimed (medium, confirmed)

**Where.** `unit_node_init` creates a unit node for any queried absence
predicate; `collect` skips all `NodeKind::Unit` nodes and never prunes
`state.units`.

**Effect.** §5.1: "Store subscriptions sparsely… allocate observation records
only for queried absence predicates and reclaim them with their consumers." A
map with a billion possible keys queried over time accumulates one unit node
per queried key forever.

**Reproducer.** Appendix A.2: 100 absent-key reads create 100 units; `collect`
reclaims 0.

**Fix.** Reference-count absence cells by the consumers that observed them
(trace-based reachability for unit nodes), or keep a sparse "queried absence"
set separate from `state.units` and prune it in `collect` when no trace
references it. Gate: `absence_cells_are_reclaimed_with_their_consumers`.

### N11 — Tree replacement rebuilds descendants (medium, confirmed)

**Where.** `TreeEditor::replace` removes every child subtree and reinstalls the
build; only the replaced node id is preserved.

**Effect.** Replacing the root (or a variant with children) gives every
descendant a fresh id. The replaced node's consumers are repaired (test
`a_replaced_node_keeps_its_identity_and_repairs_consumers` passes), but any
downstream per-node work on descendants is lost, and the README's
"identity-preserving replacement" reads more broadly than the behavior.

**Reproducer.** Appendix A.1: replacing the root with an identical document
changes item/child ids from `[1,2,3,4]` to `[5,6,7,8]`.

**Fix.** Diff the new build against the existing subtree: reuse child nodes
whose structure and fields are unchanged, keep the replaced node's id, and
re-stage only the changed slots. Gate:
`replacing_a_variant_reuses_unchanged_children`.

### N12 — No per-projection demand (medium, inspection)

**Where.** `component::call_component` evaluates the body and then every output
projection; `Inner::demand_root` evaluates every root output;
`Runtime::read_outputs`/`call` return the whole product.

**Effect.** §4.2 promises independently versioned projections (true) and §7.1
says a call "returns separately demandable result projections" (not true at the
call surface): a caller that needs output 0 still computes output 1, and a root
snapshot demands all outputs. The plan's §10.2 example ("a request for Types
alone may omit evidence computations") cannot be expressed.

**Fix.** A per-projection demand API: `Runtime::read_output::<R, _>(index)`
using the existing projection nodes, and a call form that requests one
projection; keep the tuple form as sugar. Gate:
`reading_one_projection_does_not_evaluate_the_other`.

### N13 — No routing generations (§6.2–§6.3) (high, inspection)

The runtime is a tentative owner arbiter: whichever provider commits first owns
a unit; conflicts are resolved at end of revision by reachability. There is no
owner dispatcher, no private definition capability, no membership generator, no
revisioned routing recipe, no route retirement tied to snapshot release, and no
routing version in a unit's identity. Consequences:

- A provider that stages a unit is not authorized by a route; any component can
  claim any unit until a conflict is detected (§12.1 B: provider routing is not
  an observation).
- An equal-value provider replacement does not change any dependency; consumers
  keep the old routing relationship with no observable certificate (§6.3).
- A collection observer cannot distinguish "membership resolved" from "recipe
  installed"; membership is only the per-key unit (F1 shows the derived map
  case is broken).

This is the largest architectural gap against the plan. It needs its own design
phase, not a patch; see the plan's §6.3 steps 1–5 as the specification.

### N14 — `GeneralComponent` is far from §10.5 (high, inspection)

Implemented: `initialize`/`update` returning `(State, Output)`, `GeneralCx`
unit reads, `ChangeBatch`, `Reset`/`Incremental`, atomic commit, staged
definitions, an `AsyncAdapter` that accepts only immediately-ready futures.

Missing relative to §10.5: typed `Inputs`/`Params`/`Outputs`/`Returns` with
snapshots and parameter values; `cx.request(...).await` with typed receipts and
dependency scopes; retain/replace/remove scope operations and retired-scope
notification filtering; an output transaction with field/collection mutations
and dispatcher-issued capabilities; futures that suspend instead of blocking a
worker (today a pending future returns `Unsupported`); the local-executor trait
for non-thread-safe state; update batches that distinguish certified value
changes from invalidated scopes.

The trait's doc comment describes the smaller contract, so the code is honest;
the gap is against the plan's declared interface. Treat §10.5 as a design phase
with its own gates (suspendible future across worker migration, scope
retirement, output transaction atomicity).

### N15–N23 — Remaining findings

- **N15 Explainability.** `explain_node` gives dependency versions and suspect
  flags; it cannot answer "validated vs executed", "where identity reuse
  failed", or "what blocks progress/forms a cycle" (§11.6).
- **N16 Invalidation cost.** `invalidate` marks the cone suspect but records no
  count/time; `RuntimeStats` has executions/validations/revisions only (§8.4
  asks to record that cost separately).
- **N17 Diagnostics.** `DuplicateDefinition { unit, first, second }` carries
  labels (`&'static str`), not the two source locations §2.2 asks for; cycle
  errors carry node/task ids, not invocation paths.
- **N18 Incoming adjacency.** `GraphStore` maintains `incoming`, and
  `remove_node` uses it, but `GraphRef` has no incoming read and the editor
  stages only outgoing adjacency units (§5.6 says maintain both indexed
  adjacencies).
- **N19 Fixed-point resets.** The solver recomputes from scratch on any input
  change (conservative and sound), but a reset is not reported as a cost/event
  and there is no SCC localization (§9.5: "a whole-SCC reset is a possible worst
  case and must be reported as such").
- **N20 `reactive-plan`.** Resolved: the vocabulary is re-exported as
  `reactive::plan` for tooling and diagnostics, and `reactive-runtime` no longer
  declares the unused dependency.
- **N21 Join evidence.** The join gate changes one side per transaction; §13.3
  counterexample 5 explicitly requires a two-input change test. The operator is
  also a per-key value join, not the §5.5 bag join with signed weights.
- **N22 Trees.** No cross-reference wrapper (the plan distinguishes typed
  references from owned children), and `set_root` whole-tree import reports
  neither cost nor ambiguity (§5.7's importer rule).
- **N23 Context keys.** Component arguments are whole `Hash + Eq` values;
  passing a large context to a recursive call rekeys every descendant when an
  unrelated binding changes — exactly the §13.3 counterexample the plan says to
  avoid with tracked context projections. `Param` covers collection members,
  not invocation arguments.

## 4. Remediation plan

The phases below extend the granularity plan's phases; identifiers keep the
`C` prefix here so the two documents can be tracked separately.

### Phase C0 — Correctness first (small, independent)

1. N1 derived-list reads; N2 whole-field retraction; N3 `call` conflict
   resolution; N4 edit outcome separation; N7 parallel error preservation.
2. N8 commit ownership/generation; N9 cancellation inside commit.
3. N5 collision-free identities and a registration-time collision check.
4. Absence-cell reclamation (N10) belongs with F10 but is small enough to land
   with the identity fixes.

Each item gets the gate named in its finding. C0 must not change public API
beyond the new error behaviour (edits no longer failing on refresh; `call`
conflicts erroring) and the identity representation.

### Phase C1 — Lowering policy (§2.1, §7.2)

Declared primitives, macro diagnostics for unknown calls, an adapter registry
with determinism/delta metadata, and diagnostics with source spans (N17).
Revisit the loop/`return` rejections only if the policy changes.

### Phase C2 — Routing and dispatcher (§6.2–§6.3, N13)

Design and implement routing generations: selector computation, uniqueness
validation at revision, definition capabilities, membership generation, route
retirement with snapshots, and a routing version that participates in unit
identity and consumer traces. This replaces the tentative-owner/end-of-revision
arbiter and is a prerequisite for "equal value, new dependency graph".

### Phase C3 — `GeneralComponent` §10.5 (N14)

Typed inputs/params/outputs/returns, `request().await` with receipts, dependency
scope retain/replace/remove, output transactions, suspendible futures with
worker migration, and a local-executor trait. Keep the current trait as the
synchronous subset if desired, but the plan's contract needs the scoped and
await forms.

### Phase C4 — Observation, explainability, retention (§8.4, §11.6, N15/N16, F9/F10)

Per-entry observers, invalidation counters and reset reporting, a typed
explanation (validated vs executed, identity reuse, blockers/cycles), scope and
arena reclamation, bounded invocation caches.

### Phase C5 — Evidence gaps

Join cross-term and bag-semantics gates (N21), a phase-4 gate for "downstream
progress before unrelated upstream units finish", concurrent schedule
perturbation beyond the current tests, and the external baselines that remain
unavailable offline.

## 5. Risks and ordering

- N1, N2, N4 and N3 are user-visible and should land before any new feature;
  each has a small reproducer and a clear gate.
- N5 changes identity representation and touches macro output, descriptors and
  every registry; land it early but in its own change with a compatibility
  check for existing descriptors.
- N8/N9 are protocol hardening; do them before the dispatcher work so the
  dispatcher can rely on generation-checked commits.
- N13 and N14 are design-sized; do not start them before C0/C1 because both
  change the same commit and trace surfaces.
- Every phase needs an update to `docs/architecture.md`; the current
  "granularity contract" claim cannot be stated honestly until N1/N2 and F1 are
  fixed.

## Appendix A — Reproducers

Each snippet is a standalone `examples/*.rs` against the public API; they were
run and removed. Failure modes are quoted as observed.

### A.1 Tree replacement identity

```
before: [1, 2, 3, 4]
after replace_root(identical): [5, 6, 7, 8]
descendant ids preserved: false
after replace_item(leaf): [5, 6, 7, 8]
other ids preserved: true
```

(Built from `Document::Module { items: [Sum{...}, Int(3)] }`, reading
`raw()` of items and children before and after.)

### A.2 Absence cells

```
after 100 absent-key reads: 306 units, 310 nodes
after collect:               306 units, 310 nodes (reclaimed 0 nodes)
```

The component read `table.entries().get(k).is_none()` for 100 keys that were
never inserted.

### A.3 ViewId collision

Views named `InkOwcyHFoDQS` and `MOlqwVtqzJ` in the same module both hash to
`ViewId(0x335f71be)` under `const_hash("audit_hash_view::<Name>") as u32`.
Editing the first succeeds; editing the second panics with:

```
view ViewId(0x335f71be) content has an unexpected type
```

### A.4 Derived list live read

```
live read failed: TypeMismatch { expected: "alloc::vec::Vec<u64>" }
snapshot read: [1, 2, 3]
```

### A.5 Whole-field retraction

A derived scalar `Total(u64)` defined only in the `on` branch:

```
on=true: marker=1 live total=42
on=false: marker=2 live total=42
on=true: marker=1 live total=42
on=false: marker=2 live total=42
```

### A.6 `call` conflicts

Two components (`first`, `second`) each define `Slot.field0`:

```
calls: Ok(1) Ok(2)
slot after both calls: 2
first again: Ok(1)
slot now: 2
```

### A.7 Edit outcome with a failing root

A `GeneralComponent` whose `initialize` returns an internal error, plus an
observer on a source View:

```
edit result: Err(Internal { message: "deliberate failure" })
committed value: 1
```

### A.8 Parallel error kind

A recursive component invoked from a parallel `map` member:

```
parallel map worker: Any { .. }
error kind: Panicked { message: "parallel map worker: Any { .. }" }
```

## Appendix B — Audit inventory for this pass

Read in this pass (in addition to the first-pass inventory):

- `crates/reactive-collections/src/{list,map,set,graph,reduce,join}.rs` (handles,
  editors, unit reads, parallel paths)
- `crates/reactive-store/src/{field,graph,aggregate}.rs` (scalar/collection
  `ViewField` semantics, graph in/out adjacency)
- `crates/reactive-macros/src/{view,lower,component}.rs` (generated accessors,
  identity hashes, opaque-call lowering)
- `crates/reactive-runtime/src/{runtime,general,fixed_point}.rs` (call path,
  observers, general context, solver)
- `crates/reactive-plan/src/lib.rs` (usage check)
- plan sections §2.1–§2.4, §4.1–§4.4, §5.1–§5.7, §6.1–§6.3, §7.1–§7.4,
  §8.2–§8.7, §9.1–§9.5, §10.1–§10.5, §11.1–§11.6, §12.1, §13.2–§13.3, §14

Commands: `cargo test --workspace --offline` (128 tests), `cargo test --release
--workspace --offline`, `cargo clippy --workspace --all-targets --offline -- -D
warnings`, and the reproducers above.

Not covered: `loom`/model checking of the claim and wait graphs, fuzzing of the
macros, a full concurrent schedule enumeration, and the external Salsa /
Differential Dataflow baselines (unavailable offline).
