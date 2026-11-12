import { describe, expect, it } from "vitest";
import {
  activeRevisionOf,
  annotationRevisionOf,
  atNewestOf,
  shownRevisionOf,
  type RevisionState,
} from "../../src/state/revision";
import type { RevisionWire } from "../../src/state/types";

function record(revision: number): RevisionWire {
  return {
    revision,
    label: "",
    staged: 1,
    changed: 1,
    invalidated: 0,
    published: true,
    demands: 0,
    demand_causes: [],
    executed: 0,
    validated: 0,
    cutoffs: 0,
    failed: 0,
    notifications: 0,
    duration_ns: 1,
    changed_views: [],
    cutoff_views: [],
    executed_nodes: [],
    cutoff_nodes: [],
    invalidated_seeds: [],
    activity_capped: false,
    incomplete: false,
  };
}

function state(patch: Partial<RevisionState> = {}): RevisionState {
  return {
    revision: 3,
    dataRevision: 3,
    selectedRevision: null,
    following: true,
    revisions: [record(3), record(2)],
    ...patch,
  };
}

describe("the revision on screen", () => {
  it("shows the newest revision while following", () => {
    expect(shownRevisionOf(state())).toBe(3);
    expect(atNewestOf(state())).toBe(true);
  });

  it("shows the selected record when the reader picked an older one", () => {
    const picked = state({ following: false, selectedRevision: 2 });
    expect(shownRevisionOf(picked)).toBe(2);
    expect(atNewestOf(picked)).toBe(false);
  });

  it("falls back to the numeric revision before the journal has the record", () => {
    const early = state({ revisions: [], dataRevision: 0 });
    expect(activeRevisionOf(early)).toBeNull();
    expect(shownRevisionOf(early)).toBe(3);
  });
});

describe("the revision the annotations describe", () => {
  it("is the data read while following the newest revision", () => {
    expect(annotationRevisionOf(state({ revision: 3, dataRevision: 3 }))).toBe(3);
  });

  it("stays on the old read between the revision event and its workspace", () => {
    // r3 was just announced; the headers and renders on screen are still r2.
    // Annotating at r3 would either mislabel r2's rows or blank every mark
    // until the read lands. The board keeps its truthful r2 annotations.
    expect(annotationRevisionOf(state({ revision: 3, dataRevision: 2 }))).toBe(2);
  });

  it("follows the data even when the runtime is ahead of the journal", () => {
    expect(annotationRevisionOf(state({ revision: 3, dataRevision: 4 }))).toBe(4);
  });

  it("is the selected record while an older revision is on screen", () => {
    expect(
      annotationRevisionOf(
        state({ following: false, selectedRevision: 2, revision: 9, dataRevision: 9 }),
      ),
    ).toBe(2);
  });

  it("falls back to the journal revision before any workspace read", () => {
    const empty = state({ revisions: [], revision: 0, dataRevision: 0 });
    expect(annotationRevisionOf(empty)).toBe(0);
  });
});
