# A simply typed lambda calculus, resolved with scope graphs

A worked example of the framework: a small language front end whose name
analysis is scope-graph resolution, whose type checker and evaluator run as
components, and whose whole analysis is observed on the Observatory board while
you type.

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
get this wrong; the resolver follows the binder's scope chain and stops at the
nearest binding. The second `x` is a legal shadowing binding and is reported as
such.

## The pipeline: four components

```text
resolve(term, scope, chain)   -> scope graph: scopes, term/binding nodes, P/D/R/child
check(term)                   -> scope graph: type scopes + T edges, types/errors
evaluate(term, env, depth)    -> Evaluation: one row per invocation + the summary
lower(term, chain, at)        -> Machine: one MIPS row per instruction, in order
```

There is **no elaboration stage and no `Terms` View**: the typed tree is a
`#[tree_view]` declaration, and the parser publishes it from the same records as
the raw AST, so `resolve` walks a `TermRef` directly. The grammar says which
production builds which variant (`@tree(Term::Lam)`) and which child feeds which
field (`body: Term`); a rule that adds no meaning (`Parens`, a parenthesised type)
is declared a wrapper (`@wrap(inner)`), and the tree simply does not contain it.

`syntax.rs` is the whole declaration plus one conversion: the annotation's
`TyRef` to the example's own `Type`. There is no classifier function, no rule-id
comparison, no child index, and no token or literal plumbing.

Every stage emits **while it walks**. There is no draft, no scope builder, and no
whole-table publisher: each emission is its own unit, written at the expression
that performs it. `lower` follows the same rule with rows: an arm writes the
instructions it contributes at the row it was given and recurses into its
children at the offsets it computes, so a rename re-publishes nothing and a
literal edit re-versions one row. The root only composes:

```rust
#[component(root)]
pub fn stlc() -> (ScopeGraph, Evaluation, Machine) {
    /* the shell: .data, .text, main:, the heap pointer */
    let end = match syntax::root() {
        Some(root) => {
            let module = ScopeGraph.graph().emit_node(ScopeNode::Scope { label: "module".into() });
            let _ = resolve(root.clone(), module, Vec::new());
            let _ = check(root.clone());
            let outcome = evaluate(root.clone(), Vec::new(), 0);
            Evaluation.define_result(Line(outcome.text()));
            machine::lower(root, Vec::new(), machine::SHELL)
        }
        None => machine::SHELL + 1,
    };
    /* the exit: print $v0, then halt */
    (ScopeGraph, Evaluation, Machine)
}
```

The two trees are on the board: `AstTree` is the raw parse the parser always
publishes (rule nodes, token leaves, recovery nodes), and `Syntax` is the
declared tree, drawn with the variant names this file declares.

## Identity without keys

The example never names an entity. The framework gives every lowered expression a
computation site, interned per `(invocation scope, site)`, and that site is the
identity of the unit the expression writes:

```rust
let node = graph.emit_node(payload);        // identity: this expression, in this invocation
let edge = graph.emit_edge(from, to, payload);
```

* two invocations are two scopes, so their entities are distinct;
* re-executing one invocation reuses the site, so the entity is reused;
* a site that leaves the trace retracts the units it wrote.

A **shared** entity is interned by its value instead:
`graph.emit_shared(key, payload)` roots one site per key, so every `check`
invocation that derives `Int` writes (or validates) the same type node. The node
has one owner, stays alive while any caller demands it, and retracts when the
last one stops.

## Bindings as scopes, types as shared scopes

`resolve` has no graph read-back. A binder's body is resolved under a `Vec<Frame>`
carried as an argument — the graph is the *witness*, not the query surface. The
reason is the runtime's write rule: reading a declaration that the enclosing
invocation has staged but not committed waits on that invocation's own claim.
Carrying the chain keeps the traversal honest and is why there is no `bind`-style
helper stage.

The binding payload is `{ kind, label }`: a rename changes the `D` edge name and
the label, never a term's identity. A reference writes `targets[term]`, keyed by
the parser's term identity; the later stages read that one map instead of
re-resolving.

`check` derives a variable's type from the binding it resolved to — a lambda
binding's type is its annotation, a `let` binding's is its value's type. There is
no `TypeEnv`. Every distinct rendered type gets one shared `Type` node and a `T`
edge from each typed term, so equal types are one scope and every term keeps its
own `types[term]` entry.

## Evaluation

`evaluate(term, env, depth)` keeps a dynamic environment because values, unlike
types, are not derivable from syntax: a lambda parameter's value comes from the
application and a closure captures its environment. The environment is an
ordinary argument, keyed by the binding term's parser identity. One row per
invocation lands in `Evaluation.outcomes`; the root writes the summary.

## The graph

```text
ScopeNode::Scope { label }                   a lexical scope
ScopeNode::Term  { kind, binding, label }    a term; binders carry their kind
ScopeNode::Type  { label }                   one shared scope per rendered type

P        scope -> enclosing scope
D name   scope -> declared binding term
R name   scope -> referencing variable term
child#N  term  -> field N
T        term  -> its type scope
```

`ScopeGraph` also holds `terms` (term -> node), `targets` (reference -> binding
term), `types` (term -> `Type`), and the name/type diagnostics.

## Reactivity

What the design guarantees:

* a demand at the same revision executes nothing (warm reads are free);
* a re-spelling is a payload change. The token keeps its identity, so every AST
  node whose extent is unchanged keeps its identity, so every component argument
  is unchanged, so the scope graph keeps **every node and edge identity** — the
  literal-edit test asserts exactly that, payloads included;
* a rename is a payload change too, because a frame carries the binder's
  occurrence rather than its name;
* a structural edit re-keys only the constructs whose extent changed, and a
  deleted subtree retracts its nodes, edges, targets, and type rows.

What it does not guarantee, and why:

* **Structural growth re-identifies the constructs whose extent changed.** The
  extent is the only identity that separates nested siblings sharing a boundary
  (`f 2 3` nests an `App` that starts at `f`) without a host-written key.
* **Evaluation rows whose environment changed are new rows.** A row is "this
  term evaluated in this environment", and the environment is part of what is
  being evaluated. The scope graph is untouched by such an edit.

| Edit | Behaviour |
|---|---|
| Literal re-spelling | the graph is unchanged; the literal's row keeps its key and reports the new value |
| Rename | the graph is unchanged; labels and `D`/`R` names update in place; `targets` is unchanged |
| Lambda annotation | that lambda and the terms that read its type are re-checked |
| Structural | the changed extents re-key; untouched subtrees keep their identities |
| No edit | nothing executes |

## What the board shows

- **SourceFile** — the only editable plate.
- **Text**, **Tokens**, **AstTree**, **AstMeta** — the framework pipeline.
- **Syntax** — the declared tree: one `Term` variant per node, with the
  annotation under each lambda and the literal's payload on its leaf. It is
  derived, so it is drawn, not edited.
- **ScopeGraph** — the graph plus `terms`, `targets`, `name_errors`, `types`,
  `type_errors`.
- **Evaluation** — `result`, `kind`, `steps`, diagnostics, and one row per
  `evaluate` invocation.
- **Machine** — the MIPS listing: one row per instruction, in program order,
  ready for SPIM or MARS. A rename leaves every row as it was; a literal edit
  moves one.

## Tests

The example's end-to-end tests cover the seed (`1 : Int`), lexical scoping and
shadowing, term/binding/type nodes, one canonical type scope per type, a rename
that keeps the program resolved, a literal edit that evaluates the new literal,
an annotation edit that re-checks, stale type retraction, ill-typed and
unresolved programs, parse errors, a warm read that executes nothing, the four
stage components, and every analysis View on the board. The lowering is gated
twice: by the rows it writes (a rename changes none, a literal edit changes one,
an annotation edit changes none) and by running the listing in a small simulator
and comparing what it prints with what `evaluate` says.

```sh
cargo test --example stlc_scope_graphs -p irrigation --features observatory
cargo test -p irrigation --test emission
cargo test -p irrigation-syntax --test declared_tree   # grammar -> tree, and the gates
```

The framework gates are in `crates/irrigation/tests/emission.rs`: one expression
owns one entity, a re-execution reuses it, different invocations do not collide,
and a shared node retracts with its last caller.

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

- **No host-authored identity.** An entity is the expression that emitted it, and
  a construct keeps that identity exactly while its occurrence (a token, an
  extent, a binder) survives the edit.
- **No imports or type-dependent resolution.** Only lexical scopes take part.
- **No subtyping or polymorphism.** Type equality is structural value equality.
- **Evaluation is untyped.** It can run an ill-typed term and report a stuck
  application; the type maps are the authority on well-typedness.
- **No graph-query resolution.** Resolution reads the chain, not the graph it is
  writing; a derived graph's adjacency is one whole-topology unit.
- **No surface syntax beyond this language.** The tree declares what the grammar
  can build; a construct the grammar grows and the tree does not declare fails at
  parser construction (`Grammar::map_tree`), not at read time.
- **The backend is a listing, not an assembler.** It targets 32-bit words, has no
  garbage collection (a bump allocator over 64 KiB), and leaves values untagged:
  the type maps are the authority on well-typedness, so the code assumes them. A
  literal that does not fit a word lowers to zero with a comment. The suite runs
  the emitted subset in `sim.rs` and compares the printed word with `evaluate`;
  SPIM and MARS compatibility is by construction, not by a CI step.
- **A listing's identity is its order.** The rows are positions, so an insertion
  re-places what follows it. Per-term blocks plus a placement pass would keep
  code identity independent of layout; nothing here needs it.
