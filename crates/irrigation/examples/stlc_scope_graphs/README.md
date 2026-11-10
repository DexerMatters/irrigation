# A simply typed lambda calculus, resolved with scope graphs

A worked example of the framework: a small language front end whose name
analysis is scope-graph resolution, whose type checker and evaluator run as
per-term components, and whose whole analysis is observed on the Observatory
board while you type.

```sh
cargo run --example stlc_scope_graphs -p irrigation --features observatory
# Observatory listening on http://127.0.0.1:7801
```

The example follows [`observatory_json`](../observatory_json.rs): the framework's
text, lexer, and parser are components on the board, the raw AST is visible, and
only the source document is editable.

## The language

The simply typed lambda calculus with `Int`, `Bool`, lambdas, application,
`let`, `if`, and literals:

```text
let x = 1 in
let f = \y:Int. x in
let x = true in
f 2
```

The seed proves lexical scoping: the `x` inside `f` resolves to the **first**
`x`, even though a second `x` is declared later. A name-environment walk would
get this wrong; the scope graph follows the parent edge out of the lambda's
scope and stops at the nearest binding. The second `x` is a legal shadowing
binding and is reported as such.

## The pipeline

```text
elaborate     -> Terms
resolve_name  -> ScopeGraph: binding nodes, targets, name errors
check_term    -> ScopeGraph.types / type_errors, one entry per term
evaluate      -> Evaluation, from per-term evaluator invocations
```

Every stage is an ordinary component. `Terms` and `ScopeGraph` are singleton
Views: their handles are zero-sized and their accessors are ambient tracked
reads, so **no component takes `&Terms` or `&ScopeGraph`**. The signatures are
only `elaborate()`, `resolve_name()`, `check_term(term)`,
`eval_term(term, env, depth)`, and `evaluate()`. The root returns the View
handles for registration:

```rust
#[component(root)]
pub fn stlc() -> (Terms, ScopeGraph, Evaluation) {
    let terms = elaborate();
    let scope = resolve_name();
    if let Some(root) = Terms.root() {
        let _ = check_term(root);
    }
    let evaluation = evaluate();
    (terms, scope, evaluation)
}
```

## Bindings as scopes

`resolve_name` has two units of work:

* `build_scopes()` reads `Terms.root/nodes/names` and writes the graph: scopes,
  term nodes, one **binding node** per lambda/let, `P` parent edges,
  `D` declaration edges to binding nodes, `R` reference edges, and `child`
  edges.
* `resolve_term(term)` is a separate invocation per term. A variable follows
  its `R` edge, walks `P` edges, inspects `D` edges, and writes
  `targets[term]` or `name_errors[term]`. A lambda/let checks its outer
  declarations and writes/removes the shadowing notice keyed by its binding
  node.

The binding node carries only `{ binder, value, kind }`. It deliberately has
no name, annotation, body, or type:

* a rename changes the `D` edge name, not the binding payload;
* a type annotation edit does not touch it;
* a lambda/let body edit does not touch it;
* only a let-value structural edit changes `value`.

The resolver never reads `Terms.annotations` or `Terms.literals`, so a type or
literal edit cannot invalidate it.

## Types as term entries

There is **no `TypeEnv`, no `binding_type`, and no `type_scope`**. A term's type
is a map entry:

```rust
pub struct ScopeGraph {
    pub graph: Graph<ScopeNode, ScopeEdge>, // scope/term/binding; P/D/R/child
    pub targets: Map<u64, u64>,             // reference -> binding node
    pub name_errors: Map<u64, Line>,
    pub types: Map<u64, Type>,              // term -> type
    pub type_errors: Map<u64, Line>,
}
```

`check_term(term)` derives a variable's binding type inline:

* a lambda binding's type is the binder's annotation;
* a let binding's type is the type of its value term, obtained by
  `check_term(value)`;
* the result is written to `types[term]` and the error to
  `type_errors[term]`.

Each term owns exactly its key; different check invocations write disjoint map
entries. Equal types are equal `Type` values, not one shared type scope. That is
the deliberate trade for dropping the type-scope owner: shared type nodes would
need a single writer; per-term entries need none.

The let value is checked even when unused. A variable referencing a let reuses
the same `check_term(value)` invocation.

## Evaluation

The evaluator keeps a dynamic value environment because values, unlike types,
are not derivable from syntax: a lambda parameter's value comes from the
application and a closure captures its environment.

```rust
#[component(callable)]
pub fn eval_term(term: u64, env: ValueEnv, depth: u32) -> EvalOutcome { ... }

#[component(callable)]
pub fn evaluate() -> Evaluation { ... }
```

`ValueEnv` is keyed by binding nodes, never names or types. `eval_term` is per
`(term, environment, depth)`; `evaluate` writes the one `Evaluation` summary
(result, kind, steps, diagnostics). `depth` is part of the invocation identity
so divergent recursion is bounded; a shared fuel budget is a follow-up.

## Reactivity

| Edit | Resolver | `check_term` | `eval_term` |
|---|---|---|---|
| Lambda annotation | not run | lambda + variables reading that annotation | not run |
| Let value type | not run | value + references to that let | not run |
| Rename | run (names/D edges) | not run (targets are binding ids) | not run |
| Literal | not run | not run (no literal reads) | literal path |
| Lambda body structural | run | body terms | body terms |
| Let body structural | run | let + body terms | let + body terms |
| Let value structural | run | value + let shape + references | value + environment |

The resolver's per-term split means a local scope change does not rerun every
reference. The type checker's per-term split means a local type change reruns
only the affected path. The evaluator is per `(term, env, depth)`, so a literal
edit reruns the literal and its consumers.

The remaining term-shape unit is `Terms.nodes`: a let-value structural edit
also re-runs the let's `check_term`/`resolve_term`; the body checks stay cached.
Splitting child identities further is a later refinement.

## What the board shows

- **SourceFile** — the only editable plate. Double click, type, and the host
  commits one edit; a parse error names a line and a column and leaves the last
  good tree in place.
- **Text**, **Tokens**, **AstTree**, **AstMeta**, **PipelineStats** — the
  framework pipeline.
- **Terms** — the lowered term table.
- **ScopeGraph** — the binding graph plus the `targets`, `name_errors`,
  `types`, and `type_errors` maps.
- **Evaluation** — `result`, `kind`, `steps`, and stuck diagnostics.

## Tests

The example's end-to-end tests cover:

- the seed resolves and evaluates to `1 : Int`;
- binding nodes/declarations are in the graph and types are term-keyed entries;
- equal types are equal entries, not one node;
- a type annotation edit re-runs `check_term`, not `resolve_name` or `evaluate`;
- a literal edit re-runs only `evaluate`, not the resolver or checker;
- a consistent rename re-runs `resolve_name`, keeps `targets` equal, and does
  not run the checker/evaluator;
- a structural edit retracts stale type entries (`Bool` disappears when the
  program stops using it);
- an ill-typed program reports `type_errors` and still has type entries;
- an unresolved name has no target and a name error;
- a parse error leaves the last good tree;
- every analysis View is on the board.

Run them:

```sh
cargo test --example stlc_scope_graphs -p irrigation --features observatory
cargo test -p irrigation-observe --test shared_scope_graph
```

## Collected pointers

- [Project page](https://pl.ewi.tudelft.nl/research/projects/scope-graphs/) —
  the overview, talks, and paper list these notes are drawn from.
- *A Theory of Name Resolution* (ESOP 2015) — the resolution calculus, edge
  labels, specificity, and well-formedness.
- [*Incremental type-checking for free*](https://doi.org/10.1145/3563303)
  (OOPSLA 2022) — the argument that a graph query with stable identities is
  naturally incremental.
- [*Scope Graphs: The Story so Far*](https://doi.org/10.4230/OASIcs.EVCS.2023.32)
  (2023) — the retrospective and the map of extensions.

## What this is not

- **No shared type scopes.** Types are term-keyed values; equal types are not
  one graph node. The graph is a name/binding graph, and that is the price of
  removing the type-scope owner.
- **No imports or type-dependent resolution.** Only `P` edges take part in
  resolution.
- **No subtyping or polymorphism.** Type equality is structural value equality.
- **Evaluation is untyped.** It can run an ill-typed term and report a stuck
  application; the type maps are the authority on well-typedness.
- **The term table is not a second syntax tree.** It is a semantic,
  identity-stable projection of the parser's tree, and it is not editable.
