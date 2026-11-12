import { useEffect, useRef, useState } from "react";
import { Editor } from "../canvas/Editor";
import { displayName } from "../canvas/layout";
import { useRowFlash } from "../canvas/marks";
import { countWord, fieldNote, movedField, rowCounts } from "../canvas/viewInfo";
import { ModelView } from "./Render";
import {
  activeRevision,
  annotationRevision,
  atNewest,
  ensureRender,
  select,
  useStore,
} from "../state/store";
import type { FieldWire, Selection, ViewWire } from "../state/types";

const stateWord = (state: string): string =>
  state === "not_demanded" ? "not demanded" : state.replace("_", " ");

function Fact({ label, value }: { label: string; value: string }) {
  return (
    <div className="kv-row">
      <span className="label">{label}</span>
      <span className="value mono">{value}</span>
    </div>
  );
}

/**
 * Facts on demand. The canvas is the primary reading; this panel answers
 * "what exactly is this row" without competing with it.
 */
export function Inspector() {
  const state = useStore((snapshot) => snapshot);
  const selection = state.selection;
  const node = state.detail?.node;

  if (!selection) return null;

  return (
    <aside className="inspector" aria-label="Facts">
      <button
        type="button"
        className="inspector-close"
        aria-label="Close facts"
        title="Close (Esc)"
        onClick={() => void select(null)}
      >
        <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
          <path
            d="M2 2 L8 8 M8 2 L2 8"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.4"
            strokeLinecap="round"
          />
        </svg>
      </button>
      {selection.kind === "node" ? (
        <>
          <div className="eyebrow">Node</div>
          <h1>{selection.label}</h1>
          <div className="key mono">{selection.key}</div>
          {node ? (
            <>
              <div className={`state-line state-${node.state}`}>
                <span className="glyph hollow" aria-hidden="true" />
                {stateWord(node.state)} at r{node.changed_at}
              </div>
              <div className="kv">
                <Fact label="created" value={String(node.changed_at)} />
                <Fact label="verified" value={String(node.verified_at)} />
                <Fact label="executed" value={String(node.executed_at)} />
                <Fact label="validated" value={String(node.validated_at)} />
                <Fact label="waiters" value={String(node.waiters)} />
              </div>
              <div className="section-head">
                <h2>Dependencies</h2>
                <span>{state.detail?.dependencies.length ?? 0}</span>
              </div>
              <ul className="deps">
                {(state.detail?.dependencies ?? []).slice(0, 14).map((dependency) => (
                  <li key={dependency.node}>
                    <div className="dep">
                      <span className="name">{dependency.label}</span>
                      <span className="versions mono">
                        {dependency.recorded}
                        {dependency.changed ? (
                          <span className="moved"> → {dependency.current}</span>
                        ) : null}
                      </span>
                    </div>
                  </li>
                ))}
              </ul>
            </>
          ) : (
            <p className="inspector-empty">Reading the node…</p>
          )}
        </>
      ) : selection.kind === "component" ? (
        <ComponentFacts componentKey={selection.key} label={selection.label} />
      ) : selection.kind === "row" ? (
        <RowFacts selection={selection} />
      ) : "view" in selection ? (
        <ViewDetail view={selection.view} />
      ) : (
        <p className="inspector-empty">Revision {selection.revision}.</p>
      )}
    </aside>
  );
}

/**
 * One row's unit, opened from a plate.
 *
 * The board already says *that* a row moved; this says which unit it is, which
 * field carries it, and when it last moved — the three things a reader needs to
 * connect a row back to the host's own model.
 */
function RowFacts({ selection }: { selection: Extract<Selection, { kind: "row" }> }) {
  const state = useStore((snapshot) => snapshot);
  const wire = state.views.find((candidate) => candidate.view === selection.view);
  const unit = selection.unit;
  const revision = annotationRevision(state);
  const fresh = atNewest(state);
  const moved = unit !== null && unit.changed_at === revision;

  return (
    <>
      <div className="eyebrow">Row</div>
      <h1>{selection.label}</h1>
      <div className="key mono">
        {wire ? displayName(wire.name) : selection.view} · row {selection.rowKey}
      </div>
      {unit ? (
        <>
          <div className={`state-line state-${moved && fresh ? "changed" : "clean"}`}>
            <span className={`glyph ${moved && fresh ? "" : "hollow"}`} aria-hidden="true" />
            {moved && fresh ? `changed at r${revision}` : "unchanged at this revision"}
          </div>
          <div className="kv">
            <Fact label="unit kind" value={unit.kind} />
            <Fact label="field" value={`field ${unit.field}`} />
            <Fact label="entity" value={unit.entity ?? "—"} />
            <Fact
              label="changed at"
              value={unit.changed_at === null ? "never" : `r${unit.changed_at}`}
            />
          </div>
        </>
      ) : (
        <p className="inspector-empty">
          This row is drawn without a unit: the whole value moved, and nothing
          identifies this line on its own.
        </p>
      )}
      {wire ? (
        <button
          type="button"
          className="btn inspector-action"
          onClick={() => void select({ kind: "module", view: wire.view })}
        >
          Open {displayName(wire.name)}
        </button>
      ) : null}
    </>
  );
}

function ComponentFacts({ componentKey, label }: { componentKey: string; label: string }) {
  const state = useStore((snapshot) => snapshot);
  const component = state.components.find((item) => item.component === componentKey);
  if (!component) return <p className="inspector-empty">This component is gone.</p>;
  const viewName = (key: string) => {
    const found = state.views.find((view) => view.view === key)?.name;
    return found ? displayName(found) : key;
  };
  return (
    <>
      <div className="eyebrow">{component.kind} component</div>
      <h1>{displayName(label)}</h1>
      <div className="key mono">{component.name}</div>
      <div className="kv">
        <Fact
          label="executed at"
          value={component.executed_at === 0 ? "never" : `r${component.executed_at}`}
        />
        <Fact
          label="invalidated at"
          value={component.suspect_at === 0 ? "never" : `r${component.suspect_at}`}
        />
        <Fact label="reads" value={String(component.reads.length)} />
        <Fact label="defines" value={String(component.writes.length)} />
      </div>
      <div className="section-head">
        <h2>Views</h2>
        <span>{component.reads.length + component.writes.length}</span>
      </div>
      <div className="kv">
        {component.reads.map((view) => (
          <Fact key={`in-${view}`} label="reads" value={viewName(view)} />
        ))}
        {component.writes.map((view) => (
          <Fact key={`out-${view}`} label="defines" value={viewName(view)} />
        ))}
      </div>
    </>
  );
}

/**
 * One View, opened from the board. The plate and this panel share the same
 * projection and revision, so the panel is not a stale summary: rows keep their
 * change marks, the body updates with the stream, and a source View is the same
 * live text field here as it is on its plate.
 */
function ViewDetail({ view }: { view: string }) {
  const state = useStore((snapshot) => snapshot);
  const wire = state.views.find((candidate) => candidate.view === view);
  const render = state.renders[view];
  const error = state.renderErrors[view];
  const [chosen, setChosen] = useState<number | null>(null);
  const body = useRef<HTMLDivElement | null>(null);
  const record = activeRevision(state);
  const revision = annotationRevision(state);
  const aligned = atNewest(state);

  const fields = render?.fields ?? [];
  const drawnFields =
    wire?.fields.filter((candidate) => fields.some((entry) => entry.field === candidate.field)) ??
    [];
  const field = fields.find((entry) => entry.field === chosen) ?? fields[0];
  const fieldWire =
    wire?.fields.find((entry) => entry.field === field?.field) ?? wire?.fields[0];
  const counts = rowCounts(field?.model, fieldWire);
  const fieldMoved = wire && field ? movedField(wire, field.field) : false;
  const otherChange = wire
    ? fields.filter((entry) => entry.field !== field?.field && movedField(wire, entry.field)).length
    : 0;
  const note = fieldNote(counts, fieldWire, field?.model.kind, fieldMoved, otherChange);
  const newest = atNewest(state);
  const journaled = record?.changed_views.includes(view) ?? false;
  const changed = newest ? wire?.state === "changed" && aligned : journaled;
  const fieldChange = aligned && fieldMoved;
  const marksAt = aligned && render?.revision === revision ? revision : null;
  const selectedRow =
    state.selection?.kind === "row" && state.selection.view === view
      ? state.selection.rowKey
      : null;

  useEffect(() => {
    setChosen(null);
  }, [view]);

  useEffect(() => {
    ensureRender(view);
  }, [view, wire?.content_version]);

  useRowFlash(body, marksAt === null || wire?.editable ? null : `${view}:${marksAt}:${render?.content_version}`);

  if (!wire) return <p className="inspector-empty">This View is gone.</p>;

  return (
    <div className="view-detail" role="group" aria-label={`${displayName(wire.name)} facts`}>
      <div className="eyebrow">{wire.kind} View</div>
      <h1>{displayName(wire.name)}</h1>
      <div className="key mono">{wire.view}</div>
      <div className={`state-line state-${newest ? wire.state : changed ? "changed" : "clean"}`}>
        <span className="glyph hollow" aria-hidden="true" />
        {stateWord(newest ? wire.state : changed ? "changed" : "clean")}
        <span
          className="view-detail-rev mono"
          title={newest ? "content version" : "current content version; the panel shows now"}
        >
          v{wire.content_version}
        </span>
      </div>

      <div className="view-detail-actions">
        <span className="view-detail-note">
          {wire.editable ? "live text · edits commit as you type" : "read-only · drawn from the host"}
        </span>
      </div>

      <div className="view-detail-content">
        {fields.length > 1 ? (
          <div className="view-detail-fields" role="group" aria-label={`${displayName(wire.name)} fields`}>
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
                  onClick={() => setChosen(entry.field)}
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
            className={["view-detail-fields", "field-chip-moved-note", fieldChange ? "field-chip-moved" : ""]
              .filter(Boolean)
              .join(" ")}
          >
            <span className="field-dot" aria-hidden="true" />
            {wire.fields.find((candidate) => candidate.field === field.field)?.name ??
              `field ${field.field}`}
          </div>
        ) : null}

        <div
          ref={body}
          className={[
            "view-detail-body",
            field?.model.kind === "graph" ? "view-detail-body-graph" : "",
            wire.editable ? "view-detail-body-editor" : "",
          ]
            .filter(Boolean)
            .join(" ")}
          tabIndex={0}
          role="group"
          aria-label={`${displayName(wire.name)} content`}
        >
          {wire.editable ? (
            <Editor view={wire} />
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
            <p className="inspector-empty">Drawing this View…</p>
          )}
          {error ? <p className="card-error">{error}</p> : null}
        </div>

        <div className="view-detail-foot">
          <span>{countWord(fieldWire, field?.model.kind, counts.drawn)}</span>
          {changed ? (
            <span className="changed-note">
              {marksAt !== null ? `${note} at r${revision}` : `content changed at r${revision}`}
            </span>
          ) : (
            <span>following r{revision}</span>
          )}
        </div>
      </div>

      <ViewFacts wire={wire} fields={drawnFields} />
      {record ? (
        <div className="view-detail-reaction" title={record.label || "no label"}>
          r{record.revision} · {record.changed} changed · {record.executed} executed
        </div>
      ) : null}
    </div>
  );
}

function ViewFacts({ wire, fields }: { wire: ViewWire; fields: FieldWire[] }) {
  return (
    <>
      <div className="section-head">
        <h2>Fields</h2>
        <span>{fields.length}</span>
      </div>
      <div className="kv">
        {fields.map((field) => (
          <Fact
            key={field.field}
            label={`${field.name} · ${field.kind}`}
            value={
              field.granularity === "whole_value"
                ? `whole value${field.elements === null ? "" : `, ${field.elements}`}`
                : `${field.units} ${field.units === 1 ? "unit" : "units"}`
            }
          />
        ))}
      </div>
      <div className="kv">
        <Fact label="content version" value={`v${wire.content_version}`} />
        <Fact label="routing" value={String(wire.routing)} />
        <Fact label="changed units" value={String(wire.changed_units)} />
        <Fact label="retracted units" value={String(wire.retracted_units)} />
        <Fact label="editable" value={wire.editable ? "text · host syntax" : "read-only"} />
      </div>
    </>
  );
}
