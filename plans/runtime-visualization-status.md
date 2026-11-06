# Runtime visualization: implementation status

This file records what of [`runtime-visualization-plan.md`](runtime-visualization-plan.md)
is built, where the evidence is, and where the code deliberately diverges from
the written plan. It is conservative: a feature counts only when a test or a
captured run exercises it.

## Shipped

| Plan area | Status | Evidence |
|---|---|---|
| §4.2 `observe` feature, levels, fixed observer | built | `crates/reactive-runtime/src/observe.rs`; `cargo check -p reactive-runtime` with the feature off and on |
| §4.3 event stream | built, amended (see divergences) | `RuntimeEvent`, emitted from edit commit, invalidation, demand, eval, commit, fail, wait/wake, notifications, collection |
| §4.4 instrumentation discipline | built | every emit is outside the metadata lock; the recorder is a bounded `ArrayQueue` with a drop counter |
| §4.5 demand causes | built as `HostRead`, `HostCall`, `HostSnapshot`, `ObservationRefresh` | `crates/reactive-runtime/src/task.rs`; `demand.rs` |
| §4.6 counters | built | `RuntimeStats::{validation_attempts, validation_failures, execution_failures, cutoffs, suspensions, demands, notifications}` |
| §4.8 projection | built | `Runtime::{view_facts, probe, unit_versions, unit_changed_at, inspect_snapshot, try_explain_node}` |
| §4.8 granularity | built from installed units | `granularity_of` in `inner.rs`; `granularity_follows_installed_units` test |
| §4.9 value previews | built (host formatters only) | `ValuePreview`; opaque fallback in `project::node_detail` |
| §4.10 journal | built | `crates/reactive-observe/src/journal.rs` with eviction, caps, cutoff views, no-op slot |
| §4.11 server | built with `std` HTTP, same wire protocol | `crates/reactive-observe/src/http.rs`; `tests/server.rs` |
| host input (source + actions) | built, generic | `GET/POST /api/v1/source`, `GET /api/v1/actions`, `Builder::{source, input_action}`, `SourceEditor.tsx` |
| §4.12 cadence | built per-session: renders are lazy per visible module, graph projection on demand | `collector.rs`, `store.ts` |
| §4.13 frontend | rebuilt as one zoomable canvas: every View as an editable plate, component edges, a tape, facts on selection | `web/observatory/src/canvas/`; `scripts/canvas-smoke.mjs`, `scripts/a11y.mjs` |
| §5 reference host | built | `crates/reactive/examples/observatory/`; a second host with ten Views in `examples/observe_board.rs` |
| §6.1 event truth | built | `crates/reactive-observe/tests/events.rs` (10 tests) |
| §6.2 server truth | built | `crates/reactive-observe/tests/server.rs` (12 tests, incl. source + action discovery); `tests/scale.rs` (4 tests at 50k nodes) |
| §6.3 frontend truth | built | `npm test` (7 reducer tests), `tsc --noEmit`, headless screenshots |

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
| G7 overhead | met | `cargo run --release --example observe_cost -p reactive --features observatory`: off 4073 µs, summary +2.6%, nodes +0.5%, full +0.4% on the 10k point-edit workload |
| G8 off | met | `cargo check`/`test` with the feature excluded; observe code is `#[cfg]`-gated |
| G9 security | met | loopback default, token test, no unformatted payloads |
| G10 scale | met | `cargo run --example observe_board -p reactive --features observatory` renders ten Views across all module kinds; `tests/scale.rs` builds a 50 000-node runtime and asserts the graph is capped with an exact `total_nodes`, a scoped query returns exactly its scope, a cap-ignoring renderer is cut at 200 rows with the real total, and the workspace payload stays under 8 KB. Tree/list virtualization is replaced by row caps (250/200), which is the bound the plan asked for |
| G11 failure | met | render errors and dropped unit claims are visible per module; capped/incomplete activity is stated on the tape |
| G12 build | met | 394 Rust tests, 5 example tests, `npm test`, `npm run build`, `node scripts/editor-smoke.mjs`, `node scripts/a11y.mjs` |
| G13 native | met | `every_model_kind_keeps_its_own_tag` plus a renderer per kind |
| G14 granularity | met | `granularity_follows_installed_units`, `views_report_identity_and_granularity`, the server drops whole-value claims |
| G15 modular | met | collapse/expand/rail with persisted layout, `Collapse all`, headers keep reporting state |

## Verification evidence

- `cargo test --workspace --no-fail-fast`: 391 tests pass, 0 failures.
- `cargo test --example observatory -p reactive --features observatory`: 5 source
  language tests pass (round trip, line-numbered errors, nested calls, comments,
  empty program).
- `cargo run --release --example observe_cost -p reactive --features observatory`
  measures the overhead gate.
- `npm test` in `web/observatory`: 7 reducer tests (transaction attribution,
  cutoff vs execution, no-op, sequence gap, duplicate frame, ordering).
- `node scripts/canvas-smoke.mjs` against a running host: opens a source plate's
  editor, types, waits — no submit — and asserts a new revision, the commit
  reply inside the plate (`3 statements · r6`), changed plates carrying the blink
  marker, and no console errors.
- `node scripts/a11y.mjs`: axe-core WCAG 2.1 A/AA on the board — zero violations;
  the script also tabs to a plate's Edit control and opens the editor with Enter.
- The board's design was measured with the `design-taste-frontend` inspector
  (`inspect.mjs --app`): **0 FAIL** at 1440/768/390 after the type floor was
  raised to 11px, an `h1` added, the toolbar taken out of the canvas surface, and
  the phone width fitted. The three remaining warnings are defensible: the pale
  component spines (~1.5:1) are data, not a frame; plates on a free canvas are not
  a row of cards that must share a bottom edge; the board's density is the point.
- `node scripts/a11y.mjs`: axe-core WCAG 2.1 A/AA — zero violations in all three
  modes, on both the three-View demo and the ten-View board; keyboard collapse
  verified.
- `cargo test -p reactive-observe --test scale`: 50 000 nodes, capped graph with
  an exact total, exact scoped drill-down, capped render with the real total.
- `cargo test -p reactive --test gates deep_recursion`: passes in debug. The
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
   the host formatter's type instead of fabricating one.
5. **The server is `std::net` + JSON, not Axum.** The checkout builds offline
   and Axum is not vendored; the endpoints and SSE framing match the plan.
6. **Demand cause `ObservationRefresh` replaces `Subscription { id }`.** One
   refresh serves every subscription; per-view detail lives in
   `SubscriptionNotified`.
7. **Whole-value renders drop unit claims silently.** A renderer may cite units
   for every row; when the field has no per-element identity the server renders
   them as display rows. Only a claim on a *per-entity* field that the runtime
   cannot back sets `inconsistent`.

## The board

The page is one surface, not a form:

- **One canvas.** Every View is a plate placed by a layered layout (a source on
  the left, what it feeds on the right) and can be dragged; the background pans,
  ⌘/ctrl + wheel zooms at the pointer, and `Fit` frames the board. Positions and
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
  branches in that ink, tints the spine of the View it produces, and lists the
  components in a legend with their read/write counts. Hovering an edge or a
  legend entry spotlights one component and dims the rest. Clicking a component
  opens its facts.
- **Editing is in place.** `Builder::text_view::<V>(read, write)` registers a
  text channel per View (`GET/POST /api/v1/views/{key}/text`). Double-clicking an
  editable plate opens a textarea in it; a keystroke commits after 200 ms, the
  host parses the text, the runtime stages only the units that differ, and the
  reply (`3 statements · r4`, or the parser's line-numbered error) replaces the
  hint under the editor. A View of any kind can be the editable one — the demo
  edits a tree (program text) and a list (one number per line) through the same
  channel.
- **The change is visible.** A card whose content changed in the current
  revision carries `data-blink` and takes a one-shot ring animation; rows whose
  units changed animate their background; an edge whose target changed draws
  itself as a flowing dash for the same beat.
- **No action buttons.** The named-action surface is gone: the page edits Views
  and nothing else. A host that wants an edit exposes a text channel for it.
- **The compiler pipeline is on the board, with no exception.** The
  `observatory_syntax` example installs `reactive-syntax`'s own components —
  `TextComponent`, `LexerComponent`, `ParserComponent`, `AstComponent` — plus a
  root that demands their output Views, over a **JSON grammar** and a JSON
  document. The board draws
  `SourceFile → TextComponent → Text → LexerComponent → Tokens →
  ParserComponent → CstTree` and `Tokens → AstComponent → AstTree/AstMeta` as
  ordinary nodes and branches. `SourceFile` is the editable View: typing re-runs
  the chain, because the root demands both published trees.
- **Where a change is, the runtime says.** A changed row gets one short sweep and
  then keeps a single hairline — no wash, no repeated label — until the next
  revision. Renderers cite the unit of each row they draw (a token's own text unit, a tree node's payload slot, a map entry),
  so the page can sweep just those rows. A derived tree keeps its node units in
  its tag/field/children slots, so the projection treats a tree as per-entity
  when the region has entity units — that is what lets CST/AST rows mark
  themselves. Fields with no per-element identity (a whole value) mark nothing
  per row; the plate says `content rN` instead of pretending.

```sh
cargo run --example observatory_syntax -p reactive --features observatory
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
- **Plate heights are estimates.** A plate's height comes from the View's
  element count; a render that draws more rows than its field's count (a
  reduction's aggregate row) gets a little slack and scrolls if it overflows.
  Measuring the rendered plate would remove the slack.
- **Five component inks.** A sixth component reuses the first ink; the legend
  still names it, and the edge label carries the component's name.

## Known sharp edges

- The demo's `Syntax` tree is a source View, so it is `whole_value`. The plan's
  screenshots describe it as per-node; the shipped page says `whole value` and
  explains why. That is the correct behaviour until source trees stage units.
- Rows in a whole-value render are not selectable as units; the inspector says
  so explicitly.
- A source edit rebuilds the tree (`set_root`), so node identity is fresh. Host
  edits that preserve identity (`replace_expression`) keep it, and the demo's
  `reference-z`/`rename-y` actions exercise that path.
- Two HTTP servers left over from earlier runs can hold the port; the example
  accepts `--bind` so a second instance can use another address.
- The module render cache retries once when the server hands back a render
  older than the View header. A render *newer* than the header is accepted and
  the next workspace frame catches the header up, so a fetch cannot spin.
