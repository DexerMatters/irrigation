# Runtime visualization: implementation status

This file records what of [`runtime-visualization-plan.md`](runtime-visualization-plan.md)
is built, where the evidence is, and where the code deliberately diverges from
the written plan. It is conservative: a feature counts only when a test or a
captured run exercises it.

## Shipped

| Plan area | Status | Evidence |
|---|---|---|
| §4.2 `observe` feature, levels, fixed observer | built | `crates/irrigation-runtime/src/observe.rs`; `cargo check -p irrigation-runtime` with the feature off and on |
| §4.3 event stream | built, amended (see divergences) | `RuntimeEvent`, emitted from edit commit, invalidation, demand, eval, commit, fail, wait/wake, notifications, collection |
| §4.4 instrumentation discipline | built | every emit is outside the metadata lock; the recorder is a bounded `ArrayQueue` with a drop counter |
| §4.5 demand causes | built as `HostRead`, `HostCall`, `HostSnapshot`, `ObservationRefresh` | `crates/irrigation-runtime/src/task.rs`; `demand.rs` |
| §4.6 counters | built | `RuntimeStats::{validation_attempts, validation_failures, execution_failures, cutoffs, suspensions, demands, notifications}` |
| §4.8 projection | built | `Runtime::{view_facts, probe, unit_versions, unit_changed_at, inspect_snapshot, try_explain_node}` |
| §4.8 granularity | built from installed units | `granularity_of` in `inner.rs`; `granularity_follows_installed_units` test |
| §4.9 value previews | built from runtime facts | unit kind + size in `project::node_detail`; no host formatter |
| §4.10 journal | built | `crates/irrigation-observe/src/journal.rs` with eviction, caps, cutoff views, no-op slot |
| §4.11 server | built with `std` HTTP, same wire protocol | `crates/irrigation-observe/src/http.rs`; `tests/server.rs` |
| host input (text channels) | built | `Builder::text_view`, `GET/POST /api/v1/views/{key}/text`, the plate editor |
| §4.12 cadence | built per-session: renders are lazy per visible module, graph projection on demand | `collector.rs`, `store.ts` |
| §4.13 frontend | one zoomable canvas: every View as a plate projected from the runtime, component edges, a tape, facts on selection | `web/observatory/src/canvas/`; `scripts/canvas-smoke.mjs`, `scripts/a11y.mjs` |
| §5 reference host | built | `crates/irrigation/examples/observatory/`; a second host with ten Views in `examples/observe_board.rs` |
| Shared View writers and attribution | built | per-entity `define_node`/`define_edge` and `define_edge_trusted` with endpoint membership requirements; `ViewFacts.contributors`/`ViewWire.contributors`; `StatsWire` arbitration counters; `crates/irrigation-observe/tests/shared_scope_graph.rs`; the STLC host writes one `ScopeGraph` from `resolve_name` + `check_types` |
| §6.1 event truth | built | `crates/irrigation-observe/tests/events.rs` (10 tests) |
| §6.2 server truth | built | `crates/irrigation-observe/tests/server.rs` (12 tests, incl. source + action discovery); `tests/scale.rs` (4 tests at 50k nodes) |
| §6.3 frontend truth | built | `npm test` (7 reducer + 8 layout tests), `tsc --noEmit`, headless screenshots |

## Acceptance gates

The plan's §9 gates, with the evidence that closes them:

| Gate | Status | Evidence |
|---|---|---|
| G1 semantics | met | `events.rs` (cutoff vs execution, invalidation without demand), `reduce.test.ts`, the tape strip |
| G2 passive | met | `reading_the_page_never_demands_computation`; `observation_is_passive`; the overhead run shows identical `executions` at every level |
| G3 stream | met | `the_stream_opens_with_hello_then_workspace`; gap → resync in `reduceFrame` |
| G4 projection | met | caps test, `Control` render sources, no raw values (opaque fallback) |
| G5 no-op | met | `transactions_are_recorded_and_noops_are_separate`; `a_first_edit_publishes_and_a_repeat_edit_does_not`; the tape's no-op line |
| G6 accessibility | met | `node scripts/a11y.mjs`: axe-core (WCAG 2.1 A/AA) reports zero violations in Views, Runtime and Ledger; the script also tabs to a module control and collapses it with Enter |
| G7 overhead | met | `cargo run --release --example observe_cost -p irrigation --features observatory`: off 4073 µs, summary +2.6%, nodes +0.5%, full +0.4% on the 10k point-edit workload |
| G8 off | met | `cargo check`/`test` with the feature excluded; observe code is `#[cfg]`-gated |
| G9 security | met | loopback default, token test, no unformatted payloads |
| G10 scale | met | `cargo run --example observe_board -p irrigation --features observatory` renders ten Views across all module kinds; `tests/scale.rs` builds a 50 000-node runtime and asserts the graph is capped with an exact `total_nodes`, a scoped query returns exactly its scope, a 50 000-row render is cut at 200 rows with the real total, and the workspace payload stays under 8 KB. Tree/list virtualization is replaced by row caps (250/200), which is the bound the plan asked for |
| G11 failure | met | render errors and dropped unit claims are visible per module; capped/incomplete activity is stated on the tape |
| G12 build | met | 331 Rust tests, 18 example tests, `npm test`, `npm run build`, `node scripts/canvas-smoke.mjs`, `node scripts/a11y.mjs` |
| G13 native | met | `every_model_kind_keeps_its_own_tag`; every field kind projects to its own model |
| G14 granularity | met | `granularity_follows_installed_units`, `views_report_identity_and_granularity`, the server drops whole-value claims |
| G15 modular | met | collapse/expand/rail with persisted layout, `Collapse all`, headers keep reporting state |

## Verification evidence

- `cargo test --workspace --no-fail-fast`: 331 tests pass, 0 failures.
- `cargo test --example observatory -p irrigation --features observatory`: 3
  pipeline tests pass (the seed's binders, an unresolved name, and a new binder
  reaching the symbol table after an edit).
- `cargo run --release --example observe_cost -p irrigation --features observatory`
  measures the overhead gate.
- `npm test` in `web/observatory`: 7 reducer tests (transaction attribution,
  cutoff vs execution, no-op, sequence gap, duplicate frame, ordering) and 8
  layout tests (Pascal names, one band per level and equal heights, fold,
  one-to-one alignment, one cubic per relation, distributed ports, drop
  collision escape).
- `node scripts/canvas-smoke.mjs` against a running host: opens a source plate's
  editor, types, waits — no submit — and asserts a new revision, the commit
  reply inside the plate (`29 chars · r4`), changed plates carrying the blink
  marker, and no console errors.
- `node scripts/a11y.mjs`: axe-core WCAG 2.1 A/AA on the board — zero violations;
  the script also tabs to a plate's Edit control and opens the editor with Enter.
- The board's design was measured with the `design-taste-frontend` inspector
  (`inspect.mjs --app`): **0 FAIL, 3 WARN** at 1440/768/390. The warnings are
  deliberate: edges use the component's own ink (the requested identity, more
  than four hues), a dense runtime board keeps 10–12px data and chrome text, and
  a level band may hold a short plate's whitespace because bands share one
  height.
- `node scripts/a11y.mjs`: axe-core WCAG 2.1 A/AA — zero violations in all three
  modes, on both the three-View demo and the ten-View board; keyboard collapse
  verified.
- `cargo test -p irrigation-observe --test scale`: 50 000 nodes, capped graph with
  an exact total, exact scoped drill-down, capped render with the real total.
- `cargo test -p irrigation --test gates deep_recursion`: passes in debug. The
  commit-notification helper is `#[inline(never)]` because its locals pushed the
  recursive `eval` frame over the debug stack budget.
- The pool shutdown race found while chasing a flaky suite is fixed: `Pool::drop`
  now stores the shutdown flag under the queue lock, so a worker cannot check the
  flag and then park past the notification (`inner::tests::…wakes_its_waker`
  reproduces it 50% of the time before the fix, 0/12 after).
- `npm run build` and `npm test` in `web/observatory`.
- Captured runs at 1440×900: initial board, suspect derived Views, the no-op
  notice, Runtime mode filtered to revision activity, Ledger, and the source
  editor after applying a four-statement program.

## Semantic divergences from the plan (deliberate)

1. **`TransactionCommitted.invalidated` is `u64`, and the transaction carries
   `views`.** One event owns the count; the graph gets the capped seeds from
   `Invalidated`. Nothing is summed twice.
2. **`NodeCommitted` carries `views` and `equal_views`, not one `view`.** A
   commit can define several Views; a definition that lands an equal value is
   reported as a cutoff for that View rather than as a change.
3. **Granularity is derived from installed units, not from the field kind.** A
   source tree stages one content value, so its module reports `whole_value`
   even though the field is `tree`. A derived tree (GeneralComponent output)
   would report `per_entity` when it stages per-node units. This is the honest
   rule the plan asked for, applied mechanically.
4. **No concrete value type name.** `Value` is `Arc<dyn Any>`; the runtime can
   report size but not the concrete type. The inspector shows the unit kind and
   size instead of fabricating one.
5. **The server is `std::net` + JSON, not Axum.** The checkout builds offline
   and Axum is not vendored; the endpoints and SSE framing match the plan.
6. **Demand cause `ObservationRefresh` replaces `Subscription { id }`.** One
   refresh serves every subscription; per-view detail lives in
   `SubscriptionNotified`.
7. **Rendering has one source.** A View is projected from its descriptor, its
   entities, and its unit versions; there is no host renderer, formatter, or
   public display model. A row the runtime cannot back is a display row, and a
   whole-value field draws as one value. A type that wants a better spelling
   implements `Debug`, not a second drawing.

## The board

The page is one surface, not a form:

- **One canvas, drawn as a DAG.** Every View is a plate in a top-down level
  band (sources at the top, what they feed below); a level shares one top edge
  and one height, and each plate is placed over the mean centre of its
  neighbours, so a one-to-one chain is vertical. The grip at a plate's
  top-left drags it, its body scrolls and selects, and a header chevron folds it
  to its title. The background pans, ⌘/ctrl + wheel zooms at the pointer, and
  `Fit` frames the board width in the space left by the top-left minimap (a
  long DAG is panned, not shrunk to a floor plan). `Overview` folds every plate
  to its header, which is the compact whole-DAG reading. A dropped plate is
  walked clear of its neighbours. The minimap is a rank diagram: one row per
  rank, one bar per node, component-coloured/faded wires, a right-edge camera
  bar, and signal dots for the displayed change subset. Positions, folds and
  the camera persist per host.
- **The structure is the runtime's own, under one law.** A component is a node;
  it appears because it is registered — root, general, or invoked, the same rule
  for all of them. Its reads are the Views whose units it observed; its writes
  are the Views whose units it defined or whose content it replaced. Nothing is
  inferred from names, and no host is special-cased: a parser, a lexer, or any
  custom algorithm installed as a general component is drawn exactly like the
  root, with a read branch and a write branch. Any number of general components
  can be installed; each gets its own scope, so attribution never depends on
  install order.
- `GET /api/v1/workspace` carries `components`: every registered component with
  its kind, reads and writes. The page draws a component plate between the Views
  it reads and the Views it defines, gives it an ink, draws `reads` and `defines`
  branches and their port/arrow markers in that ink, tints the spine of the View
  it produces, and lists the
  components in a legend with their read/write counts. Hovering an edge or a
  legend entry spotlights one component and dims the rest. Clicking a component
  opens its facts.
- **Editing is in place.** `Builder::text_view::<V>(read, write)` registers a
  text channel per View (`GET/POST /api/v1/views/{key}/text`). Double-clicking a
  View opens its detail panel; its Edit control opens the same editor as the
  plate's Edit control. A keystroke commits at once, the host
  stages the text edit, the framework's lexer and parser components read the
  delta, the runtime stages only the units that differ, and the reply (`29
  chars · r4`, or the parser's error) replaces the hint under the editor. The editable View is a source document or a source
  collection; the demo edits its `SourceFile` text and its weights list (one
  number per line) through the same channel. A syntax tree is derived from the
  pipeline and is never editable.
- **The change is visible.** A card whose content changed in the current
  revision carries `data-blink` and takes a one-shot ring animation; rows whose
  units changed animate their background; an edge whose target changed draws
  itself as a flowing dash for the same beat.
- **No action buttons.** The named-action surface is gone: the page edits Views
  and nothing else. A host that wants an edit exposes a text channel for it.
- **The compiler pipeline is on the board, with no exception.** The demo and the
  JSON/STLC hosts install `irrigation-syntax`'s `TextComponent`, `LexerComponent`,
  and `ParserComponent`, plus a root that demands their output Views. The board
  draws `SourceFile → TextComponent → Text → LexerComponent → Tokens →
  ParserComponent → AstTree/AstMeta` as ordinary nodes and branches.
  `SourceFile` is the editable View: typing re-runs the chain, because the root
  demands the published tree.
- **Where a change is, the runtime says.** A changed row gets one short sweep and
  then keeps a single hairline — no wash, no repeated label — until the next
  revision. The projection derives each row's unit from the field and entity it
  draws (a token's own value unit, a map entry's value, a tree leaf's payload
  slot, a graph node's payload), so the page sweeps just those rows. A derived
  tree keeps its node units in its tag/field/children slots, so the projection
  treats a tree as per-entity when the region has entity units. Fields with no
  per-element identity (a whole value) mark nothing per row; the plate says
  `whole value at rN` instead of pretending.

```sh
cargo run --example observatory_syntax -p irrigation --features observatory
# source → text → lexer → parser → ast, each a component
```

Nothing in the page names a grammar, a lexer or a parser: the structure is the
runtime's registry plus the units each component read and defined.

## Deliberate limits (v1 scope)

These are not oversights; each has a reason and a path forward.

- **Per-cell editors.** The input contract is text in, text out, one channel per
  View; the host decides the syntax. The page does not edit an individual cell of
  a list/map/set through a typed `POST /api/v1/units`, which would need a field
  type schema on the wire because values are type-erased today.
- **Per-row change marks on source trees.** A source tree stages one content
  value, so no per-node units exist to mark; the module reports `whole value`
  and the tree rows are display rows. Per-node marks need the editor to stage
  per-node units (or a derived tree producer, which already works).
- **View-level cutoff attribution for general components.** The tape counts the
  cutoffs and Runtime mode names the cut nodes; the View header says `clean`
  because the general commit path does not name the units it redefined. Scalar
  components do attribute it.
- **Historical row-level replay.** The tape replays revision-level activity and
  suppresses row marks for older revisions, because a row mark would use current
  unit versions against an old revision.
- **Runtime-mode layout for very large graphs.** The canvas caps at 150 nodes,
  filters to revision activity plus one hop, and offers no zoom/pan/clustering.
- **Tree/list virtualization.** Row caps (250/200) bound the DOM; there is no
  windowing. The caps are the mitigation the plan asked for.
- **`pnpm` naming.** The workspace builds offline with npm scripts (`npm test`,
  `npm run build`, `node scripts/*.mjs`); the plan said pnpm + Playwright, the
  shipped scripts use npm and playwright-core with the cached Chromium.
- **Host actions were removed.** An earlier pass exposed named actions as
  buttons; the board now edits source Views in place and has no action panel.
- **Plate heights are the level's.** A plate's height comes from the View's
  projection, and every expanded plate in a level takes the level's tallest
  height, so the band has one top and one bottom edge. A body taller than its
  band scrolls. Measuring every rendered plate would make the band follow the
  DOM exactly instead of its model.
- **Five component inks.** A sixth component reuses the first ink; the legend
  still names it, and the edge label carries the component's name.

## Known sharp edges

- A whole-value field (a document, a derived list) has no per-row identity, so
  its rows are display rows and the plate says `whole value at rN`.
- Rows in a whole-value render are not selectable as units; the inspector says
  so explicitly.
- The source document is the only editable syntax input. The demo's AST and the
  STLC AST are derived Views, published by the parser and read-only; the parser's
  node identity is its own, not a host edit's.
- Two HTTP servers left over from earlier runs can hold the port; the example
  accepts `--bind` so a second instance can use another address.
- The module render cache retries once when the server hands back a render
  older than the View header. A render *newer* than the header is accepted and
  the next workspace frame catches the header up, so a fetch cannot spin.
