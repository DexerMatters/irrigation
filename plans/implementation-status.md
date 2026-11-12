# Implementation status against the plan

## Coverage at a glance

| Plan section | Status | Evidence |
|---|---|---|
| §1–§4 semantic model (Views, revisions, snapshots, change laws) | implemented | kernel tests, randomized oracle |
| §5 View decomposition (scalar, list, set, map, graph, tree) | implemented | per-family gates |
| §5.5 joins | implemented as an incremental equi-join on `GeneralComponent` | combine-count gate |
| §5.3 balanced reductions | implemented (`AggSeq`, `Reduced<R>`) | logarithmic combine counts |
| §6 identity, unique definitions, routing | implemented: unique definitions, atomic definition commit, unreachable-subtree retirement, and end-of-revision conflict resolution make handover order-independent | duplicate-definition, handover and branch-retraction gates |
| §7 component compilation and lowering | implemented for the documented subset | end-to-end and macro tests |
| §8 revisions, validation, invalidation, publication | implemented | kernel tests, cutoff gates |
| §8.5–§8.7 concurrency, progress, failure, cancellation | implemented: pool, suspension, single-flight, wait graph on task identities, cancellation, panic isolation, progress counters, contention | suspension and concurrency gates |
| §9 recursion and transient parameters | implemented | recursion gates |
| §9.5 fixed points | implemented | fixed-point gates |
| §10 public API | implemented (`#[view]`, `#[component(root|callable)]`, optional `#[irrigation]`, `#[tree_view]` with a generated `TreeLayout`, `#[general_component]`, typed field handles, `GeneralComponent` sync/async, observers with change summaries, `Installed`, explanations); the syntax layer declares its tree as a `tree_view` and maps the grammar onto it (`@tree`/`@wrap`, `Grammar::map_tree`, `ParserComponent::with_tree`) | end-to-end and authoring tests, `irrigation-syntax/tests/declared_tree.rs`, `irrigation/tests/tree_layout.rs` |
| §11 architecture, effects, explainability | implemented (explain_node, docs/architecture.md) | explanation gate |
| §12 correctness and cost model | exercised by gates; measured by examples | workload matrix |
| §13 verification | randomized oracle and counterexample gates | gates file |
| §14 phases and benchmarks | phases 1–4 complete for the implemented surface; phase 5 partial (external baselines) | examples |
| §15 final recommendation | followed: typed graph language hosted in Rust | — |

## Remaining gaps

No structural gap remains in the runtime. What is left is comparative evidence:
Salsa and Differential Dataflow baselines cannot be built offline, so the
workload matrix reports from-scratch and whole-function memoization baselines
instead.

A waiting demand now yields its worker: the attempt ends with a suspension
record, the task is queued again when the awaited node completes, and the
demand restarts from its root against nodes that are already committed. The
design, its restart semantics, and the gates are in
[`../docs/architecture.md`](../docs/architecture.md).

This file records, section by section, what the workspace implements, where the
evidence is, and what remains. It is deliberately conservative: a feature is
"implemented" only when a test exercises its stated contract.

## §1–§4 Semantic model

| Requirement | Status | Evidence |
|---|---|---|
| Views are singleton typed namespaces per runtime | implemented | `irrigation-core::descriptor`, generated `ViewSchema` impls |
| Components are singleton generators with many invocations | implemented | `irrigation-runtime::component`, invocation interning by argument equality |
| Inputs/outputs are heterogeneous products with independent projections | implemented | separate output nodes; `changing_one_output_does_not_change_the_other` |
| Observable result equals from-scratch evaluation | implemented | `randomized_transactions_match_from_scratch_evaluation` (2,000 transactions) |
| Snapshot isolation | implemented | `snapshots_keep_their_revision_after_edits` |
| Change laws (apply/normalize/compare) | implemented | edits compare final values at commit; `delta.rs` laws tested |
| Three graphs kept distinct | implemented | the data graph is the user's structure; the computation graph is the node arena with its dependency traces; the control graph is the invocation/scope tree plus each node's ordered trace. They are never conflated, but the control graph has no separate arena |

## §5 View decomposition

| Family | Status |
|---|---|
| Scalar / record | implemented (`Scalar` field, unit per field) |
| List | implemented for source Views (stable element ids, per-element units, rank reads observe structure) |
| Set | implemented for source Views (per-member membership units) |
| Balanced reduction | implemented (`AggSeq`: cached lawful aggregate, logarithmic point-edit recomputation) and exposed as the `Reduced<R>` View field |
| Map | implemented for source and derived Views (membership + value per key) |
| Graph | implemented for source Views: topology, per-node payload, per-edge payload and per-node adjacency units; node deletion retracts incident edges |
| Tree | implemented: `#[tree_view]` generates typed refs, kind enums, per-variant accessors, build conversions, and an editor with identity-preserving replace/move/remove |

Negative reads are dependencies: membership units default to `false` and are
updated on insertion/removal, so `get(k) == None` wakes when `k` appears. The
gate `missing_key_reads_are_dependencies_and_other_keys_are_isolated` checks
both directions, including that an unrelated key executes no consumer work.

## §6 Identity and shared outputs

| Requirement | Status |
|---|---|
| Runtime-owned identity (no user cache keys) | implemented |
| Unique defining computation per unit | implemented; `two_providers_cannot_define_the_same_unit` |
| Dynamic routing/dispatcher generations | partially: the runtime is the dispatcher. Definitions commit atomically with the node, the unique-owner rule is enforced at commit, and end-of-revision reachability resolves handovers. There is no separate dispatcher object, and a non-selected provider that stages a unit is diagnosed rather than prevented from staging it |
| Routing published separately from values | definitions commit with the value; a provider switch replaces the unit owner atomically |

## §7 Component compilation

| Requirement | Status |
|---|---|
| Typed IR of primitives, products, switches, calls | implemented as generated builder calls rather than a reified IR arena; identity is `(scope, site)` |
| `Switch` does not build both branches | implemented (`if`/`match` lower to Rust control flow; only the taken branch runs) |
| Calls become graph requests | implemented (`call_component` interns the invocation) |
| Construction is itself incremental | implemented: site nodes are reused across executions and validated by trace |
| Fine logical nodes, economical execution | implemented; execution counts in `examples/scaling.rs` |
| Unsupported constructs rejected | implemented (loops, `return`, effect macros, unsafe, async) |

## §8 Concurrency

| Requirement | Status |
|---|---|
| One mutable transaction at a time, one active derived revision | implemented with a read/write gate |
| Per-node state (value, versions, trace, state, waiters) | implemented |
| Demand algorithm: verify, validate in order, execute, commit | implemented |
| Reverse subscriptions for invalidation | implemented |
| Worker pool, no user code under metadata locks | implemented (`scheduler.rs`) |
| Suspended continuations / work stealing | implemented: a waiting task records the wait and its attempt ends, so its worker takes queued work. One shared ready queue makes stealing unnecessary; there are no per-worker deque. Nested demand still runs inline |
| Glitch-free publication | implemented (dependency certification precedes use; cutoff keeps versions) |
| Progress accounting, cancellation, failure | implemented: `RuntimeStats::tasks` counts queued, running, and waiting tasks; a demand is complete only when its task left the table. Failures are cached per revision, cycles are detected, cancellation is checked at claim and commit and wakes suspended tasks |

## §9 Recursion and fixed points

| Requirement | Status |
|---|---|
| Structural recursion as keyed subgraphs | implemented |
| Stack safety | implemented via large worker stacks and a depth limit; not heap frames |
| Transient parameters in invocation identity | implemented |
| Large structured contexts as tracked projections | partially: `Param` provides tracked per-member parameters; user context records are ordinary values and must be `Hash + Eq` |
| Cross-worker cycle detection | implemented (wait graph under the metadata lock) |
| Declared fixed points with deletion policy | implemented: `FixedPointProblem`, `Lattice`, worklist solver, conservative recomputation; `fixed_points_inside_components_follow_insertions_and_deletions` |

## §10 Public API

| Item | Status |
|---|---|
| `#[view(source)]`, `#[view(derived)]` | implemented |
| `#[irrigation]` / `#[component]` / `#[general_component]` | implemented; the module wrapper is optional and an explicit `root`/`callable` kind is available |
| `runtime.install` / `edit` / `read` / `observe` | implemented |
| Snapshots with typed View handles | implemented |
| `#[tree_view]` | implemented |
| `GeneralComponent` | implemented: retained state, change batches, `Reset`/`Incremental` policies, atomic state/output/definition commit; async form driven by the owning worker |

## §11–§12 Architecture, correctness, cost

The crate split follows §11.1. Proof obligations are exercised by the gates
rather than mechanized. The cost model is measured in `examples/scaling.rs`:

| list length | primitive executions | edit + read |
|---|---|---|
| 100 | 5 | 53 µs |
| 1,000 | 5 | 362 µs |
| 10,000 | 5 | 4.2 ms |
| 50,000 | 5 | 31 ms |

Executed primitives are constant; the linear term is trace validation over the
mapped members (`E_v`), which the plan's cost model predicts and which this
implementation does not claim to eliminate.

## §13–§14 Verification and phases

| Phase | Status |
|---|---|
| 1 — sequential semantic kernel | complete: revisioned units, tracked reads, ordered validation, dynamic branches, negative dependencies, cutoff, unique definitions, subscriptions |
| 2 — fine-grained generated components | complete: the enum-group tree macro and identity-preserving tree edits are implemented |
| 3 — all collection Views and delta operators | complete for the implemented surface: list/map/set/graph/tree, `GeneralComponent`, and an incremental equi-join |
| 4 — parallel, pipelined execution | complete for the implemented surface: pool, suspended demand, parallel map, worker-count and repeated-schedule invariance, concurrent host reads, cross-thread single-flight contention, deep recursion, cancellation, progress counters |
| 5 — fixed points, resource control, performance | partial: fixed points, reclamation, memory accounting, balanced reductions, a join operator, a workload matrix, and a worker-scaling example implemented; from-scratch, whole-function memoization, and handwritten incremental baselines are reported in the workload matrix; Salsa and Differential Dataflow baselines are pending |

## Known deviations, stated explicitly

1. **Derived lists and sets are whole-value definitions.** Per-occurrence
   identity is not invented from content equality.
2. **Worker stack size substitutes for heap frames.** `stack_size` defaults to
   256 MiB; `max_depth` converts exhaustion into `DepthExceeded`.
3. **A suspended demand restarts from its root** instead of resuming the
   compute closure at the suspended expression. Closures are ordinary Rust and
   cannot be interrupted mid-body, so the attempt is abandoned and replayed;
   committed nodes are clean and are reused. Nested demand runs inline on its
   worker and parks that worker when it waits, so a one-worker pool cannot
   deadlock.
4. **Parallel operators use scoped threads** so closures can borrow their
   environment; those threads evaluate with a detached task identity and reuse
   the same claim and cycle bookkeeping.
5. **Tracked locals clone on use**, which is cheap for handles and visible for
   large values.
6. **Effects are not supported in computations.** They are rejected for known
   macros and otherwise cached with their node, which is the documented rule.
7. ~~Provider handover is ordered, not generation-based.~~ **Fixed.** A
   conflicting definition is installed tentatively and recorded; at the end of
   the revision the runtime resolves it by reachability: if the earlier owner is
   still reachable, both providers are live and the revision fails with
   `DuplicateDefinition`; if the earlier owner retired during the revision, the
   handover stands. The gate
   `provider_handover_survives_demand_order` switches ownership in both
   directions while the incoming provider is demanded first, and the static and
   dynamic collision gates still reject genuine conflicts.
8. ~~Definitions staged inside an untaken branch are not retracted.~~ **Fixed.**
   `Inner::commit` now retires observations that the new trace no longer
   contains: an unreachable node releases its definitions and is cleared, so it
   re-executes and restages if it becomes reachable again. View unit nodes are
   never cleared, because they are the data store. The gate
   `a_definition_inside_an_untaken_branch_is_retracted` covers publication,
   retraction and restaging; the always-stage-values pattern remains gated by
   `branch_chosen_values_republish_every_execution`.

## Next work, in plan order

The audit of granularity between View families and the follow-up plan are in
[`heterogeneous-granularity-plan.md`](heterogeneous-granularity-plan.md). It
records the confirmed defects (derived-map structure unit, source-only
observers, macro shapes) and the family matrix this list should be read against.

1. External comparative baselines (Salsa, Differential Dataflow) where the
   crates are available; the workload matrix currently reports from-scratch and
   memoized baselines.
2. A full benchmark matrix for joins, tree edits, fixed points, and worker
   scaling beyond the current examples.
3. Heap-owned frames instead of large worker stacks, if stack depth ever
   matters more than throughput.
