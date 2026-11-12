# Observatory page

The React board for [`irrigation-observe`](../../crates/irrigation-observe). Four
hosts serve it from `dist/`, all ordinary runtime programs — the page has no
special knowledge of any of them:

```sh
# JSON: lexer and parser as general components, and a sink that walks the
# published tree into a keyed table of paths.
cargo run --example observatory_json -p irrigation --features observatory -- --bind 127.0.0.1:7791

# The same pipeline over a smaller grammar, without a sink.
cargo run --example observatory_syntax -p irrigation --features observatory -- --bind 127.0.0.1:7790

# STLC with a scope-graph resolver; the ScopeGraph View is a Graph field.
cargo run --example stlc_scope_graphs -p irrigation --features observatory -- --bind 127.0.0.1:7792

# The reference demo: the framework's text/lexer/parser over a tiny language,
# the resolver over the published AST, and a second editable source (weights).
cargo run --example observatory -p irrigation --features observatory -- --bind 127.0.0.1:7777
```

The page is one canvas: Views as plates, components as nodes, an edge per
recorded read or definition, a tape of revisions, and in-place text editing for
source Views with a live delta.

```sh
npm install
npm run build      # writes dist/, which the host serves
npm run dev        # Vite on :5173, proxying /api to the host on :7788
npm test           # vitest run (reducer and layout semantics)
npm run typecheck  # tsc --noEmit
npm run smoke      # types into a source plate's field and watches the delta (playwright-core)
                   # SMOKE_VIEW names the card, SMOKE_TEXT the text it gets, and
                   # {{stamp}} expands to a fresh number so a re-run is a real edit
npm run a11y       # axe-core WCAG 2.1 A/AA pass plus a keyboard check
```

## Point it at your own runtime

The page is served by any host that can build a `irrigation_observe::Observatory`.
The smallest one is a `main`:

```rust
use irrigation_observe::{Observatory, ObserveLevel, observed_runtime};

fn main() -> std::io::Result<()> {
    let (runtime, recorder) = observed_runtime(ObserveLevel::Nodes);
    // install your components, seed your Views …
    Observatory::builder(runtime, recorder)
        .serve()
}
```

That gives an honest board with **no host drawing at all**: every View appears
as a plate, every component as a chip, every recorded read and definition as a
wire. The server projects each View from the runtime's own facts — the View's
declared shape, its entities in their own order, and the revision each unit
moved at — and spells values through the View's own `Debug` form. There is no
second renderer to register: a graph field draws a graph, a tree field draws a
tree, a reduction draws its aggregate, and every field is a tab on its plate.

`text_view::<V>(read, write)` is the only input hook. It registers a text channel
so a plate can be edited; it does not describe how anything is drawn.

Two rules make the board tell the truth rather than just look right:

1. **Demand after a write.** The page never demands: a write handler that stages
   an edit and returns leaves the derived Views unrun, and their plates sit at
   `suspect` until something asks. Call `read_outputs` (or `read_outputs_general`)
   in the handler, as every host here does.
2. **Send one edit, not a new document.** `TextDocument::replace_with(&text)`
   diffs the buffer against what you hold, so the lexer and parser stay local.
   `TextDocument::new(text)` throws the ancestry away and everything relexes.

A complete, runnable version of that file is
[`examples/observatory_min.rs`](../../crates/irrigation/examples/observatory_min.rs)
— two Views, one component. Start there and delete what you do not need.

Run it from the repository root and the built page is found automatically
(`OBSERVATORY_DIST` overrides). For a host outside this workspace, build the page
once (`npm install && npm run build`) and point `OBSERVATORY_DIST` at it — the
server says so in plain text if it cannot find it.

### Small things that make it pleasant

- `--bind <addr>` is read by `.bind_from_args()`; two hosts can run at once.
- `Fit` (or `f`) fills the width; `All` (or `a`) fits the whole pipeline in the
  window for a structural read; the zoom readout resets to 100% (`0`). Wheel
  pans, ⌘/ctrl + wheel zooms at the pointer, and a plate is dragged by the grip
  in its top-left corner. A dropped plate is moved clear of any plate it would
  overlap, and the position survives a reload.
- **The board is laid out once and then keeps that plan.** It frames the camera
  as it first draws, and from then on neither the arrangement nor the framing
  moves on its own — not for a revision that rewrites data, not for a plate whose
  list grows, not for a plate arriving. A plate keeps the box it was given and
  its body scrolls; a newcomer takes a clear slot of its own without pushing
  anything. `Tidy` forgets the dragged positions and lays the board out again,
  and `Overview` (or `o`) folds every View to its header and tidies at that size
  — the compact whole-DAG reading, with a detail opening its content again.
- A **source View is a text field all the time**. There is no Edit button: the
  plate draws its text in place, every keystroke commits, and the footer shows
  the host's reply. The same field is in the facts panel when the plate is open
  there, so the two are one draft, not two editors.
- Click a plate's name to open its detail in the side panel (double-clicking the
  header still works). Click a row with a unit to read that unit's facts; the
  plate's rows stay selectable and the panel says which field and revision it
  moved at. The chevron in a plate's header folds it to its title, and the fold
  survives a reload. While `Overview` is on, individual chevrons are hidden and
  the whole board is header-only.
- `Map` (or `m`) shows and hides the structure minimap, and `Fit` leaves its
  corner clear while it is up. `Tab` through the board pans the camera to the
  plate it lands on, because the board clips rather than scrolls.
- The page is dark by default and prints nothing about itself: open the URL and
  read the plate titles.
- With `ObserveLevel::Summary` the board still gets revisions and counts, but no
  per-node detail; `Full` records everything and is the slowest.

## The board

The layout is the runtime's own DAG: View → component for every recorded read,
component → View for every definition. Ranks come from the longest path, order
inside a rank from the mean position of neighbours, and every rank is one
horizontal band drawn downward. Each plate is then placed over the mean centre
of its neighbours, top-down: a one-to-one chain is one vertical line, and a merge
or split fans through ports instead of converging on one point. Plates in one
band share a top edge and a height, so a level reads as a level; a wide band is
panned through rather than folded into a second row. A rank that holds one node
sits under what it reads.

A single edge is a single cubic, leaving its port along that side's normal. It
does not turn through intermediate lanes: a long dependency is one smooth wire,
and the port distribution is what keeps many relations from sharing one point.
Edges take the ink of their component, with the arrowhead and port marker
matching.

A View is a plate; a component is a chip on the wire between them. There is one
chip design for every family — a root, a general component and an invoked one are
the same object to the runtime, so the board draws them the same, and the family
is a word on the chip. A plate's dot is the ink of the component that defines it;
a View the host writes has no such component and takes the neutral ink.

The board is dark by default, one ground with component inks on the data: the
plates, rows, wires and legend carry the colour, the chrome stays neutral. The
minimap in the top-left is a rank diagram of the whole structure, not a reduced
screenshot: one row per rank, one bar per node, component-coloured wires, a
right-edge bar for the camera, and a small signal dot for a plate that changed
or a component that ran. Long wires fade so they do not become a second graph.
Click or drag it to move the camera; arrow keys nudge it. `Map` hides it, and
`Fit` leaves its corner clear while it is up.

The grip at a plate's top-left drags it and remembers where it landed; `Tidy`
forgets the dragged positions and lays the board out again. That tidy is not
repeated for the reader: the board is arranged once, when its plates have all
been measured, and a plate then keeps its slot and its box while the runtime
publishes into it — the body scrolls instead of the board growing, and the
camera stays where it was left. A plate that appears later takes a clear slot of
its own and moves nothing, which is what keeps a live board readable: a change
is something to read on the plate, never a reason for the picture to jump. Wheel
pans over the board and scrolls inside a plate; ⌘/ctrl + wheel zooms at the
pointer. A View opens in the side panel from its name, with the same projection,
change marks and live field as its plate.

## Staying live

A plate is annotated for the revision its data describes. A transaction event
lands before the workspace read it belongs to, so the board keeps its last known
marks (r21, not r22) for the one round trip until the read arrives, instead of
blanking every mark and re-labelling rows it has no counts for. Following the
newest revision, that revision is the workspace read; a selected older record is
annotated from the journal.

Renders are read behind two limits, because a source edit changes most derived
Views: only plates that are on screen are re-read (a plate scrolled out of the
surface is read when it comes back), and a burst coalesces into at most one set
every 120 ms, always against the newest workspace. The first change after a
pause is read at once, so a single keystroke updates the visible plates in one
round trip; continuous typing does not turn into ten requests per character,
which is what let the stream, the workspace reads and the host's runtime lock
queue behind the page. Marks never wait for a render: they come from the
workspace, which is the cheap read. The plate's box is built from its own chrome — a
header, a one-line field strip that scrolls sideways, the body and a footer —
and a source View gets a body tall enough for a few lines of text.

## Annotations

Nothing is marked unless the runtime says so.

The leading edge is the board's state rail: one edge with up to two lanes. A
changed plate paints it in the signal colour; a selected plate paints a solid
ink lane on its outside, and the two lanes butt together so the rail reads as a
single edge rather than two bars. A selected row does the same at row scale — an
ink lane with the signal lane of a changed unit tucked inside it — and the tint
behind it is a surface, never a fill that swallows the value.

| Mark | Means |
|---|---|
| A signal bar down a plate's left edge, and a row's leading edge | the runtime reports that unit changed at the revision on screen |
| A solid ink bar on the same edge | this plate (or row) is the one open in the facts panel |
| `3 of 33 rows at r2` | the rows this render drew as changed, against what it drew |
| `whole value at r2` | the View's content changed and it has no per-row identity |
| `ran at r2, same content` | the component ran, and published what was already there |
| `ran at r2` on a chip | this component executed at the revision on screen |
| A solid edge | the component defines that View; a dashed edge: it reads one |

Selecting an older revision re-annotates the board from the journal, and the tape
says which revision is on screen. Rows are only marked for the newest revision,
because a row's unit version describes "now". A mark is only drawn when its data
describes the revision on screen: the stream announces a revision before the
workspace read that describes it has landed, so the board waits for that read
instead of dressing the previous revision's change in the new revision's number.
For an older record a plate says `content changed at r1` — the journal knows the
View moved, but the row counts belong to the render on screen, which shows now.

## What the page does not do

- It never demands computation. Every fetch is read-only; the runtime's
  `inspect_snapshot`, `probe`, and `view_facts` cannot evaluate a node.
- It never claims per-row identity for a whole-value render. Rows without a unit
  are display rows, and the plate's footer says `whole value`.
- It never derives an old change from a current node field: rows are marked only
  for the newest revision.
- It never invents a value outside the View's own spelling. A value its type
  cannot `Debug` is listed by identity; the facts panel shows the unit kind and
  size, not a guess.
- It never parses the host's language. The editor sends text, shows the host's
  reply or its error verbatim, and leaves the committed document untouched when
  the host rejects it.

## Structure

| Path | Responsibility |
|---|---|
| `src/state/store.ts` | external store, SSE application, render cache, placement persistence |
| `src/state/reduce.ts` | pure frame reduction (revision bookkeeping, gap detection) |
| `src/state/types.ts` | the wire contract, mirroring `irrigation-observe::wire` |
| `src/canvas/Canvas.tsx` | the surface: camera, fit, drag, edges, the revision on screen |
| `src/canvas/layout.ts` | ranks, level bands, parent-aligned coordinates, collision escape, plate sizes, ports, edge geometry |
| `src/canvas/Minimap.tsx` | whole-structure thumbnail, camera window, change marks, navigation |
| `src/canvas/marks.ts` | one-shot change animations, owned by the element that has the evidence |
| `src/state/revision.ts` | which revision is on screen, and whether its data has arrived |
| `src/canvas/viewInfo.ts` | the words and counts the plate and side panel share |
| `src/canvas/ViewCard.tsx` | one View: fields, drawn model, change annotation, editor |
| `src/canvas/ComponentCard.tsx` | one component: family, reads/writes, when it ran |
| `src/components/Render.tsx` | field drawings: tree, list, map, set, scalar, reduced |
| `src/components/Graph.tsx` | the graph drawing: force layout, directed edges, labels |
| `src/components/Tape.tsx` | revision tape and the exact reaction strip |
| `src/components/Inspector.tsx` | facts, dependencies, View and component metadata |
| `src/design/tokens.css` | the dark palette, a six-step type scale, the 4px space grid, weights |
| `src/design/canvas.css` / `app.css` | the board's chrome, then the page around it; no rule is defined twice |
