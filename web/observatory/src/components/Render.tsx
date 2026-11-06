import { useMemo } from "react";
import type { Model, RowState } from "../state/types";

/** The words a mark stands for, so the mark is never the only carrier. */
function markWords(state: RowState, revision: number): string | null {
  switch (state) {
    case "changed":
      return `changed at r${revision}`;
    case "suspect":
      return "not recomputed since its input moved";
    case "removed":
      return "removed";
    default:
      return null;
  }
}

/**
 * The row's mark. It comes from the server: a row without a unit is a display
 * row in a whole-value render and never claims a change of its own. The mark is
 * a shape, and the row's title carries the sentence.
 */
function Mark({ state, revision }: { state: RowState; revision: number }) {
  const words = markWords(state, revision);
  if (!words) return null;
  const shape = state === "changed" ? "mark-changed" : state === "removed" ? "mark-removed" : "mark-suspect";
  return (
    <>
      <span className={`mark ${shape}`} aria-hidden="true" />
      <span className="sr-only">{words}</span>
    </>
  );
}

type RowProps = {
  state: RowState;
  unit: boolean;
  selected: boolean;
  revision: number;
  onSelect?: () => void;
  children: React.ReactNode;
  title?: string;
};

function Row({ state, unit, selected, revision, onSelect, children, title }: RowProps) {
  const interactive = Boolean(onSelect) && unit;
  const className = [
    "row",
    `state-${state}`,
    state === "changed" ? "row-changed" : "",
    selected ? "selected" : "",
    interactive ? "interactive" : "",
  ]
    .filter(Boolean)
    .join(" ");
  const spoken = markWords(state, revision);
  const heading = [title, spoken].filter(Boolean).join(" · ") || undefined;
  if (!interactive) {
    return (
      <div className={className} title={heading}>
        {children}
      </div>
    );
  }
  return (
    // The row's own words are its accessible name: a "Select row" label that
    // repeats on every line says nothing and hides what the row holds.
    <button type="button" className={className} onClick={onSelect} title={heading}>
      {children}
    </button>
  );
}

/** A row's trailing meta: its mark, and nothing else. */
function RowMeta({ state, revision }: { state: RowState; revision: number }) {
  return (
    <span className="row-meta">
      <Mark state={state} revision={revision} />
    </span>
  );
}

export function ModelView({
  model,
  selectedRow,
  onSelectRow,
  interactiveRevision,
  revision,
}: {
  model: Model;
  selectedRow: string | null;
  onSelectRow: (key: string, label: string) => void;
  interactiveRevision: boolean;
  /** Rows are keyed by revision, so a changed row replays its mark. */
  revision: number;
}) {
  const stateOf = (state: RowState): RowState => (interactiveRevision ? state : "display");
  switch (model.kind) {
    case "tree":
      return (
        <div className="tree" aria-label="Tree rows">
          {model.rows.length === 0 ? <p className="empty">No nodes in this render.</p> : null}
          {model.rows.map((row) => (
            <Row
              key={`${row.key}:${revision}`}
              state={stateOf(row.state)}
              unit={row.unit !== null}
              selected={selectedRow === row.key}
              revision={revision}
              onSelect={() => onSelectRow(row.key, `${row.kind} ${row.label}`)}
              title={`${row.kind} at depth ${row.depth}`}
            >
              <span className="tree-depth" style={{ width: row.depth * 14 }} aria-hidden="true" />
              <span className="row-kind">{row.kind}</span>
              <span className="row-label">{row.label}</span>
              {row.value ? <span className="row-value">{row.value}</span> : null}
              <RowMeta state={stateOf(row.state)} revision={revision} />
            </Row>
          ))}
        </div>
      );
    case "list":
      return (
        <div className="list" aria-label="List rows">
          {model.rows.length === 0 ? <p className="empty">No items.</p> : null}
          {model.rows.map((row) => (
            <Row
              key={`${row.key}:${revision}`}
              state={stateOf(row.state)}
              unit={row.unit !== null}
              selected={selectedRow === row.key}
              revision={revision}
              onSelect={() => onSelectRow(row.key, `item ${row.index}`)}
            >
              <span className="row-index mono">{row.index}</span>
              <span className="row-value">{row.value}</span>
              <RowMeta state={stateOf(row.state)} revision={revision} />
            </Row>
          ))}
        </div>
      );
    case "map":
      return (
        <div className="map" aria-label="Map rows">
          {model.rows.length === 0 ? <p className="empty">No keys.</p> : null}
          {model.rows.map((row) => (
            <Row
              key={`${row.key}:${revision}`}
              state={stateOf(row.state)}
              unit={row.unit !== null}
              selected={selectedRow === row.key}
              revision={revision}
              onSelect={() => onSelectRow(row.key, row.label)}
            >
              <span className="map-key">{row.label}</span>
              <span className="row-value">{row.value ?? "absent"}</span>
              <RowMeta state={stateOf(row.state)} revision={revision} />
            </Row>
          ))}
        </div>
      );
    case "set":
      return (
        <div className="list" aria-label="Set members">
          {model.members.length === 0 ? <p className="empty">No members.</p> : null}
          <div className="chips">
            {model.members.map((member) => (
              <span
                key={`${member.key}:${revision}`}
                className={`chip ${member.present ? "present" : ""} state-${stateOf(member.state)}`}
                title={markWords(stateOf(member.state), revision) ?? undefined}
              >
                {member.member}
                <Mark state={stateOf(member.state)} revision={revision} />
              </span>
            ))}
          </div>
        </div>
      );
    case "scalar":
      return (
        <div className="scalar">
          {model.rows.map((row) => (
            <Row
              key={`${row.key}:${revision}`}
              state={stateOf(row.state)}
              unit={row.unit !== null}
              selected={selectedRow === row.key}
              revision={revision}
              onSelect={() => onSelectRow(row.key, row.label)}
            >
              <span className="map-key">{row.label}</span>
              <span className="row-value">{row.value}</span>
              <RowMeta state={stateOf(row.state)} revision={revision} />
            </Row>
          ))}
        </div>
      );
    case "graph":
      return <GraphView model={model} />;
    case "reduced":
      return (
        <div className="scalar">
          <div className="kv-row">
            <span className="label">{model.reduction}</span>
            <span className="value mono">{model.aggregate}</span>
          </div>
          {model.sample.map((item, index) => (
            <div key={`${index}-${item}`} className="kv-row">
              <span className="label">sample {index}</span>
              <span className="value mono">{item}</span>
            </div>
          ))}
        </div>
      );
    case "opaque":
      return (
        <p className="empty">
          {model.summary} · <span className="mono">{model.type_name}</span>
        </p>
      );
  }
}

/** A small deterministic layout: BFS layers, no force simulation. */
function GraphView({ model }: { model: Extract<Model, { kind: "graph" }> }) {
  const layout = useMemo(() => {
    const adjacency = new Map<string, string[]>();
    for (const node of model.nodes) adjacency.set(node.id, []);
    for (const edge of model.edges) {
      adjacency.get(edge.from)?.push(edge.to);
      adjacency.get(edge.to)?.push(edge.from);
    }
    const depth = new Map<string, number>();
    const roots = model.nodes.length > 0 ? [model.nodes[0].id] : [];
    for (const root of roots) {
      depth.set(root, 0);
      const queue = [root];
      while (queue.length > 0) {
        const current = queue.shift() as string;
        for (const next of adjacency.get(current) ?? []) {
          if (!depth.has(next)) {
            depth.set(next, (depth.get(current) ?? 0) + 1);
            queue.push(next);
          }
        }
      }
    }
    const layers = new Map<number, string[]>();
    for (const node of model.nodes) {
      const level = depth.get(node.id) ?? 0;
      const bucket = layers.get(level) ?? [];
      bucket.push(node.id);
      layers.set(level, bucket);
    }
    const positions = new Map<string, { x: number; y: number }>();
    const levels = [...layers.keys()].sort((a, b) => a - b);
    levels.forEach((level, column) => {
      const ids = layers.get(level) ?? [];
      ids.forEach((id, row) => {
        positions.set(id, { x: 24 + column * 84, y: 24 + row * 44 });
      });
    });
    const width = Math.max(160, 48 + levels.length * 84);
    const height = Math.max(
      80,
      40 + Math.max(...[...layers.values()].map((ids) => ids.length), 1) * 44,
    );
    return { positions, width, height };
  }, [model]);

  return (
    <div className="graph-wrap">
      <svg viewBox={`0 0 ${layout.width} ${layout.height}`} role="group" aria-label="Module graph">
        {model.edges.map((edge, index) => {
          const from = layout.positions.get(edge.from);
          const to = layout.positions.get(edge.to);
          if (!from || !to) return null;
          return (
            <line
              key={`${edge.from}-${edge.to}-${index}`}
              x1={from.x}
              y1={from.y}
              x2={to.x}
              y2={to.y}
              stroke="#b9c6be"
              strokeWidth={1}
            />
          );
        })}
        {model.nodes.map((node) => {
          const position = layout.positions.get(node.id);
          if (!position) return null;
          const fill = node.state === "changed" ? "var(--signal-soft)" : "var(--field)";
          return (
            <g key={node.id}>
              <rect
                x={position.x - 6}
                y={position.y - 6}
                width={54}
                height={18}
                rx={2}
                fill={fill}
                stroke="var(--line)"
              />
              <text
                x={position.x}
                y={position.y + 7}
                fontSize={9}
                fill="var(--ink)"
                className="mono"
              >
                {node.label.slice(0, 7)}
              </text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}
