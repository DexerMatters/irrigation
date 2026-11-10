# Scopes as bindings, types as term entries

**Status:** implemented in `crates/irrigation/examples/stlc_scope_graphs`.
**Scope:** the STLC example's name resolver, type checker, and evaluator.
**Related:** [`component-authoring-api-plan.md`](component-authoring-api-plan.md),
[`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md),
[`shared-output-arbitration-plan.md`](shared-output-arbitration-plan.md).

## 0. Decision

The example no longer has a `TypeEnv`, a `binding_type` component, a
`type_scope` component, or a `check_types` stage. The design is:

* `Terms` and `ScopeGraph` are singleton Views. Their handles are zero-sized;
  their accessors are ambient tracked reads. Components take only semantic
  arguments: `check_term(term)`, `eval_term(term, env, depth)`,
  `resolve_term(term)`, `resolve_name()`, `evaluate()`.
* `resolve_name` creates one **binding node** per lambda/let and maps each
  reference term to its binding node in `targets`.
* `check_term(term)` derives a variable's binding type inline: a lambda
  binding's type is the annotation; a let binding's type is
  `check_term(value)`. It writes `types[term]` and `type_errors[term]`.
* The evaluator keeps a dynamic `ValueEnv` keyed by binding nodes; values are
  application-dependent and cannot be derived from syntax. `eval_term(term,
  env, depth)` is the per-`(term, env, depth)` unit; `evaluate()` writes the one
  summary `Evaluation`.
* The scope graph is a name/binding graph. The typing output is two term-keyed
  maps. There are no type nodes, `ty` edges, `dom`/`cod` edges, or shared type
  scopes.

## 1. Why this is the simplest owner model

The framework's `Map` field is a set of independent per-key entity ports. A
`types: Map<u64, Type>` therefore lets every `check_term(term)` own exactly its
own key; different terms are different ports, so per-term invocations write
disjoint units with no arbitration. The same holds for `type_errors`,
`targets`, and `name_errors`.

Shared type nodes would need one owner. A `type_scope(Type)` component was the
smallest such owner, but it adds a component, an invocation, and a writer solely
to dedupe type nodes. The example chooses per-term map entries instead and
accepts that equal types are equal values, not one graph node. The docs state
this trade explicitly.

`resolve_name` is split into:

* `build_scopes()` — one structural stage that writes nodes/edges/bindings;
* `resolve_term(term)` — one invocation per term for targets/shadow notices;
* `resolve_name()` — `build_scopes()` then a plain loop helper that calls
  `resolve_term` for every term.

The graph stage commits before per-term resolution; no ancestor staged write is
read.

## 2. Binding nodes

```rust
pub enum BindingKind { Lambda, Let }

ScopeNode::Binding {
    binder: u64,          // the lambda/let term
    value: Option<u64>,   // the let's bound term; None for lambda
    kind: BindingKind,
}
```

* `binding_node(term)` is the stable binding identity.
* The inner scope gets `Declares { binding, name }` targeting the binding node.
* `targets` maps a reference term to its binding node.
* The payload has no name/annotation/body/type. A rename changes the `D` edge
  name; a type edit changes the annotation map; a body edit changes child
  edges; none invalidates the binding identity.

## 3. Type checking

```rust
#[component(callable)]
pub fn check_term(term: u64) -> Type {
    let ty = match Terms.nodes().get(&term) {
        Some(Term::Var) => match ScopeGraph.targets().get(&term)
            .and_then(|binding| ScopeGraph.graph().node(binding))
        {
            Some(ScopeNode::Binding { kind: BindingKind::Lambda, binder, .. }) => {
                Type::parse(&Terms.annotations().get(&binder).unwrap_or_default())
                    .unwrap_or(Type::Error)
            }
            Some(ScopeNode::Binding { kind: BindingKind::Let, value: Some(value), .. }) => {
                check_term(value)
            }
            _ => Type::Error,
        },
        Some(Term::Lam { body }) => { /* annotation domain + body codomain */ }
        Some(Term::Let { binding, body }) => { let _ = check_term(binding); check_term(body) }
        /* App, If, Int, Bool */
        None => Type::Error,
    };
    ScopeGraph.define_types(term, ty.clone());
    if matches!(ty, Type::Error) {
        ScopeGraph.define_type_errors(term, Line(...));
    } else {
        ScopeGraph.type_errors().remove_if_present(term, "typing_edge");
    }
    ty
}
```

A lambda annotation is parsed in each variable that reads it. That is
intentional per-variable granularity: it avoids a shared binding-type
component, and a type edit reruns exactly the variables that use the annotation.
A let value is checked once through `check_term(value)` and shared by all
references.

## 4. Evaluation

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ValueEnv(BTreeMap<u64, Value>); // binding node -> value

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Value { Int(i64), Bool(bool), Closure { binder: u64, body: u64, env: ValueEnv } }

#[component(callable)]
pub fn eval_term(term: u64, env: ValueEnv, depth: u32) -> EvalOutcome { ... }

#[component(callable)]
pub fn evaluate() -> Evaluation { ... }
```

* `ValueEnv` is dynamic and keyed by binding nodes; it is a semantic argument,
  not a View handle.
* `eval_term` is per `(term, env, depth)`; a literal edit reruns the literal
  path; type/rename edits rerun nothing.
* `depth` is in the invocation key and bounds divergent recursion. A shared
  fuel/`Context` budget is a follow-up.
* `evaluate` is the single writer of the `Evaluation` summary.

## 5. Granularity and cutoff

| Unit | Owner |
|---|---|
| graph nodes/edges, binding nodes | `build_scopes()` |
| `targets[term]`, `name_errors[term or binding]` | `resolve_term(term)` |
| `types[term]`, `type_errors[term]` | `check_term(term)` |
| `EvalOutcome` for `(term, env, depth)` | `eval_term(...)` |
| `Evaluation` summary | `evaluate()` |

| Edit | Resolver | Checker | Evaluator |
|---|---|---|---|
| Lambda annotation | no | lambda + readers | no |
| Let value type | no | value + references | no |
| Rename | yes (D names) | no (targets are binding ids) | no |
| Literal | no | no | literal path |
| Lambda body structural | yes | body terms | body terms |
| Let body structural | yes | let + body terms | let + body terms |
| Let value structural | yes | value + let shape + references | value + environment |

The remaining coarseness is `Terms.nodes`: child identities are part of the
term-shape unit. A let-value structural edit re-runs the let’s
`check_term`/`resolve_term`; the body checks remain cached. Splitting child
identities is a later refinement.

## 6. Consequences

* `ScopeNode::Type` and `ScopeEdge::TypeOf`/`Domain`/`Codomain` are gone;
  `graph.rs` loses `type_node`, `ensure_type`, and `typed_edges`.
* `summary` and `whole_type` read `types[root]`.
* Equal-types-one-scope is replaced by “equal types are equal map values”.
* `resolve_name` has a structural stage plus per-term resolution; a local scope
  change does not wake every reference.
* The name/type error fields are keyed maps; the board draws keyed rows.
* There is no `&Terms`/`&ScopeGraph` parameter anywhere; the root returns the
  View handles for registration.

## 7. Boundaries

* No shared type scopes; a future type-scope renderer would need an explicit
  single owner.
* `ValueEnv`/`depth` appear in evaluator invocation keys; large environments
  should move to a persistent `Context`/`Param` environment.
* No global fuel budget; the depth limit bounds recursion, and `steps` is the
  reported sum.
* Recursive `let rec` is not in STLC; it would need a provisional type or fixed
  point.
* The term-shape unit is not split into per-child units.
