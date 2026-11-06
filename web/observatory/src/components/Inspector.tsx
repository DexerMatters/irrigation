import { select, useStore } from "../state/store";
import { short } from "../canvas/Editor";

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
      {selection?.kind === "node" ? (
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
              <div className="kv" style={{ marginTop: 12 }}>
                <Fact label="created" value={String(node.changed_at)} />
                <Fact label="verified" value={String(node.verified_at)} />
                <Fact label="executed" value={String(node.executed_at)} />
                <Fact label="validated" value={String(node.validated_at)} />
                <Fact label="waiters" value={String(node.waiters)} />
              </div>
              <div className="section-head" style={{ marginTop: 16 }}>
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
      ) : "view" in selection ? (
        <ModuleFacts view={selection.view} />
      ) : (
        <p className="inspector-empty">Revision {selection.revision}.</p>
      )}
      {selection ? (
        <button type="button" className="btn quiet" style={{ marginTop: 14 }} onClick={() => void select(null)}>
          Clear
        </button>
      ) : null}
    </aside>
  );
}

function ComponentFacts({ componentKey, label }: { componentKey: string; label: string }) {
  const state = useStore((snapshot) => snapshot);
  const component = state.components.find((item) => item.component === componentKey);
  if (!component) return <p className="inspector-empty">This component is gone.</p>;
  return (
    <>
      <div className="eyebrow">{component.kind} component</div>
      <h1>{label.split("::").pop() ?? label}</h1>
      <div className="key mono">{component.name}</div>
      <div className="kv" style={{ marginTop: 12 }}>
        <Fact label="executed at" value={`r${component.executed_at}`} />
        <Fact label="invalidated at" value={component.suspect_at === 0 ? "never" : `r${component.suspect_at}`} />
        <Fact label="reads" value={String(component.reads.length)} />
        <Fact label="defines" value={String(component.writes.length)} />
      </div>
      <div className="section-head" style={{ marginTop: 16 }}>
        <h2>Views</h2>
        <span>{component.reads.length + component.writes.length}</span>
      </div>
      <div className="kv">
        {component.reads.map((view) => (
          <Fact key={`in-${view}`} label="reads" value={shortNameOf(view)} />
        ))}
        {component.writes.map((view) => (
          <Fact key={`out-${view}`} label="defines" value={shortNameOf(view)} />
        ))}
      </div>
    </>
  );
}

function shortNameOf(key: string): string {
  const view = useStoreSnapshotView(key);
  return view ?? key;
}

function useStoreSnapshotView(key: string): string | null {
  const state = useStore((snapshot) => snapshot);
  return state.views.find((view) => view.view === key)?.name.split("::").pop() ?? null;
}

function ModuleFacts({ view }: { view: string }) {
  const state = useStore((snapshot) => snapshot);
  const wire = state.views.find((candidate) => candidate.view === view);
  if (!wire) return <p className="inspector-empty">This View is gone.</p>;
  return (
    <>
      <div className="eyebrow">{wire.kind} View</div>
      <h1>{short(wire.name)}</h1>
      <div className="key mono">{wire.view}</div>
      <div className={`state-line state-${wire.state}`}>
        <span className="glyph hollow" aria-hidden="true" />
        {stateWord(wire.state)}
      </div>
      <div className="kv" style={{ marginTop: 12 }}>
        <Fact label="content version" value={`v${wire.content_version}`} />
        <Fact label="routing" value={String(wire.routing)} />
        <Fact label="changed units" value={String(wire.changed_units)} />
        <Fact label="editable" value={wire.editable ? "text · host syntax" : "read-only"} />
      </div>
      <div className="section-head" style={{ marginTop: 16 }}>
        <h2>Fields</h2>
        <span>{wire.fields.length}</span>
      </div>
      <div className="kv">
        {wire.fields.map((field) => (
          <Fact
            key={field.field}
            label={`${field.name} · ${field.kind}`}
            value={
              field.granularity === "whole_value"
                ? `whole value${field.elements === null ? "" : `, ${field.elements}`}`
                : `${field.units} units`
            }
          />
        ))}
      </div>
    </>
  );
}
