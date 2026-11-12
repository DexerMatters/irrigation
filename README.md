# Irrigation: a concurrent, fine-grained incremental computation system

A Rust workspace implementing the design in
[`plans/concurrent-incremental-system-plan.md`](plans/concurrent-incremental-system-plan.md):
user-defined components generate dependency graphs over typed **Views**, the
runtime records primitive-level observations, and changes repair only the
computations whose inputs actually changed. The authoring surface added by
[`plans/component-authoring-api-plan.md`](plans/component-authoring-api-plan.md)
generates typed field handles, makes root/callable kind explicit, and gives
`GeneralComponent` authors a macro for the boilerplate.

The observable contract is:

> the result at revision `r` equals a deterministic from-scratch evaluation at
> revision `r`, and an unchanged output keeps its change version, so it does not
> notify its consumers.

## Quick start

```rust
use irrigation::prelude::*;

#[view(source)]
pub struct Inputs {
    numbers: List<u32>,
    strict: bool,
}

#[view(derived)]
pub struct Squares(Map<u32, u32>);

#[component(root)]
pub fn analyze(inputs: &Inputs) -> (Squares, u64) {
    let numbers = inputs.numbers();
    let doubled = numbers.map(|n| u64::from(*n) * 2);
    let total = doubled.iter().sum::<u64>();
    let squares = Squares::from(numbers.map(|n| (*n * 2, *n * *n)));
    (squares, total)
}

let runtime = Runtime::new();
let app = runtime.install_root(Analyze)?;

runtime.edit_view::<Inputs>(|inputs| {
    let mut numbers = inputs.numbers();
    numbers.push(1);
    numbers.push(2);
})?;

let snapshot = runtime.read()?;
println!("{:?}", snapshot.view::<Squares>().value());
let (_, total) = app.read()?;
println!("total {total}");

let subscription = runtime.observe_view::<Squares>(|squares| {
    println!("{:?}", squares.value());
});
```

`#[component]` without an argument keeps the legacy rule (a component whose
parameters are all `&View` is a root). New code can say `#[component(root)]` or
`#[component(callable)]` explicitly. `#[irrigation]` is optional: a direct
`#[component]` on a free function behaves the same.

## Architecture

| Crate | Responsibility |
|---|---|
| `irrigation-core` | identities, structured errors, ambient computation frames, type-erased values, collision-safe dynamic keys, change contracts, View descriptors |
| `irrigation-store` | persistent AVL map/sequence/set, graph and tree stores, field storage contracts |
| `irrigation-runtime` | node arena, ordered validation, change cutoff, reverse-edge invalidation, edits, derived regions, snapshots, subscriptions, worker pool, fixed points |
| `irrigation-collections` | source list/map/set with per-entity observations, tracked parameter handles, per-member operators, derived collections |
| `irrigation-macros` | `#[view]`, `#[component(root|callable)]`, optional `#[irrigation]`, `#[tree_view]`, and `#[general_component]` lowering to tracked primitives and typed field handles |
| `irrigation-test` | deterministic generators, oracle trait, execution accounting |
| `irrigation-observe` | bounded event recorder, revision journal, View projection, std HTTP server for the board |
| `irrigation-syntax` | text → lexer → AST pipeline published as ordinary general components (a consumer, not the kernel) |
| `irrigation` | public facade |

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

## Observatory: watching the runtime work

The workspace ships an optional observation path and a page for it. The runtime
emits compact events into a bounded recorder; a projection layer turns those
events and the committed Views into one live board.

```sh
cd web/observatory && npm install && npm run build && cd ../..
cargo run --example observatory -p irrigation --features observatory
# Observatory listening on http://127.0.0.1:7777
```

The board is a single zoomable surface. Every View is a plate, drawn in its own
idiom — a syntax tree as an outline, a token stream or a value map as rows, a
reduction as one figure — and every component is a plate of its own, between the
Views it reads and the Views it defines, in an ink that also colours its wires,
ports and legend entry. The layout is the runtime's own DAG, drawn downward:
ranks from the longest path, one horizontal band per level, and each plate sits
over the mean centre of what feeds it — a one-to-one chain is one vertical line,
and a merge or split reaches it through distributed ports. Every relation is a
single cubic; a long dependency is a long smooth wire, not a detour through
turns. The top-left minimap is a rank diagram of the whole structure, with the
camera and the current changes marked. A plate's grip at its top-left drags it;
a dropped plate is walked clear of its neighbours instead of overlapping them.
Its body scrolls, selects and edits. The board is arranged once, when its plates
have all been measured, and then keeps that plan: a plate holds its slot and its
box while the runtime publishes into it, and a plate arriving later takes a slot
of its own without moving anything. `Overview` folds every plate to its header
so the whole DAG fits the viewport, and opening a detail brings its content
back. `Fit` fills the width, `All` fits the whole pipeline, and `Tidy` lays the
board out again. Reading the page never demands computation.

A source View is a text field all the time: there is no Edit button and no
"open the editor" step. Click a plate's name to open its detail; the side panel
shows the same live content, marks and field as the plate. Click a row with a
unit to read that unit's facts. Type in the field, on the plate or in the
panel, and the change commits as you type. The host commits the text edit,
the framework's lexer and parser components re-run over the delta, the runtime
stages only the units that differ,
and the board says what moved: the plate takes a signal bar, each changed row
takes a mark and a `changed at r<rev>` title, the footer counts the rows
(`3 of 33 rows at r2`) or says `whole value` when the View has no per-row
identity, a component that ran says `ran at r2`, and one that ran without
changing anything says `same content`. Selecting an older revision on the tape
re-annotates the board from the journal. A plate is drawn from its View — shape,
entities, values, and unit versions — never from a host-supplied renderer.

Four hosts ship with the workspace: the JSON pipeline, the smaller syntax
pipeline, the demo, and a simply typed lambda calculus whose name resolver is
the scope-graph resolution calculus. All four install the same framework
components; only the source text is editable, lexing and parsing are components
on the board, and the raw AST is visible. Three of them also declare their typed
tree as a `#[tree_view]` and map the grammar onto it (`@tree`/`@wrap`), so the
board draws that tree with real variant names next to the raw parse. The STLC host's scope graph is a
`Graph` View, so the board paints it as one, and it carries a fourth stage
that lowers the typed tree to a MIPS listing — one View row per instruction,
checked against the evaluator by running the listing in a test simulator. Its notes, the scope-graph
literature it follows, and the incremental experiments it encodes live in
[`crates/irrigation/examples/stlc_scope_graphs/README.md`](crates/irrigation/examples/stlc_scope_graphs/README.md).
The hosts' commands, the page's editing workflow, and the frontend's own notes
live in [`web/observatory/README.md`](web/observatory/README.md).
`--example observe_board` puts ten Views on one board, one per module kind, and
`cargo run --release --example observe_cost` reports what recording costs on the
scaling workload.

Design and delivery notes: [`plans/runtime-visualization-plan.md`](plans/runtime-visualization-plan.md)
and [`plans/runtime-visualization-status.md`](plans/runtime-visualization-status.md).

## What is implemented

| Plan area | Status |
|---|---|
| Revisioned scalar/record units, tracked reads, ordered validation, cutoff | implemented |
| Dynamic branches; inactive branches have no active trace entries | implemented |
| Negative dependencies (`get(k) == None`, `contains(x) == false`) | implemented |
| Unique definitions and dispatcher-free composition | implemented |
| Shared output arbitration: write claims, structural requirements, deferred drains, port providers | implemented, with the boundaries recorded in the plan |
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
| `TreeLayout`: a declared tree described as data (`tree_view` reflection) | implemented |
| Declared ASTs: `@tree`/`@wrap` in the grammar, `ParserComponent::with_tree` publishing a `tree_view` from the parse | implemented (`Grammar::map_tree` reports every mismatch) |
| Generic-tree readers: `rule_id`, `token`, `text`, `value`, `integer`, `boolean`, `semantic`, `range_in` | implemented (`AstMeta.fields` removed: nothing read it) |
| `GeneralComponent` (retained state, change batches, atomic state/output commit) | implemented |
| Incremental equi-join over map Views | implemented (`EquiJoin`, per-key deltas) |
| Balanced lawful reductions (`AggSeq` with cached aggregates) | implemented |
| `Reduced<R>` View field with a declared lawful reduction | implemented |
| Cache reclamation and memory accounting | implemented (`collect`, `memory_report`) |
| Async `GeneralComponent` futures | implemented (`ComponentFuture` + `AsyncAdapter`, driven on the owning worker) |
| Typed field handles | implemented (`Field` constants, typed reads/writes, `if let`, top-level `return`) |
| `#[general_component]` | implemented (stateless function form and trait-impl form) |
| Explicit component kind | implemented (`#[component(root)]` / `#[component(callable)]`) |
| Typed install handles | implemented (`Runtime::install_root` + `Installed`, general root/producer handles) |

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
* **Loops are rejected** inside components. Iteration goes through collection
  operators; the macro reports the unsupported construct instead of silently
  coarsening it. Top-level `return` and `if let` lower to ordinary control flow
  with the same dependency trace.
* **Tracked locals are cloned on use** (`observe_clone`) so a local remains
  usable after an observation. Handles are cheap to clone; large payloads should
  be `Arc`-backed by the View API.
* **Effects inside components are cached with their node.** A `println!`-style
  effect is rejected, but a counter incremented through a static would be
  skipped on reuse. Diagnostics belong in outputs and observers.
* **Cutoff uses the field's `PartialEq`, with no normalization.** Floats keep
  IEEE semantics, so `-0.0 == 0.0` and `NaN != NaN`; a computed `NaN` therefore
  re-executes every revision. Use a newtype for bitwise float equality.

## Testing

```sh
cargo test --workspace          # unit, kernel, macro, and acceptance-gate tests
cargo test --release --workspace # the same suite with optimizations on
cargo run --release --example scaling -p irrigation
cargo run --release --example reduction -p irrigation
cargo run --release --example workloads -p irrigation
cargo run --release --example parallel -p irrigation
cargo run --example name_resolver -p irrigation   # AST tree view -> symbols + diagnostics
cargo test -p irrigation-syntax --test declared_tree   # grammar -> declared tree
cargo test -p irrigation --test tree_layout            # the declaration as data
cargo run --example stlc_scope_graphs -p irrigation --features observatory  # STLC + scope graphs on the page
```

Tests live in each crate's `tests/` directory, never in the module they cover:

- `crates/<crate>/tests/*.rs` are integration tests: they use the crate the way
  a consumer does.
- `crates/<crate>/tests/unit/<module>/<name>.rs` are the unit tests that need the
  module's private items. The module declares them once with `#[path]`
  (`#[cfg(test)] #[path = "../../tests/unit/..."] mod tests;`), so they still run
  inside the library target and keep their `super::` paths, while the source
  file holds no test code.
- Examples carry no tests and no test-only scaffolding. An example demonstrates
  the public surface; it is built, not asserted.

The gates CI holds, and the ones to run before a change lands:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo check -p irrigation-runtime --no-default-features   # observation stays optional
cargo check -p irrigation --all-targets                   # and absent from the facade
cargo build -p irrigation --examples --features observatory
python3 scripts/check-md-links.py                        # every link in docs/plans resolves
```

Measured on this machine (release, one worker, one point edit at the middle of
a mapped list followed by an observer read):

| list length | primitive executions | trace validations | edit + read |
|---|---|---|---|
| 100 | 5 | 0 | 52 µs |
| 1,000 | 5 | 0 | 380 µs |
| 10,000 | 5 | 0 | 5.2 ms |
| 50,000 | 5 | 0 | 34 ms |

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

The example also prints three baselines for the same sum: from-scratch,
whole-function memoization (an edit misses the cache), and a specialized
handwritten incremental running total. At these sizes the baselines are faster:
the edit validates every item node, and summing 10,000 integers is cheaper than
that walk; the handwritten total is the floor, a fraction of a nanosecond per
point edit because it does no validation at all. Incremental machinery pays off
when the per-element computation is expensive (a type checker, a parser, a
graph analysis), which is exactly the tradeoff the plan's cost model predicts.
No speedup is claimed for trivial payloads.

Worker scaling re-evaluates a mapped list whose 1,000 members each burn a fixed
amount of CPU, changing only `RuntimeConfig::workers`:

| workers | median edit + read | speedup |
|---|---|---|
| 1 | 5.4 ms | 1.00 |
| 2 | 3.3 ms | 1.65 |
| 4 | 2.2 ms | 2.40 |

The chunk threads run the member work in parallel; the remainder is the serial
commit path, where every member re-execution takes the metadata lock to publish
its value.

Balanced reduction (`cargo run --release --example reduction -p irrigation
cargo run --release --example workloads -p irrigation`) grows
logarithmically:

| elements | combines for a set | combines for an insert |
|---|---|---|
| 1,000 | 20 | 22 |
| 10,000 | 26 | 28 |
| 100,000 | 34 | 36 |
| 1,000,000 | 40 | 42 | Wall time grows with the member trace the runtime must
validate (`E_v` in the plan's cost model), not with executed user code.

The acceptance gates in `crates/irrigation/tests/gates.rs` cover:

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
* `crates/irrigation/tests/suspension.rs`: a waiting demand yields its worker so a
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

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
