import type { EventsWire, RevisionWire } from "./types";

/**
 * The pure part of the stream: a frame plus the state it applies to produces the
 * next bookkeeping values. No DOM, no store, no React — so a test can pin the
 * semantics the page relies on.
 */

export type Bookkeeping = {
  seq: number;
  revision: number;
  revisions: RevisionWire[];
  lastNoop: { label: string; revision: number } | null;
};

export type FrameResult = {
  bookkeeping: Bookkeeping;
  /** Views whose content this frame changed; the store refetches those. */
  dirty: string[];
  /** True when a sequence gap means the client must refetch everything. */
  resync: boolean;
  /** Set when a transaction published nothing. */
  notice: string | null;
};

export function emptyBookkeeping(): Bookkeeping {
  return { seq: 0, revision: 0, revisions: [], lastNoop: null };
}

export function upsertRevision(
  revisions: RevisionWire[],
  event: Extract<EventsWire["events"][number], { kind: "transaction" }>,
): RevisionWire[] {
  const record: RevisionWire = {
    revision: event.revision,
    label: event.label,
    staged: event.staged,
    changed: event.changed,
    invalidated: event.invalidated,
    published: true,
    demands: 0,
    demand_causes: [],
    executed: 0,
    validated: 0,
    cutoffs: 0,
    failed: 0,
    notifications: 0,
    duration_ns: event.duration_ns,
    changed_views: [...event.views],
    cutoff_views: [],
    executed_nodes: [],
    cutoff_nodes: [],
    invalidated_seeds: [],
    activity_capped: false,
    incomplete: false,
  };
  return [record, ...revisions.filter((existing) => existing.revision !== event.revision)];
}

function patchLatest(
  revisions: RevisionWire[],
  patch: (record: RevisionWire) => RevisionWire,
): RevisionWire[] {
  if (revisions.length === 0) return revisions;
  return [patch(revisions[0]), ...revisions.slice(1)];
}

export function reduceFrame(current: Bookkeeping, frame: EventsWire): FrameResult {
  if (frame.seq <= current.seq) {
    return { bookkeeping: current, dirty: [], resync: false, notice: null };
  }
  if (current.seq > 0 && frame.seq > current.seq + 1) {
    // A gap is a fact about the transport, not something to smooth over.
    return {
      bookkeeping: current,
      dirty: [],
      resync: true,
      notice: "Stream gap: refetching the workspace.",
    };
  }

  let revisions = current.revisions;
  let revision = current.revision;
  let lastNoop = current.lastNoop;
  const dirty = new Set<string>();

  for (const event of frame.events) {
    switch (event.kind) {
      case "transaction": {
        if (!event.published) {
          lastNoop = { label: event.label || "untitled", revision: event.revision };
          break;
        }
        revision = event.revision;
        revisions = upsertRevision(revisions, event);
        for (const view of event.views) dirty.add(view);
        break;
      }
      case "committed": {
        // A cutoff still executed; what it did not do is change its value.
        revisions = patchLatest(revisions, (record) => ({
          ...record,
          executed: record.executed + 1,
          executed_nodes: [...record.executed_nodes, event.node],
          ...(event.changed
            ? {
                changed_views:
                  event.view && !record.changed_views.includes(event.view)
                    ? [...record.changed_views, event.view]
                    : record.changed_views,
              }
            : { cutoff_nodes: [...record.cutoff_nodes, event.node] }),
        }));
        if (event.changed && event.view) dirty.add(event.view);
        break;
      }
      case "validated":
        revisions = patchLatest(revisions, (record) => ({
          ...record,
          validated: record.validated + 1,
        }));
        break;
      case "failed":
        revisions = patchLatest(revisions, (record) => ({
          ...record,
          failed: record.failed + 1,
        }));
        break;
      case "invalidated":
        revisions = patchLatest(revisions, (record) => ({
          ...record,
          invalidated: event.count,
          invalidated_seeds: event.seeds,
        }));
        break;
      case "notified":
        revisions = patchLatest(revisions, (record) => ({
          ...record,
          notifications: record.notifications + 1,
        }));
        break;
      default:
        break;
    }
  }

  return {
    bookkeeping: { seq: frame.seq, revision, revisions, lastNoop },
    dirty: [...dirty],
    resync: false,
    notice: null,
  };
}
