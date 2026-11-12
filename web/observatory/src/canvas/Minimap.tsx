import { useRef } from "react";
import type { Camera } from "../state/store";
import type { Placement } from "../state/types";
import { type Board, type NodeId, type Point } from "./layout";

type Props = {
  board: Board;
  placed: Map<NodeId, Placement>;
  camera: Camera;
  stage: { width: number; height: number };
  inks?: Map<string, string>;
  changed?: Set<string>;
  onNavigate: (point: Point) => void;
};

/** The thumbnail's own drawing surface, independent of card sizes. */
const W = 152;
const H = 194;
const PAD = 8;
const NODE_H = 4;

type MiniNode = {
  id: string;
  rank: number;
  x: number;
  y: number;
  width: number;
  height: number;
  view: boolean;
};

type MiniEdge = {
  key: string;
  component: string;
  path: string;
  ink: string;
  long: boolean;
};

/**
 * The whole structure in one small window.
 *
 * Drawn as a rank diagram, not as a reduced screenshot: every rank keeps one
 * row and every node becomes a short bar, so a tall pipeline does not collapse
 * into a column of giant rectangles. Wires keep their component's ink, their
 * own port on each bar, and a fade for the long dependencies. The rows the
 * camera can see are tinted; there is no heavy viewport box.
 */
export function Minimap({ board, placed, camera, stage, inks, changed, onNavigate }: Props) {
  const svg = useRef<SVGSVGElement | null>(null);
  const dragging = useRef(false);

  const levels = [...new Set(board.ranks.values())].sort((left, right) => left - right);
  const levelIndex = new Map(levels.map((level, index) => [level, index]));
  const rowHeight = (H - PAD * 2) / Math.max(1, levels.length);

  let minX = Number.POSITIVE_INFINITY;
  let maxX = Number.NEGATIVE_INFINITY;
  for (const [id, at] of placed) {
    const size = board.sizes.get(id);
    if (!size) continue;
    minX = Math.min(minX, at.x);
    maxX = Math.max(maxX, at.x + size.width);
  }
  if (!Number.isFinite(minX)) {
    minX = 0;
    maxX = 1;
  }
  const span = Math.max(1, maxX - minX);
  const inner = W - PAD * 2;

  const nodes: MiniNode[] = [];
  for (const [id, at] of placed) {
    const size = board.sizes.get(id);
    const rank = board.ranks.get(id);
    if (!size || rank === undefined) continue;
    const index = levelIndex.get(rank) ?? 0;
    nodes.push({
      id,
      rank: index,
      x: PAD + ((at.x + size.width / 2 - minX) / span) * inner,
      y: PAD + rowHeight * (index + 0.5) - NODE_H / 2,
      width: Math.max(5, Math.min(30, (size.width / span) * inner)),
      height: NODE_H,
      view: id.startsWith("view:"),
    });
  }
  const byId = new Map(nodes.map((node) => [node.id, node]));

  type Attachment = { key: string; node: MiniNode; other: MiniNode };
  const outgoing = new Map<NodeId, Attachment[]>();
  const incoming = new Map<NodeId, Attachment[]>();
  const edges: Omit<MiniEdge, "path">[] = [];
  for (const edge of board.edges) {
    const source = byId.get(edge.from);
    const target = byId.get(edge.to);
    if (!source || !target) continue;
    const key = `${edge.from}->${edge.to}-${edge.kind}`;
    const out = outgoing.get(edge.from) ?? [];
    out.push({ key, node: source, other: target });
    outgoing.set(edge.from, out);
    const into = incoming.get(edge.to) ?? [];
    into.push({ key, node: target, other: source });
    incoming.set(edge.to, into);
    edges.push({
      key,
      component: edge.component,
      ink: inks?.get(edge.component) ?? "var(--line)",
      long:
        Math.abs(target.rank - source.rank) > 2 ||
        Math.abs(target.x - source.x) > W * 0.22,
    });
  }

  const starts = new Map<string, string>();
  const ends = new Map<string, string>();
  const assign = (
    list: Attachment[],
    side: "top" | "bottom",
    into: Map<string, string>,
  ) => {
    list
      .slice()
      .sort((left, right) => left.other.x - right.other.x || left.key.localeCompare(right.key))
      .forEach((entry, index) => {
        const x =
          entry.node.x -
          entry.node.width / 2 +
          ((index + 1) / (list.length + 1)) * entry.node.width;
        const y = side === "top" ? entry.node.y : entry.node.y + entry.node.height;
        into.set(entry.key, `${x} ${y}`);
      });
  };
  for (const list of outgoing.values()) assign(list, "bottom", starts);
  for (const list of incoming.values()) assign(list, "top", ends);

  const paths = edges.map((edge): MiniEdge => {
    const start = starts.get(edge.key);
    const end = ends.get(edge.key);
    if (!start || !end) return { ...edge, path: "" };
    const [sx, sy] = start.split(" ").map(Number) as [number, number];
    const [ex, ey] = end.split(" ").map(Number) as [number, number];
    const path =
      Math.abs(sx - ex) < 0.4
        ? `M ${sx} ${sy} L ${ex} ${ey}`
        : `M ${sx} ${sy} C ${sx} ${sy + Math.max(4, (ey - sy) * 0.42)}, ${ex} ${
            ey - Math.max(4, (ey - sy) * 0.42)
          }, ${ex} ${ey}`;
    return { ...edge, path };
  });

  const topWorld = -camera.y / camera.zoom;
  const bottomWorld = (stage.height - camera.y) / camera.zoom;
  const visibleRows = new Set<number>();
  for (const node of nodes) {
    const at = placed.get(node.id);
    const size = board.sizes.get(node.id);
    if (!at || !size) continue;
    if (at.y + size.height > topWorld && at.y < bottomWorld) visibleRows.add(node.rank);
  }

  const worldPoint = (event: React.PointerEvent<SVGSVGElement>): Point | null => {
    const element = svg.current;
    if (!element) return null;
    const matrix = element.getScreenCTM();
    if (!matrix) return null;
    const point = element.createSVGPoint();
    point.x = event.clientX;
    point.y = event.clientY;
    const local = point.matrixTransform(matrix.inverse());
    const index = Math.min(
      Math.max(0, Math.floor((local.y - PAD) / Math.max(1, rowHeight))),
      levels.length - 1,
    );
    const first = nodes.find((node) => node.rank === index);
    const worldY = first ? (placed.get(first.id)?.y ?? 0) + 20 : 0;
    return {
      x: minX + ((local.x - PAD) / inner) * span,
      y: worldY,
    };
  };

  const moveTo = (event: React.PointerEvent<SVGSVGElement>) => {
    const point = worldPoint(event);
    if (point) onNavigate(point);
  };

  const marks = changed;
  // Highlight the wires only when the change is a small subset; a revision that
  // rewrote most of the board would light every wire and explain nothing.
  const hotMarks =
    changed && changed.size <= Math.max(2, Math.floor(nodes.length / 4))
      ? changed
      : undefined;

  return (
    <div className="minimap" aria-label="Structure minimap">
      <svg
        ref={svg}
        viewBox={`0 0 ${W} ${H}`}
        preserveAspectRatio="xMidYMid meet"
        tabIndex={0}
        role="group"
        aria-label="Whole board; click or drag to move the camera"
        onPointerDown={(event) => {
          event.stopPropagation();
          event.preventDefault();
          dragging.current = true;
          event.currentTarget.setPointerCapture(event.pointerId);
          moveTo(event);
        }}
        onPointerMove={(event) => {
          if (!dragging.current) return;
          event.stopPropagation();
          moveTo(event);
        }}
        onPointerUp={(event) => {
          dragging.current = false;
          event.stopPropagation();
          if (event.currentTarget.hasPointerCapture(event.pointerId)) {
            event.currentTarget.releasePointerCapture(event.pointerId);
          }
        }}
        onPointerCancel={() => {
          dragging.current = false;
        }}
        onWheel={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          const step = 80;
          const current = {
            x: (stage.width / 2 - camera.x) / camera.zoom,
            y: (stage.height / 2 - camera.y) / camera.zoom,
          };
          if (event.key === "ArrowLeft") onNavigate({ x: current.x - step, y: current.y });
          else if (event.key === "ArrowRight") onNavigate({ x: current.x + step, y: current.y });
          else if (event.key === "ArrowUp") onNavigate({ x: current.x, y: current.y - step });
          else if (event.key === "ArrowDown") onNavigate({ x: current.x, y: current.y + step });
          else return;
          event.preventDefault();
          event.stopPropagation();
        }}
      >
        <title>Click or drag to move the camera. Arrow keys nudge the view.</title>
        {visibleRows.size > 0 ? (
          <rect
            className="mini-camera"
            x={W - 3}
            y={PAD + rowHeight * Math.min(...visibleRows)}
            width={3}
            height={rowHeight * (Math.max(...visibleRows) - Math.min(...visibleRows) + 1)}
            rx={1.5}
          />
        ) : null}
        <g className="mini-links">
          {paths.map((edge) => (
            <path
              key={edge.key}
              d={edge.path}
              className={[
                "mini-link",
                edge.long ? "mini-link-long" : "",
                hotMarks?.has(`component:${edge.component}`) ? "mini-link-hot" : "",
              ]
                .filter(Boolean)
                .join(" ")}
              style={{ stroke: edge.ink }}
              vectorEffect="non-scaling-stroke"
            />
          ))}
        </g>
        <g className="mini-nodes">
          {nodes.map((node) => (
            <rect
              key={node.id}
              className={["mini-node", node.view ? "mini-view" : "mini-component"].join(" ")}
              x={node.x - node.width / 2}
              y={node.y}
              width={node.width}
              height={node.height}
              rx={1.2}
              vectorEffect="non-scaling-stroke"
            />
          ))}
        </g>
        {marks ? (
          <g className="mini-marks">
            {nodes
              .filter((node) => marks.has(node.id))
              .map((node) => (
                <circle
                  key={`mark-${node.id}`}
                  className="mini-dot"
                  cx={node.x + node.width / 2 - 1}
                  cy={node.y - 1.4}
                  r={1.9}
                />
              ))}
          </g>
        ) : null}
      </svg>
    </div>
  );
}
