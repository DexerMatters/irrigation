import { activeRevision, follow, selectRevision, useStore } from "../state/store";

/**
 * The revision tape: one tick per published revision, and the exact counts the
 * stream reported for the revision in view.
 */
export function Tape() {
  const state = useStore((snapshot) => snapshot);
  const revisions = [...state.revisions].sort((left, right) => left.revision - right.revision);
  const record = activeRevision(state);
  const newest = state.revision;
  const oldest = revisions.length > 0 ? revisions[0].revision : newest;
  const span = Math.max(1, newest - oldest);
  const stats = state.stats;
  const arbitration =
    stats.write_claims + stats.write_conflicts + stats.deferred_writes + stats.demand_retries + stats.reader_waits;

  return (
    <footer className="tape-dock">
      <div className="tape-head">
        <strong>Revisions</strong>
        <span className="meta">
          {revisions.length === 0
            ? "nothing published yet"
            : `${oldest}–${newest} · ${revisions.length} retained`}
        </span>
        {arbitration > 0 ? (
          <span
            className="meta"
            title="Cumulative write arbitration: claims, conflicts, deferred write groups, demand retries, and reader waits."
          >
            {stats.write_claims} claims
            {stats.write_conflicts > 0 ? ` · ${stats.write_conflicts} conflicts` : ""}
            {stats.deferred_writes > 0 ? ` · ${stats.deferred_writes} deferred` : ""}
            {stats.demand_retries > 0 ? ` · ${stats.demand_retries} retries` : ""}
            {stats.reader_waits > 0 ? ` · ${stats.reader_waits} waits` : ""}
          </span>
        ) : null}
        {/* The board's key, on the strip rather than over the cards: the two
            marks at the ends of an edge, and which way each one points. */}
        <span className="edge-key" aria-label="Edge marks">
          <span className="edge-key-item">
            <svg width="42" height="12" aria-hidden="true">
              <line x1="1" y1="6" x2="32" y2="6" className="edge-sample reads" />
              <circle cx="3.5" cy="6" r="2.6" className="edge-port-read" />
              <path d="M34 2 L41 6 L34 10 Z" className="edge-arrow" />
            </svg>
            reads
          </span>
          <span className="edge-key-item">
            <svg width="42" height="12" aria-hidden="true">
              <line x1="1" y1="6" x2="32" y2="6" className="edge-sample writes" />
              <rect x="1" y="3.3" width="5.3" height="5.3" className="edge-port-write" />
              <path d="M34 2 L41 6 L34 10 Z" className="edge-arrow" />
            </svg>
            defines
          </span>
        </span>
      </div>
      <div className="tape" role="group" aria-label="Revisions">
        <div className="tape-axis" />
        {revisions.map((revision) => {
          const ratio = newest === oldest ? 1 : (revision.revision - oldest) / span;
          return (
            <button
              key={revision.revision}
              type="button"
              className={[
                "tick",
                revision.demands > 0 ? "demand" : "",
                record?.revision === revision.revision ? "selected" : "",
              ]
                .filter(Boolean)
                .join(" ")}
              style={{ left: `${4 + ratio * 92}%`, height: Math.min(16, 5 + Math.log2(1 + revision.changed) * 2.5) }}
              onClick={() => selectRevision(revision.revision)}
              aria-label={`Revision ${revision.revision}: ${revision.changed} changed, ${revision.executed} executed`}
              aria-pressed={record?.revision === revision.revision}
            />
          );
        })}
      </div>
      <div className="reaction">
        {record ? (
          <>
            <span className="mono" style={{ color: "var(--ink)" }}>
              r{record.revision}
            </span>
            <span className={`fact ${record.changed > 0 ? "changed" : ""}`}>
              <strong>{record.changed}</strong>changed
            </span>
            <span className="fact">
              <strong>{record.invalidated}</strong>invalidated
            </span>
            <span className="fact">
              <strong>{record.executed}</strong>executed
            </span>
            <span className="fact">
              <strong>{record.cutoffs}</strong>cutoff
            </span>
            <span className="fact">
              <strong>{record.demands}</strong>
              {record.demand_causes.length > 0
                ? ` (${record.demand_causes.join(", ").replace(/_/g, " ")})`
                : " demanded"}
            </span>
            <span className="spacer" style={{ flex: 1 }} />
            {state.following ? (
              <span className="cause">following the newest revision</span>
            ) : (
              <button type="button" className="hud-button" onClick={follow}>
                Follow
              </button>
            )}
          </>
        ) : (
          <span className="cause">waiting for the first revision</span>
        )}
      </div>
    </footer>
  );
}
