import { useState } from "react";
import { ModelView } from "../components/Render";
import {
  beginEdit,
  endEdit,
  ensureRender,
  select,
  toggleCollapsed,
  useStore,
} from "../state/store";
import type { Placement, ViewWire } from "../state/types";
import { type Box, displayName, viewNode } from "./layout";
import { countWord, fieldNote, kindWord, movedField, rowCounts } from "./viewInfo";
import { useNodeDrag } from "./drag";
import { Editor } from "./Editor";

const stateWord = (state: string): string =>
  state === "not_demanded" ? "not demanded" : state.replace("_", " ");

type Props = {
  wire: ViewWire;
  at: Placement;
  /** The plate's planned box: one band per rank, so a level ends level. */
  box: Box;
  revision: number;
  followingNewest: boolean;
  /** What the displayed revision did: rewrote the content, or ran and matched. */
  reaction: "changed" | "cutoff" | null;
  /** The ink of the component that produces (or consumes) this View. */
  ink: string;
  /** Every component that writes this View; the first is the primary. */
  producers: string[];
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
  producers,
  onDrag,
}: Props) {
  const state = useStore((snapshot) => snapshot);
  const render = state.renders[wire.view];
  const error = state.renderErrors[wire.view];
  const editing = state.editing === wire.view;
  const detailOpen =
    state.selection?.kind === "module" && state.selection.view === wire.view;
  // When the sidebar is showing this View, the editor belongs there; the plate
  // keeps its projection instead of becoming a second textarea.
  const shownEditing = editing && !detailOpen;
  const collapsed = state.overview || Boolean(state.collapsed[wire.view]);
  const node = viewNode(wire.view);
  const drag = useNodeDrag(at, (next) => onDrag(node, next));
  const [chosen, setChosen] = useState<number | null>(null);
  const fields = render?.fields ?? [];
  const field = fields.find((entry) => entry.field === chosen) ?? fields[0];
  const fieldWire =
    wire.fields.find((entry) => entry.field === field?.field) ?? wire.fields[0];
  const counts = rowCounts(field?.model, fieldWire);
  const producer = producers[0] ?? null;
  const extraProducers = Math.max(0, producers.length - 1);
  const producerTitle = producers.length > 0 ? producers.map(displayName).join(", ") : wire.kind;
  // The header state is the runtime's verdict. A plate is changed because the
  // View changed, not because the field the reader happens to be looking at
  // drew a moved row: another field may be the one that moved.
  const changed = reaction === "changed";
  const ranAlike = reaction === "cutoff";
  const fieldMoved = movedField(wire, field?.field ?? 0);
  // Only fields the projection actually drew can be switched to, so only they
  // can be "other fields" a reader might look at.
  const otherFields = fields.filter(
    (entry) => entry.field !== field?.field && movedField(wire, entry.field),
  );
  // Only a following reader can trust per-field counts: they describe "now".
  const fieldChange = followingNewest ? fieldMoved : false;
  const otherChange = followingNewest ? otherFields.length : 0;
  const fieldMark = fieldNote(
    counts,
    fieldWire,
    field?.model.kind,
    fieldMoved,
    otherChange,
  );
  const selected =
    state.selection?.kind === "node"
      ? false
      : state.selection && "view" in state.selection
        ? state.selection.view === wire.view
        : false;

  if (!editing && !collapsed) ensureRender(wire.view);

  return (
    <article
      className={[
        "card",
        changed ? "card-changed" : "",
        selected ? "card-selected" : "",
        shownEditing ? "card-editing" : "",
        collapsed ? "card-collapsed" : "",
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
      data-collapsed={collapsed ? "1" : undefined}
      onPointerDown={(event) => event.stopPropagation()}
      onDoubleClick={(event) => {
        if (
          (event.target as HTMLElement).closest(
            ".card-action, .card-grip, .field-chip, textarea, input, select, a",
          )
        ) {
          return;
        }
        void select({ kind: "module", view: wire.view });
      }}
      aria-label={`${displayName(wire.name)}, ${kindWord(fields.length, fieldWire)}`}
    >
      <header className="card-head">
        <button
          type="button"
          className="card-grip"
          {...drag}
          aria-label={`Drag ${displayName(wire.name)}`}
          title="Drag this plate"
          onClick={(event) => event.stopPropagation()}
        >
          <GripIcon />
        </button>
        <span className="card-ink" style={{ background: ink }} aria-hidden="true" />
        <span className="card-name" title={wire.name}>
          {displayName(wire.name)}
        </span>
        <span className="card-kind">{kindWord(fields.length, fieldWire)}</span>
        <span className={`card-state state-${wire.state}`} title={producerTitle}>
          <span
            className={`glyph ${wire.state === "changed" ? "" : "hollow"}`.trim()}
            aria-hidden="true"
          />
          {ranAlike ? "same" : stateWord(wire.state)}
        </span>
        <span className="card-version mono">v{wire.content_version}</span>
        {wire.editable && !collapsed ? (
          <button
            type="button"
            className="card-action"
            aria-pressed={editing}
            onClick={(event) => {
              event.stopPropagation();
              if (editing) {
                endEdit(wire.view);
              } else {
                beginEdit(wire.view);
                void select({ kind: "module", view: wire.view });
              }
            }}
          >
            {editing ? "Done" : "Edit"}
          </button>
        ) : null}
        {state.overview ? null : (
        <button
          type="button"
          className="card-action card-fold"
          aria-expanded={!collapsed}
          aria-label={collapsed ? `Expand ${displayName(wire.name)}` : `Collapse ${displayName(wire.name)}`}
          title={collapsed ? "Expand this view" : "Fold this view to its header"}
          onClick={(event) => {
            event.stopPropagation();
            toggleCollapsed(wire.view);
          }}
        >
          <FoldIcon collapsed={collapsed} />
        </button>
        )}
      </header>

      {collapsed ? null : (
        <>
          {fields.length > 1 ? (
            <div className="card-fields" role="group" aria-label={`${displayName(wire.name)} fields`}>
              {fields.map((entry) => {
                const moved =
                  followingNewest &&
                  (wire.fields.find((candidate) => candidate.field === entry.field)?.changed_units ??
                    0) > 0;
                const name =
                  wire.fields.find((candidate) => candidate.field === entry.field)?.name ??
                  `field ${entry.field}`;
                return (
                  <button
                    key={entry.field}
                    type="button"
                    className={["field-chip", moved ? "field-chip-moved" : ""].filter(Boolean).join(" ")}
                    aria-pressed={entry.field === field?.field}
                    title={moved ? `${name} changed at r${revision}` : name}
                    onClick={(event) => {
                      event.stopPropagation();
                      setChosen(entry.field);
                    }}
                  >
                    <span className="field-dot" aria-hidden="true" />
                    {name}
                    {moved ? <span className="sr-only"> changed at r{revision}</span> : null}
                  </button>
                );
              })}
            </div>
          ) : field ? (
            <div
              className={["card-fields", "card-fields-single", fieldChange ? "field-chip-moved" : ""]
                .filter(Boolean)
                .join(" ")}
            >
              <span className="field-dot" aria-hidden="true" />
              {wire.fields.find((candidate) => candidate.field === field.field)?.name ??
                `field ${field.field}`}
              {fieldChange ? <span className="sr-only"> changed at r{revision}</span> : null}
            </div>
          ) : null}

          <div
            className="card-body"
            tabIndex={0}
            role="group"
            aria-label={`${displayName(wire.name)} content`}
            onPointerDown={(event) => event.stopPropagation()}
          >
            {shownEditing ? (
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
            <span>{countWord(fieldWire, field?.model.kind, counts.drawn)}</span>
            {changed ? (
              <span className="changed-note" title={`the runtime reports this View changed at revision ${revision}`}>
                {fieldMark} at r{revision}
              </span>
            ) : ranAlike ? (
              <span className="same-note" title={`ran at r${revision} and published the same content`}>
                ran at r{revision}, same content
              </span>
            ) : (
              <span title={producerTitle}>
                {producer
                  ? `${displayName(producer)}${extraProducers > 0 ? ` +${extraProducers}` : ""}`
                  : wire.editable
                    ? "editable"
                    : "read only"}
              </span>
            )}
          </footer>
        </>
      )}
    </article>
  );
}

function GripIcon() {
  return (
    <svg className="grip-icon" viewBox="0 0 10 14" width="10" height="14" aria-hidden="true">
      <g fill="currentColor">
        <circle cx="3" cy="3" r="1.1" />
        <circle cx="7" cy="3" r="1.1" />
        <circle cx="3" cy="7" r="1.1" />
        <circle cx="7" cy="7" r="1.1" />
        <circle cx="3" cy="11" r="1.1" />
        <circle cx="7" cy="11" r="1.1" />
      </g>
    </svg>
  );
}

function FoldIcon({ collapsed }: { collapsed: boolean }) {
  return (
    <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
      <path
        d={collapsed ? "M2 4 L5 7 L8 4" : "M2 6 L5 3 L8 6"}
        fill="none"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
