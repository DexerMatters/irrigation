import type { ComponentWire, Placement, ViewRender, ViewWire } from "../state/types";

/**
 * The board's geometry.
 *
 * A View and a component are both nodes; the only thing between them is an
 * edge the runtime recorded. The layout is therefore a top-down DAG: rank by
 * longest path, order inside a rank by the mean position of its neighbours, and
 * one band per level, all its plates on one baseline. Columns were removed on
 * purpose — wrapping a rank is what made two nodes of the same level sit at
 * different heights.
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
export const GAP_X = 44;
/** Vertical gap between two ranks: one level of the DAG. */
export const RANK_GAP = 44;

const clamp = (value: number, low: number, high: number) =>
  Math.min(high, Math.max(low, value));

/** Rows the projection actually draws, so a plate fits its content. */
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

/** The height of a plate folded down to its header. */
export const COLLAPSED_HEIGHT = 32;

/** Card size from what the View actually holds. */
export function cardSize(view: ViewWire, render?: ViewRender, collapsed = false): Box {
  const field = view.fields[0];
  // The drawing's shape, not only the declared field shape, is what is on the
  // plate: a derived field may project as a graph or a reduction.
  const kind = render?.fields[0]?.model.kind ?? field?.kind ?? "opaque";
  // The model knows how many rows it drew; the field's element count is only
  // a fallback, and it is wrong for whole-value fields.
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
      return {
        width: 372,
        height: collapsed ? COLLAPSED_HEIGHT : HEAD + body + BODY_PAD + FOOT,
      };
    case "graph":
      // A graph needs room to be read as a picture, not a row.
      return { width: 456, height: collapsed ? COLLAPSED_HEIGHT : 396 };
    default:
      return {
        width: 316,
        height: collapsed ? COLLAPSED_HEIGHT : HEAD + body + BODY_PAD + FOOT,
      };
  }
}

/**
 * A component chip is a fixed size: it carries a name and a count, not data. It
 * is deliberately the size of a label so the board reads as Views joined by
 * wired components rather than a grid of equal boxes.
 */
export const COMPONENT_BOX: Box = { width: 180, height: 40 };

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
  /** The rank of every real node, used by the minimap. */
  ranks: Map<NodeId, number>;
};

/** The identity of an edge in the wire model. Reads and writes never share it. */
export const edgeKey = (edge: BoardEdge): string =>
  `${edge.from}->${edge.to}-${edge.kind}`;

/**
 * The name a reader sees. A component is a Rust path ending in a snake_case
 * function (`resolve_program`); the board names it the way its type already
 * does — `ResolveProgram` — so the wire and the legend read as one family.
 */
export function displayName(name: string): string {
  const tail = name.split("::").pop() ?? name;
  return tail
    .split(/[_\-\s]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join("");
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
  collapsed: Record<string, boolean>,
): Board {
  const viewsByKey = new Map(views.map((view) => [view.view, view]));
  const sizes = new Map<NodeId, Box>();
  const labels = new Map<NodeId, string>();

  for (const view of views) {
    const id = viewNode(view.view);
    sizes.set(
      id,
      cardSize(view, renders[view.view], Boolean(collapsed[view.view])),
    );
    labels.set(id, displayName(view.name));
  }
  for (const component of components) {
    const id = componentNode(component.component);
    sizes.set(id, COMPONENT_BOX);
    labels.set(id, displayName(component.name));
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
        label: displayName(component.name),
      });
    }
    for (const written of component.writes) {
      if (!viewsByKey.has(written)) continue;
      connect({
        from: componentNode(component.component),
        to: viewNode(written),
        component: component.component,
        kind: "writes",
        label: displayName(component.name),
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

  // One level is one row, never two. The DAG is drawn downward: rank 0 at the
  // top, every rank a single band with its plates on one baseline, and the
  // widest band defines the centre the others share. A rank that would not fit
  // the window is panned through, not folded: folding is what makes two nodes of
  // the same level sit at different heights.
  // Every plate in a level shares the level's height, so the band has both one
  // top edge and one bottom edge. A collapsed plate keeps its fold; it is not a
  // reason to stretch the others.
  for (const level of levels) {
    const ids = ranks.get(level) ?? [];
    const expanding = ids.filter(
      (id) => id.startsWith("view:") && !collapsed[id.slice("view:".length)],
    );
    const target = expanding.reduce(
      (tallest, id) => Math.max(tallest, sizes.get(id)?.height ?? 0),
      0,
    );
    if (target === 0) continue;
    for (const id of expanding) {
      const size = sizes.get(id);
      if (!size) continue;
      sizes.set(id, { width: size.width, height: target });
    }
  }

  const rowWidth = new Map<number, number>();
  const rowHeight = new Map<number, number>();
  for (const level of levels) {
    const ids = ranks.get(level) ?? [];
    rowWidth.set(
      level,
      ids.reduce((sum, id) => sum + (sizes.get(id)?.width ?? 0) + GAP_X, -GAP_X),
    );
    rowHeight.set(
      level,
      ids.reduce((tallest, id) => Math.max(tallest, sizes.get(id)?.height ?? 0), 0),
    );
  }
  const widest = Math.max(1, ...rowWidth.values());

  const placement = new Map<NodeId, Placement>();
  let y = 0;
  for (const level of levels) {
    const ids = ranks.get(level) ?? [];
    const width = rowWidth.get(level) ?? 0;
    const height = rowHeight.get(level) ?? 0;
    let x = (widest - width) / 2;
    for (const id of ids) {
      const size = sizes.get(id);
      if (!size) continue;
      placement.set(id, { x: Math.round(x), y: Math.round(y) });
      x += size.width + GAP_X;
    }
    y += height + RANK_GAP;
  }

  // Coordinates, not just order. A rank is packed by its order, but a box also
  // wants its centre under the mean centre of what feeds it. Resolve that once,
  // top-down, so every parent is already fixed when a child chooses its place:
  // a one-to-one chain becomes one vertical line instead of a row of S curves.
  const centreOf = (id: NodeId): number => {
    const at = placement.get(id);
    const size = sizes.get(id);
    return at && size ? at.x + size.width / 2 : 0;
  };
  const placeRow = (ids: NodeId[], desired: Map<NodeId, number>) => {
    const xs: number[] = [];
    let cursor = Number.NEGATIVE_INFINITY;
    for (const id of ids) {
      const size = sizes.get(id);
      if (!size) continue;
      const ideal = (desired.get(id) ?? centreOf(id)) - size.width / 2;
      const x = Math.max(ideal, cursor);
      xs.push(x);
      cursor = x + size.width + GAP_X;
    }
    let limit = Number.POSITIVE_INFINITY;
    for (let index = ids.length - 1; index >= 0; index -= 1) {
      const id = ids[index];
      const size = id ? sizes.get(id) : undefined;
      if (!id || !size) continue;
      const x = Math.min(xs[index] ?? 0, limit - size.width);
      xs[index] = x;
      limit = x - GAP_X;
    }
    ids.forEach((id, index) => {
      const at = placement.get(id);
      const x = xs[index];
      if (!at || x === undefined) return;
      placement.set(id, { x: Math.round(x), y: at.y });
    });
  };
  for (const level of levels) {
    if (level === 0) continue;
    const ids = ranks.get(level) ?? [];
    const desired = new Map<NodeId, number>();
    for (const id of ids) {
      const parents = incoming.get(id) ?? [];
      desired.set(
        id,
        parents.length === 0
          ? centreOf(id)
          : parents.reduce((sum, parent) => sum + centreOf(parent), 0) / parents.length,
      );
    }
    placeRow(ids, desired);
  }

  return { placement, sizes, edges, ranks: rank };
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

/**
 * Keep a dragged plate out of its neighbours. The automatic layout already
 * spaces ranks, but a reader can drop one plate on another; this walks the
 * dropped plate to the nearest clear side, preferring a short horizontal push
 * so the DAG's rows stay rows.
 */
export function avoidOverlap(
  node: NodeId,
  at: Placement,
  sizes: Map<NodeId, Box>,
  placement: Map<NodeId, Placement>,
  gap = 12,
): Placement {
  const size = sizes.get(node);
  if (!size) return at;
  let next = { ...at };

  const overlaps = (other: Placement, otherSize: Box) =>
    next.x < other.x + otherSize.width + gap &&
    next.x + size.width + gap > other.x &&
    next.y < other.y + otherSize.height + gap &&
    next.y + size.height + gap > other.y;

  for (let pass = 0; pass < 24; pass += 1) {
    let hit: { at: Placement; size: Box } | null = null;
    for (const [id, other] of placement) {
      if (id === node) continue;
      const otherSize = sizes.get(id);
      if (!otherSize) continue;
      if (overlaps(other, otherSize)) {
        hit = { at: other, size: otherSize };
        break;
      }
    }
    if (!hit) break;
    const left = hit.at.x - size.width - gap;
    const right = hit.at.x + hit.size.width + gap;
    const above = hit.at.y - size.height - gap;
    const below = hit.at.y + hit.size.height + gap;
    // Prefer a horizontal escape. A vertical move between two rows will often
    // land in the next card and oscillate; a side move clears the row.
    const horizontal = [
      { x: left, y: next.y },
      { x: right, y: next.y },
    ].sort((a, b) => Math.abs(a.x - next.x) - Math.abs(b.x - next.x));
    const vertical = [
      { x: next.x, y: above },
      { x: next.x, y: below },
    ].sort((a, b) => Math.abs(a.y - next.y) - Math.abs(b.y - next.y));
    next = horizontal[0] ?? vertical[0] ?? next;
  }
  return { x: Math.round(next.x), y: Math.round(next.y) };
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
function sideOf(self: Placement, size: Box, other: Placement, otherSize: Box): Anchor["side"] {
  const selfCentre = { x: self.x + size.width / 2, y: self.y + size.height / 2 };
  const otherCentre = {
    x: other.x + otherSize.width / 2,
    y: other.y + otherSize.height / 2,
  };
  const dx = otherCentre.x - selfCentre.x;
  const dy = otherCentre.y - selfCentre.y;
  const across = Math.abs(dx) / Math.max(size.width / 2, 1);
  const down = Math.abs(dy) / Math.max(size.height / 2, 1);
  if (down >= across) return dy >= 0 ? "bottom" : "top";
  return dx >= 0 ? "right" : "left";
}

function anchorOf(self: Placement, size: Box, other: Placement, otherSize: Box): Anchor {
  const side = sideOf(self, size, other, otherSize);
  const selfCentre = { x: self.x + size.width / 2, y: self.y + size.height / 2 };
  switch (side) {
    case "top":
      return { x: selfCentre.x, y: self.y, side };
    case "bottom":
      return { x: selfCentre.x, y: self.y + size.height, side };
    case "left":
      return { x: self.x, y: selfCentre.y, side };
    case "right":
      return { x: self.x + size.width, y: selfCentre.y, side };
  }
}

/** Where one end of an edge meets a plate. */
export type EdgeEnd = { x: number; y: number; side: Anchor["side"] };
export type EdgeEnds = { start: EdgeEnd; end: EdgeEnd };

/**
 * Give every edge its own attachment point. A port is a slot along the border,
 * ordered by where the other end is: the leftmost incoming wire takes the
 * leftmost slot. A node with one edge keeps the centre, so a chain still reads
 * as a single line; a node with several no longer looks like a funnel.
 */
export function assignPorts(
  board: Board,
  placement: Map<NodeId, Placement>,
): Map<string, EdgeEnds> {
  type Attachment = { key: string; which: "start" | "end"; preferred: number };
  const groups = new Map<string, Attachment[]>();
  const result = new Map<string, EdgeEnds>();

  const push = (node: NodeId, side: Anchor["side"], attachment: Attachment) => {
    const group = groups.get(`${node}|${side}`) ?? [];
    group.push(attachment);
    groups.set(`${node}|${side}`, group);
  };

  for (const edge of board.edges) {
    const from = placement.get(edge.from);
    const to = placement.get(edge.to);
    const fromSize = board.sizes.get(edge.from);
    const toSize = board.sizes.get(edge.to);
    if (!from || !to || !fromSize || !toSize) continue;
    const startSide = sideOf(from, fromSize, to, toSize);
    const endSide = sideOf(to, toSize, from, fromSize);
    const toCentre = { x: to.x + toSize.width / 2, y: to.y + toSize.height / 2 };
    const fromCentre = { x: from.x + fromSize.width / 2, y: from.y + fromSize.height / 2 };
    const key = edgeKey(edge);
    result.set(key, {
      start: { x: fromCentre.x, y: fromCentre.y, side: startSide },
      end: { x: toCentre.x, y: toCentre.y, side: endSide },
    });
    // Ports run along the border, so "left to right" on a horizontal side is x;
    // on a vertical side it is y.
    const horizontal = startSide === "top" || startSide === "bottom";
    push(edge.from, startSide, {
      key,
      which: "start",
      preferred: horizontal ? toCentre.x : toCentre.y,
    });
    const endHorizontal = endSide === "top" || endSide === "bottom";
    push(edge.to, endSide, {
      key,
      which: "end",
      preferred: endHorizontal ? fromCentre.x : fromCentre.y,
    });
  }

  const point = (
    at: Placement,
    size: Box,
    side: Anchor["side"],
    index: number,
    count: number,
  ): EdgeEnd => {
    const inset = Math.min(18, Math.min(size.width, size.height) / 4);
    const t = count <= 1 ? 0.5 : index / (count - 1);
    if (side === "top" || side === "bottom") {
      const span = Math.max(1, size.width - inset * 2);
      return {
        x: at.x + inset + span * t,
        y: side === "top" ? at.y : at.y + size.height,
        side,
      };
    }
    const span = Math.max(1, size.height - inset * 2);
    return {
      x: side === "left" ? at.x : at.x + size.width,
      y: at.y + inset + span * t,
      side,
    };
  };

  for (const [groupKey, attachments] of groups) {
    const [node, side] = groupKey.split("|") as [NodeId, Anchor["side"]];
    const at = placement.get(node);
    const size = board.sizes.get(node);
    if (!at || !size) continue;
    attachments.sort((left, right) => left.preferred - right.preferred || left.key.localeCompare(right.key));
    attachments.forEach((attachment, index) => {
      const ends = result.get(attachment.key);
      const end = point(at, size, side, index, attachments.length);
      if (!ends) return;
      if (attachment.which === "start") ends.start = end;
      else ends.end = end;
    });
  }
  return result;
}

/** The only extra a path needs: the ports the edges share a plate with. */
export type EdgeGeometry = { ends?: EdgeEnds };

/**
 * One edge, one cubic. Each end leaves its port along that side's normal, so
 * the curve meets the plate square on; a long dependency is still a single
 * smooth bend, not a sequence of routed turns.
 */
export function edgePath(
  edge: BoardEdge,
  placement: Map<NodeId, Placement>,
  sizes: Map<NodeId, Box>,
  geometry: EdgeGeometry = {},
): string | null {
  const from = placement.get(edge.from);
  const to = placement.get(edge.to);
  const fromSize = sizes.get(edge.from);
  const toSize = sizes.get(edge.to);
  if (!from || !to || !fromSize || !toSize) return null;

  const ends =
    geometry.ends ??
    (() => {
      const start = anchorOf(from, fromSize, to, toSize);
      const end = anchorOf(to, toSize, from, fromSize);
      return { start, end };
    })();

  // Each end leaves its own side along that side's normal, so the line meets a
  // border square on instead of grazing past it.
  const out = NORMALS[ends.start.side];
  const into = NORMALS[ends.end.side];
  const tail = {
    x: ends.start.x + out.x * PLATE_INSET,
    y: ends.start.y + out.y * PLATE_INSET,
  };
  const head = { x: ends.end.x, y: ends.end.y };
  const start = geometry.ends ? `M ${ends.start.x} ${ends.start.y} L ${tail.x} ${tail.y}` : null;

  const reach = Math.max(20, Math.min(120, Math.hypot(head.x - tail.x, head.y - tail.y) * 0.42));
  const controlOut = { x: tail.x + out.x * reach, y: tail.y + out.y * reach };
  const controlIn = { x: head.x + into.x * reach, y: head.y + into.y * reach };
  const curve =
    `C ${controlOut.x} ${controlOut.y}, ${controlIn.x} ${controlIn.y}, ${head.x} ${head.y}`;
  return start ? `${start} ${curve}` : `M ${tail.x} ${tail.y} ${curve}`;
}
