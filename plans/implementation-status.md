# Implementation status against the plan

This file records, section by section, what the workspace implements, where the
evidence is, and what remains. It is deliberately conservative: a feature is
"implemented" only when a test exercises its stated contract.

## §1–§4 Semantic model

| Requirement | Status | Evidence |
|---|---|---|
| Views are singleton typed namespaces per runtime | implemented | `reactive-core::descriptor`, generated `ViewSchema` impls |
| Components are singleton generators with many invocations | implemented | `reactive-runtime::component`, invocation interning by argument equality |
| Inputs/outputs are heterogeneous products with independent projections | implemented | separate output nodes; `changing_one_output_does_not_change_the_other` |
| Observable result equals from-scratch evaluation | implemented | `randomized_transactions_match_from_scratch_evaluation` (2,000 transactions) |
| Snapshot isolation | implemented | `snapshots_keep_their_revision_after_edits` |
| Change laws (apply/normalize/compare) | implemented | edits compare final values at commit; `delta.rs` laws tested |
| Three graphs kept distinct | partially | data and computation graphs are distinct; the control graph is implicit in node traces rather than a separately materialized structure |

## §5 View decomposition

| Family | Status |
|---|---|
| Scalar / record | implemented (`Scalar` field, unit per field) |
| List | implemented for source Views (stable element ids, per-element units, rank reads observe structure) |
| Set | implemented for source Views (per-member membership units) |
| Map | implemented for source and derived Views (membership + value per key) |
| Graph | implemented for source Views: topology, per-node payload, per-edge payload and per-node adjacency units; node deletion retracts incident edges |
| Tree | pending (persistent store exists; `#[tree_view]` macro not implemented) |

Negative reads are dependencies: membership units default to `false` and are
updated on insertion/removal, so `get(k) == None` wakes when `k` appears. The
gate `missing_key_reads_are_dependencies_and_other_keys_are_isolated` checks
both directions, including that an unrelated key executes no consumer work.

## §6 Identity and shared outputs

| Requirement | Status |
|---|---|
| Runtime-owned identity (no user cache keys) | implemented |
| Unique defining computation per unit | implemented; `two_providers_cannot_define_the_same_unit` |
| Dynamic routing/dispatcher generations | partially: definitions commit atomically with the node, and unique-owner checks are enforced at commit; there is no separate dispatcher API |
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
| Suspended continuations / work stealing | partially: waiting threads block on the metadata condvar; queued work is not stolen. Nested demand runs inline instead of yielding |
| Glitch-free publication | implemented (dependency certification precedes use; cutoff keeps versions) |
| Progress accounting, cancellation, failure | partially: failures are cached per revision, cycles are detected, and cancellation is enforced; explicit progress/frontier accounting is not implemented |

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
| `#[reactive]` / `#[component]` | implemented |
| `runtime.install` / `edit` / `read` / `observe` | implemented |
| Snapshots with typed View handles | implemented |
| `#[tree_view]` | pending |
| `GeneralComponent` | pending |

## §11–§12 Architecture, correctness, cost

The crate split follows §11.1. Proof obligations are exercised by the gates
rather than mechanized. The cost model is measured in `examples/scaling.rs`:

| list length | primitive executions | edit + read |
|---|---|---|
| 100 | 8 | 45 µs |
| 1,000 | 8 | 485 µs |
| 10,000 | 8 | 5.0 ms |
| 50,000 | 8 | 42 ms |

Executed primitives are constant; the linear term is trace validation over the
mapped members (`E_v`), which the plan's cost model predicts and which this
implementation does not claim to eliminate.

## §13–§14 Verification and phases

| Phase | Status |
|---|---|
| 1 — sequential semantic kernel | complete for scalar/record/list/map/set Views; graph/tree pending |
| 2 — fine-grained generated components | complete except the enum-group tree macro |
| 3 — all collection Views and delta operators | partial: list/map/set complete; graph/tree, joins, and `GeneralComponent` pending |
| 4 — parallel, pipelined execution | partial: pool, parallel map, invariance, deep recursion; continuation scheduling, cancellation tests, and schedule perturbation pending |
| 5 — fixed points, resource control, performance | partial: fixed points, reclamation, and memory accounting implemented; the full benchmark matrix is pending |

## Known deviations, stated explicitly

1. **Derived lists and sets are whole-value definitions.** Per-occurrence
   identity is not invented from content equality.
2. **Worker stack size substitutes for heap frames.** `stack_size` defaults to
   256 MiB; `max_depth` converts exhaustion into `DepthExceeded`.
3. **Waiting blocks a worker** rather than suspending a continuation. Nested
   demand runs inline, so a one-worker pool cannot deadlock.
4. **Parallel operators use scoped threads** so closures can borrow their
   environment; this is real parallelism without a custom continuation runtime.
5. **Tracked locals clone on use**, which is cheap for handles and visible for
   large values.
6. **Effects are not supported in computations.** They are rejected for known
   macros and otherwise cached with their node, which is the documented rule.

## Next work, in plan order

1. `#[tree_view]` enum-group macro and tree View family.
2. Graph View family with topology/payload separation.
3. `GeneralComponent` with output transactions, dependency scopes, and update
   batches.
4. Indexed joins and lawful balanced reductions.
5. Continuation-based scheduling, cancellation API, and schedule perturbation
   tests.
6. The full benchmark matrix (joins, trees, fixed points, worker scaling).
