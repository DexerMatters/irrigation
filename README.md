# Reactive: a concurrent, fine-grained incremental computation system

A Rust workspace implementing the design in
[`plans/concurrent-incremental-system-plan.md`](plans/concurrent-incremental-system-plan.md):
user-defined components generate dependency graphs over typed **Views**, the
runtime records primitive-level observations, and changes repair only the
computations whose inputs actually changed.

The observable contract is:

> the result at revision `r` equals a deterministic from-scratch evaluation at
> revision `r`, and an unchanged output keeps its change version, so it does not
> notify its consumers.

## Quick start

```rust
use reactive::{component, reactive, view, Runtime};

#[view(source)]
pub struct Inputs {
    numbers: List<u32>,
    strict: bool,
}

#[view(derived)]
pub struct Squares(Map<u32, u32>);

#[reactive]
mod analysis {
    use super::{Inputs, Squares};

    #[component]
    pub fn analyze(inputs: &Inputs) -> (Squares, u64) {
        let numbers = inputs.numbers();
        let doubled = numbers.map(|n| *n * 2);
        let total = doubled.iter().sum::<u64>();
        let squares = Squares::from(numbers.map(|n| (*n * 2, *n * *n)));
        (squares, total)
    }
}

let runtime = Runtime::new();
runtime.install(analysis::Analyze)?;

runtime.edit(|edit| {
    let mut inputs = edit.view::<Inputs>();
    let mut numbers = inputs.numbers();
    numbers.push(1);
    numbers.push(2);
})?;

let snapshot = runtime.read()?;
let entries = snapshot.view::<Squares>().field0();

let subscription = runtime.observe::<Squares>(|snapshot| {
    println!("{:?}", snapshot.view::<Squares>().field0());
});
```

## Architecture

| Crate | Responsibility |
|---|---|
| `reactive-core` | identities, structured errors, ambient computation frames, type-erased values, collision-safe dynamic keys, change contracts, View descriptors |
| `reactive-store` | persistent AVL map/sequence/set, graph and tree stores, field storage contracts |
| `reactive-runtime` | node arena, ordered validation, change cutoff, reverse-edge invalidation, edits, derived regions, snapshots, subscriptions, worker pool, fixed points |
| `reactive-collections` | source list/map/set with per-entity observations, tracked parameter handles, per-member operators, derived collections |
| `reactive-macros` | `#[view]`, `#[component]`, `#[reactive]` lowering to tracked primitives |
| `reactive-plan` | lowering vocabulary: sites, projections, generators, observations |
| `reactive-test` | deterministic generators, oracle trait, execution accounting |
| `reactive` | public facade |

### How a change flows

1. **Edit.** `runtime.edit(|e| ...)` stages unit writes against the committed
   snapshot. At commit the runtime compares final values; a transaction whose
   final state is equal publishes no revision.
2. **Invalidation.** Changed unit nodes mark their reverse dependents suspect at
   the new revision. A node stays suspect until it is verified *after* that
   invalidation (`suspect_at <= verified_at`).
3. **Demand.** `runtime.read()` becomes a task on a worker. Each node either
   returns a certified value, validates its ordered trace, or re-executes with a
   fresh recorder. A task that needs a node owned by another task suspends
   instead of parking its worker, and is queued again when that node commits.
4. **Cutoff.** An equal result keeps `changed_at`; only changed projections
   notify consumers. Output projections are separate nodes, so output 0 can be
   reused while output 1 changes.
5. **Definitions.** A derived View unit is defined by exactly one provider
   invocation. Definitions commit atomically with the node's value and trace;
   two providers for one unit produce a deterministic diagnostic.

## What is implemented

| Plan area | Status |
|---|---|
| Revisioned scalar/record units, tracked reads, ordered validation, cutoff | implemented |
| Dynamic branches; inactive branches have no active trace entries | implemented |
| Negative dependencies (`get(k) == None`, `contains(x) == false`) | implemented |
| Unique definitions and dispatcher-free composition | implemented |
| Recursive and mutually recursive components with argument interning | implemented |
| Transient parameters (component args and per-member `Param`) | implemented |
| Expression-level lowering: each primitive is a node; unsupported constructs are rejected | implemented |
| Tracked locals (`let` bindings) so consumers depend on producers | implemented |
| List/Map/Set source Views with per-entity membership and payload units | implemented |
| Derived Map (per key), derived List/Set (whole value, documented boundary) | implemented |
| Per-member `map`/`filter`/`flat_map` with identity-based reuse | implemented |
| Parallel worker pool, large stacks, nested-demand inline execution | implemented |
| Suspended demand: a waiting task yields its worker and resumes when its dependency commits | implemented |
| Progress accounting (`RuntimeStats::tasks`: queued, running, waiting) | implemented |
| Parallel list `map` across worker chunks, deterministic order | implemented |
| Single-flight claims, cross-task wait graph, cycle detection | implemented |
| Cancellation: a cancelled demand cannot publish | implemented |
| Snapshots pinned to a revision | implemented |
| Subscriptions that keep observation roots active | implemented |
| Declared fixed points (lattice, monotone transfer, budget, reset deletion) | implemented |
| Deep recursion (10,000 levels) | implemented with large worker stacks |
| Graph View family (topology, payloads, adjacency, node deletion) | implemented |
| Tree View family and `#[tree_view]` enum-group macro | implemented |
| `GeneralComponent` (retained state, change batches, atomic state/output commit) | implemented |
| Incremental equi-join over map Views | implemented (`EquiJoin`, per-key deltas) |
| Balanced lawful reductions (`AggSeq` with cached aggregates) | implemented |
| `Reduced<R>` View field with a declared lawful reduction | implemented |
| Cache reclamation and memory accounting | implemented (`collect`, `memory_report`) |
| Async `GeneralComponent` futures | implemented (`ComponentFuture` + `AsyncAdapter`, driven on the owning worker) |

The pending items are tracked in
[`plans/implementation-status.md`](plans/implementation-status.md).

## Honest boundaries

* **Stack safety comes from worker stack size, not heap frames.** Deep
  recursion is supported to the configured `stack_size` (256 MiB by default,
  `RuntimeConfig`), and `max_depth` turns exhaustion into a structured error.
  A production version should use heap-owned frames or fibers.
* **Derived lists and sets are whole-value definitions.** A producer that
  reconstructs a sequence has no occurrence identity to offer; the plan requires
  that boundary to be explicit. Derived maps are per-key and fine-grained.
* **A suspended demand restarts from its root, not mid-expression.** A compute
  closure is ordinary Rust, so an attempt cannot be interrupted and resumed
  inside its body; the attempt is abandoned and replayed while committed nodes
  are reused. The pool is one shared ready queue, so there are no per-worker
  deques to steal from. Parallel operators additionally use scoped threads per
  chunk, so their closures can borrow the environment.
* **Loops and `return` are rejected** inside components. Iteration goes through
  collection operators; the macro reports the unsupported construct instead of
  silently coarsening it.
* **Tracked locals are cloned on use** (`observe_clone`) so a local remains
  usable after an observation. Handles are cheap to clone; large payloads should
  be `Arc`-backed by the View API.
* **Effects inside components are cached with their node.** A `println!`-style
  effect is rejected, but a counter incremented through a static would be
  skipped on reuse. Diagnostics belong in outputs and observers.

## Testing

```sh
cargo test --workspace          # unit, kernel, macro, and acceptance-gate tests
cargo test --release --workspace # the same suite with optimizations on
cargo run --release --example scaling -p reactive
cargo run --release --example reduction -p reactive
cargo run --release --example workloads -p reactive
```

Measured on this machine (release, one worker, one point edit at the middle of
a mapped list followed by an observer read):

| list length | primitive executions | trace validations | edit + read |
|---|---|---|---|
| 100 | 5 | 0 | 53 µs |
| 1,000 | 5 | 0 | 362 µs |
| 10,000 | 5 | 0 | 4.2 ms |
| 50,000 | 5 | 0 | 31 ms |

Executions are independent of the untouched list length, which is the plan's
first scaling criterion. The workload matrix adds:

| workload | units | executions | revisions |
|---|---|---|---|
| 100 no-op transactions | 100 | 0 | 0 |
| edit an unrelated map key | 1 | 0 | 1 |
| tree leaf replacement | 100 | 8 | 1 |
| tree leaf replacement | 10,000 | 8 | 1 |

A tree leaf edit executes a constant number of primitives; its wall time grows
with the validated item list (`E_v`), not with executed user code.

The example also prints a from-scratch baseline and a whole-function memoization baseline (an edit misses the cache) for the same sum. At these
sizes the baseline is faster: the edit validates every item node, and summing
10,000 integers is cheaper than that walk. Incremental machinery pays off when
the per-element computation is expensive (a type checker, a parser, a graph
analysis), which is exactly the tradeoff the plan's cost model predicts. No
speedup is claimed for trivial payloads.

Balanced reduction (`cargo run --release --example reduction -p reactive
cargo run --release --example workloads -p reactive`) grows
logarithmically:

| elements | combines for a set | combines for an insert |
|---|---|---|
| 1,000 | 20 | 22 |
| 10,000 | 26 | 28 |
| 100,000 | 34 | 36 |
| 1,000,000 | 40 | 42 | Wall time grows with the member trace the runtime must
validate (`E_v` in the plan's cost model), not with executed user code.

The acceptance gates in `crates/reactive/tests/gates.rs` cover:

* 2,000 randomized edit transactions checked against a from-scratch oracle;
* no-op transactions publishing nothing;
* recursion, mutual recursion, and memoized invocation reuse;
* value cycles reported as errors rather than hangs;
* two providers defining one unit rejected deterministically;
* independent output cutoff;
* 10,000-deep recursion;
* identical results across 1, 2, and 4 workers;
* fixed points inside components across edge insertion and deletion;
* graph topology isolation from payload edits, and node deletion retracting edges;
* an equal output still replacing its dependency trace (cutoff is not graph retention);
* a dynamic expansion that routes two providers at one unit being diagnosed deterministically;
* a panic failing one execution scope, releasing its single-flight slot, and
  recovering on the next revision;
* two runtimes staying fully isolated (separate Views, graphs, and revisions);
* `Result` values and `?` flowing through components as tracked data, including
  error-to-success recovery.
* tree reads, identity-preserving replacement and subtree moves, subtree retraction, and leaf edits that do not rebuild the tree;
* missing-key reads as dependencies, with unrelated keys executing nothing;
* general components initialize, update from change batches, honour the default reset policy, and commit state atomically;
* async general components are driven to completion on the worker that owns the node;
* reduced sequence fields expose a cached lawful aggregate through reads and snapshots;
* an incremental equi-join matches a from-scratch join while combining only changed keys;
* cancellation publishing nothing;
* branch reclamation followed by correct repair and reconstruction;
* identical results across repeated schedules with 1–4 workers, eight
  concurrent host reads of a certified revision executing no node bodies,
  32 parallel members sharing one invocation under 2–4 workers without
  deadlock, and six rounds of contended host demand on a two-worker pool;
* `crates/reactive/tests/suspension.rs`: a waiting demand yields its worker so a
  third-party demand runs and releases the blocked computation (the same test
  fails if waits park the worker), cancellation reaches a suspended task, many
  concurrent demands share one invocation across suspensions, and progress
  counters drain when demands complete.

## Internals

[`docs/architecture.md`](docs/architecture.md) explains the node model, revision
protocol, concurrency rules, unit layouts, and lowering contract.

## Reading the plan

The implementation follows the plan's phase order. Where the plan and the code
diverge, the divergence is listed in
[`plans/implementation-status.md`](plans/implementation-status.md) rather than
hidden.
