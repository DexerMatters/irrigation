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
| `state` | `Vacant`, `Running { owner: TaskId, generation: u64 }`, `Ready`, `Failed` |
| `waiters` | tasks suspended on this node |
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

## Value equality

Cutoff is the View field's `PartialEq`; the runtime never normalizes a value
first. Integers, strings, and collections therefore cutoff exactly. Floats keep
IEEE semantics: `-0.0 == 0.0`, and `NaN != NaN`, so a computed `NaN` re-executes
on every revision. A host that wants bitwise float cutoff wraps the value in a
newtype with that `PartialEq`; the field's equality is the contract.

Hashes never establish equality. They bucket component invocations, whose
arguments are then compared with `PartialEq`; a collision only costs a
comparison.

## Routing generations

A logical unit carries a routing generation alongside its value version.
Defining a unit from a different provider (including a handover that keeps the
value equal) advances the routing generation; retracting the owner does too.
Consumers record the routing generation in their trace and validation rejects an
observation whose routing changed even when the value did not. The runtime
exposes `provider_of` and `routing_generation`, and `observe_routing` registers
a host subscription that fires on provider changes (including equal-valued
handovers) but not on value-only edits. and `Change` marks routing-only
updates for general components. Each View region also carries a routing version bumped on any provider
addition, handover, or removal; `Runtime::routing_stamp` captures a
`RoutingStamp` for the region and `validate_routing` rejects it after routing
changes (a value-only edit leaves it valid). This is a revisioned routing
certificate and recipe-validation primitive; the full owner dispatcher,
selector recipes, and snapshot-tied retirement are still future work.

## Identity

- `ViewId` and `ComponentId` are the full 64-bit FNV hash of the fully
  qualified name. Registering a View whose id already belongs to a different
  name is rejected; the old 32-bit truncation could alias two namespaces.
- `SiteId` is sequential per component. Every component body executes in its own
  scope — a root-style component called as a plain function opens a child scope
  keyed by its component id — so sites only need to be unique within one
  component.
- `ScopeId`s are allocated from a counter, never hashed. Map-member scopes
  intern their key with `child_scope_keyed`, which compares keys by equality
  inside a hash bucket; a collision costs one comparison, not aliasing.

## Concurrency

- One mutable transaction at a time, one active derived revision: a `Gate`
  separates edits from demands. Its read permits are counters rather than
  guards, because a task keeps its permit while suspended and resumes on
  whichever worker picks it up.
- A pool of workers with large stacks executes demand tasks. A demand that is
  already on a worker runs inline, so a one-worker pool cannot deadlock on
  itself.
- Single-flight is per node: the claim, the waiter registration, and completion
  are one state transition under the metadata mutex.
- A task that needs a node owned by another task suspends (see below). Before
  recording the wait it walks the wait graph on task identities; a reachable
  cycle becomes `ComputeError::Cycle`. User code never runs under the metadata
  lock.
- Cancellation is checked before claiming, and again inside `commit` while the
  metadata lock is held; a cancelled attempt publishes nothing and releases its
  claim. Cancelling a suspended task queues it so it can observe the
  cancellation. A commit also verifies the claim's owner and generation, so a
  stale attempt can never publish.
- Parallel collection operators use scoped threads over worker chunks and join
  in order, so results are independent of completion order. A failing member
  resumes its original engine payload, preserving `Cycle`, `Cancelled`, and
  other structured errors.

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

A collection field's structure is maintained by the runtime from the committed
content, using the content vtable's `structure` projection and its equality for
cutoff: a map's key set, a list's id sequence, a set's members, a graph's
topology, and a reduced field's sequence. Definition installs (both per-entity
and whole-value), membership retractions, and whole-field retractions refresh
it; value-only edits do not. Derived lists have two storage modes: a whole-value definition with positional
identities, and a keyed definition (`define_keyed`) with explicit `u64`
identities, one value unit per entity, and an assembled storage value. The
keyed mode keeps member scopes keyed by identity, so a reorder or an unrelated
value edit reuses per-member `map` results. Derived sets have the same two modes: whole-value, or keyed `define_keyed`
definitions that stage one membership unit per member plus the structure draft.
In the keyed mode `contains` reads the member's membership unit, so adding or
removing one member does not invalidate another member's consumers. Retracting a
membership unit certifies it as false; it must not clear the certification that
the same node uses as its membership record. Derived `Reduced` and `Graph` fields are
rejected at compile time: those families need a `GeneralComponent` or a source
View.

Source units are created by edits; derived units by definitions. A definition is
owned by exactly one provider node; a second live provider is a deterministic
`DuplicateDefinition` error, including on the `call` path. Installation is
two-phase: every target region is validated before anything is retracted or
installed, so a rejected commit leaves the regions untouched. Retracting a
whole-field definition restores the field to its empty value and invalidates its
readers; retracting an entity unit marks it suspect and clears its certification.
Absence-predicate cells are kept only by live traces and are reclaimed with the
consumer that queried them.

## Observation

`edit` returns the transaction's outcome. Refreshing observers is a separate
step: if the root cannot be demanded afterwards (no root is installed, or its
demand fails), the failure is recorded in `Runtime::notification_error` and the
edit still succeeds. A source-only runtime captures its snapshot without
demanding a root.

## Lowering vocabulary

`reactive::plan` publishes the site/projection/generator vocabulary used to
describe lowered components (`Site`, `SiteShape`, `Projection`, `Generator`).
The compiler still lowers to direct builder calls; the vocabulary exists for
diagnostics, tooling, and future backends.

## Explanation

Site identity reuse is counted (`site_creations`/`site_reuses`) in runtime
statistics and explanations, and `wait_cycles` reports node cycles in the wait
graph alongside `wait_edges`. An `Explanation` also carries the node's scope,
site, and human-readable scope ancestry (interned discriminant or keyed member),
so a host can compare identity paths when reuse changes.

Every node records `executed_at` and `validated_at` separately, plus its
scheduling state and waiter count. `explain_node` exposes them with the
dependency versions, so a host can tell "replayed from a validated trace" from
"re-executed"; `Explain::waiting_tasks` reports suspended demand tasks.

## Observation scopes

`observe` subscribes to a whole View content version. `observe_units` watches an
explicit set of logical units, and `observe_entry` watches one map entry's
membership and value units; these fire only when a watched unit's `changed_at`
moves, so an edit to another key does not wake the callback.

## Fixed points

The solver reports worklist `iterations` and the number of transfer
`contributions`; every solve restarts from the declared seeds, so the
contribution count is the reset's cost.

## Retention

`collect` reclaims unreachable nodes, absence-predicate unit cells, invocation
records, and interned scopes that no live node references; `ReclaimStats`
reports each count and `memory_report` exposes scopes, task records, and
structure units. Reclaimed arena slots are reused: `NodeId` packs a 20-bit slot
and a 12-bit generation, allocation advances the generation, and a stale id
fails the generation check (`Runtime::is_live` reports it without panicking).
`RuntimeConfig::auto_collect_every` optionally reclaims on demand entry after a
revision interval; the default keeps collection manual.
`RuntimeConfig::max_invocations` bounds the invocation cache: when exceeded, the
least recently used invocations whose bodies no live trace references are
evicted (referenced entries are kept, so the cache can exceed the cap by the
in-flight set).

## Demand surfaces

`read_outputs` demands the body and every output projection. `read_projection`
demands the body and one projection node; the siblings stay unevaluated and
unversioned. `call_projection` is the same shape for component invocations. The
body still returns the whole tuple, so projection demand selects which output
nodes run, not which tuple members are computed.

## Tree replacement

`TreeEditor::replace` reconciles the new build positionally against the existing
subtree. A child whose variant is unchanged keeps its identity and has its
fields and children updated in place; a changed variant is rebuilt. Replacing a
node with an identical document therefore preserves every descendant id, and a
field edit on a leaf keeps that leaf's id.

## Component lowering

- Every value-producing expression becomes `reactive::eval(site, || ...)`, keyed
  by `(scope, site)`. Site ids are sequential per component; component bodies
  are isolated by their scopes.
- `#[component(strict, allows = [helper, ...])]` rejects free-function calls
  that are not declared primitives, keeping the unsupported-call policy
  explicit. The default remains permissive while legacy components migrate;
  method calls, IO, and hidden mutation are not yet checked.
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
An update must retain the dependencies it did not re-read: `GeneralCx::observe`
records an observation without using the value, and the join uses it for the
key-set structure units and untouched joined keys. `TypedGeneralComponent`
layers typed `Inputs` (re-read before each run), fixed `Params`, and typed
`Returns` over the erased interface; `TypedAdapter` bridges it to
`GeneralComponent` and `Runtime::install_typed_general` installs it.
`AsyncGeneralComponent` futures are driven to completion: a pending future
parks the demanding thread until its waker (possibly another thread) unparks
it, then polls again. `cx.request(unit).await`/`request_typed` return a receipt
future: a certified or cleanly replayable unit completes immediately. When the
unit is being computed by another task, a pooled demand is marked waiting and
its attempt unwinds (`Suspended`), yielding the worker; the node's completion
queues the task and the demand restarts. An inline demand registers the
future's waker and parks instead. The commit stays atomic; the worker is held while the
future is pending, so true worker suspension is still future work. With signed `i64` sides and
multiplication the equi-join is a bilinear bag join; negative weights retract.
`Reset` reinitializes (always sound); `Incremental` calls `update`. State,
output, trace, and definitions commit atomically. `GeneralCx` supports
dependency scopes: reads inside `begin_scope`/`end_scope` are attributed to the
scope, `retain` keeps a scope's changes for the next batch, and changes to
retired scopes are filtered. `cx.output()` returns a typed `OutputTxn` that
stages per-unit definitions (`define`, `field`, `entry`, `membership`). `EquiJoin` and `FixedPointProblem`
are built on this boundary.

## Where to extend

- New View family: add a field type in `reactive-collections`, a unit layout,
  descriptor vtable, and macro recognition.
- New operator: prefer a per-member scope with `Param`; fall back to
  `GeneralComponent` when the algorithm needs retained state.
- Scheduling changes must preserve the claim/commit protocol and the
  `suspect_at <= verified_at` invariant.

## Suspension

A demand is a task (`task.rs`, `demand.rs`). The host submits it with a read
permit and a result channel; workers run attempts. When an attempt needs a node
that another task owns, `wait_for` registers the task on that node, sets its
status to `Waiting`, and returns `ComputeError::Suspended`. That error travels
through user code as an engine payload, the same channel cancellation and cycles
use: each node boundary releases its execution mark and passes it on, and the
attempt ends at the task boundary. The worker returns to the pool.

When a node commits or fails, it queues the tasks suspended on it. A queued task
restarts from its demand root. Nodes committed before the suspension are clean,
so the restart reuses them; nothing has to capture and resume a Rust stack.

Closures are ordinary Rust and cannot be interrupted mid-body, so an attempt is
a restart, not a resumption at the suspended expression. A node the task claimed
before suspending stays claimed — its owner is the task identity — which keeps
single-flight intact across suspensions. The next attempt sees
`Running { owner: me }` with the node not on its execution stack, treats it as a
resume, and continues. `TaskSlot::held` records those claims so a task that
fails or is cancelled after a restart still releases them; a waiter on a
released claim is queued and re-claims the node itself.

Inline tasks are demands that run on their calling thread: a runtime with no
workers, or a nested demand on a worker. They cannot yield, so their waits park
on the node-completion condvar. That keeps the one-worker and nested-demand
cases exactly as they were.

Progress accounting is the task status table: `RuntimeStats::tasks` counts
queued, running, and waiting tasks. A demand completes only when its task has
left the table, so "every worker is idle" can never be mistaken for completion.
Dynamic expansion permits are unnecessary here: demand expands only through
nested inline evaluation, which cannot outlive the attempt that started it, and
through parallel operator chunks, which are joined before the attempt returns.

Two details are easy to get wrong:

- Engine failures travel as panic payloads, so the runtime installs a hook
  filter (`quiet_engine_panics`) that ignores engine payloads and delegates user
  panics. Without it every suspension would print a backtrace.
- A task can be woken while its previous attempt is still unwinding. The second
  job is a no-op — the body is only taken while the status is `Queued` and the
  body is present — and the finishing attempt requeues the task itself.

### Why restarts are sound

A restart repeats orchestration, not committed work. Component bodies are pure:
effects are rejected by the lowering, and nested commits are idempotent because
a committed node is clean at its revision. Staged definitions and observations
live in the frame that unwinds, so nothing half-applied survives. The claim is
the only state kept across an attempt, and the next attempt rebuilds the rest
from the same revision.

The acceptance gates are the oracle: `a_waiting_demand_yields_its_worker_to_queued_work`
fails under the blocking implementation, `a_suspended_demand_can_be_cancelled`
checks that cancellation reaches a suspended task, and the randomized oracle
plus the worker-count and contention gates check that results do not depend on
how often an attempt restarted.
