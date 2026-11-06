import type { ComponentWire, Placement, ViewRender, ViewWire } from "../state/types";

/**
 * The board's geometry.
 *
 * A View and a component are both nodes; the only thing between them is an
 * edge the runtime recorded. The layout is therefore layered — rank by longest
 * path, order inside a rank by the mean position of its neighbours — and then
 * folded into columns, so a long pipeline reads as a page rather than one tall
 * strip.
 *
 * Nothing here knows about a particular host: the nodes are the Views and
 * components the runtime reports, and the edges are its recorded reads and
 * definitions.
 */

export type Box = { width: number; height: number };

const HEAD = 30;
const FOOT = 22;
const ROW = 22;
const BODY_PAD = 8;

/** Horizontal gap between two nodes in one rank. */
export const GAP_X = 56;
/** Vertical gap between two ranks, and between two shelves of a column. */
export const GAP_Y = 48;
/** Gap between two columns of shelves. */
export const COLUMN_GAP = 132;

const clamp = (value: number, low: number, high: number) =>
  Math.min(high, Math.max(low, value));

/** Rows a render actually draws, so a plate fits its renderer's output. */
function drawnRows(render: ViewRender | undefined): number | null {
  const model = render?.fields[0]?.model;
  if (!model) return null;
  switch (model.kind) {
    case "tree":
    case "list":
    case "map":
      return model.rows.length;
    case "set":
      return model.members.length;
    case "scalar":
      return model.rows.length;
    case "reduced":
      return model.sample.length + 1;
    default:
      return null;
  }
}

/** Card size from what the View actually holds. */
export function cardSize(view: ViewWire, render?: ViewRender): Box {
  const field = view.fields[0];
  const kind = field?.kind ?? "opaque";
  // A renderer knows how many rows it drew; the field's element count is only
  // a fallback, and it is wrong for scalar Views whose renderer draws a table.
  const rows = drawnRows(render) ?? field?.elements ?? null;
  const rowHeight = kind === "reduced" ? 28 : ROW;
  const body =
    rows === null
      ? 96
      : kind === "set"
        ? clamp(Math.ceil(rows / 3) * ROW, 30, 5 * ROW)
        : clamp((rows + 1) * rowHeight, 30, 14 * rowHeight);
  switch (kind) {
    case "tree":
      return { width: 372, height: HEAD + body + BODY_PAD + FOOT };
    case "graph":
      return { width: 372, height: 268 };
    default:
      return { width: 316, height: HEAD + body + BODY_PAD + FOOT };
  }
}

/**
 * A component chip is a fixed size: it carries a name and a count, not data. It
 * is deliberately the size of a label so the board reads as Views joined by
 * wired components rather than a grid of equal boxes.
 */
export const COMPONENT_BOX: Box = { width: 178, height: 40 };

export type NodeId = string;
export const viewNode = (key: string): NodeId => `view:${key}`;
export const componentNode = (key: string): NodeId => `component:${key}`;

export type BoardEdge = {
  from: NodeId;
  to: NodeId;
  /** The component that owns the edge. */
  component: string;
  kind: "reads" | "writes";
  label: string;
};

export type Board = {
  /** Where every node goes, before any drag is applied. */
  placement: Map<NodeId, Placement>;
  sizes: Map<NodeId, Box>;
  edges: BoardEdge[];
};

export function shortName(name: string): string {
  return name.split("::").pop() ?? name;
}


/**
 * Splits the shelves into columns so the board fits its surface best.
 *
 * Every split keeps the reading order, and the column count is chosen by the
 * scale the whole board would need: a board that fits at half size is worse than
 * one that fits at four fifths, whatever the column count. The partition itself
 * is the one whose tallest column is shortest (a dynamic program over the cut
 * points), so no column is left with a tail.
 */
function fold<T extends { width: number; height: number; rank: number }>(
  shelves: T[],
  viewport: { width: number; height: number },
): { shelves: T[]; width: number; height: number }[] {
  const build = (cuts: number[]) => {
    const columns: { shelves: T[]; width: number; height: number }[] = [];
    let start = 0;
    for (const cut of [...cuts, shelves.length]) {
      const run = shelves.slice(start, cut);
      if (run.length === 0) continue;
      columns.push({
        shelves: run,
      width: run.reduce((widest, shelf) => Math.max(widest, shelf.width), 0),
        height: run.reduce(
          (total, shelf, index) => total + shelf.height + (index > 0 ? GAP_Y : 0),
          0,
        ),
      });
      start = cut;
    }
    return columns;
  };
  const score = (columns: ReturnType<typeof build>) => {
    const width =
      columns.reduce((total, column) => total + column.width, 0) +
      COLUMN_GAP * Math.max(0, columns.length - 1);
    const height = columns.reduce((tallest, column) => Math.max(tallest, column.height), 0);
    // The scale the board would need on this surface; bigger is better.
    return Math.min(viewport.width / Math.max(width, 1), viewport.height / Math.max(height, 1));
  };
  // The partition that minimises the tallest column, for exactly `count`
  // columns: dp[i][j] is the best of the first i shelves in j columns.
  // A cut is legal only between two ranks: the rows of one wrapped rank belong
  // together, and splitting them across columns reads as a mistake.
  const canCut = (at: number) =>
    at > 0 && at < shelves.length && shelves[at]?.rank !== shelves[at - 1]?.rank;
  const partition = (count: number): number[] => {
    const n = shelves.length;
    if (count >= n) return [];
    const prefix = (from: number, to: number) =>
      shelves
        .slice(from, to)
        .reduce((total, shelf, index) => total + shelf.height + (index > 0 ? GAP_Y : 0), 0);
    const best: number[][] = Array.from({ length: count + 1 }, () =>
      Array.from({ length: n + 1 }, () => Number.POSITIVE_INFINITY),
    );
    const from: number[][] = Array.from({ length: count + 1 }, () =>
      Array.from({ length: n + 1 }, () => 0),
    );
    best[0]![0] = 0;
    for (let column = 1; column <= count; column += 1) {
      for (let end = 1; end <= n; end += 1) {
        for (let start = 0; start < end; start += 1) {
          if (start > 0 && !canCut(start)) continue;
          const previous = best[column - 1]![start] ?? Number.POSITIVE_INFINITY;
          const candidate = Math.max(previous, prefix(start, end));
          if (candidate < (best[column]![end] ?? Number.POSITIVE_INFINITY)) {
            best[column]![end] = candidate;
            from[column]![end] = start;
          }
        }
      }
    }
    const cuts: number[] = [];
    let end = n;
    for (let column = count; column > 1; column -= 1) {
      const start = from[column]![end] ?? 0;
      cuts.unshift(start);
      end = start;
    }
    return cuts.filter((cut) => cut > 0 && cut < n);
  };

  // A phone gets one column: a graph folded into two strips at 390px is a
  // postage stamp, while one column of full-width plates is a list you scroll.
  if (viewport.width < PHONE_BREAKPOINT) {
    return build([]);
  }
  const limit = Math.min(5, shelves.length);
  const candidates: { columns: ReturnType<typeof build>; value: number }[] = [];
  for (let count = 1; count <= limit; count += 1) {
    const columns = build(partition(count));
    candidates.push({ columns, value: score(columns) });
  }
  const best = Math.max(...candidates.map((candidate) => candidate.value));
  // Fewer columns wins unless the extra one is clearly better: a card growing a
  // few pixels must not reshuffle the whole board.
  const enough = candidates.find((candidate) => candidate.value >= best * 0.94);
  return enough?.columns ?? candidates[0]?.columns ?? build([]);
}

/**
 * Lays the runtime's own structure out: View → Component (reads) and
 * Component → View (writes). A component sits between the Views it reads and
 * the Views it defines, because that is the only position the edges allow.
 */
export function layoutBoard(
  views: ViewWire[],
  components: ComponentWire[],
  renders: Record<string, ViewRender>,
  viewport: { width: number; height: number },
): Board {
  const viewsByKey = new Map(views.map((view) => [view.view, view]));
  const sizes = new Map<NodeId, Box>();
  const labels = new Map<NodeId, string>();

  for (const view of views) {
    const id = viewNode(view.view);
    sizes.set(id, cardSize(view, renders[view.view]));
    labels.set(id, shortName(view.name));
  }
  for (const component of components) {
    const id = componentNode(component.component);
    sizes.set(id, COMPONENT_BOX);
    labels.set(id, shortName(component.name));
  }

  const outgoing = new Map<NodeId, NodeId[]>();
  const incoming = new Map<NodeId, NodeId[]>();
  for (const id of sizes.keys()) {
    outgoing.set(id, []);
    incoming.set(id, []);
  }
  const edges: BoardEdge[] = [];
  const connect = (edge: BoardEdge) => {
    if (!sizes.has(edge.from) || !sizes.has(edge.to)) return;
    edges.push(edge);
    outgoing.get(edge.from)?.push(edge.to);
    incoming.get(edge.to)?.push(edge.from);
  };
  for (const component of components) {
    for (const read of component.reads) {
      if (!viewsByKey.has(read)) continue;
      connect({
        from: viewNode(read),
        to: componentNode(component.component),
        component: component.component,
        kind: "reads",
        label: shortName(component.name),
      });
    }
    for (const written of component.writes) {
      if (!viewsByKey.has(written)) continue;
      connect({
        from: componentNode(component.component),
        to: viewNode(written),
        component: component.component,
        kind: "writes",
        label: shortName(component.name),
      });
    }
  }

  // Longest path from a source. A cycle cannot happen in a working graph, but an
  // in-progress one must not spin here: the passes are bounded.
  const rank = new Map<NodeId, number>();
  for (const id of sizes.keys()) rank.set(id, 0);
  for (let pass = 0; pass < sizes.size; pass += 1) {
    let moved = false;
    for (const edge of edges) {
      const next = (rank.get(edge.from) ?? 0) + 1;
      if (next > (rank.get(edge.to) ?? 0)) {
        rank.set(edge.to, next);
        moved = true;
      }
    }
    if (!moved) break;
  }

  const ranks = new Map<number, NodeId[]>();
  for (const id of sizes.keys()) {
    const level = rank.get(id) ?? 0;
    const bucket = ranks.get(level) ?? [];
    bucket.push(id);
    ranks.set(level, bucket);
  }
  const levels = [...ranks.keys()].sort((left, right) => left - right);
  for (const level of levels) {
    ranks.set(
      level,
      (ranks.get(level) ?? []).sort((left, right) =>
        (labels.get(left) ?? "").localeCompare(labels.get(right) ?? ""),
      ),
    );
  }

  // Order inside a rank by the mean position of its neighbours, one sweep down
  // and one up: enough to remove the crossings a naive order creates, cheap
  // enough to run whenever a card changes size.
  const order = new Map<NodeId, number>();
  const reindex = (ids: NodeId[]) => ids.forEach((id, index) => order.set(id, index));
  levels.forEach((level) => reindex(ranks.get(level) ?? []));
  const barycentre = (id: NodeId, neighbours: Map<NodeId, NodeId[]>): number => {
    const around = neighbours.get(id) ?? [];
    if (around.length === 0) return order.get(id) ?? 0;
    const total = around.reduce((sum, other) => sum + (order.get(other) ?? 0), 0);
    return total / around.length;
  };
  const byCentre = (left: { id: NodeId; key: number }, right: { id: NodeId; key: number }) =>
    left.key - right.key ||
    (labels.get(left.id) ?? "").localeCompare(labels.get(right.id) ?? "");
  for (let sweep = 0; sweep < 2; sweep += 1) {
    for (const level of levels) {
      const ids = ranks.get(level) ?? [];
      ranks.set(
        level,
        ids
          .map((id) => ({ id, key: barycentre(id, incoming) }))
          .sort(byCentre)
          .map((entry) => entry.id),
      );
      reindex(ranks.get(level) ?? []);
    }
    for (let index = levels.length - 1; index >= 0; index -= 1) {
      const level = levels[index];
      if (level === undefined) continue;
      const ids = ranks.get(level) ?? [];
      ranks.set(
        level,
        ids
          .map((id) => ({ id, key: barycentre(id, outgoing) }))
          .sort(byCentre)
          .map((entry) => entry.id),
      );
      reindex(ranks.get(level) ?? []);
    }
  }

  // One shelf per rank, wrapped: a rank with more nodes than a column can hold
  // becomes several rows, because a single row of ten plates is a strip nobody
  // can read at any zoom.
  const shelfLimit = Math.min(900, Math.max(420, viewport.width * 0.6));
  const shelves: { ids: NodeId[]; width: number; height: number; rank: number }[] = [];
  for (const level of levels) {
    const ids = ranks.get(level) ?? [];
    const whole = ids.reduce((sum, id) => sum + (sizes.get(id)?.width ?? 0) + GAP_X, -GAP_X);
    // A rank of two or three plates is one row even when it is wide: a fan of
    // three outputs belongs together. Only a rank that is both long and wide is
    // broken up, because a strip of ten cannot be read at any zoom.
    if (ids.length <= 3 && whole <= shelfLimit * 1.7) {
      const height = ids.reduce(
        (tallest, id) => Math.max(tallest, sizes.get(id)?.height ?? 0),
        0,
      );
      shelves.push({ ids, width: whole, height, rank: level });
      continue;
    }
    let run: NodeId[] = [];
    let width = 0;
    const flush = () => {
      if (run.length === 0) return;
      const height = run.reduce(
        (tallest, id) => Math.max(tallest, sizes.get(id)?.height ?? 0),
        0,
      );
      shelves.push({ ids: run, width, height, rank: level });
      run = [];
      width = 0;
    };
    for (const id of ids) {
      const card = sizes.get(id)?.width ?? 0;
      if (run.length > 0 && width + GAP_X + card > shelfLimit) flush();
      width += (run.length > 0 ? GAP_X : 0) + card;
      run.push(id);
    }
    flush();
  }

  // A row of plates shares a bottom edge: every View in one rank takes the
  // tallest View's height, so a rank reads as a band and not a ragged edge. A
  // component keeps its own height; it carries no rows to balance.
  const boxes = new Map(sizes);
  for (const shelf of shelves) {
    for (const id of shelf.ids) {
      const size = boxes.get(id);
      if (!size || !id.startsWith("view:")) continue;
      // A small difference is tidied up; a large one is left alone, because the
      // band it would leave empty is the worst thing a plate can show.
      boxes.set(id, {
        width: size.width,
        height: Math.min(shelf.height, Math.round(size.height * 1.2)),
      });
    }
  }

  // Fold the shelves into columns, keeping the reading order: a column fills top
  // to bottom before the next one starts. How many columns is a question about
  // the surface the board is shown on: the same graph folds into three strips on
  // a wide window and into two on a tablet, and either way the fit is the best
  // one available.
  type Block = { shelves: typeof shelves; width: number; height: number };
  const columns: Block[] = fold(shelves, viewport);

  // Every column starts on one line. A column that ends earlier than its
  // neighbour is the shape of the graph — a shorter branch — not a layout
  // accident, and one shared top edge is what makes the board read as one grid.
  const placement = new Map<NodeId, Placement>();
  // The columns' own spans: a node may slide inside its column, never out of it,
  // because the columns are what keep two shelves from overlapping.
  const span = new Map<NodeId, { min: number; max: number }>();
  let columnX = 0;
  for (const block of columns) {
    let y = 0;
    for (const shelf of block.shelves) {
      let x = columnX + (block.width - shelf.width) / 2;
      for (const id of shelf.ids) {
        const size = boxes.get(id);
        if (!size) continue;
        placement.set(id, { x: Math.round(x), y: Math.round(y) });
        span.set(id, { min: columnX, max: columnX + block.width - size.width });
        x += size.width + GAP_X;
      }
      y += shelf.height + GAP_Y;
    }
    columnX += block.width + COLUMN_GAP;
  }

  // A rank that holds one node is a junction: a root that reads four stages, a
  // component that consumes one. Put it under what it reads, not in the middle
  // of an empty column — the edges then arrive from both sides instead of one
  // long sweep across the board.
  for (const shelf of shelves) {
    const only = shelf.ids[0];
    if (shelf.ids.length !== 1 || !only) continue;
    const size = boxes.get(only);
    const at = placement.get(only);
    if (!size || !at) continue;
    const centres = (incoming.get(only) ?? [])
      .map((parent) => {
        const parentAt = placement.get(parent);
        const parentSize = boxes.get(parent);
        return parentAt && parentSize ? parentAt.x + parentSize.width / 2 : null;
      })
      .filter((centre): centre is number => centre !== null);
    if (centres.length === 0) continue;
    const centre = centres.reduce((sum, value) => sum + value, 0) / centres.length;
    const limits = span.get(only);
    const x = Math.round(centre - size.width / 2);
    placement.set(only, {
      x: Math.max(limits?.min ?? 0, Math.min(limits?.max ?? x, x)),
      y: at.y,
    });
  }

  return { placement, sizes: boxes, edges };
}

/**
 * The board's envelope, including nodes the reader dragged out of the layout.
 * The link layer sizes itself from this, so an edge never ends at a clip box.
 */
export function boardBounds(
  placement: Map<NodeId, Placement>,
  sizes: Map<NodeId, Box>,
): { minX: number; minY: number; maxX: number; maxY: number; width: number; height: number } {
  let minX = Number.POSITIVE_INFINITY;
  let minY = Number.POSITIVE_INFINITY;
  let maxX = Number.NEGATIVE_INFINITY;
  let maxY = Number.NEGATIVE_INFINITY;
  for (const [id, at] of placement) {
    const size = sizes.get(id);
    if (!size) continue;
    minX = Math.min(minX, at.x);
    minY = Math.min(minY, at.y);
    maxX = Math.max(maxX, at.x + size.width);
    maxY = Math.max(maxY, at.y + size.height);
  }
  if (!Number.isFinite(minX)) {
    return { minX: 0, minY: 0, maxX: 640, maxY: 480, width: 640, height: 480 };
  }
  return { minX, minY, maxX, maxY, width: maxX - minX, height: maxY - minY };
}

/** Below this surface width the board is one column and fits the width only. */
export const PHONE_BREAKPOINT = 620;

/** How far outside a plate an edge starts, so its tail port clears the border. */
export const PLATE_INSET = 7;

/** A point in board space. */
export type Point = { x: number; y: number };

/** The point an edge leaves a box: a side centre, chosen by where the other is. */
type Anchor = { x: number; y: number; side: "top" | "bottom" | "left" | "right" };

/** The outward direction of each side: what "square on" means for that border. */
const NORMALS: Record<Anchor["side"], Point> = {
  top: { x: 0, y: -1 },
  bottom: { x: 0, y: 1 },
  left: { x: -1, y: 0 },
  right: { x: 1, y: 0 },
};

/**
 * The side an edge leaves a plate from: the one whose direction to the other
 * plate dominates. A plate below is met from the bottom, one beside from the
 * side, one above from the top — and the comparison is normalised by the plate's
 * own half-extents, so a wide, short plate prefers its long sides.
 */
function anchorOf(self: Placement, size: Box, other: Placement, otherSize: Box): Anchor {
  const selfCentre = { x: self.x + size.width / 2, y: self.y + size.height / 2 };
  const otherCentre = {
    x: other.x + otherSize.width / 2,
    y: other.y + otherSize.height / 2,
  };
  const dx = otherCentre.x - selfCentre.x;
  const dy = otherCentre.y - selfCentre.y;
  const across = Math.abs(dx) / Math.max(size.width / 2, 1);
  const down = Math.abs(dy) / Math.max(size.height / 2, 1);
  if (down >= across) {
    return dy >= 0
      ? { x: selfCentre.x, y: self.y + size.height, side: "bottom" }
      : { x: selfCentre.x, y: self.y, side: "top" };
  }
  return dx >= 0
    ? { x: self.x + size.width, y: selfCentre.y, side: "right" }
    : { x: self.x, y: selfCentre.y, side: "left" };
}

/**
 * One edge as a cubic. It leaves the side that faces the other plate, so a
 * pipeline reads as a continuous curve: down inside a column, across at a fold.
 * Nothing is routed around obstacles — the layout already keeps a rank clear of
 * its neighbours, and a curve that bends around every card is noise.
 */
export function edgePath(
  edge: BoardEdge,
  placement: Map<NodeId, Placement>,
  sizes: Map<NodeId, Box>,
): string | null {
  const from = placement.get(edge.from);
  const to = placement.get(edge.to);
  const fromSize = sizes.get(edge.from);
  const toSize = sizes.get(edge.to);
  if (!from || !to || !fromSize || !toSize) return null;
  const start = anchorOf(from, fromSize, to, toSize);
  const end = anchorOf(to, toSize, from, fromSize);

  // Each end leaves its own side along that side's normal, so the line meets a
  // border square on instead of grazing past it: a horizontal side gets a
  // horizontal tangent, a vertical side a vertical one.
  const out = NORMALS[start.side];
  const into = NORMALS[end.side];
  const tail = {
    x: start.x + out.x * PLATE_INSET,
    y: start.y + out.y * PLATE_INSET,
  };
  // The arrow's tip is the path's last vertex, so the head touches the plate it
  // points at.
  const head = { x: end.x, y: end.y };
  const reach = Math.max(26, Math.min(150, Math.hypot(head.x - tail.x, head.y - tail.y) * 0.45));
  const controlOut = { x: tail.x + out.x * reach, y: tail.y + out.y * reach };
  const controlIn = { x: head.x + into.x * reach, y: head.y + into.y * reach };
  return (
    `M ${tail.x} ${tail.y} ` +
    `C ${controlOut.x} ${controlOut.y}, ${controlIn.x} ${controlIn.y}, ${head.x} ${head.y}`
  );
}
