# Component stage outputs: compute and return, do not publish by hand

**Status:** proposal. Design only; no source changes in this file.
**Scope:** the `#[component]` output boundary, the public View-patch surface, and
how `stlc_scope_graphs` would use them.
**Out of scope:** scheduler changes, a new relation language, and automatic
conversion of arbitrary Rust values into Views.
**Related:** [`component-authoring-api-plan.md`](component-authoring-api-plan.md),
[`shared-output-arbitration-plan.md`](shared-output-arbitration-plan.md),
[`shared-view-scope-graph-plan.md`](shared-view-scope-graph-plan.md), and
[`views-components-granularity-audit.md`](views-components-granularity-audit.md).

---

## 0. Recommendation in one page

The example already does the reading and walking correctly. The asymmetry is on
the **write** side: a component body reads Views and then calls `define_*`,
`graph::publish`, `targets().define_all`, and `define_name_errors` directly.
Those writes are the "external help" the question is about.

The smallest general change that removes them is:

1. **A typed `ViewPatch` value.** It is ordinary data describing the units a
   writer wants to publish. It is not a View handle, and constructing it does
   not touch the runtime.
2. **`IntoViewPatch<V>`.** A component's pure result converts into a patch for
   the View it answers. This is where `TermTable -> Terms`,
   `NameAnalysis -> ScopeGraph`, and `Typing -> ScopeGraph` live.
3. **`#[component(publishes = V)]`.** The macro registers `V` as an output View
   and generates the publish step. User code returns a result; the framework
   applies the patch after the body.
4. **`ComponentSpec::publish` / `RootComponent::publish`.** A defaulted trait
   hook. `call_component` and the root runner call it once per body evaluation,
   inside the same computation, so publication is atomic with the body and
   still subject to the existing write arbitration.

The STLC stages then look like this:

```rust
#[component(callable, publishes = Terms)]
pub fn elaborate() -> TermTable { terms::lower_now() }

#[component(callable, publishes = ScopeGraph)]
pub fn resolve_name() -> NameAnalysis {
    let draft = graph::build(/* reads Terms */);
    let (targets, errors) = graph::resolve(&draft);
    NameAnalysis { draft, targets, errors }
}

#[component(callable, publishes = ScopeGraph)]
pub fn check_types() -> Typing {
    let checked = types::check(/* reads Terms + targets */);
    Typing(checked)
}

#[component(callable, publishes = Evaluation)]
pub fn evaluate() -> Evaluated { eval::evaluate(/* reads */) }
```

The bodies only read Views and return local data. The `IntoViewPatch`
implementations are the only place that says how a result becomes units. The
existing per-unit granularity, shared-writer rules, cut-off behavior, and board
visibility do not change.

Do **not** start with a declarative Datalog or a "make every helper call a
component" pass. Both are much larger changes and neither is needed to remove
the explicit writes.

---

## 1. What "smart" can mean here

There are three separable ideas in the question; keeping them separate avoids
solving the wrong one.

| Idea | Already possible? | What is missing |
|---|---|---|
| Walk a tree View inside a component | yes | nothing structural; helpers are ordinary Rust |
| Read another View and track the dependency | yes | nothing; every generated read records the unit |
| Return a result instead of writing the result View | partly | multi-field Views and shared Graphs have no return-value publication path |

The first two are already the normal model. `name_resolver` is the existence
proof:

```rust
#[component]
pub fn unresolved(expr: ast::ExprRef, defined: Vec<String>) -> Vec<String> {
    // recursive component calls over tree refs
    args.iter()
        .flat_map(|arg| unresolved(arg.clone(), defined.clone()))
        .collect()
}
```

That component walks a `Tree` View, recurses, and returns a value. The
`name_resolver` stage also returns `(Symbols, Diagnostics)` instead of calling
`define_*`; the generated `from` constructors do the publication. The input
side is therefore not the gap.

The output side is:

- `#[component]` outputs are component-local values. They are memoized and
  cutoff independently, but they are not View units.
- A View is published by side effect. A body calls `View.define_*`, or a
  generated `View::from(...)` constructor that calls `define_*` underneath.
- Multi-field Views such as `Terms` have no `from` constructor at all; they
  require one `define_*` per field.
- A shared `Graph` View cannot use `ScopeGraph::from(nodes, edges)`: that is
  `define_graph`, which claims the whole field and retracts the other writer's
  units. `resolve_name` and `check_types` must use per-node/per-edge writes,
  which is exactly the machinery leaking into the stage body.

So the useful goal is not "make the component walk better"; it is
**"make the component's result the output, and make publication a framework
concern."**

---

## 2. What the current API already gets right

These pieces should be kept, not replaced:

- **Tracked reads are ambient.** `Terms.nodes()` and `root()` record precisely
  the units the stage read; no manual dependency declaration is needed.
- **Tree references are ordinary component arguments.** `ast::ExprRef` is
  `Hash + Eq + Clone`, so recursive component calls and memoization already
  work.
- **Component outputs are typed and cutoff separately.** A pure result type
  already gets `Clone + PartialEq + Send + Sync` checks and independent
  projections.
- **Root output View registration exists.** `RootComponent::VIEWS` registers
  the Views a root returns; `name_resolver` uses it without a host call.
- **Ordinary component writes are already restated as a whole.** A lowered
  component's body replaces its own previous definition set. A patch publisher
  can rely on this instead of inventing a new ownership model.
- **`GeneralComponent` already has `OutputTxn`.** The runtime can stage typed
  definitions atomically; the missing piece is exposing a value-shaped boundary
  for ordinary components, not a new commit mechanism.

The proposal is additive: `define_*` remains public for hosts, editors,
`GeneralComponent`, and cases where a direct write is genuinely local. The new
path is for ordinary stage components.

---

## 3. Design options

### Option A — helper functions, one publish at the end

Keep the current public API. Push all computation into pure helpers returning a
`TermTable`, `NameAnalysis`, `Typing`, and `Evaluated`, then have each component
call one publish helper.

**Pros:** no framework change; easy migration; improves the example today.
**Cons:** still an explicit publish call, still `define_*` at the boundary. It
does not answer the public-API question.

**Verdict:** a useful first refactor, not the destination.

### Option B — return a View handle

Make every stage return `Terms`, `ScopeGraph`, or `Evaluation`, as
`name_resolver` does for `Symbols`/`Diagnostics`.

**Pros:** familiar; no `define_*` in the body; root View registration already
understands output View types.
**Cons:** a View handle is a unit struct, not the data. The writes already
happened inside `View::from`. Multi-field Views have no constructor. A shared
Graph needs `define_graph`, which is wrong for two writers. This only moves the
side effect into a constructor.

**Verdict:** good for one-field/whole-value Views, insufficient for `Terms` and
`ScopeGraph`.

### Option C — return a typed View patch (recommended)

A patch is local data; the framework publishes it. This is the only option that
combines pure bodies, multi-field outputs, and shared writers without a second
language.

**Pros:** no writes in the body; explicit conversion per View; per-unit
granularity; shared Graph composition; testable without a runtime.
**Cons:** one new trait, generated patch types, new macro attribute, and a
retraction/whole-field policy to settle.

**Verdict:** recommended.

### Option D — declarative relations / Datalog

Describe `Declares`, `Parent`, `Occurs`, and `Resolves` as relations and let the
framework compute a fixed point.

**Pros:** name resolution becomes a query rather than hand-written BFS.
**Cons:** a second language; specificity, shadowing, and diagnostics are not
plain reachability; it hides units and costs; it conflicts with the authoring
plan's "typed view of the existing model" principle.

**Verdict:** possible later for graph queries, not for this problem.

### Option E — recursive component per tree node

Make each term a component invocation that publishes its own fragment.

**Pros:** per-term memoization and narrow invalidation.
**Cons:** aggregation, shared type scopes, and stable term identities become
much harder; a type scope may be written by several term invocations and would
need a separate owner; it multiplies component nodes and makes the "one stage =
one graph writer" story less clear.

**Verdict:** keep stage-level components for the STLC. Revisit only after patch
publication exists and a measured granularity need appears.

---

## 4. The recommended API

### 4.1 `ViewPatch<V>`

A patch is a value with typed fields and a publish method:

```rust
pub trait ViewPatch<V: ViewSchema> {
    fn publish(self);
}
```

`publish` is the only place that names unit keys. It should be callable only
inside a computation, through the runtime's existing output-staging path.
Constructing a patch outside a computation is inert; publishing outside a
computation panics or returns an error, like the existing generated writers.

Generated shape for a multi-field View:

```rust
pub struct TermsPatch {
    nodes: Option<Vec<(u64, Term)>>,
    names: Option<Vec<(u64, String)>>,
    annotations: Option<Vec<(u64, String)>>,
    literals: Option<Vec<(u64, Literal)>>,
    root: Option<u64>,
    diagnostics: Option<Vec<Line>>,
}

impl TermsPatch {
    pub fn nodes<I: IntoIterator<Item = (u64, Term)>>(self, items: I) -> Self;
    // ... one builder per field
}

impl ViewPatch<Terms> for TermsPatch {
    fn publish(self) { /* map entries and list fields */ }
}
```

Notes:

- `Option` distinguishes "this writer does not own this field" from "this
  writer owns it and it is empty". A total stage should set every field it
  owns; omission is a retraction under ordinary-component restatement.
- Map fields publish per entry by default, not with whole-field `define_all`.
  That preserves composability and per-key cutoff. The runtime already
  restates a lowered component's whole output, so entries the stage stopped
  producing are retracted without a whole-field claim.
- Scalar fields publish one unit. List/set fields choose whole-value or keyed
  publication explicitly; the generated patch should not silently choose a
  whole-field write for a View another writer may share.
- Graph fields use a `GraphPatch` with per-node/per-edge entries, not
  `define_graph`:

```rust
pub struct GraphPatch<N, E> {
    nodes: Vec<(u64, N)>,
    edges: Vec<(u64, u64, u64, E)>,
}
```

`GraphPatch::publish` loops `define_node` and `define_edge_trusted`. A separate
`WholeGraphPatch` may call `define_graph` when one writer owns the field. The
default is the composable form.

### 4.2 `IntoViewPatch<V>`

A result type converts into a patch:

```rust
pub trait IntoViewPatch<V: ViewSchema> {
    type Patch: ViewPatch<V>;
    fn into_patch(self) -> Self::Patch;
}
```

Object safety is not needed; the macro can call it generically. Concrete
implementations can be hand-written where the mapping is semantic:

```rust
impl IntoViewPatch<Terms> for TermTable {
    type Patch = TermsPatch;

    fn into_patch(self) -> TermsPatch {
        TermsPatch::default()
            .nodes(self.nodes)
            .names(self.names)
            .annotations(self.annotations)
            .literals(self.literals)
            .root(self.root)
            .diagnostics(self.diagnostics)
    }
}
```

`NameAnalysis` and `Typing` are STLC-specific result structs:

```rust
pub struct NameAnalysis {
    pub graph: graph::Draft,
    pub targets: BTreeMap<u64, u64>,
    pub errors: Vec<String>,
}

pub struct Typing {
    pub types: BTreeMap<u64, Type>,
    pub errors: Vec<String>,
}

impl IntoViewPatch<ScopeGraph> for NameAnalysis { /* nodes/edges + targets + name_errors */ }
impl IntoViewPatch<ScopeGraph> for Typing { /* type nodes/edges + type_errors */ }
```

This is intentionally not automatic field-name matching. `Typing` has no
`graph` field, yet it must produce type scopes and `ty` edges. The semantic
mapping is more honest as an explicit impl, and it is ordinary Rust.

For simple result types, a derive can remove the impl later:

```rust
#[derive(IntoViewPatch)]
#[view(Terms)]
pub struct TermTable { /* same field names/types */ }
```

But the derive should be opt-in and checked against the View declaration; it
must not rewrite a type to make field names match.

### 4.3 `#[component(publishes = V)]`

Add one attribute form:

```rust
#[component(callable, publishes = Terms)]
pub fn elaborate() -> TermTable { /* pure */ }
```

Generated behavior:

1. Validate the return type is usable as `IntoViewPatch<V>`.
2. Add `V::DESCRIPTOR` to the component's output View set.
3. Generate `ComponentSpec::publish` / `RootComponent::publish`:

```rust
fn publish(output: &Self::Output) {
    irrigation::IntoViewPatch::<Terms>::into_patch(output.clone()).publish();
}
```

4. For a tuple return, support `publishes = [A, B]` with positional outputs; one
   View per tuple member. Start with one view and an indexed form only if a
   real consumer needs it.

The root needs a way to register Views its stages publish but it does not
return. Two acceptable spellings:

```rust
#[component(root, views = [Terms, ScopeGraph, Evaluation])]
pub fn stlc() -> u64 { ... }
```

or a builder/host call:

```rust
runtime.register_view(Terms::DESCRIPTOR);
```

The explicit root list is preferred: `install_root` already consumes
`RootComponent::VIEWS`, and it keeps snapshot reads working before the first
demand. Do not make `ensure_view` the only registration path; it is a
commit-time fallback, not an install contract.

### 4.4 Publication timing

The trait hook is better than rewriting the user's body:

```rust
// ComponentSpec, default no-op
fn publish(_output: &Self::Output) {}

// call_component body evaluation
let output = eval_value(..., || {
    let output = S::run(&args);
    S::publish(&output);
    value_of(output)
}, ...);
```

The same hook is called once by the root runner. This has the properties the
stage needs:

- publication happens inside the body's computation, so reads/writes/commit
  remain one transaction;
- a panic during conversion or publication aborts the run and leaves the
  previous content untouched;
- publication happens once per body evaluation, not once per output projection;
- no component body contains a generated publish call, so user code cannot
  accidentally publish twice or in a different order;
- existing components are unaffected by the default no-op.

If the component only publishes and its pure result is large, an opt-out can
avoid retaining it:

```rust
#[component(callable, publishes = Terms, output = ())]
pub fn elaborate() -> TermTable { ... }
```

This should be designed carefully; the first version may simply keep the result
as the output. The clone in `publish(&Output)` is a real cost for a large
`ScopeGraph` draft. If profiling matters, the hook can take `&Output` and the
conversion can borrow; or the component can return a dedicated result type that
is cheap to clone. Do not make `Output` move-only just for this feature.

### 4.5 Shared writers

`ScopeGraph` is the stress case. The patch design must preserve every existing
rule:

- `resolve_name`'s patch contains name nodes/edges, `targets`, and
  `name_errors`; it omits `type_errors`.
- `check_types`' patch contains type nodes/edges and `type_errors`; it omits
  `targets` and `name_errors`.
- Neither patch calls `define_graph`.
- Graph nodes and edges are published per entity. `GraphPatch` uses
  `define_node`/`define_edge_trusted`; edges still claim their own ports.
- Field patches publish per entry. `targets` is not a whole-field
  `define_all`; its entries are independent units.
- Ordinary component restatement retracts units a writer no longer produces,
  and only that writer's units. A `check_types` run must not retract
  `resolve_name`'s name edges.
- A patch field can distinguish "writer does not own this field" from "writer
  owns it and it is empty." This is why `Option` at the patch-field level is
  necessary.

This is not a new arbitration mechanism. It is the existing one with a value
boundary in front of it.

---

## 5. STLC target shape

### 5.1 Pipeline

```rust
#[component(callable, publishes = Terms)]
pub fn elaborate() -> TermTable { terms::lower_now() }

#[component(callable, publishes = ScopeGraph)]
pub fn resolve_name() -> NameAnalysis {
    let root = Terms.root();
    let nodes = Terms.nodes();
    let names = Terms.names();
    let draft = graph::build(root, |id| nodes.get(&id), |id| names.get(&id));
    let (targets, errors) = graph::resolve(&draft);
    NameAnalysis { draft, targets, errors }
}

#[component(callable, publishes = ScopeGraph)]
pub fn check_types() -> Typing {
    let checked = types::check(
        Terms.root(),
        |id| Terms.nodes().get(&id),
        |id| Terms.annotations().get(&id),
        |id| ScopeGraph.targets().get(&id),
    );
    Typing(checked)
}

#[component(callable, publishes = Evaluation)]
pub fn evaluate() -> Evaluated {
    eval::evaluate(
        Terms.root(),
        |id| Terms.nodes().get(&id),
        |id| Terms.literals().get(&id),
        |id| ScopeGraph.targets().get(&id),
    )
}
```

The semantic helpers may still return `graph::Draft` and `BTreeMap`; those are
local values, not published state. The stage body never calls `graph::publish`,
`ScopeGraph::GRAPH.ensure`, `targets().define_all`, or `define_*_errors`.

### 5.2 What remains explicit, and why

- **The View name is explicit** in `publishes = ScopeGraph`. A return type alone
  cannot tell the framework whether `NameAnalysis` answers `ScopeGraph` or a
  future `NameTable`.
- **The conversion is explicit** in `IntoViewPatch`. `Typing -> ScopeGraph` is a
  semantic mapping, not a structural clone.
- **The shared patch mode is explicit** (`GraphPatch` vs `WholeGraphPatch`).
  This is the one place where a wrong default would silently break shared
  writers.
- **The root's View list is explicit** if the root does not return the Views.
  This is installation metadata, not per-run boilerplate.

### 5.3 What the board sees

Exactly the same Views and units as now:

- `Terms`: maps, root, diagnostics;
- `ScopeGraph`: graph, targets, name errors, type errors;
- `Evaluation`: result, kind, steps, diagnostics.

The View version/changed-unit tests remain the authority. A patch publication
that produces the same units must be a no-op, not a revision.

---

## 6. Tree walking and granularity

The patch design does not require recursive components. The recommended split
stays:

- **Stage-level component** for the pipeline boundary. It calls ordinary
  recursive helpers (`lower`, `check`, `evaluate`) and returns one patch.
- **Tree-view component recursion** only where per-node memoization is the
  point, as in `name_resolver::unresolved`. This is already supported and needs
  no new public API.

Why not automatically turn helper calls into components:

- the macro would create one node per call site, not per semantic term;
- it would change the cost and dependency trace without the author asking;
- the authoring plan already documents opaque helper calls as a deliberate
  granularity choice;
- a recursive helper that returns a patch does not need a node per term to
  remove writes.

Optional ergonomics, independent of patches:

- `Tree::visit(root, |node| ...)` / `TreeCursor` for read-only walks;
- `Tree::fold` for local accumulation;
- a `#[component(tree = ...)]` recursive form if the existing explicit calls
  prove too repetitive.

These are conveniences, not the fix for "no external writes."

---

## 7. What about a graph-query API?

Name resolution is graph reachability, not tree recursion. Today
`graph::resolve` is already pure and returns `(targets, diagnostics)`. A future
public `GraphQuery` could make that reusable:

```rust
let resolved = graph.query()
    .parents(ScopeEdge::Parent)
    .declarations(|scope| declarations(scope))
    .nearest(|scope, name| ...);
```

That would remove the BFS loop from the example but would not remove the output
write; it still has to publish `targets` and `name_errors`. It is therefore
orthogonal to this plan.

A full relation/fixpoint API would be a separate research plan. It would need to
represent specificity, ambiguity, shadowing, and diagnostics as first-class
outputs. Do not couple the output-publication work to it.

---

## 8. Implementation phases

### Phase 0 — prove the boundary without new API

- Extract `NameAnalysis` and `Typing` in the example.
- Make each stage body call pure helpers and exactly one publish helper.
- Add a source-level regression check that stage bodies contain no `define_*`
  calls except inside the publish/conversion module.

This is a small example-only refactor and can ship immediately.

### Phase 1 — core patch values and trait

- Add `ViewPatch<V>` / `IntoViewPatch<V>` in the public facade (or collections;
  pick the crate that already owns generated field handles).
- Add `GraphPatch` and `WholeGraphPatch`.
- Implement patches by hand for `Terms`, `ScopeGraph`, and `Evaluation`.
- Add unit tests that patch publication produces the same units as the current
  explicit writers.

### Phase 2 — component publication hook

- Add defaulted `publish` to `ComponentSpec` and `RootComponent`.
- Call it once in `call_component`, `call_component_projection`, and the root
  runner.
- Add `#[component(publishes = V)]` and root `views = [...]`.
- Register output Views at install time.
- Gate the feature behind tests for no-op cutoff, panic atomicity, and
  callable-component use.

### Phase 3 — generated patches

- Generate patch types/builders for scalar, map, list, set, reduced, and graph
  fields.
- Generate `IntoViewPatch` only for structurally identical result types, and
  only behind an opt-in derive.
- Keep hand-written impls for semantic mappings (`Typing -> ScopeGraph`).

### Phase 4 — optional tree helpers

- Add `Tree::visit`/`Tree::fold` if the example still has repetitive walks.
- Do not add automatic component-per-node lowering.

---

## 9. Tests and acceptance

| Gate | Test |
|---|---|
| Unit parity | A patch from `TermTable` publishes the same unit keys/values as the current `define_*` loop. |
| Shared graph | `resolve_name` and `check_types` patches compose into one `ScopeGraph`; neither retracts the other's units. |
| Stale units | A reference that becomes unresolved loses its target; a type scope that disappears is retracted. |
| Incrementality | Type edit skips `resolve_name`; literal edit skips both graph writers; rename re-runs `resolve_name` but not `check_types`/`evaluate`. |
| Granularity | Editing one map entry publishes one entry unit, not a whole-field replacement. |
| No-op | An equal pure result publishes no revision and executes nothing downstream. |
| Atomicity | A panic in conversion/publication leaves the previous View content intact. |
| Registration | Root `views = [...]` makes snapshots readable before/after the first demand; duplicate declarations are harmless. |
| Tree walk | A recursive tree-view component (`name_resolver`) still returns values and publishes through the same boundary. |
| Callable publish | A callable component with `publishes` works when called from a root; its View writes are attributed to the right scope. |
| Existing API | Components without `publishes` behave exactly as today; `GeneralComponent` and direct `define_*` hosts are unchanged. |

The STLC acceptance stays the same as the current example:

```sh
cargo test --example stlc_scope_graphs -p irrigation --features observatory
cargo fmt --all --check
cargo clippy -p irrigation --all-targets --all-features -- -D warnings
```

plus a source-level assertion that the four stage bodies contain no explicit
`define_*`/`graph::publish` calls.

---

## 10. Risks and open questions

1. **Patch API becoming a second DSL.** Keep it data, typed, and generated from
   the View declaration. No control flow, no relation syntax, no implicit field
   matching.
2. **Hiding a whole-field write.** `define_all` on a shared Map or `define_graph`
   on a shared Graph is the failure mode. Default to per-entry/per-entity and
   make whole-field publication an explicit type.
3. **Retraction semantics.** Ordinary components already restate their output;
   confirm that a patch's omitted field/entity is retracted only for that
   writer, never for the other graph writer.
4. **Cloning the output.** `publish(&Output)` clones unless the conversion
   borrows. Start with the simple version; add a move-only publication token
   only if measurements justify it.
5. **Root registration.** Decide between an explicit `views = [...]` list and
   automatic registration from `publishes` on callees. The root macro cannot
   see callees, so explicit is the honest first version.
6. **Callable components.** Restrict `publishes` to root stages initially if
   nested publication ordering is ambiguous. The hook can stay general.
7. **GeneralComponent.** `OutputTxn` already covers custom algorithms. Do not
   force patches onto it in the first phase.
8. **`output = ()` opt-out.** Nice for large results, but it adds a second
   output contract. Defer until a real result type is too expensive to retain.
9. **Derive ergonomics.** `#[derive(IntoViewPatch)]` is convenient but can
   silently map fields by name. Gate it behind an explicit View argument and
   keep semantic mappings hand-written.
10. **Patch identity and cutoff.** A patch is not itself a published value. The
    component output remains the cutoff value; patch units move only when their
    values move. This needs a no-op test, not a new version protocol.

---

## 11. Assumptions

This plan interprets the question as:

> "Can a stage component read Views and walk its input, then return a result,
> with the framework publishing the result's View units, instead of the body
> calling `define_*` and graph write helpers?"

It does **not** assume that intermediate values such as `TermTable`,
`graph::Draft`, `NameAnalysis`, or `Typing` disappear. Those are local values,
not external writes, and they are useful for testing. If the goal is to remove
those too, that is a declarative-query design (Option D), not this plan.

It also does not assume that every helper call becomes an incremental node.
Stage-level components plus ordinary recursion remain the default; tree-view
component recursion stays available where it pays for itself.
