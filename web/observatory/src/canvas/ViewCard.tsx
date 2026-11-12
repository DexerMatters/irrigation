import { useEffect, useRef, useState } from "react";
import { ModelView } from "../components/Render";
import {
  atNewest,
  ensureRender,
  select,
  setViewVisible,
  toggleCollapsed,
  useStore,
} from "../state/store";
import type { Placement, ViewWire } from "../state/types";
import { type Box, displayName, viewNode } from "./layout";
import { countWord, fieldNote, kindWord, movedField, rowCounts } from "./viewInfo";
import { useNodeDrag } from "./drag";
import { Editor } from "./Editor";
import { usePlateFlash, useRowFlash } from "./marks";

const stateWord = (state: string): string =>
  state === "not_demanded" ? "not demanded" : state.replace("_", " ");

type Props = {
  wire: ViewWire;
  at: Placement;
  /** The plate's planned box: one band per rank, so a level ends level. */
  box: Box;
  revision: number;
  /** What the displayed revision did: rewrote the content, or ran and matched. */
  reaction: "changed" | "cutoff" | null;
  /** The ink of the component that produces (or consumes) this View. */
  ink: string;
  /** Every component that writes this View; the first is the primary. */
  producers: string[];
  onDrag: (node: string, at: Placement | null) => void;
};

/**
 * One View on the canvas: a draggable card that draws a host-written text
 * channel as a live editor, and every other View as its native render. A card
 * that changed says so in its own footer, and the rows inside say which part
 * moved — when the data on screen is really the revision it is labelled with.
 */
export function ViewCard({
  wire,
  at,
  box,
  revision,
  reaction,
  ink,
  producers,
  onDrag,
}: Props) {
  const state = useStore((snapshot) => snapshot);
  const plate = useRef<HTMLElement | null>(null);
  const body = useRef<HTMLDivElement | null>(null);
  const render = state.renders[wire.view];
  const error = state.renderErrors[wire.view];
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
  // The plate is annotated for the revision its data describes; the header state
  // is the runtime's verdict for that same read.
  const newest = atNewest(state);
  const aligned = newest;
  const changed = reaction === "changed";
  const ranAlike = reaction === "cutoff";
  // An older revision is annotated from the journal, not from the live headers:
  // the header's own state describes "now", and "changed" there would keep
  // claiming a change for every plate that moved since.
  const headerState = newest
    ? wire.state
    : changed
      ? "changed"
      : ranAlike
        ? "cutoff"
        : "clean";
  // A render is a snapshot with the marks of its own revision. Only the render
  // drawn at the revision on screen may accuse its rows.
  const marksAt = aligned && render?.revision === revision ? revision : null;
  const fieldMoved = movedField(wire, field?.field ?? 0);
  // Only fields the projection actually drew can be switched to, so only they
  // can be "other fields" a reader might look at.
  const otherFields = fields.filter(
    (entry) => entry.field !== field?.field && movedField(wire, entry.field),
  );
  const fieldChange = aligned && fieldMoved;
  const otherChange = aligned ? otherFields.length : 0;
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
  const selectedRow =
    state.selection?.kind === "row" && state.selection.view === wire.view
      ? state.selection.rowKey
      : null;

  useEffect(() => {
    // Read the projection from an effect, never during render: asking for it
    // updates the store, and a store update belongs after the paint. This is
    // the first read (and a re-read when the plate is opened again); later
    // revisions are read by the store's visibility batch, so a plate scrolled
    // out of the surface is not re-projected on every keystroke.
    if (!collapsed) ensureRender(wire.view);
  }, [wire.view, collapsed]);

  useEffect(() => {
    // Only a plate on screen is worth re-reading; the store uses this to skip
    // renders for plates scrolled out of the surface, which is what kept the
    // runtime busy rebuilding views nobody was looking at.
    const element = plate.current;
    if (!element) return;
    if (typeof IntersectionObserver === "undefined") {
      setViewVisible(wire.view, true);
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => setViewVisible(wire.view, entries.some((entry) => entry.isIntersecting)),
      { rootMargin: "160px" },
    );
    observer.observe(element);
    return () => {
      observer.disconnect();
      setViewVisible(wire.view, false);
    };
  }, [wire.view]);

  usePlateFlash(plate, changed, changed ? String(revision) : null);
  useRowFlash(
    body,
    marksAt === null || wire.editable
      ? null
      : `${wire.view}:${marksAt}:${render?.content_version}`,
  );

  return (
    <article
      ref={plate}
      className={[
        "card",
        changed ? "card-changed" : "",
        selected ? "card-selected" : "",
        wire.editable ? "card-source" : "",
        collapsed ? "card-collapsed" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      style={{
        left: at.x,
        top: at.y,
        width: box.width,
        // The plate keeps the height the layout planned for it, even while a
        // source is edited: an edge that ends at the planned bottom but a card
        // that grew past it is worse than a scrolling field.
        height: box.height,
      }}
      data-blink={changed ? "1" : undefined}
      data-view={wire.view}
      data-collapsed={collapsed ? "1" : undefined}
      onPointerDown={(event) => event.stopPropagation()}
      onDoubleClick={(event) => {
        if (
          (event.target as HTMLElement).closest(
            ".card-action, .card-grip, .field-chip, textarea, input, select, a, .card-name",
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
        <button
          type="button"
          className="card-name"
          title={`${wire.name} · open its facts`}
          onClick={(event) => {
            event.stopPropagation();
            void select({ kind: "module", view: wire.view });
          }}
        >
          {displayName(wire.name)}
        </button>
        <span className="card-kind">{kindWord(fields.length, fieldWire)}</span>
        <span className={`card-state state-${headerState}`} title={producerTitle}>
          <span
            className={`glyph ${headerState === "changed" ? "" : "hollow"}`.trim()}
            aria-hidden="true"
          />
          {ranAlike ? "same" : stateWord(headerState)}
        </span>
        <span
          className="card-version mono"
          title={newest ? "content version" : "current content version; the plate shows now"}
        >
          v{wire.content_version}
        </span>
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
                const moved = aligned && movedField(wire, entry.field);
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
            ref={body}
            className="card-body"
            tabIndex={0}
            role="group"
            aria-label={`${displayName(wire.name)} content`}
            onPointerDown={(event) => event.stopPropagation()}
          >
            {wire.editable ? (
              <Editor view={wire} compact />
            ) : field ? (
              <ModelView
                model={field.model}
                selectedRow={selectedRow}
                marksAt={marksAt}
                onSelectRow={(key, label, unit) => {
                  void select({ kind: "row", view: wire.view, rowKey: key, label, unit });
                }}
              />
            ) : (
              <p className="card-empty">{error ? "not drawn" : "drawing…"}</p>
            )}
            {error && field ? <p className="card-error">{error}</p> : null}
          </div>

          <footer className="card-foot">
            <span>{countWord(fieldWire, field?.model.kind, counts.drawn)}</span>
            {changed ? (
              <span
                className="changed-note"
                title={`the runtime reports this View changed at revision ${revision}`}
              >
                {/* Row counts describe the render on screen. Until the render
                    for this revision lands, or against an older record, only
                    the header can speak: no counts borrowed from another
                    revision's drawing. */}
                {marksAt !== null ? `${fieldMark} at r${revision}` : `content changed at r${revision}`}
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
                    ? "host source"
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
