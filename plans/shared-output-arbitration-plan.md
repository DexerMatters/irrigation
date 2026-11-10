# Shared output arbitration: ports, claims, and write conditions

**Status:** implemented, with the explicit boundaries below.

Landed: validity stamps and commit-time trace validation with bounded demand
retry; entity and whole-field write claims with `WriteConflict`; reader waits on
live claims; node-scoped structural requirements with a demand-scoped deferred
write queue; `PortProvider` demand for missing graph endpoints; graph
`define_node`/`define_edge` with endpoint requirements; checked map
insert/update/update-if-present/remove-if-present; checked list set-at and
bounds failures; `require_value` for eager value guards; explicit
`retract_unit` and keyed-set member retraction; `value_version` so a change
inside revision 0 is distinguishable from an equal redefinition; claim,
conflict, deferred-write and retry counters; the nine `arbitration.rs` gates.

Graph node removal now rewrites the whole graph (force-retracting old node,
edge, and topology units before installing the survivors), so it does not depend
on a previous branch site being reclaimed. `resolve_conflicts` treats a stale
same-unit provider as a real collision again, preserving dynamic duplicate
diagnostics. A two-worker gate (`a_reader_waits_for_a_live_writer_claim`) holds a
writer inside the evaluation node that owns its claim and proves the reader
suspends (`reader_waits` stat) before the writer commits.

`require_value_await` now waits for a live writer and re-evaluates its predicate
before failing. `reader_waits` counts claim waits, and the two-worker gate proves
a reader suspends rather than reading stale content.

Checked tree move/remove methods now report structured precondition errors, and
the migration `WriteArbitration::Off` mode is available (claims disabled). The
tree editor already rejected missing parents, descendant destinations, and
illegal slots; the checked wrappers make the failure explicit.

The observe path now has an `arbitration` event (`write_claim`,
`write_conflict`, `deferred_write`, `reader_wait`, `demand_retry`) alongside the
stats counters.

`WriteArbitration::Strict` is implemented: `plan_route` is the explicit
declaration, and an undeclared write or requirement port fails with
`WriteOutOfScope`. The gate covers both the rejected and the planned runtime.

Derived tree output transactions claim the whole View (`tree_output` /
`tree_patch`), which is the closure claim for destructive subtree writes. Source
tree edits are serialized by the edit gate.

Explicit boundaries of the implementation:

- **GeneralComponent output units are not entity-claimed.** Each output View
  already has one registered producer and `bad_producer` orders readers; adding
  per-unit claims to general components created a producer/reader wait cycle in
  the syntax pipeline, so the single-producer invariant is the ordering
  mechanism there. Generated View components, tree output transactions, and the
  graph APIs do claim.
- **Source graph editors keep their legacy dangling-edge behavior.** A source
  `GraphEditor::add_edge` without endpoint nodes still edits the graph as before;
  derived `define_graph` validates endpoints and fails instead of dropping.
  Enforcing the source rule would break the existing differential/dangling-edge
  fixtures, so it is a documented boundary rather than a silent change.
- **Strict declarations use `plan_route`.** The optional macro
  `writes = [...]` attribute was not added; a `plan_route` entry is the explicit
  declaration that `WriteArbitration::Strict` checks.
- **Tree insertion is `replace`/`move`.** The tree model has no empty child-slot
  API; checked `replace`/`move_subtree`/`remove_subtree` cover the structural
  write surface.

The phases below are retained as the implementation record.

The request has two layers:

1. Several components write into the same View (or the same component), and
   readers must see a coherent result without user-visible locks.
2. A writer may require the View to satisfy a condition before it writes. The
   canonical example is a graph edge: defining an edge between two nodes means
   the edge writer must wait for both nodes to exist. The condition is part of
   the write, not an extra step the user has to remember.

The user-facing result must be transparent: ordinary `define_*` calls produce
either the expected value or an actionable dependency error. No manual waits,
leases, or transaction plumbing.

The plan covers all View families, source edits, derived definitions, general
components, snapshots, observers, routing, cancellation, reclamation, and the
scheduler. It does not add merge policies or last-writer-wins; the existing rule
of one defining computation per logical unit remains.

---

## 0. Summary of the design

Four concepts, layered on the existing node/unit/revision model:

| Concept | What it answers |
|---|---|
| **Port** | The address of a writable/readable fact: one `UnitKey` or its owner key. |
| **Write claim** | "This computation is writing / will write these ports." Used for conflict and for read ordering. |
| **Requirement** | "This write is valid only if these ports currently satisfy these predicates." Used for conditional writes such as graph edges, tree slots, list indexes, and unique indexes. |
| **Validity stamp** | "This observation was made before the last invalidation." Used to reject a commit that read stale state and force a bounded retry. |

The protocol is:

```text
writer:
  evaluate requirements (tracked reads, may wait/defer/fail)
  claim ports (exactly one live writer per overlapping scope)
  stage definitions
  commit only when the trace and every requirement are still valid

reader:
  read the committed value
  if a live claim covers the port, wait for its owner
  if a claim appears later, the read is invalidated and retried before the
  demand can return

conflict:
  two live claims on the same/overlapping port -> WriteConflict
  requirement has no possible producer in this revision -> PreconditionUnmet
  requirement graph has a cycle -> Cycle
```

Two resolution modes keep the common case fast and the hard case honest:

- **Wait** when a live or demandable writer can satisfy the condition in this
  revision. The reader/writer suspends, the other writer proceeds, and the
  waiter resumes.
- **Defer** when the producer may be called later in the same demand but is not
  running yet. The conditional write is held in a demand-scoped queue and
  resolved at the end of the demand, after all components have run.

Anything else fails with a structured, permanent error. The runtime never waits
across a revision: a demand holds a read permit, and an edit cannot run while it
does. A condition that only a future source edit can satisfy is reported as such.

---

## 1. The mental model and the framework mapping

### 1.1 Shared invocations: already supported

Several callers may demand the same component invocation. `call_component`
interns an invocation by argument equality
([`component.rs`](../crates/irrigation-runtime/src/component.rs)), single-flight
shares one execution, concurrent callers suspend, and each output projection has
its own node and cutoff. The existing gates cover this:

- `concurrent_host_reads_share_one_execution`
- `parallel_members_share_one_invocation_without_deadlock`
- `concurrent_demands_share_an_invocation_across_suspensions`
- `changing_one_output_does_not_change_the_other`

No change is needed for this half. Two callers of one component cannot conflict
because the invocation is a pure function of interned arguments and the runtime
shares its committed result.

### 1.2 Shared View ports: partially supported

A derived View can be written by `define_*` calls from any computation. The
runtime enforces one provider per logical unit, but it does not order a reader
against a writer that has not run yet, and it cannot express "this write requires
that condition". That is the gap this plan addresses.

A **port** is the existing `UnitKey`, or its owner key: for an entity unit, the
membership/value pair shares one owner
([`inner.rs`](../crates/irrigation-runtime/src/inner.rs#L2301)). Different map keys
are different ports; a scalar field is one port; a whole-field definition covers
every port under that field.

### 1.3 A shared component is a board of ports

The request describes components "outputting into" and "inputting from" the same
component. The clean mapping onto this framework is a View whose units are the
ports:

```rust
#[view(derived)]
pub struct Board(Map<u32, u32>);

#[component]
pub fn producer_a() -> u32 {
    Board.define(1, 10);
    0
}

#[component]
pub fn producer_b() -> u32 {
    Board.define(2, 20);
    0
}

#[component]
pub fn consumer() -> u32 {
    Board.get(&1).unwrap_or_default()
        + Board.get(&2).unwrap_or_default()
}
```

Key `1` and key `2` are different ports; the producers do not conflict.
`consumer` reads both and waits for whichever writer is live. If the shared
component must retain state across changes, implement it as a `GeneralComponent`
that is the single writer of its output Views and consumes a `ChangeBatch` of
the input ports. If it fans out to several consumers, that is already supported
by separate output projections.

A condition is attached to a write on this board:

```text
producer_edge:
  require Board.node[from] present
  require Board.node[to] present
  define Board.edge[e] = payload
```

The user writes `Board.define_edge(e, from, to, payload)`; the family API emits
the requirements. Expanded, the call is:

```text
require_present(node[from])          // a tracked read
require_present(node[to])            // a tracked read
claim edge[e]                        // exactly one live writer
define edge[e] = (from, to, payload)
```

If a node is missing:

```text
live node writer        -> wait, then re-read
registered provider     -> demand it, then re-read
producer may run later  -> defer the edge write to the end of the demand
nothing can produce it  -> PreconditionUnmet { ports: [node[from], node[to]] }
```

The edge write and the node writes are disjoint ports, so two edges incident to
the same node do not conflict unless the graph operation itself is a closure
write (a node deletion, for example).

### 1.4 What "transparent" means

For a successful demand, the user observes the same result as a serial execution
in which:

- every write that committed before the demand completed is visible;
- every writer ran after the producers of the ports its requirements named;
- no read observed a state that a later commit invalidated.

The runtime may execute a component more than once (retry), may wait on another
task, and may defer a conditional write to the end of the demand. None of that
is visible in the API. A failed demand returns a structured error naming the
condition, the ports, and the reason (no producer, source edit required,
contradiction, cycle), not a stale value.

---

## 2. Current support and the two gaps

### 2.1 Support matrix

| Requirement | Today | Evidence |
|---|---|---|
| Several callers share one invocation | supported | single-flight, suspension, shared-invocation gates |
| Per-output independent cutoff | supported | `changing_one_output_does_not_change_the_other` |
| Different components define disjoint units of one View | supported once writers run before readers | §2.3 probe |
| Two components define the **same** unit | rejected at the end of the revision with `DuplicateDefinition` naming both sites | `two_providers_cannot_define_the_same_unit`, `two_called_components_cannot_define_one_unit` |
| Intentional provider handover | supported; selector or end-of-revision reachability decides | `provider_handover_survives_demand_order`, `routing_subscriptions_fire_only_on_provider_changes` |
| A read waits for a producer of a **GeneralComponent** output View | supported for installed output Views through `state.producers` and `bad_producer` | [`inner.rs`](../crates/irrigation-runtime/src/inner.rs#L219), `#L426` |
| A read waits for a generated component that will `define_*` the unit | **not supported** | §2.2 |
| A late write invalidates a reader already certified in the same revision | **not sound** | §2.2 |
| Read/write cycle detection | only node waits; no read/write condition graph | `wait_for`, `would_cycle` |
| A write can require another port's presence/value | **not supported** | §2.3 |
| Whole-view writer vs unit writer | documented as incompatible, not enforced outside duplicate detection | `plans/concurrent-incremental-system-plan.md` §6.3 |
| Derived aggregate units (structure, topology, adjacency) | runtime-maintained, not claimable | §4.2.3 |

### 2.2 Why a reader does not wait for a generated producer

`Inner::read_unit` asks two questions:

```rust
cf. crates/irrigation-runtime/src/inner.rs#L423
bad_producer(&state, node, revision)           // producer is not verified
unit_definition_is_current(&state, node, revision)
```

Both consult `state.producers`, which is populated only by
`Runtime::install_general` and `Runtime::install_general_component`
([`runtime.rs`](../crates/irrigation-runtime/src/runtime.rs#L207), `#L265`).
`Runtime::install` for a generated root registers its Views but does not insert a
producer. A unit defined by a generated component's `define_*` call has no
producer to demand. The read sees whatever content is committed at that moment.

When the unit is first read before it is defined, `unit_node_init` materializes
it from the current View content and certifies it at `revision`
([`inner.rs`](../crates/irrigation-runtime/src/inner.rs#L406)). A later definition
commits at the same revision and calls `invalidate`, which sets
`suspect_at = revision` ([`state.rs`](../crates/irrigation-runtime/src/state.rs#L791)).
The reader's `verified_at` is already `revision`, so the clean predicate

```rust
suspect_at <= verified_at
```

still holds. The next demand re-certifies the stale value without re-running the
body. There is no commit-time trace check that would notice the reader's
observation was invalidated after it was made.

### 2.3 The two concrete gaps

**Read-before-write.** This test was run against the current tree and then
removed; it is small enough to re-add as the first gate.

```rust
#[view(source)]
pub struct Flag { on: bool }

#[view(derived)]
pub struct Shared(Map<u32, u32>);

#[irrigation]
mod probe {
    use super::{Flag, Shared};

    #[component]
    pub fn reader() -> u32 {
        Shared.get(&1).unwrap_or(0)
    }

    #[component]
    pub fn writer() -> u32 {
        Shared.define(1, 42);
        0
    }

    #[component]
    pub fn root(flag: &Flag) -> u32 {
        let first = reader();   // read before the write
        let _ = writer();       // define(1, 42)
        if flag.on() { first + 1 } else { first }
    }
}
```

Observed:

```text
root output = 0
shared      = [(1, 42)]

after an unrelated edit forces the root body to run again:
root output = 1        expected 43
shared      = [(1, 42)]
```

The write committed but the reader never saw it, even after a later revision.

**Conditional write.** The current graph API has two shapes:

- the source editor (`GraphEditor::add_edge`) trusts the edit transaction and
  does not require its endpoints to exist
  ([`graph.rs`](../crates/irrigation-collections/src/graph.rs));
- a derived graph is defined as a whole (`define_graph`), and edges whose
  endpoints are not in the same call are silently dropped
  ([`graph.rs`](../crates/irrigation-collections/src/graph.rs)).

There is no per-edge derived write that can say "define this edge, but only when
both endpoint nodes exist, and wait for the component that creates them". The
same gap appears for:

- a tree child insert that requires the parent and an empty child slot;
- a list insert that requires `index <= len`;
- a map update that requires the key to be present;
- a unique index that requires the index key to be absent while the entity is
  written;
- a node deletion that must wait for incident edges to disappear or cascade.

The runtime currently has no concept of a write precondition, so each of these
is either unchecked, order-dependent, or silently dropped.

---

## 3. Semantics

### 3.1 Port scopes

A write covers a scope, not only a single unit:

```text
View(v)                    every field and entity in one View
Field(v, f)                every entity of one field, plus its structure unit
Entity(v, f, key)          one entity's membership/value pair
```

For a unit with `entity: None` (a scalar field or a whole-field definition), the
entity scope is the field's value unit. For an entity unit, the scope is the
owner key (membership covers the pair). The runtime-maintained projections
(content node, structure unit, graph topology, adjacency, reduction aggregate)
are **not claimable**; the runtime writes them while installing definitions,
under the state lock. Two disjoint key writers may therefore both refresh the
same structure unit without conflicting with each other; readers of the
structure see one value per commit.

Two claims conflict when their scopes intersect:

| Claim A | Claim B | Result |
|---|---|---|
| `Entity(v,f,k)` | `Entity(v,f,k)` | **conflict** (exact-same-position) |
| `Entity(v,f,k1)` | `Entity(v,f,k2)` | compatible |
| `Entity(v,f,k)` | `Field(v,f)` | **conflict** (the field writer may overwrite the entity) |
| `Field(v,f1)` | `Field(v,f2)` | compatible |
| `Field(v,f)` | `View(v)` | **conflict** |
| `View(v)` | anything in `v` | **conflict** |
| anything in `v1` | anything in `v2`, `v1 != v2` | compatible |

This keeps the plan's boundary: a whole-view or whole-field writer cannot
coexist with independent patch writers. It also gives "exactly the same
position" a precise meaning: `Entity` equality. A coarser overlapping scope is
a conflict for the same reason: the coarser writer can overwrite the position.

### 3.2 Claims

**C1 — One live writer per scope.** At most one live write claim may cover a
port. This generalizes the unique-definition invariant from a unit to a scope.

**C2 — Write/write conflict is rejected.** A second live claim whose scope
intersects an existing claim fails the demand with a structured conflict naming
both writers and their source locations. The framework never picks a winner by
timing. An intentional ownership change is a handover, authorized by the
existing `RouteSelector` or by the earlier owner retiring during the revision;
it is not a conflict.

**C3 — A claim is not a definition.** A claim reserves a port for one
computation while that computation is running. Only staged drafts install. A
claim that never becomes a draft is released at commit or on failure and
installs nothing.

**C4 — Claims are acquired in a canonical order.** A write that claims several
ports checks all of them under one metadata-lock transition and records them in
a deterministic order (`view`, `field`, entity key). This makes two multi-port
writers either conflict or both succeed, never deadlock on claim acquisition.

### 3.3 Requirements (write conditions)

A **requirement** is a predicate over a set of ports that must hold when the
write commits. A requirement is evaluated by reading its ports inside the
writer's computation frame; those reads become ordinary dependencies, so the
commit-time trace check revalidates the predicate. A requirement also carries a
description for diagnostics.

#### 3.3.1 Structural and value requirements

| Kind | Examples | Can it be deferred? | Why |
|---|---|---|---|
| **Structural** | node/member/key/slot/index present or absent; child slot empty; index in range; edge absent/present | yes | the body does not need the condition's value to compute the write |
| **Value** | node payload equals X; old map value satisfies a predicate; parent's tag is `Block`; a field's value is unchanged | no | the body used the value to compute the write; a deferred evaluation would be a lie |

This distinction is the main boundary. It gives transparent behaviour where it
is sound, and an honest blocking/failure where it is not.

#### 3.3.2 Modes

A requirement has a resolution mode:

| Mode | Behaviour when false | Use |
|---|---|---|
| `Wait` | wait for a live or demandable writer; if none, fail | a producer is running or can be demanded now |
| `Defer` | hold the write in the demand-scoped queue and resolve at the end of the demand | a producer may be called later in the same demand; structural requirements only |
| `Fail` | fail immediately with `PreconditionUnmet` | checked APIs (`define_edge_checked`) and value requirements |

`Skip` is not a requirement mode. It is a family-method outcome:
`update_if_present`/`remove_if_present` read the key and return without staging
when it is absent. The read is a normal dependency; no node-wide guard is
recorded. This matters because requirements are node-scoped: a node that writes
several units must not have one no-op skip all of them.

The View family API picks a default per operation:

- graph `define_edge`: `Defer` (structural: endpoints);
- graph `define_edge_checked`: `Fail`;
- map `insert`: `Fail` on an existing key; a live claim on the same key is a
  `WriteConflict` at claim time (not a condition wait);
- map `update_if_present`: read the key; return without staging when absent;
- list `insert`: `Defer` on the index (structural);
- tree `insert_child`: `Defer` on the parent and empty slot;
- tree `move`: `Defer` on the source/destination parents and slots;
- a value condition: `Wait` or `Fail`, never `Defer`.

The user can override the default per call; the default is the transparent one.

#### 3.3.3 Consistency contract

> **Write consistency.** A write commits only when every requirement it declared
> was true at commit, and every port it read is unchanged since the read.
>
> **Read consistency.** A demand that completes at revision `r` has observed
> every write and deferred write that committed before its completion. A write
> that commits after one of the demand's reads forces that read to be
> re-evaluated before the demand returns, or the demand fails without returning
> a stale value.

The first is the compare-and-swap property: check the condition, then commit
while holding the lock, so no writer can slip in between. The second is the
validity-stamp/retry mechanism.

### 3.4 Resolution order for a false requirement

The runtime resolves a false requirement in this order:

```text
1. Is the requirement self-referential?
   (the port is owned/claimed by this same computation)
   -> PreconditionUnmet { reason: SelfDependent } (program error); a
      read-your-own-write is not supported in one body; compute locally or use
      a GeneralComponent.

2. Is a live writer claiming a port that could change the requirement?
   -> If waiting is safe (no held claim overlaps the waiter's dependency
      closure), release claims and wait; then re-evaluate.
   -> If waiting would create a cycle, return Cycle with the condition path.

3. Is there a registered `PortProvider` for a missing port?
   -> release claims, demand the provider, re-evaluate.
   -> only an installed provider can be demanded; a planned route is an
      ownership authorization, not a call entry point. The runtime does not
      invent a component invocation.

4. Is the requirement structural and the mode Defer?
   -> record a pending write on the demand; continue the body.
   -> drain at the end of the demand.

5. Otherwise:
   -> PreconditionUnmet { reason: NoProducer | SourceEditRequired |
      Contradiction | ValueMismatch }
```

Why waiting is not always allowed: a writer may hold a claim on a port that the
writer it is waiting for needs. Waiting while holding it is a deadlock. The
baseline rule is **release claims before waiting**, then re-acquire and
re-validate after the wait. The wait graph still detects cycles for the cases
where a wait is unavoidable.

Why there is no cross-revision wait: a demand holds a read permit; an edit
needs the write permit. Waiting for an edit while holding the read permit
deadlocks the gate. A source-only condition therefore fails with
`SourceEditRequired`, and the host edits and re-demands.

### 3.5 Ordering rules

**R1 — One live writer per scope.** See C1.

**R2 — Write/write conflict.** See C2.

**R3 — Read-after-write waits.** A read of a port covered by a live claim waits
for the claiming node to commit or fail, then reads. This is the "B goes first,
then A" case. The wait uses the existing task suspension and helps a pooled
writer if the reader is inline.

**R4 — Late writes win, and the reader retries.** If a read happens before the
claim exists, it is provisional. A later claim/write covering the port
invalidates the read. A node whose trace was invalidated after it was observed
cannot commit: it returns a retry outcome, the task restarts from its demand
root, and the next read observes the writer.

**R5 — Disjoint writes are unordered and parallel.** Claims that do not
intersect impose no order. A reader of several ports waits only for the writers
that cover those ports; a reader of unrelated ports proceeds immediately.

**R6 — Requirements are checked before claims where possible.** Evaluate a
requirement's reads before claiming the write set, unless the requirement is a
self-read of the same port (update-if-present). This is what prevents the common
deadlock: a writer does not hold a claim while waiting for the writer that could
satisfy its condition.

**R7 — Commit is all-or-nothing.** A node's value, trace, and immediate
definitions commit atomically. A deferred group's definitions commit atomically
at drain. A failed requirement commits nothing from that group.

**R8 — Cycles are errors, not deadlocks.** If A waits for a port written by B
and B waits for a port written by A, the wait graph has a cycle. Return
`ComputeError::Cycle` with a path naming both components, ports, and conditions.

**R9 — No cross-revision wait.** A source-only condition fails; it does not hold
the read gate. A condition that could become true in a future revision requires
the host to edit and re-demand.

**R10 — Failure propagates to waiters.** If a writer fails (conflict,
cancellation, user panic, cycle, unmet condition), tasks waiting on its claim or
condition are woken with the failure; they do not silently fall back to the
previous content.

### 3.6 Handover is not a conflict

The existing model distinguishes:

- the earlier provider is live at the end of the revision → `DuplicateDefinition`;
- the earlier provider retired during the revision → the handover stands.

A claim from a new provider is:

- a **conflict** if the earlier provider is live and no selector/plan authorizes
  the change;
- a **handover** if a `RouteSelector` selects the new provider, or the earlier
  provider has retired and its claim has been released.

`Runtime::plan_route` (or a generalized `plan_ports`) can pre-authorize a
provider for a port before any value exists. A claim from any other component is
then `DefinitionDenied`, as it is today. A requirement can be satisfied by a
handover: the reader waits for the selected provider, sees the new value, and
records the new routing generation.

### 3.7 What this is not

- **Not last-writer-wins.** One live writer per port; ordering a reader after
  that writer does not choose between writers.
- **Not a merge layer.** Two writers of the same port are an error. A user who
  wants a merge writes an explicit combining component that reads both inputs
  and defines one output.
- **Not user-visible locks or transactions.** The user calls ordinary View
  methods; claims, waits, and deferred writes are scheduling facts.
- **Not a general constraint solver.** Requirements are local predicates over
  ports, evaluated as tracked reads. Global invariants (acyclicity, sortedness,
  cardinality over the whole collection, referential integrity across arbitrary
  Views) need a `GeneralComponent`, a fixed point, or an explicit index View.
  The plan supports local conditions and gives the tools for the rest; it does
  not pretend to prove arbitrary invariants.
- **Not omniscient static analysis.** Reads remain dynamically discovered, as
  `plans/concurrent-incremental-system-plan.md` §2.3 requires. Writes and their
  requirements are claimed at the call; a late claim is handled by R4, and a
  late producer by defer/drain.
- **Not a source-edit coordinator.** Source edits remain serialized by the write
  gate; conditions over source Views fail rather than wait across revisions.

---

## 4. Design

The design has three independent parts:

- **A. Validity stamps and commit-time validation** — correctness under
  concurrent writers and late invalidation.
- **B. Claims** — ordering and exact-port conflicts.
- **C. Requirements** — conditional writes, wait/defer/fail, and the deferred
  drain.

Part A is required even if no claims or conditions are added. Part B makes the
common case ordered and the conflict early. Part C makes the graph/tree/list/map
scenarios transparent.

### 4.1 Part A — validity stamps and commit-time trace validation

#### 4.1.1 The defect is the coarse stamp

`NodeRecord` tracks `suspect_at` and `verified_at` by revision. Revision is the
right unit for public snapshots, but too coarse to distinguish two events inside
one revision:

```text
revision r:
  reader reads u at r, verifies at r
  writer commits u at r
  invalidate sets reader.suspect_at = r
  reader.verified_at == r, suspect_at <= verified_at -> "clean"
```

The runtime needs a monotonic **validity stamp** that advances on every
invalidation, independent of the revision. The public revision stays unchanged.

#### 4.1.2 Data

```rust
// state.rs
pub struct NodeRecord {
    // existing ...
    pub suspect_validity: u64,
    pub verified_validity: u64,
}

pub struct State {
    // existing ...
    next_validity: u64,
}

impl State {
    pub fn next_validity(&mut self) -> u64 {
        self.next_validity += 1;
        self.next_validity
    }
}

// frame.rs
pub struct Dep {
    pub node: NodeId,
    pub version: u64,
    pub routing: u64,
    pub validity: u64,   // the dependency's suspect_validity when observed
}
```

A node is **clean** when the existing revision/value rules hold and

```text
suspect_validity <= verified_validity
```

`invalidate` assigns one fresh validity stamp to the whole invalidation batch and
sets `suspect_validity` on each seed and dependent. `commit`, `eval`, and
`read_unit` set `verified_validity = suspect_validity` when they certify.
`retire_unreachable` and `force_recompute` advance the stamp as well.

Recording `validity` in every `Dep` is the key. It is one `u64`, and it makes
"was this observation invalidated after I made it?" answerable without walking
the reverse graph at commit time.

#### 4.1.3 Commit-time validation

Before a node publishes, under the metadata lock, validate its trace:

```rust
fn trace_is_committable(state: &State, deps: &[Dep]) -> bool {
    deps.iter().all(|dep| {
        let record = state.node(dep.node);
        record.changed_at == dep.version
            && record.routing_at == dep.routing
            && record.suspect_validity == dep.validity
    })
}
```

If it fails, the attempt did not observe a state that is still current; return
`ComputeError::Retry` (internal control flow, like `Suspended`). The claim is
released, the task is requeued, and the demand restarts. The check is O(trace),
the same order as the existing validation walk. A later optimization can skip
dependencies whose `verified_validity` is newer than the attempt's start stamp,
but the simple version is the correctness baseline.

The check must run **before** `install_definitions` and before any content write:
a rejected attempt may not leave a tentatively installed definition behind. The
existing two-phase content/definition install stays where it is; the trace check
is the first phase.

#### 4.1.4 Retry at the demand boundary

- Add `ComputeError::Retry` and `Attempt::Retry` (or reuse the existing
  scheduling-error channel with a non-waiting requeue path).
- A pooled task that returns `Retry` is queued again immediately; it has no
  waiter edge and no gate change. An inline task loops in `demand_inline`.
- `RuntimeConfig::max_retries` (default 32) bounds the loop. Exhaustion is
  `ComputeError::UnstableDemand { ports, retries }`, not an infinite loop.
- Cancellation is checked before every retry. A cancelled retry publishes
  nothing.
- Committed nodes from earlier attempts are reused: their values/traces are
  valid unless a later write invalidated them, in which case the validity stamp
  makes them re-run.

#### 4.1.5 Why this fixes the read-before-write reproducer

```text
root body:
  reader reads Shared[1] (absent), records dep(membership, validity V0)
  writer defines Shared[1] = 42
    invalidate membership: suspect_validity = V1
    invalidate reader:     suspect_validity = V1
    invalidate root:       suspect_validity = V1
  root commit:
    trace contains reader output; its recorded validity is V0, current V1
    -> Retry
  root restarts:
    reader is dirty (V1 > its verified_validity V0)
    reader validates its trace: membership version changed -> re-executes
    reader sees 42
  root commits with 43 (or 42 without the flag)
```

This is the "B goes first" outcome without any explicit ordering: B's write won
because it committed, and A was re-evaluated before its demand could return.

### 4.2 Part B — write claims

#### 4.2.1 Claim record and indexes

```rust
pub struct WriteClaim {
    pub owner: NodeId,                 // the defining computation
    pub task: TaskId,                  // for wait/cycle identity
    pub scope: WriteScope,             // View / Field / Entity
    pub state: ClaimState,             // Active | Deferred | Released
    pub label: &'static str,
    pub location: Option<&'static Location<'static>>,
    pub generation: u64,               // node claim generation
}
```

The claim table is indexed three ways:

```text
entity_claims: FxHashMap<UnitKey, WriteClaim>          // normalized to owner key
field_claims:  FxHashMap<(ViewId, FieldId), WriteClaim>
view_claims:   FxHashMap<ViewId, WriteClaim>
```

Only one claim is allowed per key at a time. The existing `Region.owners` stays
the committed owner record; claims are the pre-commit intent.

A unit read checks the three indexes in order: its entity claim, the field
claim, then the view claim. A field claim additionally checks the field's entity
bucket, so a coarse claim cannot miss a live entity claim. Keep a
`entity_claims_by_field: FxHashMap<(ViewId, FieldId), SmallVec<UnitKey>>` (or a
per-field live count); the number of live claims is bounded by the number of
in-flight writer attempts, not by the size of the View.

#### 4.2.2 Claiming a write

`define_unit` and the collection `define`/`define_all`/`define_keyed` helpers
claim before they stage a draft. The claim is acquired in the current frame and
released on commit, fail, cancel, retirement, or a deferred drain.

```rust
pub fn claim_write(scope: WriteScope) -> Result<()> {
    let frame = current()?;
    let node = frame.node();
    let state = runtime.lock();
    if let Some(existing) = state.conflicting_claim(&scope) {
        if existing.owner != node {
            return Err(ComputeError::WriteConflict {
                port: scope.describe(),
                first: describe_provider(existing.label, existing.location),
                second: describe_provider(frame.label(), frame.location()),
            });
        }
    }
    state.claims.insert(scope, ClaimRef { node, task: task::current(), ... });
    Ok(())
}
```

A computed key claims its entity when the `define(key, value)` call is reached.
A `define_all` on a map/list/set may either claim each entity as it is staged or
claim the field once. Field claims are cheaper but serialize disjoint keys; the
plan recommends entity claims for per-key definitions and a field claim only for
a whole-value reconstruction.

A claim is not a definition. It reserves a port for one computation while that
computation is running; `install_definitions` still stages and commits only the
drafts the body produced. A claim that never becomes a draft is released at
commit or on failure and installs nothing.

#### 4.2.3 Derived ports are not claimable

The following units are written by the runtime while it installs definitions,
not by user computations:

- `RegionContent` (the whole View content node);
- `UnitKind::Structure` (a map key set, list rank sequence, set members, graph
  topology, reduced sequence);
- graph adjacency / incoming-adjacency units for source graphs (updated by the
  runtime from the committed content, or computed from topology for derived
  graphs);
- a reduced field's cached aggregate.

They are **derived projections**. A user writer must not claim them, and two
writers of disjoint source units must not conflict merely because both refresh
the same projection. The runtime refreshes them sequentially under the lock and
invalidates their readers normally.

A requirement that reads a derived projection (for example `keys()` or
`neighbors(v)`) is a read of the field-level projection. It waits for live
claims on the underlying field: any live entity/field writer in that field may
change the projection. This can over-wait, but it is correct and simple. A
per-key condition is preferable when the user can express it.

#### 4.2.4 Reading a claimed port

In `read_unit`, after the existing `bad_producer` check:

```text
if an Active claim covers this port and its owner is not me:
    if the owner node is running:
        wait_for(owner_node)          // R3, existing suspension machinery
        continue                      // re-read after it commits/fails
    if the owner node failed:
        return the owner's failure    // R10
    // the claim was released without a commit: read the committed value

if a Deferred claim covers this port and its owner is not me:
    read the committed value (the deferred write is not visible yet)
    record the normal dependency; the drain will invalidate this read
```

`wait_for` already handles pooled suspension, inline parking, cancellation, and
the wait-graph cycle check. A claim wait is another node wait, keyed by the owner
node. The reader does not wait for "the whole writer component" if the writer is
a map producer; it waits for the node that claimed *this port*. A single writer
node may claim many ports; waiting on it once is enough.

If a claim is released because the writer suspended and its attempt unwound, the
reader that was waiting on the node remains queued until the node commits or
fails. If the writer's next attempt no longer writes that port, the reader wakes
on commit and reads the old value. That is correct; it may be one wasted wait.

#### 4.2.5 Claim lifetime and ordering

The sound baseline is **node/task scoped until the attempt ends**: commit, fail,
cancel, retirement, or drain releases the claim. A suspension keeps the claim
and the node `Running`, so readers wait for the eventual commit. On a restart,
the body re-registers the claims it still writes; claims no longer reached are
released at commit. A stale claim only over-serializes; it never produces a
wrong value.

Rules that prevent claim deadlocks:

1. **Evaluate requirements before claims.** Read the condition's ports first,
   then claim the write set. This avoids holding the very port the condition
   waits for.
2. **No waiting while holding an overlapping claim.** If a requirement fails
   while this writer holds a claim that the awaited writer needs, release the
   claim, wait, then re-acquire and revalidate. The baseline can always release
   and retry; the wait graph also detects the cycle if release is not possible.
3. **Claim many in canonical order under one lock.** Sort by
   `(ViewId, FieldId, entity key)`; either all claims are acquired or none.
4. **Never claim a derived projection.** See §4.2.3.
5. **Deferred writes hold claims only on ports disjoint from the
   requirements' may-change sets.** If a deferred write's condition depends on a
   port it would claim, it must not reserve that port; it defers without the
   claim and tries again at drain time. Otherwise the producer it is waiting for
   would be blocked by its own reservation.

#### 4.2.6 Release and wake

On commit:

```text
install immediate definitions (existing)
release Active claims
keep Deferred claims for the pending group (they release at drain/failure)
wake tasks waiting on this node (existing wake_waiters)
invalidate changed units (existing; validity stamps advance)
```

On fail:

```text
record the failure (existing)
release claims
wake waiters with the failure (R10)
```

On retirement/collection: release claims before clearing the node, so a waiter
cannot be left on a dead owner.

On defer: the claim's state becomes `Deferred`; readers do not wait on it, but
other writers conflict with it. At drain it becomes `Active`/committed or is
released.

#### 4.2.7 Conflict details

- Same entity from two components in one revision: `WriteConflict`, both
  locations, no definitions installed by the loser. The error is permanent.
- Entity vs field: conflict, even if the field writer currently writes a
  different key; the field claim means "this computation may reconstruct the
  whole field".
- Field vs field, view vs anything: conflict.
- Disjoint entities: no conflict, no shared lock beyond the metadata lock that
  already serializes commits.
- A second claim from the **same owner** (e.g. the same node defines a key twice)
  is idempotent at the claim layer; the existing `DuplicateDefinition` check on
  the draft list still diagnoses two drafts for one unit in one run.
- A claim from a provider that is planned for the port is accepted; any other
  provider gets `DefinitionDenied` before the claim.
- A claim from a new provider while the old provider is live and a selector
  chooses the new provider is a handover: the old definition record is dropped,
  the old provider is forced to re-run, and the claim transfers. Without a
  selector, it is a conflict.

#### 4.2.8 Closure claims: destructive writes are not per-member

A claim scope is static. A write that retracts a **closure** cannot enumerate
the closure's members and claim only those members: a concurrent writer can add
a new member after the enumeration, and the destructive write would miss it.
This is a time-of-check/time-of-use bug, not a theoretical concern.

Concrete cases:

- deleting a graph node cascades its incident edges; an edge added between the
  read of the incident set and the commit would escape the cascade;
- deleting a tree subtree removes all descendants; a child inserted into the
  subtree in that window would escape;
- rebuilding a list from a prefix/suffix snapshot misses an append into the
  window;
- a set whole-value replacement removes members not in the new set; a
  concurrent member insert would be silently overwritten.

Rule: a destructive/closure write claims a scope that **covers every possible
member**, in practice the field (or the View, if the operation spans fields):

```text
delete graph node      -> Field(graph) claim (or a dedicated closure scope)
delete tree subtree    -> Field(tree) claim
rebuild list           -> Field(list) claim
replace set            -> Field(set) claim
```

This is coarse, but it is the price of a static claim and it matches the
existing rule that a whole-field writer cannot coexist with patch writers. A
fine-grained delete can be added later with a dedicated "closure" scope that
conflicts with member inserts but not with unrelated fields; the baseline uses
the field claim.

A non-destructive closure read (for example, computing a count or a fold) is
just a read: it records the structure dependency and retries if the structure
changes. Only the write side needs the closure claim.

### 4.3 Part C — requirements and deferred writes

#### 4.3.1 Requirement record

```rust
pub struct WriteRequirement {
    pub owner: NodeId,
    pub ports: SmallVec<[UnitKey; 4]>,   // observed ports
    pub mode: RequirementMode,          // Wait | Defer | Fail
    pub kind: RequirementKind,          // Structural | Value
    pub description: String,            // "graph edge endpoints exist"
    pub location: Option<&'static Location<'static>>,
}
```

Requirements are **node-scoped**: all definitions staged by a computation are
guarded by all requirements it declared. A component that needs different
conditions for different writes should stage them in different nodes/components;
this keeps the commit check simple and makes the error attribution exact.

Only **structural** requirements are deferrable, and their record is deliberately
small and re-evaluable: a kind (`Present`, `Absent`, `All`), the observed ports,
and the expected boolean. The drain re-reads those units; it never captures a
user closure or a borrowed value across the demand. A **value** requirement is
evaluated eagerly in the body and is resolved with `Wait`/`Fail`; it is not
deferred and therefore never needs a stored predicate. This is what keeps
`PendingWrite` serializable and the drain deterministic.

#### 4.3.2 Evaluation is a tracked read

`require_present(port)` reads the membership/value unit and records the read in
the current frame. `require_absent(port)` reads the same unit and expects false.
`require_value(port, predicate)` reads the value and checks. `require_all` is a
conjunction. The reads are ordinary dependencies, so:

- `trace_is_committable` already revalidates them at commit;
- an invalidation after the read forces a retry (Part A);
- a live claim on the port is found by the claim lookup (Part B);
- a false requirement can be resolved by wait/defer/fail.

A requirement also captures a **may-change set**: the ports and scopes that
could change the predicate. For `require_present(node[n])`, that is the node's
port and the field-level structure. For `require_absent(index[k])`, it is the
index key's port and the field-level structure. The may-change set is used to
find live writers/providers when the condition is false.

#### 4.3.3 Resolution

```rust
fn resolve_requirement(req: &WriteRequirement) -> Resolution {
    if req.ready() { return Resolution::Satisfied; }
    if req.is_self_dependent() { return Resolution::Fail(SelfDependent); }

    let claims = state.active_claims_overlapping(req.may_change_set());
    // Deferred claims are deliberately excluded: their owners are not running,
    // so waiting on them would stall the drain. They are ordered at drain time
    // by the pending-group dependency graph.
    if !claims.is_empty() {
        // A live writer can change the requirement. Release this writer's
        // overlapping claims first (see §4.2.5), wait, then re-evaluate.
        return Resolution::Wait(claims);
    }

    if let Some(provider) = state.provider_for(req.ports()) {
        return Resolution::Demand(provider);   // release claims, demand, retry
    }

    if req.kind == Structural && req.mode == Defer {
        return Resolution::Defer;
    }

    Resolution::Fail(req.reason())
}
```

The decision tree is evaluated under the state lock for the claim/provider
lookup, then the wait/demand happens outside the lock. After a wait or demand,
the requirement is re-read; the loop is bounded by the retry budget.

#### 4.3.4 Waiting

- Release the writer's claims that overlap the awaited writer's claim set (the
  baseline releases all claims, then re-acquires; simpler and safe).
- Use `wait_for(owner_node)`; pooled tasks suspend, inline tasks park/help.
- On wake, re-read the requirement and re-acquire claims.
- If the writer failed, return its failure (R10).
- If a cycle is detected, return `Cycle` with the condition path.
- A wait on a `Deferred` claim is not allowed; a deferred writer is not running
  and will not commit until the drain. The requirement either resolves at drain
  or fails. This prevents a reader from waiting on a write that is itself
  waiting for a producer.
- On a workerless runtime (`workers = 0`) or a `LocalGeneralComponent`, no other
  thread can make progress. The runtime demands a registered provider inline or
  fails with `PreconditionUnmet`; it does not park the only thread. This is the
  same rule that already forbids a local component from waiting on a remote
  worker.

#### 4.3.5 Provider demand

A condition can be satisfied by a producer that has not been called. The runtime
can demand it only if the host registered a plan for the port:

```rust
pub trait PortProvider: Send + Sync {
    /// Runs the computation that produces `port`, if it is not already present.
    fn provide(&self, port: &UnitKey, cx: &mut ProviderCx<'_>) -> Result<()>;
}

Runtime::install_port_provider(view: ViewId, provider: Arc<dyn PortProvider>)
```

`plan_route` is the static form: a port is assigned to a component before any
value exists, and a claim from another component is denied. It does not supply
a call entry point, so a requirement cannot demand the planned component from
the plan alone. `PortProvider` is the dynamic form: it maps a missing port to a
demand. Install both when the host wants both authorization and demand. The
provider runs as a nested demand; its commits are normal; the requirement
re-evaluates afterwards.

This is how a graph host can make `define_edge` transparent even when the node
producer has not been called:

```rust
runtime.install_port_provider(<Nodes as ViewSchema>::DESCRIPTOR, Arc::new(NodeProvider));
// NodeProvider::provide(node_port) -> nodes::ensure_node(node_id)
```

The provider itself is ordinary component code; it can be a no-arg call, a
parameterized invocation, or a `GeneralComponent` request. The runtime does not
guess arguments from a `UnitKey`; the host's provider does. That is the explicit
boundary that keeps the core general.

Without a provider/plan, a missing port is either deferred (structural) or
failed (value/source). It is never guessed.

#### 4.3.6 Deferred writes and the end-of-demand drain

A deferred write is a write whose structural requirements are currently false
and for which no live/demandable producer exists. It is recorded, not installed.
The owner node still commits its value and trace; only its definition group is
held. The queue is demand-scoped: it belongs to the top-level task, not to the
runtime globally.

```rust
pub struct PendingWrite {
    pub owner: NodeId,
    pub task: TaskId,
    pub revision: Revision,
    pub generation: u64,
    pub group: SmallVec<[DefinitionDraft; 4]>, // all writes of one owner
    pub retractions: SmallVec<[UnitKey; 4]>,   // units the run no longer defines
    pub restate_all: bool,                     // the run restates its whole set
    pub requirements: SmallVec<[WriteRequirement; 4]>,
    pub claims: SmallVec<[WriteScope; 4]>,     // disjoint from requirements
    pub trace: SmallVec<[irrigation_core::frame::Dep; 8]>, // the owner's read set
}
```

The group is the owner's complete definition output for the attempt: drafts,
retractions, and the restatement flag. It replaces the owner's previous
definitions atomically at drain. Until then the old definitions remain visible;
a reader sees the previous committed state, not a half-retracted one. If the
group fails, the old definitions remain and the demand fails. The owner's
`definitions` list is updated at drain, not at the value commit.

Retirement, cancellation, or a new attempt of the owner discards the old group;
a later attempt stages a fresh one.

A concurrent demand that reads a deferred port before the drain sees the
previous state. The drain is a later commit at the same revision; that demand is
not stale, it observed the state before the deferred write committed. If the
drain invalidates one of its reads, the validity stamp and the demand retry
handle it.

`GeneralComponent` is excluded from deferral. Its retained state, output, and
definitions commit atomically; a deferred group would split that commit. A
requirement inside a general component is resolved with `Wait`/provider demand,
or fails. The same applies to an async general component: it may park, but it
does not hand its output transaction to a later drain.

Deferral is a **progress device**, not an install device. A deferrable
requirement is false, so its tracked read is of the false state; the producer that
makes it true changes that unit and invalidates the group's trace. At drain the
usual outcome is therefore "the condition is now true, but the drafts were
computed from stale reads: retry the owner". The owner re-executes against the
now-committed state and installs immediately. Deferral exists so the demand can
continue far enough to call that producer at all; without it, a sequential root
body that writes the edge before the nodes would abort before reaching the node
producer. Direct installation at drain is the shortcut for a group whose trace
is still valid (the condition became true without changing a recorded read); it
is not the common path.

Drain at the end of the top-level demand:

1. Re-validate the pending group's recorded trace and re-evaluate every pending
   requirement against the now-committed state. A group whose owner trace was
   invalidated does not install: discard it and set the demand's retry flag;
   the demand loop re-runs the owner against the new state, or fails if the
   retry budget is exhausted. The drafts were computed from the old reads and
   must not be installed on a stale trace.
2. Build a dependency graph: pending group A depends on group B if A's
   requirement reads a port B defines. Topologically order.
3. For each group in order, if all requirements hold and the claim/conflict
   check still passes, install its definitions atomically:
   - update `Region.owners`, content, structure, unit values;
   - advance `changed_at`/`routing_at`;
   - add the `DefinitionRecord` to the owner node's `definitions`;
   - invalidate dependents with a fresh validity stamp.
4. If a group's requirements still fail, fail the demand with
   `PreconditionUnmet`; discard all remaining pending groups. No partial group
   is installed.
5. If a dependency cycle exists, fail with `Cycle` (condition path).
6. After the drain, validate the root's trace again. A drained write may have
   invalidated a read the root made; if so, force the root body to recompute
   (clear its value/trace in the same way `force_recompute` does) and retry the
   root. The retry reuses the now-committed definitions; the pending queue is
   empty or idempotent. This retry belongs to the demand loop in §4.4, not to a
   nested re-execution inside the drain.

Cancellation or failure discards the queue and releases its claims. Observers
and snapshots see only installed definitions; a pending write is invisible until
the drain.

Why the owner's value may commit before its definitions: the value is the
component's output, computed from inputs; the definitions are its View effects.
The framework already separates output projections from definitions. A component
whose output literally reports "I wrote the edge" should not do that in a
deferred write; outputs are data, effects are definitions. This is the same
purity boundary the framework already documents.

#### 4.3.7 Commit-time check

For an immediate commit:

```text
for every requirement: its read versions and validity stamps are in the trace
trace_is_committable(deps) must pass
```

For a deferred group:

```text
re-evaluate the requirement's predicate against the committed state
under the lock (not by re-running the body)
trace/versions of the pending group's reads must still match
```

The check and the definition install happen under one lock transition, so the
compare-and-swap property holds. If a requirement became false after the check
but before install, that cannot happen: no other commit can interleave.

#### 4.3.8 Failure taxonomy

| Case | Error | Permanent? |
|---|---|---|
| Two live writers, overlapping scope | `WriteConflict { port, first, second }` | yes |
| Missing port, no producer, no source edit possible | `PreconditionUnmet { reason: NoProducer }` | yes |
| Missing port that only a source edit can create | `PreconditionUnmet { reason: SourceEditRequired }` | yes (edit + re-demand) |
| Requirement contradicted by the same state | `PreconditionUnmet { reason: Contradiction }` | yes |
| Requirement reads the port this writer defines | `PreconditionUnmet { reason: SelfDependent }` | yes |
| Value requirement false and no producer | `PreconditionUnmet { reason: ValueMismatch }` | yes |
| Requirement producer failed | the producer's error | depends |
| Requirement dependency cycle | `Cycle { path }` | yes |
| Retry budget exhausted | `UnstableDemand { retries, ports }` | yes |
| Write outside declared set in strict mode | `WriteOutOfScope { port }` | yes |

`SourceEditRequired` is permanent for the current demand but actionable: the host
edits the source View and re-demands. The error text says which ports are
missing and which View to edit.

### 4.4 Demand protocol, end to end

```text
demand(root):
  loop (bounded by max_retries):
    task = current task
    result = eval(root)
    match result:
      Ok(value):
        drain_deferred(root)          // may install, retry an owner, or fail
        if drain_retry_needed(root):  // a drained write invalidated the root
            continue
        resolve_conflicts; return value
      Err(Retry):
        if retries < max { continue } else { fail UnstableDemand }
      Err(other):
        discard_deferred; return other

eval(node):
  if node clean at revision and validity:
      record dep(node, version, routing, validity); return value
  claim node (single-flight) or wait
  validate ordered trace
  if valid:
      verify node; record dep; return value
  execute body:
      requirements: read ports; if false, wait/demand/defer/fail
      writes: claim_write (conflict -> WriteConflict)
      definitions: stage drafts
  commit:
      if !trace_is_committable(deps): release claims; return Retry
      if any requirement is deferred:
          publish value/trace; record PendingWrite; keep Deferred claims
      else:
          publish value/trace; install immediate definitions; release claims
      return value
```

Important ordering points:

- Requirements are evaluated before claims when possible (R6), to avoid
  holding the very port the requirement waits for.
- A claim is registered **before** the definition is staged, so a reader cannot
  observe a half-installed write. Commit is atomic.
- `read_unit` checks claims before returning a value. It also checks
  `bad_producer`; a general-component producer and a claim can both be present,
  and either wait is valid.
- `commit` validates the trace and requirements under the same lock that
  installs definitions.
- `demand_root` drains deferred writes and resolves pending duplicate/handover
  conflicts at the end.

### 4.5 Determinism, worker-count invariance, and livelock

The result of a demand must not depend on worker count or scheduling:

- claims are first-come under the metadata lock; for **disjoint** ports the
  order is irrelevant;
- for **overlapping** ports the result is always a conflict error, not a winner
  chosen by timing;
- for **read/write** the result is the writer's value: either the reader waits
  (claim visible) or retries (claim late);
- for **conditions**, the result is the condition's true state at commit; a
  late producer is either deferred/drained or fails deterministically.

A write claim is stable until the writer commits or fails, so a reader cannot be
starved by an unbounded series of claims. Two disjoint writers cannot block each
other. A writer waiting for a reader while holding a claim is the only cycle
source; the release-before-wait rule removes the common case, and the wait graph
diagnoses the rest. The retry bound turns any remaining pathological schedule
into a structured error instead of a hang.

### 4.6 Family mapping

| View family | Claim scope | Automatic requirements | Derived ports | Notes |
|---|---|---|---|---|
| scalar field | `Entity` = field value unit | none structural; value requirements explicit | structure unit | one value unit |
| map | `Entity` per key; `Field` for `define_all` | `insert` requires absent, `update` requires present (value condition), `remove` present/absent | key-set structure | per-key source-of-truth units; structure is runtime-maintained |
| list (source) | `Entity` per element; `Field` for structural rebuild | `insert` index in range, `set`/`remove` index present | rank sequence / structure | rank is structural; element identity stable |
| list (derived whole-value) | `Field` | none per element | whole content | no occurrence identity; documented boundary |
| list (derived keyed) | `Entity` per id; `Field` only for a whole-value rebuild | id present for update, absent for insert | assembled sequence is runtime-maintained | disjoint ids do not conflict on the assembled view |
| set | `Entity` per member; `Field` for whole-value | insert/remove idempotent; explicit checked variants | member structure | keyed sets refresh their membership projection from member units |
| graph | `Entity` per node/edge; `Field` for `define_graph` and destructive node removal | edge endpoints present; insert requires edge absent, update requires edge present; node remove is a field/closure claim with a cascade, or a no-incident-edges check | topology, adjacency, incoming adjacency | adjacency is runtime-maintained; a cascade delete uses a field claim so a concurrent edge cannot escape |
| tree | `Entity` per node/field slot; `Field` for whole tree and destructive subtree removal | parent present, child slot empty/present, node present/absent | parent/child projections as applicable | move requires source and destination parents; subtree delete uses a field/closure claim |
| `Reduced` | `Field` | none per element | cached aggregate | whole-value reduction |
| `GeneralComponent` output | `Entity`/`Field` at commit | `OutputTxn::require` / `cx.require` | its own retained state | already one producer per output View; `bad_producer` orders readers |
| source `Edit` | global write gate | editor checks | runtime refreshes | source units are not claimable by components; a derived define into a source View is `DefinitionDenied`; conditions over source Views fail rather than wait |

The key rule: **claim source-of-truth units, never derived projections**. This is
what lets two graph edge writers on the same node both succeed even though both
change the node's adjacency projection, and what lets two keyed-list writers on
disjoint ids both succeed even though both change the assembled sequence.

#### 4.6.1 Public API usage coverage

Every public surface is affected differently; the plan does not introduce a
parallel API for shared writes.

| Surface | What changes |
|---|---|
| `Runtime::install` (generated root) | The root body's `define_*` calls claim and may carry requirements. The root is not registered in `state.producers`; Part A/claims make its reads correct. |
| `Runtime::call` / `ComponentSpec` | Invocation outputs stay shared and cut off independently. Definitions staged inside the invocation claim/require; `call` drains deferred writes and resolves pending conflicts at its boundary as today. |
| `Runtime::read` / `read_outputs` / `read_projection` | Demand roots retry on stale traces, drain deferred writes, and return only a consistent value. |
| `Runtime::edit` / `Edit` | Source writes remain serialized by the write gate. The editor validates structural invariants at commit (graph edge endpoints, tree parents); it does not claim. |
| `Snapshot` / `capture_snapshot` | Only committed content. A pending/deferred write is invisible; a snapshot's versions are the committed ones. |
| `observe` / `observe_units` / `observe_entry` / `observe_routing` | Only committed writes notify. A drain commits before the demand returns; observers see the final state. A wait does not notify. |
| `GeneralComponent` / `TypedGeneralComponent` | It is already a single producer of its output Views. Its `OutputTxn`/`cx` gains `require`; its commit is atomic; readers are ordered by `bad_producer` and claims. |
| `AsyncGeneralComponent` | Same, with the existing parking rule; a condition wait must not hold a worker when a pooled writer can proceed. |
| `LocalGeneralComponent` | Workerless inline; a wait parks the only thread, so a requirement must already be satisfied or fail. The runtime must not choose `Wait` when no other thread can produce; it returns `PreconditionUnmet` instead. |
| `FixedPointProblem` | The worklist runs with per-write claims/requirements; global invariants (convergence, acyclicity) stay with the solver, not the condition layer. |
| `EquiJoin` / `AggSeq` / `Reduced` | Derived aggregates are runtime-maintained; users read them, never claim them. A join's input changes are ordinary invalidation. |
| `Param` / keyed members | Per-member scopes and claims; disjoint members parallel, overlapping members conflict. |
| `Graph` / `Tree` / `Map` / `List` / `Set` handles | The family table and scenario matrix give the per-operation claims/requirements. |
| `collect` / `memory_report` | A node with a live claim or pending group is reachable; collection must not clear it. |
| `plan_route` / `RouteSelector` / `routing_recipe` | The route is the authority for handover; an unplanned claim is `DefinitionDenied`. A requirement can wait for the selected owner. |
| `explain` / `identity_report` / `RuntimeStats` | New counters/fields for claims, requirement waits, deferred writes, conflicts, retries. |

### 4.7 Scenario matrix

The matrix is the contract this plan is judged against. "Wait" means the task
suspends on a live claim; "defer" means the write is held to the end-of-demand
drain; "fail" is a structured error; "skip" is a no-op.

#### Scalars and records

| # | Scenario | Condition / conflict | Resolution | Result |
|---|---|---|---|---|
| 1 | two writers define the same field | exact port conflict | fail `WriteConflict` | deterministic conflict |
| 2 | reader before writer | no claim at read; late write | retry (Part A) | reader sees the writer |
| 3 | writer requires another field's value | value condition; live writer | wait; if no producer, fail | condition holds or actionable error |
| 4 | update-if-present | key present; read old value | skip if absent; claim key | no-op or update |
| 5 | writer requires a whole record's tag | value condition | wait/fail | body never computes on the wrong tag |

#### Map

| # | Scenario | Condition / conflict | Resolution | Result |
|---|---|---|---|---|
| 6 | two writers, disjoint keys | no overlap | parallel / any order | both entries |
| 7 | two writers, same key | exact port conflict | fail `WriteConflict` | deterministic conflict |
| 8 | insert-if-absent | key absent; two inserters | both claim the key; one conflicts | one insert, one conflict |
| 9 | update-if-present vs delete | key present; delete claims key | conflict/retry | one wins, the other retries/fails |
| 10 | whole-field `define_all` vs key writer | field vs entity | fail `WriteConflict` | no accidental last-writer |
| 11 | iteration (`keys`) while keys are added | structure projection | wait for live field writers; retry | complete key set or retry |
| 12 | unique index | entity port + index port absent | claim both atomically; conflict on index | no duplicate |
| 13 | `node_map` from a graph | source changes | ordinary read deps; per-key claims | independent entries |

#### List and set

| # | Scenario | Condition / conflict | Resolution | Result |
|---|---|---|---|---|
| 14 | append twice | structure/field conflict | claim field or tail; conflict | serialized appends or explicit keyed list |
| 15 | insert at `i` | `i <= len` structural | defer/wait for structure writer; claim field/neighbour | valid index or error |
| 16 | set/remove at `i` | `i < len` | wait/retry | valid index or error |
| 17 | two inserts at the same index | field/neighbour conflict | conflict | one order, deterministic |
| 18 | keyed list update | id present; absent for insert | claim entity | per-id independence |
| 19 | whole-value derived list | field claim | no per-element concurrency | documented boundary |
| 20 | set add/remove same member | exact port conflict | fail `WriteConflict` | deterministic |
| 21 | set whole-value vs member | field vs entity | conflict | no partial set |

#### Graph

| # | Scenario | Condition / conflict | Resolution | Result |
|---|---|---|---|---|
| 22 | edge before its nodes | endpoints present; no live node writer | defer, then retry after the node producers; or provider demand | edge appears once nodes exist |
| 23 | edge and nodes in one component | node claims + edge claims | atomic group | all or nothing |
| 24 | delete node with incident edges | cascade | field/closure claim; retract node + all incident edges | snapshot-complete retraction; no edge escapes |
| 25 | delete node only-if-empty | no incident edges | read edges; wait for removers or fail | no dangling references |
| 26 | two edges on the same node | entity ports disjoint; adjacency derived | both succeed; adjacency refreshed | parallel edges |
| 27 | edge update vs delete | same edge port | conflict/retry | one wins |
| 28 | move edge endpoints | old/new endpoints present | claim edge + require nodes | valid move or error |
| 29 | self-loop / parallel edges | edge identity distinct | compatible | graph semantics preserved |
| 30 | node producer absent for a condition | no live/planned provider | `PreconditionUnmet { NoProducer }` | actionable error |

#### Tree

| # | Scenario | Condition / conflict | Resolution | Result |
|---|---|---|---|---|
| 31 | insert child into an occupied slot | slot empty | wait for mover/remover; else fail | no overwrite |
| 32 | insert child under a missing parent | parent present | defer/wait for parent producer | child waits for parent |
| 33 | move subtree into its descendant | destination parent must not be in the moved subtree | fail `PreconditionUnmet { Contradiction }` | cycle rejected |
| 34 | move vs delete of the source parent | source slot claim + node claim | conflict/retry | consistent tree |
| 35 | two moves of the same subtree | same node/slot claims | conflict | one move |
| 36 | replace node payload | node present | ordinary read/claim | payload update |
| 37 | remove node with children | cascade or require empty | field/closure claim; retract subtree / fail | no orphan |
| 37a | two inserts under one parent, different slots | parent's child list/slot structure | claim the parent child scope; conflict or serialize | deterministic sibling order |

#### Cross-cutting

| # | Scenario | Condition / conflict | Resolution | Result |
|---|---|---|---|---|
| 38 | cross-View reference | target port present | read target; wait/retry | referential check or explicit index |
| 39 | fan-in barrier | all inputs present | wait for all live/planned producers; else fail | row emitted only when complete |
| 40 | condition cycle | A needs B, B needs A | `Cycle` | no deadlock |
| 41 | source-edit-only condition | no producer in this revision | `SourceEditRequired` | host edits and re-demands |
| 42 | cancellation during wait | task cancelled | release, discard pending | no publish |
| 43 | panic in a writer | writer fails | release claims, wake waiters | waiters fail |
| 44 | handover satisfies a condition | new provider selected | wait for new owner / routing gen | reader sees new route |
| 45 | condition on a whole structure | many key writers | wait for all live field writers | complete structure or retry |
| 46 | general component output condition | producer node | `bad_producer` demand | value before use |
| 47 | async general component | producer pending | suspend/park as today | no worker deadlock |
| 48 | observer/snapshot during pending | pending not installed | sees previous content | no half state |
| 49 | reclamation with pending claims | pending owner reachable | not collected | claims safe |
| 50 | worker count 1/2/4 | schedule perturbation | same final result | invariant |
| 51 | writer reads a port it is writing | self-dependent condition | fail `PreconditionUnmet { reason: SelfDependent }`; the new value is not visible in the same body | compute locally or use `GeneralComponent` |
| 52 | `define_graph` names an edge whose endpoint is absent | same call, contradiction | fail `PreconditionUnmet { Contradiction }` | no silent edge drop |
| 53 | source editor adds an edge before its nodes | edit validation at commit | edit error | no dangling source edge |
| 54 | deferred owner trace invalidated before drain | producer changed another read | discard group, retry owner | no stale deferred install |
| 55 | condition depends on another demand's deferred write | private queue is not a producer | wait for the task if live, else fail | deterministic, no cross-queue peek |

### 4.8 Cross-cutting considerations

**Snapshots and versions.** A snapshot captures committed content and versions.
A pending/deferred write is not committed and does not appear. A snapshot taken
while a writer is waiting sees the previous value, which is consistent. Drained
writes bump `changed_at`/`routing_at` and invalidate readers normally. Two
snapshots taken at the same revision, one before and one after a drain, carry
different per-View versions; the version is the ordering fact, not the revision
alone. This is the same caveat the runtime already has for several definitions
committing within one revision.

**Observers and subscriptions.** Only commits notify. A deferred drain commits
before the demand returns; the final content is visible to the host, but the
callback refresh follows the same path as any derived commit during a read (the
host's edit/notification path, not a callback inside the drain). A failed drain
commits nothing from the pending group.

**Routing and handover.** A requirement reads a port whose provider may change.
The read records the routing generation; a handover invalidates the read/retry.
A selector-authorized handover is not a conflict; a requirement waiting for a
live provider follows the selected owner.

**Planned routes.** `plan_route` pre-assigns a port to a provider before values
exist. A claim from another provider is `DefinitionDenied`. A requirement can
use the plan as a provider pointer if a `PortProvider` is installed; otherwise it
defers/fails.

**Cancellation.** Waiting tasks use the existing cancellation path. A cancelled
task releases claims and discards pending writes. A drain checks cancellation
before each group; no partial group commits.

**Panics.** A panic in a writer fails its node, releases claims, and wakes
waiters with the failure. A panic during a drain fails the demand and discards
the queue.

**Effects and purity.** Components are pure; effects are rejected. A deferred
write is an effect that is committed later; a component must not report the
effect in its output. This is the same boundary as today's "no println in a
component".

**Reclamation.** Pending claims and pending writes keep their owners reachable.
Collection must not reclaim a node with a live claim or a pending group. A
released/failed group drops its references.

**Parallel operators.** A `map`/`filter` member may write a unit (through a
`GeneralComponent`/operator). Claims are per node; parallel members with
disjoint ports proceed; overlapping ports conflict deterministically. A
condition wait inside a parallel chunk suspends/park according to the existing
rule; the chunk join does not deadlock because the wait graph includes the
owner.

**Cross-demand deferred writes.** A deferred queue is demand-scoped. A
requirement never reads another top-level demand's private pending queue: it
resolves against committed state and live tasks. If the only producer has
deferred its write in another demand, the requirement waits for that task if it
is live, or fails; it does not inspect the other queue. This keeps drain order
local and deterministic. A workload that needs conditional writes across
demands should use one shared root demand or a `PortProvider` that performs the
write in the current demand.

**Fixed points.** A fixed-point region may not satisfy an arbitrary condition
monotonically; conditions are checked per write, and the solver's worklist
retries. Global invariants (acyclicity, convergence) remain the solver's job,
not the condition layer's.

**Async general components.** A pending future holds its worker today; a
condition wait inside it uses the same parking rule. True worker suspension for
async remains future work; conditions do not make it worse.

**Source edits.** `Edit` holds the write gate and is serialized with demands. A
derived writer cannot claim a source unit. A requirement over a source View that
is false fails with `SourceEditRequired`; it does not wait across revisions.
Source editors should validate structural invariants at commit: a graph
`add_edge` whose endpoints are not in the final content, or a tree move into a
non-existent parent, is an edit error, not a dangling write. This is a
validation in the editor, not a claim; the write gate already serializes it.

**Error caching.** A failed node's failure is cached at the revision, as today.
A `WriteConflict`/`PreconditionUnmet` is permanent, so it is safe to cache.
`SourceEditRequired` is cached until the next revision.

**Cost.** Claims are O(1) lookups per define/read plus an O(live claims) conflict
check; requirements add one tracked read per condition; commit validation is
O(trace). Deferred drain is O(pending groups + edges). The plan's cost model
(`E_v`) is unchanged except for the extra trace check; measure it on the scaling
workload and add the fast path if needed. A structural condition is an ordinary
dependency, so a producer that rewrites the port while the condition remains
true still forces a retry; re-evaluating only the predicate at commit (and
skipping the retry when it still holds) is a later optimization, not the
baseline.

---

## 5. Public surface

### 5.1 Errors

```rust
ComputeError::WriteConflict {
    port: String,          // "Board.field0[7]"
    first: String,         // "producer_a at src/main.rs:42:9"
    second: String,        // "producer_b at src/main.rs:57:9"
}

ComputeError::PreconditionUnmet {
    condition: String,     // "graph edge endpoints exist"
    ports: Vec<String>,    // ["Nodes.field0[1]", "Nodes.field0[2]"]
    reason: PreconditionReason, // NoProducer | SourceEditRequired | Contradiction | ValueMismatch | SelfDependent
}

ComputeError::UnstableDemand { retries: u32, ports: Vec<String> }
ComputeError::WriteOutOfScope { port: String }
```

`WriteConflict`, `PreconditionUnmet`, `UnstableDemand`, and `WriteOutOfScope`
are permanent (`is_permanent` true; `SourceEditRequired` is permanent for the
current demand but comes with a "edit this View and re-demand" hint). Keep
`DuplicateDefinition` for two drafts of one unit in one execution and for the
end-of-revision duplicate path; keep `Cycle` for read/write and condition
cycles. Add `Retry` as internal control flow and never let it escape a public
demand.

### 5.2 View family APIs (the transparent layer)

The family methods attach claims and structural requirements automatically:

```rust
// graph: per-edge derived writes (new)
impl GraphRef<N, E> {
    pub fn define_node(&self, node: u64, payload: N); // upsert
    pub fn insert_node(&self, node: u64, payload: N); // require node absent
    pub fn update_node(&self, node: u64, payload: N); // require node present
    pub fn define_edge(&self, edge: u64, from: u64, to: u64, payload: E); // upsert
    pub fn insert_edge(&self, edge: u64, from: u64, to: u64, payload: E); // require edge absent
    pub fn update_edge(&self, edge: u64, payload: E);                      // require edge present
    pub fn define_edge_checked(&self, edge: u64, from: u64, to: u64, payload: E);
    pub fn remove_node(&self, node: u64);            // field/closure claim + cascade
    pub fn remove_node_if_empty(&self, node: u64);   // requires no incident edges
    // validates every edge endpoint against `nodes`; a missing endpoint is
    // PreconditionUnmet { Contradiction }, never a silently dropped edge
    pub fn define_graph(&self, nodes: &[(u64, N)], edges: &[(u64, u64, u64, E)], label: &str);
}

// map: checked variants
impl MapRef<K, V> {
    pub fn insert(&self, key: K, value: V);         // require absent
    pub fn update(&self, key: K, value: V);         // require present
    pub fn update_if_present(&self, key: K, f: impl FnOnce(&V) -> V);
    pub fn remove_if_present(&self, key: K);
}

// list: checked variants
impl ListRef<T> {
    pub fn insert_at(&self, index: usize, value: T); // require index <= len
    pub fn set_at(&self, index: usize, value: T);    // require index < len
    pub fn remove_at(&self, index: usize);           // require index < len
}

// tree: editor checks
// insert_child(parent, slot, child) requires parent present + slot empty
// move(subtree, new_parent, slot) requires both parents + slot empty
```

The unchecked/legacy methods stay for source editors and for users who know the
order; the checked methods are the transparent default for derived writes. The
method names make the requirement visible without making the user manage it.

### 5.3 Explicit requirements

For domain conditions:

```rust
irrigation::require_present(port);
irrigation::require_absent(port);
irrigation::require_value(port, |v| predicate(v));
irrigation::require_all(ports);
```

These are called inside a component body and apply to all definitions staged by
that node. A `require` that cannot be resolved fails the demand with the
condition text and ports. A user who wants a conditional no-op uses an ordinary
tracked read and an `if`; the dynamic branch retracts the write when the
condition becomes false, which is the existing incremental behaviour.

A `PortProvider` can be installed for a View to make missing ports demandable:

```rust
runtime.install_port_provider(<Nodes as ViewSchema>::DESCRIPTOR, Arc::new(NodeProvider));
```

### 5.4 Observation and diagnostics

Add to the observe path:

- `WriteClaimed { node, port, scope }`
- `WriteConflict { first, second, port }`
- `ReaderWaitingForWrite { reader, writer, port }`
- `RequirementWaiting { node, condition, ports }`
- `WriteDeferred { node, port, condition }`
- `DeferredDrained { node, group, ok }`
- `DemandRetried { task, reason }`

`RuntimeStats`/`Explain` gain `write_claims`, `write_conflicts`,
`reader_write_waits`, `requirement_waits`, `deferred_writes`, `demand_retries`,
`unstable_demands`. The board can draw a claim edge, a requirement as a dashed
guard, and a deferred write as a pending edge that becomes solid at drain.

### 5.5 Configuration and compatibility

- `RuntimeConfig::max_retries: u32` (default 32).
- `RuntimeConfig::write_arbitration: Off | Claims | Strict`.
  - `Off` is a one-release migration escape hatch; it must not be the default in
    a release that advertises write consistency.
  - `Claims` is the default: implicit claims, requirements, retries, conflicts.
  - `Strict` additionally requires an explicit `writes`/`requires` declaration
    and rejects an undeclared claim/requirement with `WriteOutOfScope`.
- The semantics change is observable: read-before-write now sees the writer; a
  graph edge waits for its nodes; a missing producer is an error instead of a
  silent drop. Document it in the changelog and the README's honest boundaries.
- `#[component(writes = [...], requires = [...])]` is optional; the generated
  code and family methods work without it. `Strict` uses it for checking.

Existing code changes in these observable ways:

| Existing pattern | Change | Migration |
|---|---|---|
| Reader before a generated writer | now sees the writer; earlier it could see the previous value or fail | this is the bug fix; no opt-in needed |
| Two writers of the same unit, alternating revisions | handover still works; simultaneous live writers now fail earlier | use `RouteSelector` or `plan_route` for intentional handover |
| Graph `define_graph` with an edge whose endpoint is not in `nodes` | now `PreconditionUnmet { Contradiction }` instead of silently dropping | add the node or remove the edge; a `drop_unconnected_edges` compatibility helper can be provided for one release |
| Source `GraphEditor::add_edge` before its nodes | now an edit validation error at commit | add nodes first or use the checked editor |
| Tree move into a missing parent / occupied slot | now an edit validation error | fix the operation or use the unchecked editor |
| `define_all` whole-field write alongside a key writer | now a conflict instead of an order-dependent result | use per-key defines or a dispatcher |
| A component that reports its deferred write in its output | the output commits before the definition; it may be inconsistent | compute the output from inputs; effects are definitions |

A release that turns this on should call out the read-before-write fix and the
new graph/tree validations explicitly. The `Off` mode exists only to unblock a
downstream user for one release; it is not the supported semantics.

---

## 6. Implementation phases

### Phase 0 — correctness prerequisite (no claims)

Files: `crates/irrigation-core/src/frame.rs`, `crates/irrigation-core/src/error.rs`,
`crates/irrigation-runtime/src/state.rs`, `inner.rs`, `demand.rs`, `task.rs`.

Work:

1. Add `validity` to `Dep`; add `suspect_validity`/`verified_validity` to
   `NodeRecord`; allocate stamps in `invalidate`, `commit`, `eval`, `read_unit`,
   `retire_unreachable`, `force_recompute`.
2. Add `trace_is_committable` and call it from `commit` before definitions.
3. Add `Retry` and the demand retry loop with a bound.
4. Add the reproducer as `read_after_deferred_write_sees_the_write`.
5. Audit every clean predicate (`eval`, `read_unit`, `ensure_view_current`,
   `unit_definition_is_current`, `certify_observed`, `read`).

Gates:

```text
read_after_deferred_write_sees_the_write
late_write_forces_reader_retry_before_the_demand_returns
retry_reuses_committed_nodes
retry_does_not_publish_cancelled_work
retry_budget_is_bounded
identical_results_across_1_2_4_workers
all existing gates stay green
```

### Phase 1 — claims, waits, and exact conflicts

Files: `state.rs`, `inner.rs`, `helpers.rs`, `general.rs`, `error.rs`.

Work:

1. `WriteScope`, `WriteClaim`, three indexes, `conflicting_claim`.
2. Claim in `define_unit` and collection define helpers before staging.
3. `read_unit` waits on Active claims; ignores Deferred claims but records deps.
4. Release claims on commit/fail/cancel/retire; wake waiters.
5. `WriteConflict` with both provider descriptions.
6. Claim-many in canonical order; release-before-wait.
7. Counters and observation events.

Gates:

```text
a_reader_waits_for_a_ready_writer
a_failed_writer_fails_its_waiters
disjoint_key_writers_do_not_conflict
same_port_writers_produce_a_write_conflict
entity_and_field_claims_conflict
whole_view_writer_conflicts_with_a_unit_writer
claim_conflict_is_reported_before_definitions_publish
read_write_cycle_is_diagnosed
cancellation_releases_a_claim
a_claim_is_not_left_behind_by_a_panic
```

### Phase 2 — structural requirements and deferred writes

Files: `state.rs`, `inner.rs`, `helpers.rs`, `demand.rs`, `task.rs`.

Work:

1. `WriteRequirement`, `require_present/absent/value/all` primitives.
2. Requirement evaluation as tracked reads; may-change set.
3. Wait for live claims (release-before-wait); provider demand hook.
4. `PendingWrite` queue, end-of-demand drain, topological order, cycle check.
5. Commit-time requirement revalidation; group atomicity.
6. Error variants and `is_permanent`.
7. Graph per-edge/per-node derived writes as the first consumer.

Gates:

```text
graph_edge_waits_for_its_endpoints
graph_edge_before_nodes_is_deferred_then_installed_on_retry
graph_edge_with_missing_nodes_is_precondition_unmet
graph_edge_checked_fails_instead_of_deferring
graph_delete_node_cascades_incident_edges
graph_delete_if_empty_waits_or_fails
graph_two_edges_on_one_node_do_not_conflict_on_adjacency
concurrent_edge_add_during_node_delete_is_caught_by_the_closure_claim
tree_insert_child_requires_parent_and_empty_slot
tree_move_requires_both_parents
tree_move_into_descendant_is_rejected
list_insert_requires_index_in_range
map_insert_requires_absent_key
map_update_requires_present_key
unique_index_claim_is_atomic
condition_cycle_is_diagnosed
source_edit_only_condition_is_reported
deferred_group_commits_atomically
deferred_group_failure_installs_nothing
deferred_group_whose_owner_trace_changed_is_not_installed
```

### Phase 3 — all families and cross-cutting coverage

Work:

- Map/list/set/graph/tree/reduced/derived families: complete the automatic
  requirements and the derived-port classification.
- `GeneralComponent`/`OutputTxn::require`, typed adapters, async.
- `Edit` source checks: the editor may validate structural conditions at commit
  (e.g., no dangling graph edge) without claims; the write gate already
  serializes.
- Cross-View conditions and `PortProvider`; planned routes.
- Snapshots/observers/board projection; `Explain` fields.
- Fixed points/parallel operators interaction tests.

Gates:

```text
every_family_matrix_row_has_a_gate
general_component_requirement_orders_its_readers
async_general_component_requirement_does_not_deadlock
snapshot_during_deferred_write_sees_previous_content
observer_fires_once_after_deferred_drain
routing_handover_satisfies_a_requirement
planned_route_rejects_an_unplanned_claim
parallel_members_with_disjoint_ports_do_not_conflict
fixed_point_region_with_requirement_converges
cross_demand_deferred_queue_is_not_peeked
local_general_component_requirement_does_not_park_the_only_thread
workerless_runtime_requirement_fails_instead_of_parking
```

### Phase 4 — declarations, diagnostics, performance, migration

Work:

- `#[component(writes = [...], requires = [...])]`, `Strict`, `WriteOutOfScope`.
- Observation/board rendering of claims/guards/pending writes.
- Measure commit trace check and claim lookup; add the fast path.
- Changelog/migration; remove `Off` after one release.

Gates:

```text
declared_writes_are_visible_before_the_body_runs
strict_mode_rejects_an_undeclared_write
observer_sees_claim_wait_retry_and_drain_events
commit_trace_validation_does_not_change_execution_counts
shared_board_workload_scales_with_workers
no_retry_on_a_disjoint_writer_workload
```

---

## 7. Alternatives considered

| Alternative | Why not |
|---|---|
| Last-writer-wins | No semantic basis; value depends on task timing. The framework's §2.2 rejects it. |
| One global write lock around every demand | Removes concurrency and still does not order a reader that read before the write; defeats pipelining. |
| Full static read/write sets | Contradicts dynamic dependency discovery (§2.3); computed keys cannot be predicted. |
| One task per port | Explodes task count; node-level single-flight is the right granularity. |
| Bump the revision on every derived definition | Changes snapshots mid-demand; public revision semantics break. |
| Merge function on the View | The request rejects the same position; merge is application meaning and belongs in an explicit combining component. |
| Only retries, no claims | Correct after Phase 0 but wastes a reader's execution and cannot make a running writer "ready" before the read. |
| Only claims, no retries | Fails when a claim is registered after the read (computed keys, sibling order); not sound. |
| Only deferred writes, no waits | A live writer would not be ordered before a reader until the drain; correct but extra executions and late visibility. |
| Blocking on every false condition | Deadlocks the sequential root body when the producer has not been called; needs defer or provider demand. |
| Arbitrary global invariants in the core | Not expressible as local port predicates; use `GeneralComponent`/fixed points/explicit index Views. |

---

## 8. Open decisions

1. **Deferred writes vs failing fast as the default.** Recommendation: structural
   requirements defer; value requirements wait/fail. This makes graph edges and
   tree inserts transparent without lying about value-dependent writes.
2. **Where the requirement is attached.** Node-scoped (all writes of a node) is
   the baseline. Per-definition groups are a later refinement if a component
   stages writes with different conditions.
3. **Provider demand API.** `PortProvider` vs a richer `plan_ports` recipe.
   Recommendation: start with `install_port_provider` + `plan_route`; add
   recipes only when a host needs parameterized providers.
4. **Derived ports in claim lookup.** Baseline: a requirement on a structure unit
   waits for any live field claim. A finer index (structure-to-field claim
   buckets) can reduce over-waiting later.
5. **Graph/tree claim granularity.** Start with per-entity claims + runtime-owned
   adjacency/parent projections; use field claims for whole-value rebuilds. A
   later audit can refine the closure (node + incident edges) per operation.
6. **Retry budget.** 32 is a starting point; measure on the shared-board and
   graph workloads.
7. **`WriteConflict` vs `DuplicateDefinition`.** Keep both: conflict for
   overlapping live claims, duplicate for two drafts in one execution and for the
   end-of-revision handover/duplicate path.
8. **Does a wait on a writer count as a dependency?** The wait is scheduling, not
   an observation; the subsequent read records the unit. Do not add the writer
   node to the reader's trace just for the wait, or traces would change shape
   depending on timing.
9. **Deferred group and node value atomicity.** The value commits before the
   definitions. A component whose output reports its own effect should be
   restructured; document this as the same purity boundary as effects.
10. **Source editor checks.** Source edits are serialized; should the editor
    enforce structural invariants (e.g., no dangling graph edge) at commit? The
    plan recommends yes for source `add_edge`/`move`, as a validation, not a
    claim.

---

## 9. Evidence log

The read-before-write probe in §2.3 was run against the current tree:

```text
cargo test -p irrigation --test zz_probe_shared -- --nocapture
root output = 0
shared      = [(1, 42)]
root output after edit = 1        expected 43
shared after edit      = [(1, 42)]
```

The temporary test file was removed. The disjoint-key probe returned `30` and
`[(1, 10), (2, 20)]`, confirming that disjoint writers compose when the read
happens after both writes.

Existing evidence for the supported parts:

- [`gates.rs`](../crates/irrigation/tests/gates.rs):
  `two_providers_cannot_define_the_same_unit`,
  `provider_handover_survives_demand_order`,
  `two_called_components_cannot_define_one_unit`,
  `concurrent_host_reads_share_one_execution`,
  `parallel_members_share_one_invocation_without_deadlock`,
  `changing_one_output_does_not_change_the_other`.
- [`suspension.rs`](../crates/irrigation/tests/suspension.rs):
  `concurrent_demands_share_an_invocation_across_suspensions`,
  `a_waiting_demand_yields_its_worker_to_queued_work`,
  `a_suspended_demand_can_be_cancelled`.

The design rule is the one already stated in
[`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md)
§2.2: dependency knowledge cannot choose a value, so the runtime keeps one
defining computation per port and diagnoses the rest. This plan adds the missing
half: who observes that computation, in what order, and under which conditions.
