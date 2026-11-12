import { useId, useMemo } from "react";
import type { GraphEdge, GraphNode, Model } from "../state/types";

type GraphModel = Extract<Model, { kind: "graph" }>;
type Point = { x: number; y: number };

/**
 * The plate a graph is drawn on. The server's model carries nodes and directed
 * edges; this file turns that into the picture a reader expects from the word
 * "graph": placed nodes, drawn wires, arrowheads, and the labels the host
 * supplied — never a column of adjacency strings.
 */

const WIDTH = 448;
const HEIGHT = 350;
const PAD = 20;
const NODE_HEIGHT = 21;

const clamp = (value: number, low: number, high: number) =>
  Math.min(high, Math.max(low, value));

function labelLines(label: string): string[] {
  const words = label.split(/\s+/).filter(Boolean);
  if (words.length === 0) return [""];
  const lines: string[] = [];
  let current = "";
  for (const word of words) {
    const candidate = current ? `${current} ${word}` : word;
    if (current && candidate.length > 24) {
      lines.push(current);
      current = word;
    } else {
      current = candidate;
    }
  }
  if (current) lines.push(current);
  if (lines.length > 2) {
    lines.length = 2;
    lines[1] = `${lines[1]?.slice(0, 23) ?? ""}…`;
  }
  return lines.map((line) => (line.length > 26 ? `${line.slice(0, 25)}…` : line));
}

function linesWidth(lines: string[]): number {
  const longest = lines.reduce((length, line) => Math.max(length, line.length), 0);
  return clamp(longest * 6.1 + 20, 40, 200);
}

type Placed = {
  node: GraphNode;
  lines: string[];
  x: number;
  y: number;
  width: number;
  height: number;
};

type EdgePath = {
  edge: GraphEdge;
  d: string;
  label: Point | null;
  changed: boolean;
};

/**
 * A deterministic force layout, then a collision pass.
 *
 * Small graphs are the target, so the simulation is the classic one — repulsion
 * between every pair, springs along edges, a little gravity — seeded on a circle
 * so the same model always lands in the same place. The collision pass is what
 * stops two long labels from sitting on top of each other; the viewBox is fitted
 * afterwards, against the node rectangles rather than their centres.
 */
function layout(model: GraphModel) {
  const ids = new Set(model.nodes.map((node) => node.id));
  const edges = model.edges.filter((edge) => ids.has(edge.from) && ids.has(edge.to));
  const nodes = model.nodes.map((node) => {
    const lines = labelLines(node.label);
    return {
      node,
      lines,
      width: linesWidth(lines),
      height: lines.length > 1 ? 31 : NODE_HEIGHT,
    };
  });
  if (nodes.length === 0) {
    return {
      placed: [] as Placed[],
      paths: [] as EdgePath[],
      width: WIDTH,
      height: HEIGHT,
    };
  }
  if (nodes.length === 1) {
    return {
      placed: [{ ...nodes[0], x: WIDTH / 2, y: HEIGHT / 2 }],
      paths: [] as EdgePath[],
      width: WIDTH,
      height: HEIGHT,
    };
  }

  const centre = { x: WIDTH / 2, y: HEIGHT / 2 };
  const radius = Math.min(WIDTH, HEIGHT) * 0.33;
  const points: Point[] = nodes.map((_, index) => {
    const angle = (index / nodes.length) * Math.PI * 2 - Math.PI / 2;
    return {
      x: centre.x + Math.cos(angle) * radius,
      y: centre.y + Math.sin(angle) * radius,
    };
  });
  const index = new Map(model.nodes.map((node, at) => [node.id, at]));
  const links = edges
    .map((edge) => ({
      edge,
      from: index.get(edge.from),
      to: index.get(edge.to),
    }))
    .filter((link): link is { edge: GraphEdge; from: number; to: number } =>
      link.from !== undefined && link.to !== undefined && link.from !== link.to,
    );

  const area = WIDTH * HEIGHT;
  const k = Math.sqrt(area / nodes.length) * 0.62;
  const iterations = nodes.length > 80 ? 140 : 260;
  const turns = nodes.length > 40 ? 18 : 34;

  for (let step = 0; step < iterations; step += 1) {
    const heat = 1 - step / iterations;
    const limit = 2 + heat * 9;
    const disp: Point[] = points.map(() => ({ x: 0, y: 0 }));

    for (let left = 0; left < points.length; left += 1) {
      for (let right = left + 1; right < points.length; right += 1) {
        let dx = points[left].x - points[right].x;
        let dy = points[left].y - points[right].y;
        let distance = Math.hypot(dx, dy);
        if (distance < 0.01) {
          // Two nodes landed on one point: split them deterministically.
          dx = ((left + 1) % 7) + 0.5;
          dy = ((right + 1) % 5) + 0.5;
          distance = Math.hypot(dx, dy);
        }
        const spread =
          (nodes[left].width + nodes[right].width) / 4 +
          (nodes[left].height + nodes[right].height) / 4;
        const push = (k * k * (1 + spread / 34)) / distance;
        const ux = (dx / distance) * push;
        const uy = (dy / distance) * push;
        disp[left].x += ux;
        disp[left].y += uy;
        disp[right].x -= ux;
        disp[right].y -= uy;
      }
    }

    for (const link of links) {
      const dx = points[link.from].x - points[link.to].x;
      const dy = points[link.from].y - points[link.to].y;
      const distance = Math.max(0.01, Math.hypot(dx, dy));
      const pull = (distance * distance) / k;
      const ux = (dx / distance) * pull;
      const uy = (dy / distance) * pull;
      disp[link.from].x -= ux;
      disp[link.from].y -= uy;
      disp[link.to].x += ux;
      disp[link.to].y += uy;
    }

    for (let at = 0; at < points.length; at += 1) {
      disp[at].x += (centre.x - points[at].x) * 0.012;
      disp[at].y += (centre.y - points[at].y) * 0.012;
      const length = Math.max(0.01, Math.hypot(disp[at].x, disp[at].y));
      const step = Math.min(limit, length);
      points[at].x = clamp(
        points[at].x + (disp[at].x / length) * step,
        nodes[at].width / 2 + 4,
        WIDTH - nodes[at].width / 2 - 4,
      );
      points[at].y = clamp(
        points[at].y + (disp[at].y / length) * step,
        nodes[at].height / 2 + 4,
        HEIGHT - nodes[at].height / 2 - 4,
      );
    }
  }

  // Rectangles, not points: resolve the overlaps the force pass leaves behind.
  for (let turn = 0; turn < turns; turn += 1) {
    let moved = false;
    for (let left = 0; left < nodes.length; left += 1) {
      for (let right = left + 1; right < nodes.length; right += 1) {
        const gapX =
          (nodes[left].width + nodes[right].width) / 2 + 8 - Math.abs(points[left].x - points[right].x);
        const gapY =
          (nodes[left].height + nodes[right].height) / 2 + 6 - Math.abs(points[left].y - points[right].y);
        if (gapX <= 0 || gapY <= 0) continue;
        moved = true;
        // Push along the axis that needs the least correction.
        const shift = Math.min(gapX, gapY) / 2 + 0.5;
        const alongX = gapX <= gapY;
        const sign =
          (alongX ? points[left].x - points[right].x : points[left].y - points[right].y) >= 0
            ? 1
            : -1;
        if (alongX) {
          points[left].x += shift * sign;
          points[right].x -= shift * sign;
        } else {
          points[left].y += shift * sign;
          points[right].y -= shift * sign;
        }
      }
    }
    if (!moved) break;
  }

  // The viewBox is the content's own bounds. Shrinking the coordinates into a
  // fixed plate would shrink the gaps while leaving the rectangles their size,
  // which is how labels end up on top of each other. The SVG scales the whole
  // picture instead, text included.
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  for (let at = 0; at < nodes.length; at += 1) {
    minX = Math.min(minX, points[at].x - nodes[at].width / 2);
    maxX = Math.max(maxX, points[at].x + nodes[at].width / 2);
    minY = Math.min(minY, points[at].y - nodes[at].height / 2);
    maxY = Math.max(maxY, points[at].y + nodes[at].height / 2);
  }
  // A floor keeps a two-node graph from being displayed at billboard size.
  const width = Math.max(240, maxX - minX + PAD * 2);
  const height = Math.max(160, maxY - minY + PAD * 2);
  const shiftX = (width - (maxX - minX)) / 2 - minX;
  const shiftY = (height - (maxY - minY)) / 2 - minY;
  const placed: Placed[] = nodes.map((node, at) => ({
    ...node,
    x: points[at].x + shiftX,
    y: points[at].y + shiftY,
  }));

  const paths = edgePaths(edges, placed);
  return { placed, paths, width, height };
}

/** The point on a rectangle's border in the direction of `toward`. */
function borderPoint(rect: Placed, toward: Point): Point {
  const dx = toward.x - rect.x;
  const dy = toward.y - rect.y;
  const halfW = rect.width / 2;
  const halfH = rect.height / 2;
  if (dx === 0 && dy === 0) return { x: rect.x, y: rect.y - halfH };
  const scale = Math.min(
    halfW / Math.max(1e-6, Math.abs(dx)),
    halfH / Math.max(1e-6, Math.abs(dy)),
  );
  return { x: rect.x + dx * scale, y: rect.y + dy * scale };
}

function edgePaths(edges: GraphEdge[], placed: Placed[]): EdgePath[] {
  const byId = new Map(placed.map((node) => [node.node.id, node]));
  const pairCount = new Map<string, number>();
  const changed = new Map(placed.map((node) => [node.node.id, node.node.state === "changed"]));
  const paths: EdgePath[] = [];
  for (const edge of edges) {
    const from = byId.get(edge.from);
    const to = byId.get(edge.to);
    if (!from || !to) continue;
    if (from === to) {
      // A self-loop leaves and re-enters the top of its own rectangle.
      const halfW = from.width / 2;
      const halfH = from.height / 2;
      paths.push({
        edge,
        d: `M ${from.x - halfW} ${from.y - halfH + 1} C ${from.x - halfW - 26} ${
          from.y - halfH - 30
        } ${from.x + halfW + 26} ${from.y - halfH - 30} ${from.x + halfW} ${from.y - halfH + 1}`,
        label: { x: from.x, y: from.y - halfH - 15 },
        changed: changed.get(edge.from) || changed.get(edge.to) || false,
      });
      continue;
    }
    const key = [edge.from, edge.to].sort().join("→");
    const count = pairCount.get(key) ?? 0;
    pairCount.set(key, count + 1);
    const start = borderPoint(from, { x: to.x, y: to.y });
    const end = borderPoint(to, { x: from.x, y: from.y });
    const dx = end.x - start.x;
    const dy = end.y - start.y;
    const length = Math.max(0.01, Math.hypot(dx, dy));
    // Parallel and antiparallel edges bow apart instead of lying on one line.
    const side = (count % 2 === 0 ? 1 : -1) * (6 + Math.floor(count / 2) * 10);
    const nx = (-dy / length) * side;
    const ny = (dx / length) * side;
    const c1 = { x: start.x + dx / 3 + nx, y: start.y + dy / 3 + ny };
    const c2 = { x: start.x + (dx * 2) / 3 + nx, y: start.y + (dy * 2) / 3 + ny };
    const mid = {
      x: 0.125 * start.x + 0.375 * c1.x + 0.375 * c2.x + 0.125 * end.x,
      y: 0.125 * start.y + 0.375 * c1.y + 0.375 * c2.y + 0.125 * end.y,
    };
    paths.push({
      edge,
      d: `M ${start.x} ${start.y} C ${c1.x} ${c1.y}, ${c2.x} ${c2.y}, ${end.x} ${end.y}`,
      label: edge.label ? mid : null,
      changed: changed.get(edge.from) || changed.get(edge.to) || false,
    });
  }
  return paths;
}

function stateWords(node: GraphNode): string {
  switch (node.state) {
    case "changed":
      return "changed at this revision";
    case "suspect":
      return "not recomputed since its input moved";
    case "removed":
      return "removed";
    default:
      return "unchanged";
  }
}

/**
 * One graph View. Nodes are rectangles sized to their labels, edges are curves
 * with arrowheads, and every visible mark has a title so the picture is never
 * the only carrier.
 *
 * `marks` is false when the drawing cannot speak for the revision on screen (a
 * render drawn at an earlier one): node states are then shown as clean, so the
 * picture never claims a change of its own.
 */
export function GraphView({ model, marks }: { model: GraphModel; marks: boolean }) {
  const shown = useMemo<GraphModel>(
    () =>
      marks
        ? model
        : {
            ...model,
            nodes: model.nodes.map((node) => ({
              ...node,
              state: (node.unit ? "clean" : "display") as GraphNode["state"],
            })),
          },
    [model, marks],
  );
  const { placed, paths, width, height } = useMemo(() => layout(shown), [shown]);
  const marker = `graph-arrow-${useId().replace(/[:]/g, "")}`;

  if (placed.length === 0) {
    return <p className="empty">No nodes in this graph.</p>;
  }

  return (
    <div className="graph-wrap">
      <svg
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        aria-label={`Graph with ${placed.length} node${placed.length === 1 ? "" : "s"} and ${paths.length} edge${paths.length === 1 ? "" : "s"}`}
      >
        <defs>
          <marker
            id={marker}
            viewBox="0 0 10 10"
            refX="8.5"
            refY="5"
            markerWidth="7"
            markerHeight="7"
            markerUnits="userSpaceOnUse"
            orient="auto-start-reverse"
          >
            <path d="M1 1 L9 5 L1 9 Z" className="graph-arrow" />
          </marker>
        </defs>

        {paths.map((path, index) => (
          <g key={`${path.edge.from}->${path.edge.to}-${index}`} className="graph-edge">
            <title>
              {`${path.edge.from} ${path.edge.label ?? "→"} ${path.edge.to}`}
            </title>
            <path
              d={path.d}
              className={["graph-wire", path.changed ? "is-changed" : ""].filter(Boolean).join(" ")}
              markerEnd={`url(#${marker})`}
            />
            {path.label && paths.length <= 48 ? (
              <text
                className="graph-edge-label"
                x={path.label.x}
                y={path.label.y}
                textAnchor="middle"
              >
                {path.edge.label}
              </text>
            ) : null}
          </g>
        ))}

        {placed.map((node) => {
          const moved = node.node.state === "changed";
          const lines = node.lines ?? [node.node.label];
          const classes = ["graph-node", `is-${node.node.state}`].join(" ");
          return (
            <g
              key={node.node.id}
              className={classes}
              transform={`translate(${node.x}, ${node.y})`}
            >
              <title>{`${node.node.label} · ${stateWords(node.node)}`}</title>
              <rect
                x={-node.width / 2}
                y={-node.height / 2}
                width={node.width}
                height={node.height}
                rx={5}
                className="graph-node-box"
              />
              <text className="graph-node-label" textAnchor="middle">
                {lines.map((line, index) => (
                  <tspan
                    key={`${index}-${line}`}
                    x={0}
                    y={lines.length === 1 ? 3.6 : index === 0 ? -2.2 : 9.2}
                  >
                    {line}
                  </tspan>
                ))}
              </text>
              {moved ? (
                <circle
                  className="graph-node-mark"
                  cx={node.width / 2 - 4}
                  cy={-node.height / 2 + 4}
                  r={2.3}
                >
                  <title>changed at this revision</title>
                </circle>
              ) : null}
            </g>
          );
        })}
      </svg>
    </div>
  );
}

export type { GraphEdge, GraphNode };
