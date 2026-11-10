# A concurrent, fine-grained incremental computation system in Rust

**Technical design and feasibility plan — 31 October 2026**

**Navigation:** [Decision](#1-decision-build-a-demand-driven-graph-of-incremental-programs) · [Evidence](#3-evidence-and-the-design-choices-it-supports) · [Algorithms](#5-view-decomposition-and-delta-algorithms) · [Concurrency](#8-incremental-validation-concurrency-and-pipelines) · [Recursion](#9-recursion-transient-contexts-and-genuine-fixed-points) · [API](#10-proposed-public-api) · [Verification](#13-verification-completed-for-this-plan) · [Implementation gates](#14-implementation-sequence-and-acceptance-gates)

## 1. Decision: build a demand-driven graph of incremental programs

Build a Rust library and a small macro-based language in which **user-defined components generate dependency/computation graphs over decomposable Views**. The runtime executes those graphs, propagates changes, and schedules independent work. A component is not a permanently running actor, nor a function that reruns in its entirety whenever an input changes.

Use three mechanisms behind one public interface:

1. **Demand-driven self-adjusting computation** for field access, branching, recursive calls, and dynamically discovered dependencies.
2. **Specialized delta operators** for lists, maps, sets, indexed joins, and reductions. These avoid rerunning a collection traversal merely because one member changed.
3. **A `GeneralComponent` interface** for user-supplied incremental algorithms, with the same scheduling, dependency, snapshot, and output-publication rules.

Keep logical dependency granularity separate from scheduling granularity: a field or an arithmetic operation can be individually tracked without becoming an operating-system task.

### The contract

Within one runtime:

- Every declared View type identifies one logical View instance.
- Every declared Component type identifies one registered generator. It may generate many recursive invocation records and many computation nodes.
- Input and output interfaces are heterogeneous products, normally expressed as tuples or named records.
- Several components may read the same View or populate different parts of the same View.
- The runtime owns identity, dependency recording, invalidation, memoization, communication, and scheduling.
- The observable result at revision `r` equals a deterministic from-scratch evaluation at revision `r`.
- Standard generated components track primitive reads and operations, not just whole function calls. Unchanged output units do not notify their consumers.
- Collections retain element identity across supported edits. Graphs retain node and edge identity. Tree edits preserve unaffected nodes.
- Recursive and mutually recursive generators are supported; genuine cyclic value dependencies require separately defined fixed-point semantics.

**Feasibility judgment:** the foundation is implementable in stable Rust. The most difficult work is the expression-level macro lowering, identity preservation through structural edits, and a correct parallel validation/publication protocol. The literature supports the individual mechanisms; their proposed integration still requires implementation and measurement.

### What “verified” means here

This plan includes primary-source research, explicit correctness conditions, counterexamples to unsafe designs, complexity analysis, and an executable Rust research model. The model passes eight tests, including 10,000 edited-input transactions checked against a from-scratch oracle. It does not establish production parallel-runtime correctness or performance. Those have concrete acceptance gates in §14.

## 2. Make the ambitious requirements precise

### 2.1 Finest granularity means a defined primitive boundary

For ordinary generated components, promise **primitive-level dependency precision and change cutoff**:

- Reading `map[k].field` observes that entry's presence and that field, not every entry.
- Reading an enum discriminant does not observe all its payloads.
- Reading a graph edge's weight does not observe unrelated edges.
- An inactive branch has no active subscriptions.
- A scalar primitive is reconsidered only after a consumed input changes or its control/identity context changes.
- A result unit that compares equal retains its change version.
- Collection algorithms consume structural deltas rather than re-traversing all existing members.

This is a stronger contract than function-level memoization. It is not a promise of minimum possible work among all algorithms. For example, a change to the first element of a prefix-sum list can change every later result. A change to a widely referenced declaration can genuinely affect most of a program. An arbitrary black-box function has no automatically available efficient derivative.

**Do not silently make unsupported code coarse.** The standard component macro should reject operations it cannot lower with the promised tracking contract. Such code must acquire a supported primitive adapter or be placed in `GeneralComponent`, where the user chooses the internal algorithm and granularity.

“React only when changed” means no unnecessary **body execution or output event** after semantic equality is established. Validation, scheduling bookkeeping, and equality testing can still cost work. No engine can know that a changed input leaves an arbitrary output unchanged without enough evaluation or a valid incremental rule to establish that fact.

### 2.2 Building dependencies removes races only with a definition rule

Consider two definitions of the same scalar unit:

```text
A defines V.x = 1
B defines V.x = 2
```

Knowing both dependency sets does not select a value. Serializing them merely introduces last-writer semantics. The design therefore makes **a unique defining computation per logical output unit** a construction invariant, rather than introducing runtime merging.

Allowed:

- `A` defines `V.left`, while `B` defines `V.right`.
- Two callers demand the same definition; the runtime shares it.
- A dispatcher chooses `A` or `B` based on tracked data.
- `B` consumes the value of `A` and defines the next internal version in an explicit transformation chain.
- An explicit user operation combines results, such as a sum or a set union. This is program meaning, not a conflict-resolution policy.

Ambiguous duplicate definitions are diagnosed at graph construction or dynamic expansion, with the two source locations. They are never resolved by task timing. Thus components can share a View without exposing locks or merge logic, but **arbitrary conflicting definitions cannot be accepted without semantics**.

### 2.3 Dynamic dependencies are discovered, not omnisciently predicted

For `table[compute_key(input)]`, the entry dependency is not known until the key is available. A component generator can know the recipe for discovering the dependency, but not generally its result ahead of evaluation.

Represent this as a staged graph: the key computation precedes an entry-request continuation. Dependency discovery is itself reactive and cached. Separate construction from execution conceptually, while permitting execution to resume construction when a dynamic choice becomes known.

### 2.4 Familiar data structures, with a tracked access boundary

Users should manipulate typed data, not numeric runtime IDs. However, transparent tracking cannot coexist with unrestricted raw references to mutable backing storage. The public contract is ordinary-looking constructors, lookup, iteration, pattern matching, and editing through generated handles. Reads occur in a snapshot or component; mutation occurs in an edit transaction.

No unrestricted `DerefMut`, raw mutable slices, or hidden global state inside a standard component. File contents, configuration, clock values, and other external inputs enter through explicit source Views. This preserves deterministic replay.

## 3. Evidence and the design choices it supports

The following are primary papers or official implementation documentation. The “adopt” column describes this proposal, not claims that those systems already provide the complete requested interface.

| Evidence | Established mechanism | Adopt here / boundary |
|---|---|---|
| [Adapton, PLDI 2014](https://matthewhammer.org/adapton/adapton-pldi2014.pdf), especially §5 | A demanded computation graph, ordered validation, dependency replacement, and reuse across changing demands | Lazy demand plus active subscriptions; avoid eagerly rerunning all descendants |
| [Incremental Computation with Names, OOPSLA 2015](https://research.cs.queensu.ca/home/jana/papers/noma/Hammer15_names.pdf) | Names control reuse; the calculus establishes from-scratch consistency | Hide names behind structural edits and generated call sites; automatic identity matching remains our responsibility |
| [A Theory of Changes for Higher-Order Languages, PLDI 2014](https://inc-lc.github.io/resources/pldi14-ilc-author-final.pdf), §2–3 | Typed change structures and compositional derivatives, parameterized by primitive implementations | Give every supported collection operator a change rule; arbitrary Rust helpers do not automatically inherit efficient derivatives |
| [Efficient Parallel Self-Adjusting Computation, SPAA 2021](https://www.cs.cmu.edu/~blelloch/papers/3409964.3461799.pdf), §3–4 | Read/series/parallel traces; safe parallel propagation for its nested-parallel, single-assignment model | Retain control structure in traces; do not transfer its theorem to our unrestricted dynamic graph |
| [Differential Dataflow, CIDR 2013](https://www.microsoft.com/en-us/research/wp-content/uploads/2013/01/differentialdataflow.pdf), §3–4 | Weighted changes indexed by partially ordered times, including nested iteration | Optional relational/fixed-point execution region, not the representation of every scalar and tree field |
| [DBSP, PVLDB 2023](https://www.vldb.org/pvldb/vol16/p1601-budiu.pdf), §2–4 | Compositional stream incrementalization; algebraic optimization of operators including joins; machine-checked underlying theory | Delta algebra and operator laws; its proofs do not verify this runtime or arbitrary non-group edits |
| [Naiad, SOSP 2013](https://www.microsoft.com/en-us/research/wp-content/uploads/2013/11/naiad_sosp2013.pdf), §2 | Logical timestamps and progress tracking for cyclic, pipelined execution | Explicit completion accounting; use full frontiers only if overlapping epochs/nested streaming justify them |
| [Build Systems à la Carte, ICFP 2018](https://www.microsoft.com/en-us/research/wp-content/uploads/2018/03/build-systems.pdf), §4–5 | Separation of the scheduler from the decision to rebuild; suspension for dynamic dependencies | Separate task scheduling, cache validity, and generator expansion |
| [Rust compiler incremental-query guide](https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation-in-detail.html) | Red/green validation and unchanged-result cutoff; dependency order matters | Use per-unit output versions and ordered control validation |
| [Salsa documentation](https://docs.rs/salsa/latest/salsa/) and [algorithm](https://salsa-rs.github.io/salsa/reference/algorithm.html) | Rust field dependencies, keyed tracked functions, generated handles, and backdating | A practical baseline and API reference; whole-query caching alone is below the required computation granularity |
| [JastAdd circular attributes](https://jastadd.cs.lth.se/web/documentation/reference-manual.php) and [concurrent attributes](https://jastadd.cs.lth.se/web/concurrent.php) | Parameterized tree attributes and explicit circular evaluation; concurrency requires effect discipline | Treat inherited context and synthesized results generically; require convergence conditions for cycles |
| [Rust procedural macros](https://doc.rust-lang.org/stable/reference/procedural-macros.html) | Attribute and function-like macros transform token streams | Generate a typed incremental IR; stable macros are not a whole-program type-aware Rust compiler |

### Why not choose one existing engine wholesale?

**Salsa alone:** an excellent control experiment for query-level incrementality. It does not by itself supply the proposed collection-delta language, arbitrary per-expression restart points, or shared-output routing semantics.

**Differential Dataflow/DBSP alone:** strong for relational collections and iterative dataflow. Translating every borrowed tree projection and contextual recursive call into relation records introduces an unnecessary representation and scheduling burden.

**One actor per component:** keeps the wrong granularity and makes data exchange, ordering, and deadlock user-visible.

**One task per primitive:** preserves granularity but makes scheduling overhead dominate ordinary arithmetic.

**Recommended:** a small typed graph kernel, generated accessors and computation builders, an incremental collection library, and an optional relational execution adapter. Compare against existing engines before replacing their specialized strengths. Rust implementations already exist for [Differential Dataflow](https://github.com/TimelyDataflow/differential-dataflow) and [Feldera's DBSP-based engine](https://github.com/feldera/feldera); implementation feasibility is stronger than a paper-only proposal, but integration cost must still be measured.

## 4. Semantic model: Views, requests, and revisioned values

### 4.1 Singleton means one namespace per runtime

A View declaration is a schema and namespace, not one heap object for every element. For runtime `R`:

```text
view_instance = (R, ViewType)
component_definition = (R, ComponentType)
unit = (view_instance, entity_identity, projection)
invocation = (component_definition, argument_identity)
```

The last three identifiers are private runtime details. A `Syntax` View may contain a million nodes while remaining one instance of `Syntax`. An `Infer` component remains one registered generator while having many invocations for different expressions and contexts.

Two syntactically different declarations with equal payload types are different View types. If a program needs two independent maps with the same schema, it declares two newtypes. Generic instantiations count as distinct concrete types. Multiple runtimes permit isolated projects and tests without process-global singletons.

Transient call results live in an internal singleton result namespace associated with the component, indexed by invocation and output position. Returning an ordinary value does not create a new global View instance per call.

### 4.2 Inputs and outputs are typed products

Conceptually:

```text
C : (V1, V2, ..., Vn ; P1, P2, ..., Pk)
       -> Plan<(W1, W2, ..., Wm ; R1, R2, ..., Rj)>
```

`V/W` are View namespaces or typed projections. `P/R` are transient constructor arguments and results, such as a context, expected value, or local evidence. These transients can differ at each recursive step.

At the public surface, the macro hides `Plan`. Internally, outputs have separately versioned projections: changing tuple position 1 must not invalidate consumers that read only position 0. Named output records receive the same treatment.

### 4.3 Snapshot and change laws

Let `S_r` be the immutable input state of revision `r`, and `F_D` the from-scratch evaluation of the demanded program fragment `D`.

The primary invariant is:

```text
observe(D, r) = F_D(S_r)
```

Each View supplies a valid-edit type `ΔV`, an application operation, and normalization:

```text
apply(v, empty) = v
apply(apply(v, d1), d2) = apply(v, compose(d1, d2))
apply(v, normalize(v, d)) = apply(v, d)
```

Composition is ordered and may be valid only for the appropriate base state. Tree edits and list splices are not assumed commutative or invertible. Normalize at the outer transaction boundary so an edit followed by its reversal produces no observable change when the final state is equal.

For each incremental operator with retained state `s`:

```text
(y, s) = initialize(x)
(dy, s') = update(s, x, dx)
apply(y, dy) = evaluate_from_scratch(apply(x, dx))
```

Heterogeneous products apply these laws componentwise. The design follows typed change structures rather than requiring all data to be subtraction-friendly. The derivative correctness equation is the key interface idea in [the incremental lambda-calculus paper](https://inc-lc.github.io/resources/pldi14-ilc-author-final.pdf).

### 4.4 Three graphs, kept distinct

1. **Data graph:** the user's tree, list, map, or general graph.
2. **Computation graph:** primitive computations and the values they consume.
3. **Control/generation graph:** which branches, iterations, providers, and recursive invocations exist.

A cyclic data graph does not automatically imply a cyclic computation. A program may inspect a graph node without recursively traversing every adjacent node. Conversely, two functions can create a computation cycle over an acyclic tree.

## 5. View decomposition and delta algorithms

### 5.1 Required observation units

| View | Independently observable units | Structural observations / edits |
|---|---|---|
| Scalar | One value; records decompose by field | Replace if unequal |
| List/vector | Element identity, element fields | Ordered membership, positional selection, length, ranges; insert/remove/move/splice |
| Set | Membership of one value | Cardinality, enumeration; insert/remove |
| Map | Presence of key; fields of its value | Key enumeration, length, ranges for ordered maps; insert/remove/modify |
| Graph | Node presence/fields; edge presence/endpoints/fields | Per-node incoming/outgoing adjacency; insert/remove/rewire |
| Tree | Node kind, fields, parent relation, child slots | Ordered child lists; subtree insert/remove/move; variant replacement |

**Negative reads are dependencies.** `map.get(k) == None`, `set.contains(x) == false`, an empty adjacency list, and “no child at position `i`” must become invalid when the corresponding entity appears. Subscribe to key-specific membership cells or indexed structural predicates, not only to existing payload cells.

Store subscriptions sparsely. A map with a billion possible keys need not allocate a cell for each nonexistent key; allocate observation records only for queried absence predicates and reclaim them with their consumers.

### 5.2 Scalar and record implementation

Use immutable value versions. Separate `verified_at` from `changed_at`; equality determines the latter. Use exact equality or trusted canonical identity for correctness. Hashes may accelerate lookup but collisions must not establish semantic equality.

Define scalar equality deliberately: for floating-point values choose documented bitwise or application-specific semantics; ordinary `NaN != NaN` otherwise prevents useful cutoff. The default system must not silently equate observably different values.

Records expose field projections. Returning `(type_value, diagnostic_list)` does not create one indivisible cache cell.

### 5.3 Lists: stable members, reactive order

Back a list with an indexed balanced sequence tree and stable element handles. Maintain separately:

- Element identity and payload.
- Ordered membership and subtree sizes.
- Positional selectors and range observations.

`items[i]` observes which element occupies rank `i`; inserting at the front can legitimately change many positional results. In contrast, `items.iter().map(f)` associates `f` with member identity, so existing mapped bodies survive insertion and movement. An `enumerate()` consumer additionally observes position and can require a changed suffix.

Expected implementation costs with a balanced representation:

- Locate rank / insert one element: `O(log n)` plus payload and allocation.
- Elementwise map after one payload edit: one affected mapping plus `O(log n)` structural/output maintenance where applicable.
- Associative tree reduction: `O(log n)` aggregate recomputations per point edit.
- A non-associative left fold: potentially `O(n)` dependent suffix work.

Do not rebalance using fresh identities for all leaves. Interior balancing nodes are implementation details; preserve leaf identity and carefully maintain observer predicates as tree shapes change.

### 5.4 Maps and sets

Use keyed persistent or revisioned storage with separate membership and payload versions. Route a key update to readers of that key and to affected collection operators. Ordinary hash-table lookup is expected constant time; a persistent ordered map generally costs `O(log n)`.

For a generated `map`, `filter`, or `flat_map`, retain one computation instance per stable input member and a recorded set of produced members. On a change, replace only that instance's outputs, retracting disappeared members. Filter membership is separately reactive from the retained value.

For set-valued transformations where multiple members produce the same value, maintain support counts or provenance inside the **set operator**. Removing one support must not remove a still-supported value. This is set semantics, not a solution for arbitrary competing writers.

### 5.5 Joins and multi-input updates

Maintain indexes on join keys. For a bilinear bag join, using the old `A` and `B`:

```text
Δ(A ⋈ B) = (ΔA ⋈ B) + (A ⋈ ΔB) + (ΔA ⋈ ΔB)
```

The last term matters when both heterogeneous inputs change in the same transaction. An equivalent implementation can join `ΔA` with new `B`, then old `A` with `ΔB`; it must not double-count the cross term. Signed weights represent insertion and retraction for this operator family. See the bilinear rule in [DBSP §3](https://www.vldb.org/pvldb/vol16/p1601-budiu.pdf).

This rule does not authorize arithmetic subtraction on arbitrary tree patches or scalar types. Use the appropriate View delta algebra.

### 5.6 Graphs

Represent edges independently from endpoints, allowing parallel edges and self-loops. Maintain indexed incoming and outgoing adjacency. Node deletion retracts incident edges according to a documented API rule; that work is at least proportional to the affected degree.

`neighbors(v)` observes that adjacency collection; `edge.weight()` observes one payload projection. Changing a weight should not invalidate a consumer that reads only topology.

Connectivity, dominators, reachability, or arbitrary graph folds need algorithms, not just a graph container. Standard dataflow operators can express some of them. Specialized dynamic algorithms belong in `GeneralComponent`; genuine recursive analyses may use the fixed-point facility in §9.

### 5.7 Trees and enum groups

Use a module-level macro so the complete mutually recursive type group is visible:

```rust
#[tree_view(root = Document, view = Syntax)]
mod syntax {
    pub enum Document {
        Module { items: Vec<Expr> },
    }
    pub enum Expr {
        Literal(Literal),
        Name(String),
        Call { callee: Box<Expr>, args: Vec<Expr> },
        Let { name: String, value: Box<Expr>, body: Box<Expr> },
    }
    pub enum Literal {
        Int(i64),
        Text(String),
    }
}
```

This is proposed API syntax, not an available crate. The macro generates:

- Owned construction values and typed read/edit handles.
- Storage for all enum variants in one `Syntax` namespace.
- A separate discriminant cell and field/child projections.
- Tracked `Vec`, `Box`, and `Option` structural adapters.
- Pattern-match projections that do not eagerly read every payload field.
- Parent/child relations, transactions, delta types, and identity-preserving edits.
- Validation of root reachability and the ownership tree invariant.

Cross-references use a generated typed reference wrapper explicitly distinguished from owned children. Shared ownership or cycles through owned child fields are rejected in tree mode; use graph mode for those relations. Nonrecursive leaf payloads need supported scalar/record adapters. External generic containers require registered structural adapters, rather than guessed layouts.

Moving a subtree preserves its node identities but changes its parent/position observations. Replacing a variant changes the tag and invalidates removed projections; unchanged external nodes remain reusable.

If a producer reconstructs an entire unrelated owned tree, the runtime cannot infer perfect historical correspondence, especially for duplicate-looking nodes. Prefer edit-preserving constructors. An importer may perform structural reconciliation, but must report its cost and ambiguity; content equality alone is not occurrence identity. This is particularly important at the boundary to a custom incremental parser.

## 6. Identity and shared-output definitions without user keys

### 6.1 Identity is runtime-owned, not absent

Use four internal identity sources:

1. Persistent source entities receive opaque generational identities when created.
2. Recursive calls use the component identity and canonical arguments.
3. Generated intermediate nodes use invocation identity, macro-assigned call site, and enclosing stable member identities.
4. Immutable value parameters use equality-checked interning where profitable.

Never use an element's numeric position as its identity in identity-preserving traversal. Never treat a source-code byte offset as a permanent AST identity. Never use only a hash as a correctness key.

Macro call-site IDs are stable within the built program. Changing the binary invalidates the in-memory plan; durable caching across different binaries is a separate, later design.

Users see `ExprRef`, map keys that belong to their own problem, and ordinary values. They do not allocate cache keys, choose node IDs, or call invalidation functions. Domain keys such as a variable's name remain legitimate data, distinct from internal identity management.

### 6.2 Resolve an output definition by demand

A derived View is a typed namespace of definitions. For a request such as `Types.get(expr)`, the route is:

```text
request output projection
  -> validate routing decision
  -> identify its unique provider invocation
  -> expand that generator as needed
  -> demand the provider's result projection
```

Provider registration includes its output schema and routing rule. Statically disjoint fields and distinct enum-pattern cases can be checked during composition. Unrestricted computed key ranges cannot generally be proved disjoint at compile time.

For dynamic routing, require one **owner dispatcher** for that region. It chooses the provider invocation using tracked computations, issues private definition capabilities, and records the routing dependency. Provider identity includes the invocation arguments, not merely the Component type. Independent children may fill disjoint units with those capabilities. Two same-provider demands share work; a second unrelated definition is an error.

For output enumeration, the region also has a membership generator. This makes “which keys exist?” a real incremental computation, not an assumption that every possible key is populated.

### 6.3 Publish routing separately from completing values

A **routing generation** is an immutable mapping recipe for one region at one input revision. It selects provider invocations, including their transient arguments. Publishing this recipe does not eagerly compute every value in the region.

When a dispatcher switches a region from provider `A` to `B`:

1. Resolve the selector and validate the route's uniqueness at the requested revision.
2. Atomically install the new routing generation and its lazy definition recipes.
3. Demand each requested unit through that generation; commit its value and dependency trace together when ready.
4. Complete a tuple observer only when all requested projections are certified for its revision and routing generations. Complete a collection observer only after membership and its requested entries are resolved; the recipe alone does not establish membership.
5. Retire old routes and subscriptions after older snapshots release them.

The dispatcher is the sole authority for key-to-provider selection. A lazy provider cannot opportunistically claim other keys. Every presence/value read validates and pins its routing generation, including a missing-key read or an equal-valued provider replacement. A value can retain `changed_at` after equality comparison while its routing/dependency certificate changes.

If `V.y` reads `V.x` under the new route, `V.x` can commit before `V.y` finishes; there is no artificial whole-region completion barrier. An observer requesting both waits for both. A stateful `GeneralComponent` has a different atomicity boundary: its new state and output patch commit together (§10.5).

Even if both providers produce equal values, the new dependency/control graph must replace the old one. Equality suppresses value notifications, not dependency maintenance.

A whole-View writer overlaps every subunit writer. It cannot coexist with arbitrary independent patch writers. Decompose the definition, use a dispatcher, or express an explicit sequence of transformations. This restriction is what makes merge/conflict-resolution algorithms unnecessary.

## 7. A component compiles to an incremental program

### 7.1 Generated intermediate representation

The minimum useful IR contains:

```text
Constant / ParameterProjection
ReadField / ReadMembership / SelectRank / ReadAdjacency
PurePrimitive
Product / Project
Switch / MatchTag
Call / RecursiveCall
MapMembers / FilterMembers / FlatMapMembers
ReduceTree / FoldSequence / IndexedJoin
DefineProjection / DefineMembership
GeneralOperator
```

Retain data dependencies and control structure. Sequential dependencies are not replaced by an unordered bag of reads. Independence appears as parallel child regions whose inputs are available separately.

`Switch` has a condition and a selected-region factory. It must not build/evaluate both branches eagerly. `MapMembers` associates child generators with stable members. `Call` interns argument identity and returns separately demandable result projections.

### 7.2 Lowering the function body

For the supported Rust-shaped subset:

- `let`, scalar expressions, and field projections become typed primitive nodes.
- `if`/`match` become guarded continuations.
- Tuple and record construction become independently tracked projections.
- Calls to component functions become graph requests, not immediate recursive host-stack execution.
- Supported collection iteration becomes a per-member graph plus structural operators.
- A loop-carried accumulator becomes an explicit dependency chain unless a declared lawful reduction permits balancing.
- `?` and `Result` become value/control paths, with errors tracked like other values.

This is a DSL embedded in Rust, not a magical annotation on unrestricted Rust. Generate typed Rust builder calls and let the Rust compiler infer and check their types. A stable procedural macro sees syntax, not arbitrary external function bodies or the compiler's fully typed IR. That boundary follows from the [Rust macro interface](https://doc.rust-lang.org/stable/reference/procedural-macros.html).

Group recursive component declarations in an annotated module for initial implementation. Export generated builder descriptors for cross-module calls. Supported primitive adapters declare deterministic behavior, argument/output decomposition, and delta semantics. Reject opaque calls, arbitrary I/O, hidden mutation, unsupported reference escapes, and unlowered iterator adapters in standard components.

### 7.3 Construction must itself be incremental

Do not rerun the entire outer builder to repair one leaf. Cache each control-region expansion. A changed condition repairs the selected branch; a collection insertion constructs one member subgraph; a recursive result change resumes only consuming continuation nodes.

Construction-time reads go through the same tracked read mechanism as ordinary computations. Otherwise a cached graph can retain a stale branch or provider route even when all visible result nodes appear valid.

### 7.4 Fine logical nodes, economical physical execution

Represent logical nodes compactly in per-invocation arenas. Queue regions or ready batches, not every addition. Within a scheduled region, a bitmap/worklist identifies the actual stale primitives; valid primitives are skipped.

Fusion is permitted only when the implementation preserves the promised per-primitive invalidation and change cutoff. “Fuse, then rerun the whole function” is a coarsening and fails the contract. Share common projections and repeated identical requests. Avoid allocating a dependency object for constants or immutable values with no changing inputs.

## 8. Incremental validation, concurrency, and pipelines

### 8.1 Revision protocol

Start with **one mutable input transaction at a time and one active derived-computation revision**, while allowing many workers inside that revision. Readers may retain completed immutable snapshots. New edits can queue, or explicitly supersede an interactive request; they do not mutate a running snapshot.

This already supports parallel components and pipelines within a revision. Overlapping active revisions are a later optimization, not a prerequisite for useful concurrency.

Transaction sequence:

1. Collect edits against a known source snapshot.
2. Normalize edits and compare final values at touched units.
3. Publish a new immutable source snapshot and its changed-unit set.
4. Mark affected dependency certificates suspect; enqueue observed roots, not every affected body.
5. Demand and repair required computations against that snapshot.
6. Publish observer results only when their demanded scope is complete.

A manual `get` demands a result lazily. A subscription keeps an observation root active, so changes eventually trigger a fresh result without the user manually requesting recomputation.

### 8.2 Per-node state

Each memoized primitive or region records:

```text
value/output projections
changed_at[projection]
verified_at
ordered/structured dependency trace
active control-region children
state: Vacant | Checking | Running | Ready | Failed | Cancelled
owner task and generation
waiters
```

A dependency records the observed projection version, not only the provider's aggregate version. A provider can be re-executed while one of its projections remains unchanged.

### 8.3 Demand algorithm

Conceptual sequential specification; the parallel implementation must refine it:

```text
demand(node, revision):
    if node is a source: return value at revision
    if already verified at revision: return cached projection
    claim single-flight ownership for (node, revision)
    if another task owns it: register waiter and suspend

    if a prior trace exists:
        validate control prerequisites and reads in their dependency order
        at the first changed controlling/input observation:
            stop validating obsolete continuations
            repair the affected generated region
        if every necessary observation is unchanged:
            mark verified; complete waiters; return cached result

    evaluate/repair using a fresh dependency recorder
    stage result projections, dependency replacements, and output definitions
    compare changed projections with previous values
    atomically commit the staged node/scope record
    retain old changed_at for equal projections
    notify consumers of changed projections only
    complete waiters
```

For a fine-grained region, “repair” means rerun affected primitive nodes, not the whole user function. The accompanying research model tests a coarser scalar version of this validation rule.

**Critical branch case:** the old trace is `read condition; read old child`. If an edit selects a different child and deletes the old child, validating the old child before the condition can access invalid data. Validate control first and stop at the first invalidating observation. Ordered validation is specifically discussed in [the rustc query documentation](https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation.html).

### 8.4 Invalidation strategy and its actual cost

Use reverse subscriptions for active demand regions to mark reachable certificates suspect, with deduplication per revision. This can cost the size of the old reachable dependency cone even when output equality later stops execution. Record that cost separately from recomputation.

For inactive caches, either retain invalidation coverage or force revision validation when they are demanded again. A node with detached reverse edges must never be assumed clean merely because no notification arrived.

Later optimization: hierarchical change summaries and subscription indexes can skip unaffected trace regions. False positives are allowed in such summaries; false negatives are not. Do not claim `O(changed outputs)` total cost when validation still walks many edges.

### 8.5 Parallel scheduling without worker starvation

Use a worker pool with work stealing and explicit suspended continuations. A task waiting for another task yields its worker; it does not block a pool thread on a synchronous recursive call.

Single-flight ownership is per `(invocation or primitive, revision)`. Claiming, installing a waiter, and checking completion form one synchronized state transition, preventing lost wakeups. Execute user code outside metadata locks.

Within a region:

- Spawn independent children after their guards and inputs are established.
- Preserve sequential edges and parent/child ownership.
- Use stable join order for order-sensitive operators.
- Let downstream regions consume a completed upstream unit while unrelated upstream units are still running.

Never evaluate every old dependency in parallel just because it appears in the cache. Some belong to branches that will no longer exist.

### 8.6 Glitch-free publication

A downstream computation consumes values certified for the same input revision. It may reuse an unchanged old value only after validation certifies it for the new revision.

For the diamond `x -> a, b -> c`, `c` must not combine new `a` with unvalidated old `b`. Resolve both dependencies at revision `r`; unchanged output backdating does not waive revision validation.

Each computation unit publishes its output projections and dependency trace through an immutable commit record. Different units under a validated routing generation may commit independently, permitting pipelines. An observer requesting a whole tuple or collection waits for its complete demand scope at one revision; it never receives a partially completed mixture. A per-entry observer can complete sooner. A custom stateful operator publishes all changes associated with one state transition atomically, as specified in §10.5.

### 8.7 Progress, failure, and cancellation

For the first implementation, completion is scoped to a demand token:

- Count queued/running tasks, suspended tasks, dynamic expansion permits, and in-flight completion messages.
- Register child work before retiring the parent's permission to create it.
- A task waiting on a dependency remains unfinished even if every work queue is empty.
- A demand completes only after all its requested projections are ready and no outstanding expansion can add required work.

A cancelled generation cannot publish. Stage changes privately; check the generation at commit; release waiters with cancellation or failure. Deterministic domain errors are cacheable result values. A panic aborts its execution scope and cannot leave a partial output or permanently occupied single-flight slot.

If overlapping revisions or nested streaming loops are introduced, use timestamped progress/frontiers rather than extending an ad hoc “queue empty” flag. [Naiad §2](https://www.microsoft.com/en-us/research/wp-content/uploads/2013/11/naiad_sosp2013.pdf) provides the relevant completion model. Its distributed machinery is unnecessary for the initial single-process design.

## 9. Recursion, transient contexts, and genuine fixed points

### 9.1 Structural recursion is ordinary incremental demand

A recursive invocation is a keyed subgraph, not a new Component definition. For a tree computation:

```text
analyze(node, context)
  -> inspect node kind and selected fields
  -> construct child contexts
  -> demand analyze(child, child_context)
  -> combine selected child outputs
```

A local edit repairs the affected node, its dependent context constructions, and ancestor/sibling computations that actually consume changed values. A tree can be traversed from the top, bottom, or both. There is no requirement that every component be a bottom-up fold.

Execute deep recursion with heap-owned frames or explicit continuations. Do not let a deeply nested user tree overflow the Rust stack, and do not instantiate an unbounded generic type for recursive plan construction.

### 9.2 Transient parameters are semantic inputs

An expected type, environment, mode, or accumulator may change on every call. A transient that affects output must be included in invocation identity or represented as a tracked parameter projection. “Transient” does not mean invisible to the cache.

Two strategies:

- **Small immutable values:** canonicalize the full value and include it in the call argument tuple. Different values are different invocations; bounded caching prevents unbounded history retention.
- **Large structured contexts:** generated context records contain stable handles and field/key projections. Passing a context does not read every binding; lookup records the requested binding and relevant absence/shadowing tests.

For environments, prefer persistent frame identities with tracked binding membership/value observations. A lookup of `x` records absence in intervening frames and presence/value in the resolving frame. Editing unrelated `y` should not invalidate it. Replacing the whole environment by a newly interned full map on every edit would needlessly rekey every descendant call.

Context frames produced by a recursive invocation use generated provenance identities and staged field definitions. Their lifetime follows the owning invocation and active snapshots. This is internal machinery; users pass a typed `Scope` value.

Transient return values use the same decomposition: `(result, new_context, evidence)` has independent projections. A changed context only propagates to consumers of its changed projections.

**Exporting contextual results:** `infer(expr, scope_a)` and `infer(expr, scope_b)` are different provider invocations. They can coexist as invocation-local returns, but they cannot both define `Types[expr]`. The dispatcher for that output chooses one invocation, including its context, through a tracked rule. If the application intentionally exports both, its schema must distinguish the semantic contexts, for example `Map<(ExprRef, AnalysisMode), Type>`. `AnalysisMode` is user data, not an internal cache key. No context-dependent result is silently exported to a context-free singleton slot.

### 9.3 Mutual recursion: function graph versus invocation graph

`infer` and `check` can call each other without a value cycle:

```text
infer(parent, env)
  -> check(child, env, expected)
       -> infer(grandchild, env2)
```

The component-definition graph is cyclic; the actual invocation graph can still be acyclic because arguments descend structurally. Ordinary demand evaluation handles this.

Generate all declarations before linking recursive bodies. Type-erase or box recursive executable descriptors at the storage boundary so mutually recursive Rust types remain finite. Use normal typed handles at the public boundary.

### 9.4 Detect cycles across workers

Maintain task-to-invocation wait edges, not only thread-local recursion stacks. Before committing a suspension edge, detect whether the waiter is reachable from its dependency in the current wait graph. The initial implementation can use an explicit graph walk under a dedicated coordination lock; optimize only after measuring contention.

When a cycle is found:

- Ordinary component: return a structured cycle error with the invocation path.
- Explicit fixed-point region: transfer the participating invocations to that region's solver.

Do not mistake shared concurrent demand for recursion, and do not let two workers wait forever for each other.

### 9.5 Fixed points have declared semantics

A cycle such as `x = !x` cannot be repaired by scheduling. A cycle such as `x = x` has multiple solutions. Require an explicitly declared fixed-point family:

```text
domain: partially ordered values with bottom and join
transfer: monotone
termination: finite-height domain, or a separately justified convergence strategy
meaning: least fixed point under the declared order
```

Use a worklist over affected equations, scheduling dependents only after a value actually grows. A product domain accommodates heterogeneous mutually recursive outputs if its component orders and transfers satisfy the contract. This is the same kind of convergence discipline described for [JastAdd circular attributes](https://jastadd.cs.lth.se/web/documentation/reference-manual.php).

**Deletion is the difficult case.** Starting from yesterday's fixed point and only applying joins can preserve facts whose support disappeared. For the general lattice interface, conservatively reset the affected solver region to bottom and recompute. Localize regions where sound; a whole-SCC reset is a possible worst case and must be reported as such.

For relational analyses with insertion and retraction, use an operator family with explicit support/retraction semantics, potentially Differential Dataflow/DBSP. Do not use naive support counting for arbitrary cyclic proofs: mutually supporting facts can survive incorrectly. A custom deletion-aware solver belongs in `GeneralComponent` unless its laws are part of the standard operator library.

Consequently, **fine observation granularity does not guarantee fine deletion work for every cyclic algorithm**. This remains an algorithm-specific limit, not something the macro can conceal.

## 10. Proposed public API

The examples below specify the intended user experience. They are not presented as compiling library examples. The only implemented code accompanying this plan is the explicitly labeled research model.

### 10.1 Declare ordinary-shaped Views

```rust
#[view(source)]
struct Options {
    strict: bool,
    target: String,
}

#[view(derived)]
struct Types(Map<syntax::ExprRef, Type>);

#[view(derived)]
struct Diagnostics(List<Diagnostic>);
```

`Syntax`, generated in §5.7, is one tree View. `Types` and `Diagnostics` are separate singleton View types. `Type` and `Diagnostic` are ordinary user data, with derived decomposition/equality as needed. No parser, type checker, symbol table, or specific IR is built into the runtime.

### 10.2 Functions become dependency builders

```rust
#[irrigation]
mod analysis {
    #[component]
    pub fn analyze(syntax: &Syntax, options: &Options)
        -> (Types, Diagnostics)
    {
        let checked = syntax.root().items().map(|expr| {
            inspect(expr, Scope::empty(), options.strict())
        });
        (Types::from(checked.map(|item| (item.expr(), item.ty()))),
         Diagnostics::from(checked.flat_map(|item| item.diagnostics())))
    }

    #[component]
    fn inspect(expr: syntax::ExprRef, scope: Scope, strict: bool)
        -> Checked
    {
        let (ty, evidence) = infer(expr, scope);
        Checked::new(expr, ty, diagnostics_for(expr, ty, evidence, strict))
    }

    #[component]
    fn infer(expr: syntax::ExprRef, scope: Scope) -> (Type, Evidence) {
        match expr.kind() {
            ExprKind::Literal(value) => (literal_type(value), Evidence::empty()),
            ExprKind::Name(name) => scope.lookup_with_evidence(name),
            ExprKind::Call { callee, args } => {
                let (signature, callee_evidence) = infer(callee, scope);
                let arg_evidence = check_arguments(args, scope, signature);
                (signature.result(),
                 Evidence::concat(callee_evidence, arg_evidence))
            }
            ExprKind::Let { name, value, body } => {
                let (bound, value_evidence) = infer(value, scope);
                let (ty, body_evidence) = infer(body, scope.with(name, bound));
                (ty, Evidence::concat(value_evidence, body_evidence))
            }
        }
    }

    #[component]
    fn check(expr: syntax::ExprRef, scope: Scope, expected: Type)
        -> Evidence
    {
        // A real language may have structural checking cases before this one.
        let (actual, evidence) = infer(expr, scope);
        Evidence::concat(evidence, compatible(actual, expected))
    }
}
```

In this sketch, helper operations such as `literal_type`, `check_arguments`, `diagnostics_for`, and `compatible` must be declared components or supported primitives. They are not an untracked escape hatch. `check_arguments` can request `check` for individual children, completing the mutual-recursion pattern. `Checked`, `Scope`, `Type`, and `Evidence` are user-declared types, not runtime compiler concepts.

The lowering treats expression handles as references into a View, immutable small arguments as transient parameters, and result fields as reactive projections. The source text stays close to a normal recursive program; the macro supplies dependency-builder behavior.

Evidence is returned and connected to diagnostics, rather than discarded as if checking were a side effect. A request for `Types` alone may omit evidence computations that cannot affect the type projection; a request for diagnostics demands the evidence. The macro diagnoses unused reactive results. The root's mapping selects which contextual `inspect` invocation supplies each exported `Types[expr]`; other `infer` contexts remain invocation-local.

### 10.3 Connect, edit, and observe

```rust
let runtime = Runtime::new();
runtime.install(analysis::analyze)?;

runtime.edit(|edit| {
    edit.view::<Options>().set_strict(true);
    edit.view::<Syntax>().set_root(document);
})?;

let snapshot = runtime.read::<(Types, Diagnostics)>()?;
let diagnostics = snapshot.view::<Diagnostics>();

let subscription = runtime.observe::<Diagnostics>(|snapshot, changes| {
    show_diagnostics(snapshot, changes);
});

runtime.edit(|edit| {
    // Typed navigation locates the node; there is no user-maintained cache ID.
    edit.view::<Syntax>().root().items().at(3).replace(new_expression);
})?;
```

Installing a root component recursively registers its referenced component descriptors and verifies View routing. If two components define distinct fields of one View, the composition surface names those domain fields. It does not request an internal ownership token from the user.

Snapshot handles have a lifetime bound to the snapshot. Editing uses generated setters and collection methods; raw mutable borrowing of reactive backing storage is unavailable. Explicitly requesting a detached owned copy is allowed, but its copy cost is visible and later reads of that copy are not live observations.

### 10.4 Multiple components defining one View

The composition point uses ordinary field construction to declare the output routing:

```rust
#[view(derived)]
struct Report {
    summary: String,
    details: List<Detail>,
}

#[component]
fn report(source: &Source, options: &Options) -> Report {
    Report {
        summary: summarize(source),
        details: if options.strict() {
            detailed_report(source)
        } else {
            brief_report(source)
        },
    }
}
```

Here `summarize`, `detailed_report`, and `brief_report` are user-defined components. The generated composition exports the first component's result into `Report.summary` and the selected component's result into `Report.details`. They populate the same View instance, with no merge policy, locks, or user-generated IDs. Changing `strict` repairs the details route; the summary remains reusable. Independent fields can execute in parallel.

For a map-valued View, the equivalent composition maps each semantic key to one selected invocation. Two calls to the same Component type with different transient parameters remain different invocations; the composition must select one or distinguish their semantic output keys.

### 10.5 `GeneralComponent`: custom incrementality without runtime keys

The interface needs initialization, incremental update, dependencies, multi-output staging, and lifecycle handling. Conceptual Rust signature:

```rust
pub type ComponentFuture<'a, T> = std::pin::Pin<Box<
    dyn std::future::Future<Output = Result<T, ComputeError>> + Send + 'a
>>;

pub trait GeneralComponent: Send + Sync + 'static {
    type Inputs: ViewTuple;
    type Params: ParameterTuple;
    type Outputs: ViewTuple;
    type Returns: ParameterTuple;
    type State: Send + Sync + 'static;

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Reset
    }

    fn initialize<'a>(
        &'a self,
        cx: &'a mut GeneralCx,
        input: SnapshotOf<Self::Inputs>,
        params: ValuesOf<Self::Params>,
        out: &'a mut OutputTxn<Self::Outputs, Self::Returns>,
    ) -> ComponentFuture<'a, Self::State>;

    fn update<'a>(
        &'a self,
        cx: &'a mut GeneralCx,
        state: &'a Self::State,
        input: SnapshotOf<Self::Inputs>,
        params: ValuesOf<Self::Params>,
        changes: UpdateBatch<Self::Inputs, Self::Params>,
        out: &'a mut OutputTxn<Self::Outputs, Self::Returns>,
    ) -> ComponentFuture<'a, Self::State>;
}
```

Design choices:

- `update` returns replacement state; old state remains valid if evaluation fails or is cancelled. Large states use persistent/shared internals rather than full cloning.
- A changed parameter can select a new invocation, initialized from scratch, or update a tracked parameter projection within the same invocation. The engine specifies which event happened; it never mixes the two identities.
- Input snapshots and `cx.request(...).await` record reads and component demands. The boxed future suspends without blocking a worker. Snapshot/parameter wrappers are owned, snapshot-safe, and transferable; `State: Sync` permits a transferable future to retain `&State`. A synchronous algorithm can return a ready future.
- `UpdateBatch` includes source/parameter deltas **and changes to retained requested dependencies**, including transient result projections and routing changes. It may contain only dependency changes even when declared Inputs/Params are unchanged. Events identify typed request receipts/scopes created by `cx`, not user-assigned numeric keys. Distinguish certified value changes from suspect-scope invalidations. If a controlling input changes, do not force an obsolete requested branch merely to compute its delta; deliver the scope invalidation and let the update rebuild the active requests.
- The default `DependencyUpdates::Reset` policy reinitializes on requested-dependency invalidation. A custom implementation can opt into `DependencyUpdates::Incremental`, accepting the obligation to handle the delivered receipts and invalidated scopes in `update`. Missing delta history also produces explicit `Reset`; no update is silently omitted. Reinitialization replaces all old dependency scopes only when its new state/output commit succeeds.
- Output transactions address typed fields and collection entries, enforce dispatcher-issued definition capabilities, and publish the output patch with the replacement state atomically. A transaction may inspect its own draft locally. Another component cannot read that draft: a request that requires the uncommitted output participates in ordinary dependency-cycle detection, rather than bypassing isolation.
- Dependency refresh uses explicit retain/replace/remove operations on opaque scopes returned by `cx`. The engine retains untouched scopes, atomically replaces revisited scopes, and removes retired scopes with the state/output commit. Reading only changed inputs must not drop still-relevant unchanged subscriptions; a dependency abandoned by a changed branch must be explicitly retired. In-flight notifications from a retired scope generation are ignored.
- A future carries its own dependency recorder and execution generation; worker migration must not depend on stale thread-local context. Send-only or non-thread-safe state requires a separate local-executor trait, outside this initial transferable interface.
- Resources tied to the returned state are released through ordinary ownership; external irreversible side effects remain outside computation and occur only after successful observation.

A custom incremental parser can retain parser state, consume text edits, and emit tree edits with typed handles. A custom graph algorithm can retain an index and produce changed node facts. Neither requires parser-specific or checker-specific machinery in the core.

Ordinary component users never implement this trait. It is the deliberate algorithm-level escape hatch.

## 11. Rust implementation architecture

### 11.1 Crate boundaries

```text
irrigation-core       schemas, typed handles, change contracts, erased descriptors
irrigation-store      revisioned units, sequence/map/graph storage, identity
irrigation-runtime    demand, validation, worker scheduling, publication, progress
irrigation-collections incremental collection operators and indexes
irrigation-macros     view/component/tree-group lowering and diagnostics
irrigation-test       from-scratch oracle hooks, event traces, property generators
```

These are responsibility boundaries, not a demand to publish seven packages immediately. Start in one workspace and split crates only where dependencies and compile times justify it. The macro crate must be separate because Rust requires a `proc-macro` crate. Two consumers sit outside the kernel and are not boundaries it must preserve: `irrigation-observe` (the observation path and its server) and `irrigation-syntax` (an incremental text/lexer/parser pipeline).

### 11.2 Heterogeneous tuples and type erasure

At API boundaries, generate tuple implementations and typed projections. A macro can lower arbitrary-length user tuples into an internal heterogeneous list; Rust does not require native variadic generics for this. Small manually generated tuple arities are an implementation convenience, not a semantic maximum. Named bundle structs are more readable for large interfaces.

Internally, register a typed arena/vtable for each concrete schema. Erase types only in scheduling and registry handles. Validate the descriptor type before downcasting; do not scatter `Any` casts through operator code. `TypeId` is appropriate for in-process registration, not a persistent cache format.

Component descriptors are immutable and `Send + Sync`. Per-invocation mutable operator state has one task owner. Results crossing worker boundaries satisfy the necessary `Send`/`Sync` constraints. Non-thread-safe custom algorithms may use an explicitly local executor; they cannot silently compromise the global contract.

### 11.3 Storage and memory ownership

Begin with safe Rust, immutable version records, `Arc`-backed payloads where useful, compact generational arena indices, and short-lived metadata locks. Optimize allocation after measuring.

A strong `Arc` on every dependency edge would retain cycles. Store dependency graph edges as arena IDs; use explicit reachability from source roots, subscriptions, live snapshots, and retained memo roots for reclamation. Generation checks prevent a recycled slot from satisfying a stale handle.

Reclaim removed branches, obsolete contexts, old reverse edges, detached collection outputs, and superseded revisions. Bound memo retention by policy; eviction loses performance, not correctness, because later demand reconstructs and revalidates the graph.

### 11.4 Synchronization rules

- One successful task claim per node/revision/generation.
- No user code under registry or dependency-graph locks.
- One atomic commit record links values, dependencies, control children, and completion state.
- Define one lock order for metadata operations involving multiple shards.
- Retain explicit wait edges for cross-worker cycle checks.
- Use release/acquire publication for immutable ready records only after a concrete memory-ordering design is reviewed.

A correct lock-based initial implementation is preferable to an unverified lock-free design. Use established work-stealing/queue components after checking their current APIs and selecting versions; no specific crate version is assumed in this plan.

### 11.5 Effects and deterministic operations

The generated language must distinguish computations from observation effects. A `println!`, filesystem write, or diagnostic push into a global mutable vector cannot execute inside a replayable component. Instead, diagnostics are output data; printing happens in an observer.

An associative reduction needs an explicit lawful operation. Do not reassociate floating-point addition or an arbitrary user closure merely to create parallelism. Stable order is part of semantics where the operation is order-sensitive.

### 11.6 Explainability without user-managed machinery

Offer an optional inspector that answers:

- Why did this output change?
- Which field/key/branch invalidated it?
- What was validated versus actually executed?
- Where did identity reuse fail?
- Which expensive scope is a `GeneralComponent`?
- What blocks progress or forms a cycle?

Source spans and typed paths are useful; exposing raw internal IDs as required API arguments is not.

## 12. Correctness argument and performance analysis

### 12.1 Proof obligations

**A. Change correctness.** Each View's normalized patch preserves its intended final state. Each incremental operator satisfies the update equation in §4.3.

**B. Complete read tracking.** Every value or control choice influencing a generated result is an immutable argument or a recorded observation. Membership, absence, provider routing, and structural reads count.

**C. Valid cache reuse.** Reuse requires all controlling observations and relevant input projections to be validated for the requested revision with the versions previously consumed.

**D. Definition uniqueness.** Every demanded derived unit resolves to exactly one definition, including its dynamic dispatch decision.

**E. Atomic graph/value replacement.** Routing recipes publish as revisioned generations. Each completed unit commits its values and trace together under its pinned generation; collection observations also require certified membership. Stateful custom transitions commit state and output patches together. No observer combines values with stale routing or dependency certificates.

**F. Concurrent refinement.** Every successful parallel observation equals a legal execution of the sequential demand specification for the same snapshot. Shared demand is single-flight; cancelled work cannot commit; wait cycles cannot silently deadlock.

**G. Fixed-point correctness.** The declared solver computes its specified fixed point and handles deletions according to its contract. A cycle error is not an approximate value.

For finite acyclic invocation graphs, a proof proceeds by induction over demanded dependencies: valid source values establish leaves; valid operator laws and complete reads establish parents; exact equality permits cutoff; atomic commits and snapshot isolation preserve the induction under parallel scheduling. Dynamic branches use the newly evaluated control value to select the relevant subgraph. Fixed-point regions need a separate convergence and leastness argument.

This is a proof outline with identified obligations, not a completed mechanized proof. The existing papers establish related restricted systems; their proofs are evidence for feasibility, not transferable certificates for new code.

### 12.2 Cost model

Let:

- `N`: materialized logical computation nodes.
- `E`: retained dependency edges.
- `C`: normalized changed input units.
- `A`: affected primitive computations actually executed.
- `E_v`: dependency edges visited during invalidation and validation.
- `D`: dependency/control edits and newly constructed nodes.
- `O`: changed output units materialized.
- `W_A`: useful execution work of affected primitives.
- `L_A`: critical-path work of the affected execution.

A realistic target is:

```text
update work = O(|C| + |E_v| + |D| + |O| + W_A + index/equality costs)
memory      = O(N + E + retained values + indexes + live snapshots)
parallel time ≈ W / P + L_A + scheduling/synchronization costs
```

The parallel-time expression is a planning model, not a proved bound for this design. Balance batching against critical-path latency. Large `E_v`, large equality costs, or context rekeying can dominate even when `A` is small.

The SPAA 2021 paper proves expected `O(WΔ + RΔ·h)` work and high-probability `O(s·h)` span for its own RSP-trace model, where `h` is trace height, `RΔ` affected reads, and `s` original span. Its single-assignment/nested-parallel assumptions matter. Our general sharing and dynamically routed outputs require additional analysis. [Theorem 1.1](https://www.cs.cmu.edu/~blelloch/papers/3409964.3461799.pdf).

### 12.3 Expected update behavior

| Edit / computation | Target behavior | Necessary worst case |
|---|---|---|
| No-op edit | No derived body execution or value event | Equality/normalization cost at touched inputs |
| One mapped element changes | One mapping region plus output maintenance | Mapping body's own affected dependency cone |
| Insert before a stable member traversal | Create one member region; preserve existing mapped work | Positional consumers may change across a suffix |
| One input of balanced lawful reduction | Recompute a logarithmic ancestor path | `O(log n)` aggregate nodes; payload operation cost may vary |
| Tree leaf changes | Repair its dependent projections/calls | Up to the entire tree if every result truly depends on it |
| Unused environment binding changes | No affected lookup body after precise validation | Coarse context representation would violate this target |
| High-degree graph node removed | Retract its incident edges and affected results | At least `Ω(degree)` |
| Global rename / exported signature change | Only real consumers update | Potentially most of the project |
| Fixed-point fact removed | Deletion-aware repair where supported | Whole affected solver region for general lattice reset |

### 12.4 Memory is the principal price of extreme granularity

Illustrative capacity estimate, not a measurement: at 48 bytes per node and 24 bytes per edge, ten million nodes with three edges each consume approximately `480 MB + 720 MB = 1.2 GB` of metadata before values, indexes, allocator overhead, and snapshots.

Therefore:

- Store compact field/node IDs and inline small edge lists.
- Share projections and intern only profitable values.
- Avoid tracking pure constants.
- Reclaim obsolete control regions promptly.
- Preserve fine logical granularity while batching physical work.
- Measure allocated bytes and peak retained bytes, not only elapsed time.

No small universal overhead factor is promised. Expression-level tracing can be more expensive than recomputation for tiny computations; the implementation must establish where this tradeoff becomes worthwhile.

## 13. Verification completed for this plan

### 13.1 Source review

Reviewed the core algorithm/semantics material in the linked Adapton, Names, incremental lambda-calculus, parallel self-adjusting computation, DBSP, Differential Dataflow, Naiad, and build-systems papers. Checked Rust/Salsa/JastAdd documentation for practical constraints. The reference links in §3 identify the original material, rather than secondary summaries.

The concurrent JastAdd research landing page was available, but its linked extended-report PDF was not retrievable in this session. This plan uses the official manual/concurrency documentation for those claims and does not rely on an uninspected proof from that PDF.

### 13.2 Executable Rust research model

Accompanying file: `verification-model.rs`.

Ran with `rustc 1.93.0 (254b59607 2026-01-19)`, edition 2024. The model uses only the standard library. Eight tests passed:

1. No-op and unrelated input edits do not rerun the observed body.
2. An unchanged intermediate output stops downstream execution.
3. Switching branches replaces the active dependency set.
4. Control is validated before an old branch that becomes invalid after deletion.
5. **10,000 deterministic generated transactions** match direct from-scratch evaluation.
6. **625 small integer cases** verify the bilinear delta rule, including simultaneous changes.
7. A static ownership table lets different providers share a View but rejects different definitions of one unit.
8. A recursion model with alternating entry-kind tags distinguishes full-value transient arguments in its cache.

The tests were written and observed failing before the model implementation. The final run passed all eight. The ownership test does not exercise dynamic routing replacement; the recursive test does not exercise structured-context projection reuse. This model checks selected sequential semantic mechanisms. It has no macro lowering, concurrent scheduler, real collection storage, cancellation, fixed-point solver, or benchmark implementation; the results cannot establish those capabilities.

To reproduce from the directory containing the model:

```sh
rustc --edition=2024 --test verification-model.rs -o /tmp/irrigation-model-tests
/tmp/irrigation-model-tests
```

### 13.3 Counterexamples used to refine the design

- **Two scalar writers:** dependency knowledge cannot determine the intended result. Resolve definitions before execution.
- **Deleted old branch:** unordered cache validation can evaluate a now-invalid computation. Preserve control order.
- **Whole-context keys:** changing an unrelated binding can rekey every recursive call. Track context projections.
- **Index-based list identity:** front insertion destroys reuse despite unchanged members. Separate membership identity and rank.
- **Join with two changed inputs:** omitting the cross term produces wrong results. Test multi-input deltas.
- **Unchanged value, changed branch:** retaining the old dependency graph produces future stale results. Replace trace even under cutoff.
- **Deletion in a cyclic lattice:** growth-only iteration can preserve unsupported facts. Reset or use a justified retraction algorithm.
- **Empty queues:** suspended work and outstanding graph expansion can still exist. Track demand completion explicitly.

## 14. Implementation sequence and acceptance gates

Build the risk-bearing semantics before convenience layers. The endpoint includes all requested View families, recursive/mutually recursive component generators, transparent standard incrementality, shared-output routing, and `GeneralComponent`. An early milestone is not a substitute for that endpoint.

### Phase 1 — Sequential semantic kernel

Implement revisioned scalar/record units, typed products, tracked reads, ordered validation, dynamic branches, negative dependencies, exact cutoff, and unique definitions. Add explicit demand roots and subscriptions.

**Pass when:** randomized edit sequences match a separate full evaluator; branch deletion and equal-output/new-dependency cases pass; no-op changes execute no derived bodies; two incompatible definitions fail deterministically. Provider identity distinguishes different transient-argument invocations of the same component.

### Phase 2 — Fine-grained generated components

Implement typed IR, local expression lowering, branch factories, tuple projections, ordinary recursive and mutually recursive calls, transient parameters, and source-level diagnostics. Add the enum-group tree macro and identity-preserving tree edits.

**Pass when:** one field change cannot rerun unrelated primitives in the same function; deep recursion is stack-safe; a moved subtree preserves eligible invocation reuse; unsupported opaque calls are rejected rather than silently coarsened; two outputs of different types have independent cutoff.

**Hard gate:** a macro that only memoizes the entire function does not pass this phase.

### Phase 3 — All collection Views and delta operators

Implement vector/list, set, map, general graph, tree structural deltas, per-member map/filter/flat-map, lawful reductions, indexed joins, and dynamic output membership/provider routing. Add structural context records and dependency-scoped `GeneralComponent` state transitions.

**Pass when:** a custom operator depending only on a dynamically requested transient result is notified when that result changes, can switch dependency scopes, and ignores later changes from retired scopes. Insertion at the front retains identity-based mapped work; rank-sensitive behavior remains correct; missing-key insertion wakes the right consumer; graph payload edits leave topology-only consumers alone; multi-input updates and output retractions match from scratch.

### Phase 4 — Parallel, pipelined execution

Introduce single-flight claims, worker stealing, suspended continuations, wait-graph cycle detection, atomic output publication, progress accounting, failure, and cancellation. Keep one active revision initially.

**Pass when:** switching a routed region where `V.y` reads `V.x`, one value stays equal, and one member disappears causes neither deadlock nor stale subscriptions; tuple/collection observers never mix routing generations. Results are invariant across worker counts and legal schedules; a diamond never emits mixed-revision tuples; multiple simultaneous requests execute one shared invocation; cross-worker cycles terminate with the correct outcome; cancellation cannot publish; tiny worker pools do not deadlock. Demonstrate downstream progress before unrelated upstream units finish.

Use model checking for synchronization state machines, randomized schedule perturbation, and stress tests; run race/undefined-behavior tooling appropriate to the eventual implementation. Tests of a sequential oracle alone do not pass this gate.

### Phase 5 — Fixed points, resource control, and performance

Add declared lattice regions with a correct deletion policy, optional relational solver integration, cache eviction/reclamation, memory accounting, and profiling. Add adapters demonstrating self-defined incremental algorithms without adding domain-specific core concepts.

**Pass when:** insertions and deletions match a from-scratch fixed-point solver; abandoned branches and snapshots are reclaimed; cache eviction changes cost but not results; benchmarks meet the algorithm-specific scaling criteria below.

### Benchmark design

Use the same outputs and equality semantics across implementations. Pin source revisions, compiler versions, optimization flags, allocator choices, hardware, and worker counts when the actual benchmark is built.

Baselines:

1. Optimized from-scratch evaluator in Rust.
2. Whole-function memoization.
3. A Salsa implementation with comparable manually chosen fine query boundaries, not an intentionally coarse baseline.
4. The proposed runtime with one worker and multiple workers.
5. Differential Dataflow/DBSP for relational fixed-point workloads where their model fits.
6. A specialized handwritten incremental algorithm for representative workloads, showing the price of transparent machinery.

Workloads:

- Point update, insertion, removal, move, and splice in lists at increasing sizes.
- Map lookup hits/misses and batch multi-input joins.
- Deep and wide enum trees, repeated equal-looking subtrees, and subtree moves.
- Context-sensitive mutually recursive tree analysis with local versus widely shared context changes.
- General graph node/edge edits, high-degree deletions, and cyclic analysis retractions.
- A user-provided incremental algorithm through `GeneralComponent`.
- No-op transactions, inactive outputs, branch switching, and rapid superseding edits.

Report p50/p95/p99 edit-to-observer latency, total work, validation work, primitive executions, changed output count, graph construction/reclamation, allocations, peak memory, and parallel speedup. Separate warm-cache from cold-start measurements.

Scaling acceptance is more useful than fabricated speedup promises:

- Identity-based mapped body executions are independent of untouched list length for a point edit.
- Balanced lawful reduction updates grow logarithmically in sequence length.
- Unrelated map keys/fields do not execute their consumers.
- Output equality prevents downstream body execution.
- Independent batched work benefits from extra workers before memory/scheduling limits dominate.
- End-to-end time includes source ingestion, graph maintenance, equality, and output materialization—not just the inner delta loop.

Performance targets in milliseconds should be set from a representative compiler workload and hardware, not asserted before one exists.

## 15. Final recommendation

Proceed with a **typed incremental graph language hosted in Rust**, with function and enum-group macros as its ordinary interface. Make components singleton graph generators, Views singleton typed namespaces, and recursive calls runtime-owned invocation subgraphs. Use precise primitive observations, stable structural identity, collection delta rules, and unchanged-output cutoff together.

The key engineering rule is: **keep the user program simple by making the internal semantics explicit, not by pretending they are unnecessary.** Ownership of definitions, control-dependent graph construction, revision consistency, transient context identity, and fixed-point meaning belong in the runtime contract. Users should supply data types and transformations; they should not manage keys, locks, invalidation, or dependency messages.

The result can provide extremely fine-grained transparent incrementality for the supported language. Universal optimal increments for arbitrary Rust, arbitrary reconstructed data, or arbitrary cyclic computations are not achievable promises. The proposed boundaries make the requested facility both ambitious and implementable.
