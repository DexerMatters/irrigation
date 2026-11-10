import type { Model, RowState } from "../state/types";
import { GraphView } from "./Graph";

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
              title={[`${row.kind} at depth ${row.depth}`, row.label, row.value]
                .filter(Boolean)
                .join(" · ")}
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
              title={row.value}
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
              title={`${row.label} = ${row.value ?? "absent"}`}
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
                title={[member.member, markWords(stateOf(member.state), revision)]
                  .filter(Boolean)
                  .join(" · ")}
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
              title={`${row.label} = ${row.value ?? "absent"}`}
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
