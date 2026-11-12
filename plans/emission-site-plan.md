# Emission identity: three components that write while they walk

**Status:** implemented. The framework surface is `emission_id`,
`GraphRef::emit_node`/`emit_edge`/`emit_edge_trusted`/`emit_shared`, and the
match/`if let` binding fix in the lowering. The reference host is
`crates/irrigation/examples/stlc_scope_graphs`.
**Evidence:** `crates/irrigation/tests/emission.rs`,
`crates/irrigation/tests/macro_lowering.rs` (the scrutinee-binding gate), and the
example's end-to-end tests.
**Supersedes:** the stage split in
[`term-keyed-typing-plan.md`](term-keyed-typing-plan.md) (elaborate /
`build_scopes` / `check_all` / `evaluate_now`) and the example-update note in
[`shared-view-scope-graph-plan.md`](shared-view-scope-graph-plan.md).

## 1. The problem

The STLC host published its analysis by building whole tables and then writing
them: `elaborate` lowered a `Terms` View, `build_scopes` built a `Draft` graph,
`publish` looped over it, `check_all` looped over types, `evaluate_now` wrote one
summary. Entity identities were authored by the host — `TERM_TAG`, `TYPE_TAG`,
`MASK`, `edge_id`, `type_node`, `child_id` — because `define_node`/`define_edge`
take a `u64`.

The target shape was three components that emit as they traverse, with no
whole-table publication and no host-authored identity:

```text
resolve(term, scope, chain) -> graph nodes/edges (P, D, R, child)
check(term)                 -> type scope + T edge, types/errors
evaluate(term, env, depth)  -> one evaluation row + the summary
```

Three facts blocked it:

1. **No framework-owned entity identity.** A graph write needed a caller-chosen
   `u64`, and the only stable scheme available to the host was a hash.
2. **One provider per unit** (`helpers.rs::define_unit`). A per-term `check`
   cannot own a canonical `Type` node: the second term writing it is a different
   provider, and that is a modelling error, not contention.
3. **A component cannot read what its enclosing invocation staged.** The read
   waits on the ancestor's live claim, so "emit the declaration, then resolve the
   body against it" deadlocks.

The third fact is also why the resolver carries its scope chain as an argument
instead of querying the graph; a derived graph's adjacency is one whole-topology
unit, which a writer would observe and invalidate.

## 2. The framework surface

### 2.1 `emission_id`

Every component body expression is lowered to `eval(SiteId, …)`, and a site is
interned per `(invocation scope, expression site)` (`frame::commit_scope` +
`site_node`). That is the identity an owned emission uses:

```rust
// core/src/ops.rs
pub fn emission_id() -> u64 {
    u64::from(frame::current_or_engine_panic("emission_id").node().0)
}
```

It is stable across re-executions of one invocation, distinct among invocations,
and retired with the site. No caller names it.

### 2.2 Graph verbs

```rust
// collections/src/graph.rs — impl GraphRef
pub fn emit_node(&self, payload: N) -> u64;
pub fn emit_edge(&self, from: u64, to: u64, payload: E) -> u64;          // checked endpoints
pub fn emit_edge_trusted(&self, from: u64, to: u64, payload: E) -> u64; // same write group
pub fn emit_shared<K: Any + Hash + Eq + Send + Sync + Debug>(&self, key: K, payload: N) -> u64;
```

`emit_node`/`emit_edge` are the checked forms: an earlier statement's emission
has committed, so both endpoints exist. `emit_shared` roots one site at
`ScopeId::ROOT` per key and evaluates it through `eval_node`; the first caller
stores the payload, later callers validate the same node, and the node retracts
when the last caller's site leaves its trace. That is the single owner a
canonical type scope needs.

### 2.3 Pattern bindings observe their scrutinee

The lowering previously **shadowed** every name a `match`/`if let` pattern
introduces (`shadow_pattern` → a binding with no node). A site built from such a
name had no dependency at all and froze at its first answer. That is fatal for
"walk and emit": `binding`, `body`, and a projected `value` would be constants
across edits.

The lowering now binds each pattern name to the **scrutinee's node**
(`bind_pattern_to`), which is sound: the name is a projection of the scrutinee's
value, so a read of it must observe that value. Closure parameters keep the
shadow rule (they are per-iteration elements, not the scrutinee), and plain `let`
bindings already tracked their initializer's node.

Gate: `a_match_arm_binding_observes_its_scrutinee_not_the_shadowed_local` — the
arm body's trace is exactly the scrutinee node; changing the scrutinee re-runs
it, changing the shadowed local does not.

## 3. The example

- `terms.rs` and the `Terms` View are gone; `syntax.rs` reads the parser tree
  (rule, child, name, annotation type, literal) per unit.
- `graph.rs` is payloads only (`ScopeNode`, `ScopeEdge`, `TermKind`,
  `BindingKind`); no draft, publish, tag, mask, or hash.
- `resolve`, `check`, `evaluate` are the three stage components; the root emits
  the module scope, runs them in order, and writes the summary.
- `resolve` carries `Vec<Frame>` (scope, name, binder) as an argument; it emits
  term nodes, scope nodes, `P`/`D`/`R`/`child` edges, `terms`, `targets`, and
  name diagnostics as it walks.
- `check` writes `types`/`type_errors`, and per term emits a shared `Type` node
  (via `emit_shared(rendered_type, …)`) plus a `T` edge.
- `evaluate` writes one `Evaluation.outcomes` row per invocation (`emission_id`
  is the key), because a lambda body can run under several environments.

## 4. Consequences

- **Edits that re-key the parse tree re-run the affected path.** The parser's
  node identity is its extent; a token that keeps its occurrence keeps its node,
  so a re-spelling and a rename are payload-only edits (`plans/identity-plan.md`
  closes the token-identity gap this bullet used to describe). A *structural*
  edit still re-keys the constructs whose extent changed; the alternative is
  host-authored positional identity, which this change removes. Warm reads still
  execute nothing, and a structural deletion retracts the removed subtree's
  units.
- **Stage granularity is coarser than the term-keyed plan claimed.** A literal
  edit may re-resolve the containing subtree when the parser re-keys it; the
  example's tests assert the behaviour that actually holds (the new literal is
  evaluated, the edited program is re-checked), not a cutoff the design cannot
  promise.
- **A per-term component can own a canonical node only through
  `emit_shared`.** The one-provider rule is unchanged and documented at
  `define_unit`.
- **`max_invocations` must stay at its default `None`** for per-invocation
  emission: eviction can drop an invocation record whose definitions are still
  live. The default is `None`; raising the cap is a future decision, not a
  silent one.

## 5. Gates

- `crates/irrigation/tests/emission.rs`: one expression owns one entity; a
  re-execution reuses it; different invocations do not collide; an emitted edge
  connects the emitted nodes; equal keys share one node; a shared node retracts
  with its last caller.
- `crates/irrigation/tests/macro_lowering.rs`: the scrutinee-binding gate above,
  plus the existing shadowing gates for `let` and closure parameters.
- `cargo test --example stlc_scope_graphs -p irrigation --features observatory`:
  seed, scoping, shadowing, nodes/edges, one type scope per type, rename,
  literal, annotation, structural retraction, ill-typed/unresolved/parse-error
  cases, warm read, three stage components, and the board Views.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` and
  `cargo test --workspace` are green.
