# Shared Views in practice: one scope graph, typing edges, and transparency

**Status:** implemented. The plan was revised after its first fixture disproved a
name-only topology manifest: the runtime refreshes a field's `Structure` unit
from committed content (`Inner::refresh_structure`), so a writer cannot publish a
subgraph manifest that survives another writer's commit. The landed shape uses
the content topology and kind tags, and states the resolver cut-off honestly.

**The example no longer carries kind tags.** Shared writers and the one-graph
rule still hold, but the STLC host now uses framework-owned emission identity
and an interned shared type site, so it writes no tag bits or host keys. See
[`emission-site-plan.md`](emission-site-plan.md); the fixture
(`crates/irrigation-observe/tests/shared_scope_graph.rs`) is unchanged.

Landed in this change:

- `define_edge` checks endpoint **membership** ports instead of the whole
  topology; `define_edge_trusted` stages a complete fragment whose endpoints are
  in the same write group.
- One `ScopeGraph` View (`graph` + `diagnostics`) was written by the shared
  writer stages; the STLC example has since moved to binding nodes and
  term-keyed type maps (see the example update below). `NameGraph`, `TypingEdges`, and
  `assemble` are deleted.
- Node/edge kind tags and the resolver's tag-filtered content read.
- `ViewFacts.contributors` / `ViewWire.contributors`; `StatsWire` arbitration
  counters; the page's `+N` writers and tape arbitration line.
- `crates/irrigation-observe/tests/shared_scope_graph.rs` and the example's
  one-graph tests.

**Owners:** runtime/collections arbitration, the observation projection, and the
`stlc_scope_graphs` reference host.

---

> **Example update:** the STLC example now uses binding nodes, term-keyed
> `types`/`type_errors` maps, and per-term `check_term`; it no longer writes
> typing edges into the shared graph. The framework shared-writer fixture
> (`crates/irrigation-observe/tests/shared_scope_graph.rs`) is unchanged. The
> historical stage lists below describe the shared-writer landing, not the
> current example. See
> [`term-keyed-typing-plan.md`](term-keyed-typing-plan.md).

## 1. The situation

The reference host publishes the same analysis three times:

```text
build_scopes   -> NameGraph     (scopes/terms + P/D/R/child)
check_types    -> TypingEdges   (type scopes + ty/dom/cod)
assemble       -> ScopeGraph    (the union)
```

The split came from the old one-producer rule. It is the wrong shape for the
example: typing is supposed to *add edges to the scope graph*, and three graphs
show a pipeline of pictures instead of one graph and its writers. The runtime now
supports several components writing disjoint units of one View, so the target is:

```text
build_scopes   -> ScopeGraph: name nodes/edges
resolve_names  -> Resolution (reads the graph)
check_types    -> ScopeGraph: type nodes + ty/dom/cod + diagnostics
evaluate       -> Evaluation (reads Terms + Resolution)
```

One graph, two writers. No `TypingEdges`, no `assemble`.

Two facts discovered by the fixture constrain the design:

1. **The topology unit is content.** `refresh_structure` recomputes a field's
   `Structure` unit from the committed content after definitions. There is no
   way for one writer to publish a name-only subgraph manifest: the next commit
   replaces it with the union. `GraphRef::topology()`/`nodes()`/`edges()` see
   the **whole content**, not a per-writer subgraph.
2. **A second writer can still add entities.** `define_node`/`define_edge`
   claim per-entity ports, so one writer's nodes/edges do not conflict with the
   other's. `define_graph` claims the field and is unusable on a shared field.

The honest consequence: if the resolver reads the graph's topology, a type edit
changes that unit and the resolver is invalidated. It re-executes, its answer is
equal, and the cutoff keeps typing/evaluation's downstream consumers out. The
resolver's *result* is version-stable; its *stage* is not skipped. §5 records
that trade-off and the fallback for callers that need the stage skipped.

## 2. Goals and non-goals

Goals:

1. One `ScopeGraph` View on the board, written by `build_scopes` and
   `check_types`, with name and typing edges in one drawing.
2. Typing publishes type scopes and `ty`/`dom`/`cod` edges directly into the
   graph, plus type diagnostics on the same View. No judgement table.
3. The resolver observes name nodes/edges only (via ID tags), never type node or
   edge payloads; its published answer cuts off when only typing moved.
4. Every writer of a shared View is visible: contributors in the projection and
   the component panel, arbitration counters in the wire and the tape.
5. An integrated fixture proves content union, tag filtering, writer survival,
   cutoffs, and counter exposure across revisions.
6. Documentation and error messages agree about the model.

Non-goals:

- A merge policy or last-writer-wins. One owner per logical unit.
- A published subgraph manifest. The runtime owns the topology unit; a separate
  name-only View or table is the fallback if exact resolver stage-skipping is
  required.
- Changing source/tree editing semantics.
- A general graph editor for the page.

## 3. The model

The arbitration plan defines the primitives
([`shared-output-arbitration-plan.md`](shared-output-arbitration-plan.md)).
This host uses:

| Concept | Use here |
|---|---|
| Port | one graph node's owner pair; one graph edge's owner pair; the diagnostics list field |
| Write claim | `define_node`/`define_edge` claim the entity pair; `define_graph` is forbidden on the shared graph |
| Requirement | the checked `define_edge` requires endpoint memberships; a writer that has both endpoints in its own group uses `define_edge_trusted` instead |
| Deferral | the checked form defers an edge whose endpoint comes from another later writer; the trusted form installs a complete fragment atomically |
| Port provider | not needed: every endpoint is written by one of the two writers in the same demand |
| Retry | a read invalidated by a later commit retries; the wire exposes the count |

### 3.1 Ownership matrix

| Port | Owner | Notes |
|---|---|---|
| scope/term node owner pair | `build_scopes` | IDs tagged scope/term |
| `P`, `D`, `R`, `child` edge owner pair | `build_scopes` | endpoints are name nodes |
| `Structure` unit | runtime-refreshed | content topology; not a writer claim |
| type node owner pair | `check_types` | IDs tagged type |
| `ty`, `dom`, `cod` edge owner pair | `check_types` | `ty` from a name term node to a type node |
| `diagnostics` field | `check_types` | separate field; disjoint from the graph field |

No component calls `define_graph` on `ScopeGraph`. A field claim would overlap
every entity claim and retract the other writer's units.

### 3.2 Endpoint membership, not topology

`define_edge` checks endpoint presence through the two membership units, not
through the field's topology. The first fixture used `topology_has_node`, which
made every edge writer read the whole graph topology; a name edit then woke the
typing writer and vice versa. Presence is a membership fact, so the checked
form reads only the endpoint ports. The example's writers also use
`define_edge_trusted` because each emits a complete fragment (all of its
endpoints are in the same write group), avoiding a requirement on uncommitted
ports. The trust is local: the edge still claims its own port, and the content
projection only draws edges whose endpoints exist.

### 3.3 Identity tags and filtering

The resolver must read name facts without observing typing payloads. IDs carry
their space:

- nodes: `Scope` (small), `Term` (`0b01` tag), `Type` (`0b10` tag);
- edges: the top three bits encode the edge kind (`P`, `D`, `R`, `child`,
  `ty`, `dom`, `cod`).

The resolver reads the graph's node/edge ID lists (the topology unit changes
with either writer) and filters by tag **before** calling `node(id)`/`edge(id)`.
It therefore observes the topology unit and name payload units, never typing
payload units. On a type edit it re-executes from the topology change; its
output is equal, so downstream stages are cut off.

The page and host snapshots read committed content and show the union.

## 4. Transparency requirements

A developer must be able to answer:

1. **Who wrote this View?** All contributors, not only the first.
2. **Is this the whole content?** Yes: the graph drawing and snapshots are
   content; the topology unit is content too. (No hidden subgraph manifest.)
3. **What did arbitration do?** Claims, conflicts, deferred writes, retries,
   waits — counted and exposed.
4. **Why did a write fail or wait?** Structured errors name ports and providers.

### 4.1 Projection data

Additive:

- `ViewFacts.contributors: Vec<ComponentId>` — every distinct component owning
  at least one unit, plus a registered producer; sorted and deduplicated.
  `producer` stays the primary (first) owner for the existing plate/ink.
- `ViewWire.contributors: Vec<String>` — stable wire keys.
- `StatsWire` gains `write_claims`, `write_conflicts`, `deferred_writes`,
  `demand_retries`, `reader_waits` (already in `RuntimeStats`).
- Component `writes` already attributes every View per unit; the two writers
  both report `ScopeGraph`.

### 4.2 Page

- The View's state chip tooltip names every contributor; the Canvas derives the
  list from `component.writes`. A clean or cut-off View's footer also shows the
  primary writer and `+N`; a changed View spends the footer on the change note,
  so the chip tooltip and the component panel carry the writer list.
- The tape footer shows a compact arbitration line when any counter is nonzero:
  `claims`, `deferred`, `retries`, `waits`. Counters are cumulative.
- The graph plate is the real content. No second graph is needed.

### 4.3 API and errors

- Document that a graph `Structure` unit is content-derived and that
  `GraphRef::nodes()`/`edges()` read the whole graph. No `define_topology`
  manifest API in this phase.
- `WriteConflict`/`PreconditionUnmet` already name ports and providers; keep.
- `ViewFacts.producer` docs say “primary contributor”; the plural list is
  authoritative.

## 5. Semantics and invariants

| Id | Invariant | Gate |
|---|---|---|
| S1 | Exactly one analysis graph View; no `NameGraph`, `TypingEdges`, `assemble`. | example test; workspace JSON |
| S2 | Content contains name edges and typing edges after one demand. | example + fixture |
| S3 | The resolver's published targets do not change when only typing units change; its version is cut off. It may re-execute after the topology change. | example test |
| S4 | A type annotation edit re-runs `check_types`; `build_scopes`/`evaluate` stay at their revisions; resolution's version stays. | example test |
| S5 | A literal edit re-runs `evaluate` only; graph and resolution versions stay. | example test |
| S6 | A consistent rename re-runs `build_scopes` and `resolve_names`; `check_types`/`evaluate` are cut off; old `ty` edges remain (positional term identities). | example test |
| S7 | A structural edit retracts stale type units and their edges; no ghost edges in content. | example test |
| S8 | `contributors` names both writers; both components list the View in `writes`. | fixture |
| S9 | `write_claims`/`deferred_writes`/`demand_retries` are exposed; the example's trusted fragments need no deferral, so `deferred_writes` may be zero there. | fixture/wire |
| S10 | Board shows one graph with the union and the writer names. | canvas smoke |

S3 is the cost of the content-owned topology. If a caller needs the resolver
stage skipped (not just cut off), the fallback is a **name facts table** (a map
of name nodes/edges, one graph plate + one table) or a separate name graph
(two graph plates). That is a deliberate follow-up, not a hidden divergence.

## 6. The integrated fixture

`crates/irrigation-observe/tests/shared_scope_graph.rs`:

- a source `Mode { names, types }` with one field per writer;
- a derived `Shared { graph: Graph<u64, u64> }`;
- `names` writes nodes/edges via `define_node`/`define_edge`;
- `types` writes its own nodes and `ty` edges (no name entities);
- `names_only` reads the **content topology**, filters
  name-tagged IDs, and returns the name ids it observed;
- the root calls names, types, reader, and reads `content_topology()` for the
  union.

Assertions:

1. content = union; `names_only` = name-filtered subset;
2. both components are contributors and both list the View in `writes`;
3. a name edit re-runs `names`; a type edit re-runs `types`; the reader's
   filtered output is stable across a type edit (the resolver cutoff);
4. a type edit changes the content and the topology, so the reader stage
   re-executes; its filtered name result and `Resolution`-equivalent output stay
   equal;
5. new entities from one writer survive the other writer's restatement;
6. `write_claims` and the wire counters are exposed (the trusted fragment path
   does not defer, so `deferred_writes` may stay zero here);
7. no ghost edges after a structural edit (example-scale test).

The example's end-to-end tests add:

- cold seed `1 : Int`;
- annotation, literal, rename, structural edits;
- one graph View only;
- type diagnostics on the graph View;
- the graph content has `D` and `ty` edges.

## 7. Implementation phases

### Phase 1 — framework surface and fixture

- Remove the abandoned `define_topology`/`content_topology` API experiments;
  document that the `Structure` unit is content-derived.
- Change `define_edge` to check endpoint **membership** ports, not the whole
  topology; add `define_edge_trusted` for a writer that defines both endpoints in
  the same group.
- Add node/edge tag helpers in the example `graph` module.
- `ViewFacts.contributors`, `ViewWire.contributors`, `StatsWire` counters.
- The shared-graph fixture above; wire/projection tests. (Landed at
  `crates/irrigation-observe/tests/shared_scope_graph.rs`.)
- Frontend types + footer `+N`/title + tape arbitration line.

### Phase 2 — example

- `ScopeGraph { graph, diagnostics }`; delete `NameGraph`, `TypingEdges`,
  `assemble`.
- `build_scopes`: `ensure_view`, `define_node`/`define_edge` for names.
- `resolve_names`: read content topology, filter name tags, then read payloads.
- `check_types`: `define_node` type nodes, `define_edge` typing edges (no term
  nodes), `define_diagnostics`.
- Update summary/tests/README/root README/plan index.

### Phase 3 — verification

- example tests, workspace tests, clippy, fmt, links;
- board smoke and a screenshot with one graph;
- status/evidence docs updated.

## 8. Risks and open questions

- **R1 — resolver re-execution.** Accepted and asserted as a version cutoff.
  If stage skipping matters, the name facts table is the fallback.
- **R2 — edge requirements and deferral.** The example's writers emit complete
  fragments and use `define_edge_trusted`, so `deferred_writes` is zero there.
  The checked `define_edge` is the cross-writer form; the arbitration suite
  covers its edge-first/deferred path. If a future writer needs the checked form
  at scale, predeclare the node ports or split the node stage.
- **R3 — primary producer label.** The page shows the primary + N; JSON has all.
- **R4 — cross-writer removal.** A structural name edit can remove incident
  typing edges from content before typing restates. Typing reads the term shapes
  and re-runs; the fixture and example assert no ghost edges after demand.
- **R5 — frontend churn.** Additive; the component panel already reports both
  writers. The JSON is authoritative if the UI change slips.
- **Q1 — name facts table?** Only if resolver stage skipping is required.
- **Q2 — per-revision arbitration deltas?** Follow-up; cumulative stats first.

## 9. Done

- one graph View, written by names and types;
- S1–S10 pass;
- the page names all contributors and shows arbitration counters;
- the fixture proves content union, tag filtering, and cutoffs;
- docs and gates are green.
