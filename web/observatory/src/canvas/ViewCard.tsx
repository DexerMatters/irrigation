import { useState } from "react";
import { ModelView } from "../components/Render";
import { beginEdit, endEdit, ensureRender, select, useStore } from "../state/store";
import type { Model, Placement, ViewWire } from "../state/types";
import { type Box, viewNode } from "./layout";
import { useNodeDrag } from "./drag";
import { Editor, short } from "./Editor";

const stateWord = (state: string): string =>
  state === "not_demanded" ? "not demanded" : state.replace("_", " ");

/** The runtime's own word for what this plate holds. A View with several
 *  fields says `fields`; a single-field View says which shape it is. */
function kindWord(wire: ViewWire): string {
  if (wire.fields.length > 1) return "fields";
  return wire.fields[0]?.kind ?? "view";
}

/**
 * What the footer counts. A render that drew rows is the truth about what is on
 * the plate; the schema's element count is only a fallback, because a tree View
 * has one unit and thirty-three rows.
 */
function countWord(wire: ViewWire, drawn: number): string {
  const field = wire.fields[0];
  if (!field) return "—";
  if (field.kind === "scalar") return `${wire.fields.length} fields`;
  if (drawn > 0) return `${drawn} rows`;
  if (field.granularity === "whole_value") {
    return field.elements === null ? "whole value" : `${field.elements} rows`;
  }
  return field.units === null ? "—" : `${field.units} units`;
}

/**
 * What the current render says about itself: how many rows it drew, how many of
 * them moved at the revision on screen, and how many carry an identity at all.
 */
function rowCounts(model: Model | undefined): {
  drawn: number;
  changed: number;
  identified: number;
} {
  if (!model) return { drawn: 0, changed: 0, identified: 0 };
  const states = (rows: { state: string; unit: unknown }[]) => ({
    drawn: rows.length,
    changed: rows.filter((row) => row.state === "changed").length,
    identified: rows.filter((row) => row.unit !== null).length,
  });
  switch (model.kind) {
    case "tree":
    case "list":
    case "map":
      return states(model.rows);
    case "set":
      return states(model.members);
    case "scalar":
      return states(model.rows);
    case "graph":
      return states(model.nodes);
    default:
      return { drawn: 0, changed: 0, identified: 0 };
  }
}

type Props = {
  wire: ViewWire;
  at: Placement;
  /** The plate's planned box: one height per rank, so a row ends level. */
  box: Box;
  revision: number;
  followingNewest: boolean;
  /** What the displayed revision did: rewrote the content, or ran and matched. */
  reaction: "changed" | "cutoff" | null;
  /** The ink of the component that produces (or consumes) this View. */
  ink: string;
  producer: string | null;
  onDrag: (node: string, at: Placement | null) => void;
};

/**
 * One View on the canvas: a draggable card that draws its native render, or the
 * host's text editor when the reader opens it. A card that changed says so in
 * its own footer, and the rows inside say which part moved.
 */
export function ViewCard({
  wire,
  at,
  box,
  revision,
  followingNewest,
  reaction,
  ink,
  producer,
  onDrag,
}: Props) {
  const state = useStore((snapshot) => snapshot);
  const render = state.renders[wire.view];
  const error = state.renderErrors[wire.view];
  const editing = state.editing === wire.view;
  const node = viewNode(wire.view);
  const drag = useNodeDrag(at, (next) => onDrag(node, next));
  const [chosen, setChosen] = useState<number | null>(null);
  const fields = render?.fields ?? [];
  const field = fields.find((entry) => entry.field === chosen) ?? fields[0];
  const counts = rowCounts(field?.model);
  // A keyed View keeps its content version across a value-only edit, so the
  // rows are the second witness: if any of them says it moved at this revision,
  // the plate changed, whatever the version says.
  const contentMoved = wire.content_version === revision;
  const changed = reaction === "changed" && (contentMoved || counts.changed > 0);
  const ranAlike = reaction === "changed" && !changed;
  const selected =
    state.selection?.kind === "node"
      ? false
      : state.selection && "view" in state.selection
        ? state.selection.view === wire.view
        : false;

  if (!editing) ensureRender(wire.view);

  return (
    <article
      className={[
        "card",
        changed ? "card-changed" : "",
        selected ? "card-selected" : "",
        editing ? "card-editing" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      style={{
        left: at.x,
        top: at.y,
        width: box.width,
        // The plate keeps the height the layout planned for it, even while the
        // editor is open: an edge that ends at the planned bottom but a card that
        // grew past it is worse than a scrolling editor.
        height: box.height,
        
      }}
      data-blink={changed ? "1" : undefined}
      data-view={wire.view}
      {...drag}
      onDoubleClick={() => {
        if (wire.editable) beginEdit(wire.view);
        else void select({ kind: "module", view: wire.view });
      }}
      aria-label={`${wire.name}, ${kindWord(wire)}`}
    >
      <header className="card-head">
        <span className="card-ink" style={{ background: ink }} aria-hidden="true" />
        <span className="card-name" title={wire.name}>
          {short(wire.name)}
        </span>
        <span className="card-kind">{kindWord(wire)}</span>
        <span className={`card-state state-${wire.state}`} title={producer ?? wire.kind}>
          <span
            className={`glyph ${wire.state === "changed" ? "" : "hollow"}`.trim()}
            aria-hidden="true"
          />
          {ranAlike ? "same" : stateWord(wire.state)}
        </span>
        <span className="card-version mono">v{wire.content_version}</span>
        {wire.editable ? (
          <button
            type="button"
            className="card-action"
            aria-pressed={editing}
            onClick={(event) => {
              event.stopPropagation();
              if (editing) endEdit(wire.view);
              else beginEdit(wire.view);
            }}
          >
            {editing ? "Done" : "Edit"}
          </button>
        ) : null}
      </header>

      {fields.length > 1 ? (
        <div className="card-fields" role="group" aria-label={`${short(wire.name)} fields`}>
          {fields.map((entry) => (
            <button
              key={entry.field}
              type="button"
              className="field-chip"
              aria-pressed={entry.field === field?.field}
              onClick={(event) => {
                event.stopPropagation();
                setChosen(entry.field);
              }}
            >
              {wire.fields.find((candidate) => candidate.field === entry.field)?.name ??
                `field ${entry.field}`}
            </button>
          ))}
        </div>
      ) : field ? (
        <div className="card-fields card-fields-single">
          {wire.fields.find((candidate) => candidate.field === field.field)?.name ??
            `field ${field.field}`}
        </div>
      ) : null}

      <div
        className="card-body"
        tabIndex={0}
        role="group"
        aria-label={`${short(wire.name)} content`}
        onPointerDown={(event) => event.stopPropagation()}
      >
        {editing ? (
          <Editor view={wire} />
        ) : field ? (
          <ModelView
            model={field.model}
            selectedRow={null}
            interactiveRevision={followingNewest}
            revision={revision}
            onSelectRow={() => undefined}
          />
        ) : (
          <p className="card-empty">drawing…</p>
        )}
        {error ? <p className="card-error">{error}</p> : null}
      </div>

      <footer className="card-foot">
        {wire.fields[0]?.render === "generic" ? (
          <span
            className="generic-note"
            title="drawn from the runtime's facts; register a renderer for this View to draw it yourself"
          >
            generic
          </span>
        ) : null}
        <span>{countWord(wire, counts.drawn)}</span>
        {changed ? (
          <span className="changed-note" title={`changed at revision ${revision}`}>
            {counts.changed > 0
              ? `${counts.changed} of ${counts.drawn} rows at r${revision}`
              : counts.identified > 0
                ? `rows added or removed at r${revision}`
                : `whole value at r${revision}`}
          </span>
        ) : ranAlike ? (
          <span className="same-note" title={`ran at r${revision} and published the same content`}>
            ran at r{revision}, same content
          </span>
        ) : (
          <span title={reaction === "cutoff" ? `ran at r${revision} and produced the same value` : undefined}>
            {reaction === "cutoff"
              ? `ran at r${revision}, no change`
              : producer
                ? short(producer)
                : wire.editable
                  ? "editable"
                  : "read only"}
          </span>
        )}
      </footer>
    </article>
  );
}
