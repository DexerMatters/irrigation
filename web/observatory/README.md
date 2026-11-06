# Observatory page

The React board for [`reactive-observe`](../../crates/reactive-observe). Two hosts
serve it from `dist/`, and both are ordinary runtime programs — the page has no
special knowledge of either:

```sh
# JSON: lexer and parser as general components, and a sink that walks the
# published tree into a keyed table of paths.
cargo run --example observatory_json -p reactive --features observatory -- --bind 127.0.0.1:7791

# The same pipeline over a smaller grammar, without a sink.
cargo run --example observatory_syntax -p reactive --features observatory -- --bind 127.0.0.1:7790

# A small language: a root that invokes five components, two sources, three
# derived Views.
cargo run --example observatory -p reactive --features observatory -- --bind 127.0.0.1:7777
```

The page is one canvas: Views as plates, components as nodes, an edge per
recorded read or definition, a tape of revisions, and in-place text editing for
source Views with a live delta.

```sh
npm install
npm run build      # writes dist/, which the host serves
npm run dev        # Vite on :5173, proxying /api to the host on :7788
npm test           # vitest run (reducer semantics)
npm run typecheck  # tsc --noEmit
npm run smoke      # edits a plate in place against a running host (playwright-core)
                   # SMOKE_VIEW names the card, SMOKE_TEXT the text it gets, and
                   # {{stamp}} expands to a fresh number so a re-run is a real edit
npm run a11y       # axe-core WCAG 2.1 A/AA pass plus a keyboard check
```

## Point it at your own runtime

The page is served by any host that can build a `reactive_observe::Observatory`.
The smallest one is a `main`:

```rust
use reactive_observe::{Observatory, ObserveLevel, observed_runtime};

fn main() -> std::io::Result<()> {
    let (runtime, recorder) = observed_runtime(ObserveLevel::Nodes);
    // install your components, seed your Views …
    Observatory::builder(runtime, recorder)
        .serve()
}
```

That already gives an honest board with **no renderer at all**: every View
appears as a plate with its rows, every component as a chip, every recorded read
and definition as a wire. The server draws each View from the runtime's facts —
the field's shape, its entities in their own order, and the revision each unit
moved at — and spells values with the element type's `Debug` form. A plate drawn
that way says `generic` in its footer.

Register a renderer when you want a *better* drawing than the facts can give:

| call | what it buys |
|---|---|
| `.renderer::<V>(\|snapshot\| RenderModel::…)` | a host drawing: Tree/List/Map/Set/Scalar/Reduced/Graph, your labels and grouping |
| `.field_renderer::<V>(…)` | several fields, switchable on the plate |
| `.formatter::<V>(\|snapshot\| ValuePreview { … })` | the facts panel's summary for a View |
| `.text_view::<V>(read, write)` | Edit on the plate commits text back to the host |

A View that cannot be `Debug` opts out with `#[view(derived, hide)]`: its plate
keeps rows and change marks and spells no values.

Three rules make the board tell the truth rather than just look right:

1. **Cite a unit per row.** A row built with `UnitRef::entity(...)` or
   `UnitRef::from_key(&tree_units::tag(...))` is checked against the runtime, so it
   can say `changed at r12`. A row without one is a display row, and the plate
   says `whole value` instead of pretending.
2. **Demand after a write.** The page never demands: a write handler that stages
   an edit and returns leaves the derived Views unrun, and their plates sit at
   `suspect` until something asks. Call `read_outputs` (or `read_outputs_general`)
   in the handler, as every host here does.
3. **Send one edit, not a new document.** `TextDocument::replace_with(&text)`
   diffs the buffer against what you hold, so the lexer and parser stay local.
   `TextDocument::new(text)` throws the ancestry away and everything relexes.

A complete, runnable version of that file is
[`examples/observatory_min.rs`](../../crates/reactive/examples/observatory_min.rs)
— two Views, one component, and no renderer at all. Start there and delete what
you do not need.

Run it from the repository root and the built page is found automatically
(`OBSERVATORY_DIST` overrides). For a host outside this workspace, build the page
once (`npm install && npm run build`) and point `OBSERVATORY_DIST` at it — the
server says so in plain text if it cannot find it.

### Small things that make it pleasant

- `--bind <addr>` is read by `.bind_from_args()`; two hosts can run at once.
- `Fit` (or `f`), `Tidy`, wheel to pan, ⌘/ctrl + wheel to zoom; a plate drags and
  remembers where it landed.
- The page is dark by default and prints nothing about itself: open the URL and
  read the plate titles.
- With `ObserveLevel::Summary` the board still gets revisions and counts, but no
  per-node detail; `Full` records everything and is the slowest.

## The board

The layout is the runtime's own structure: View → component for every recorded
read, component → View for every definition. Ranks come from the longest path,
order inside a rank from the mean position of neighbours, and the ranks fold into
as many columns as the surface needs — the same graph is three strips on a wide
window and two on a tablet. Every column starts on one line, every plate in a
rank shares a bottom edge, and a rank that holds one node sits under what it
reads.

A View is a plate; a component is a chip on the wire between them. There is one
chip design for every family — a root, a general component and an invoked one are
the same object to the runtime, so the board draws them the same, and the family
is a word on the chip. A plate's dot is the ink of the component that defines it;
a View the host writes has no such component and takes the neutral ink.

The board is dark by default, one ground with one accent on the data: the plates
and their rows carry the colour, the chrome stays neutral.

A plate drags itself and remembers where it landed; `Tidy` forgets the dragged
positions and lays the board out again. Wheel pans, ⌘/ctrl + wheel zooms at the
pointer. Dragging never selects text — the board is an instrument, not a
document; text is selectable in the editor and the facts panel.

## Annotations

Nothing is marked unless the runtime says so.

| Mark | Means |
|---|---|
| A signal bar down a plate's left edge, and a row's leading edge | the runtime reports that unit changed at the revision on screen |
| `3 of 33 rows at r2` | the rows this render drew as changed, against what it drew |
| `whole value at r2` | the View's content changed and it has no per-row identity |
| `ran at r2, same content` | the component ran, and published what was already there |
| `ran at r2` on a chip | this component executed at the revision on screen |
| A solid edge | the component defines that View; a dashed edge: it reads one |

Selecting an older revision re-annotates the board from the journal, and the tape
says which revision is on screen. Rows are only marked for the newest revision,
because a row's unit version describes "now".

## What the page does not do

- It never demands computation. Every fetch is read-only; the runtime's
  `inspect_snapshot`, `probe`, and `view_facts` cannot evaluate a node.
- It never claims per-row identity for a whole-value render. Rows without a unit
  are display rows, and the plate's footer says `whole value`.
- It never derives an old change from a current node field: rows are marked only
  for the newest revision.
- It never invents a value. A View without a formatter shows its unit kind and
  size, not a guess.
- It never parses the host's language. The editor sends text, shows the host's
  reply or its error verbatim, and leaves the committed document untouched when
  the host rejects it.

## Structure

| Path | Responsibility |
|---|---|
| `src/state/store.ts` | external store, SSE application, render cache, placement persistence |
| `src/state/reduce.ts` | pure frame reduction (revision bookkeeping, gap detection) |
| `src/state/types.ts` | the wire contract, mirroring `reactive-observe::wire` |
| `src/canvas/Canvas.tsx` | the surface: camera, fit, drag, edges, the revision on screen |
| `src/canvas/layout.ts` | ranks, ordering, column folding, plate sizes, edge geometry |
| `src/canvas/ViewCard.tsx` | one View: fields, native render, change annotation, editor |
| `src/canvas/ComponentCard.tsx` | one component: family, reads/writes, when it ran |
| `src/components/Render.tsx` | native drawings: tree, list, map, set, graph, scalar, reduced, opaque |
| `src/components/Tape.tsx` | revision tape and the exact reaction strip |
| `src/components/Inspector.tsx` | facts, dependencies, View and component metadata |
| `src/design/` | tokens and layout |
