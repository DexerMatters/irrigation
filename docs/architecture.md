# Architecture notes

This document explains how the runtime works, so future changes can preserve its
invariants instead of rediscovering them.

## Crate direction

```
reactive-core      identities, errors, frames, values, keys, descriptors
   ^        ^
   |        |
reactive-store   persistent structures, field storage
   ^        ^
   |        |
reactive-runtime   nodes, demand, edits, regions, pool, general, fixed points
   ^        ^
   |        |
reactive-collections   list/map/set/graph/tree/join/reduce
   ^
reactive-macros    view/component/tree lowering (proc-macro)
   ^
reactive           facade + tests + examples
```

`reactive-core` never depends on the runtime. `reactive-macros` only emits code
that names the facade, so generated code compiles in user crates.

## The node

Every memoized computation is a `NodeRecord`:

| Field | Meaning |
|---|---|
| `value` | last committed `Arc<dyn Any + Send + Sync>` |
| `changed_at` | revision when the value last changed |
| `verified_at` | revision for which the value is certified |
| `suspect_at` | revision of the last invalidation |
| `trace` | ordered observations: `(node, version)` |
| `dependents` | reverse edges, used by invalidation |
| `state` | `Vacant`, `Running { owner, generation }`, `Ready`, `Failed` |
| `general` / `general_state` | custom component index and retained state |

The central invariant is:

```
a node is clean at revision r  <=>  suspect_at <= verified_at and value.is_some()
```

A node invalidated at revision `r` stays suspect until it is verified again.
Comparing `suspect_at` with the current revision alone is wrong; the randomized
oracle caught exactly that bug.

## Revision protocol

1. **Edit.** `runtime.edit(|e| ...)` stages unit writes. At commit the runtime
   compares final values per unit. A transaction whose final state equals the
   previous state publishes nothing.
2. **Invalidation.** Changed unit nodes mark their reverse dependents suspect at
   the new revision (`suspect_at = r`), deduplicated by a visited set.
3. **Demand.** `read`/`read_outputs`/`call` run on a pool worker inside a scope
   frame carrying a cancellation token. `Inner::eval`:
   - returns a certified value,
   - short-circuits a clean node,
   - otherwise claims single-flight ownership,
   - validates the ordered trace, stopping at the first observation that cannot
     be certified without its own closure,
   - executes with a fresh recorder,
   - commits value, trace, definitions, and retained state atomically.
4. **Cutoff.** An equal value keeps `changed_at`; only changed projections notify
   consumers. Component output projections are separate nodes.

Validation is ordered because control must be checked before a branch that may
have been deleted. The lowering preserves source order, so `eval` re-executes in
the same order and never touches an obsolete branch.

## Concurrency

- One mutable transaction at a time, one active derived revision: a `RwLock`
  gate separates edits from demands.
- A pool of workers with large stacks executes demands. A demand that is already
  on a worker runs inline, so a one-worker pool cannot deadlock on itself.
- Single-flight is per node: the claim, the waiter registration, and completion
  are one state transition under the metadata mutex.
- Waiters park on a global condvar. Before parking they check the wait graph;
  a reachable cycle becomes `ComputeError::Cycle`. User code never runs under
  the metadata lock.
- Cancellation is checked before claiming and before committing; a cancelled
  attempt publishes nothing and leaves nodes vacant.
- Parallel collection operators use scoped threads over worker chunks and join
  in order, so results are independent of completion order.

## Views and units

A View is a typed namespace. Its content is one immutable value; observations
are logical units:

| Family | Units |
|---|---|
| scalar | one value unit per field |
| map | structure, per-key membership, per-key value |
| list | structure (element ids), per-element membership and value |
| set | structure, per-member membership |
| graph | topology, per-node membership and payload, per-edge payload, per-node adjacency |
| tree | root, per-node tag, per-field leaf, per-child-slot, parent, membership |

Source units are created by edits; derived units by definitions. A definition is
owned by exactly one provider node; a second provider is a deterministic
`DuplicateDefinition` error. Definitions commit with their provider's value and
trace, so routing changes are atomic.

## Component lowering

- Every value-producing expression becomes `reactive::eval(site, || ...)`, keyed
  by `(scope, site)`.
- Every `let` binding records the node that produced its value; uses observe that
  node (`observe_clone`/`observe_ref`) so consumers depend on producers.
- Closures are lowered inline. A closure may be called many times in one scope,
  so per-expression nodes inside it would alias across calls. Collection
  operators create a member scope and per-member parameter/result nodes, which
  restores per-member identity.
- `map`/`filter`/`flat_map` on a collection handle are tracked nodes; on a
  standard iterator chain they are lazy, decided by the receiver's shape.
- Calls to component functions become invocations keyed by argument equality;
  each output is a separate node.
- Unsupported constructs (loops, `return`, effect macros, unsafe, async) are
  rejected rather than silently coarsened.

## Custom algorithms

`GeneralComponent` retains state and consumes a `ChangeBatch` of changed units.
`Reset` reinitializes (always sound); `Incremental` calls `update`. State,
output, trace, and definitions commit atomically. `EquiJoin` and `FixedPointProblem`
are built on this boundary.

## Where to extend

- New View family: add a field type in `reactive-collections`, a unit layout,
  descriptor vtable, and macro recognition.
- New operator: prefer a per-member scope with `Param`; fall back to
  `GeneralComponent` when the algorithm needs retained state.
- Scheduling changes must preserve the claim/commit protocol and the
  `suspect_at <= verified_at` invariant.
