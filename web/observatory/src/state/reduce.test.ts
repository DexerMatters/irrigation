import { describe, expect, it } from "vitest";
import { emptyBookkeeping, reduceFrame, upsertRevision } from "./reduce";
import type { EventsWire, RevisionWire } from "./types";

const transaction = (
  revision: number,
  views: string[],
  published = true,
): EventsWire["events"][number] => ({
  kind: "transaction",
  revision,
  label: "",
  staged: 3,
  changed: 2,
  published,
  invalidated: 7,
  views,
  duration_ns: 1_000,
});

const frame = (seq: number, events: EventsWire["events"]): EventsWire => ({ seq, events });

const knownRevision = (revision: number): RevisionWire => ({
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
});

describe("reduceFrame", () => {
  it("applies a transaction and names the Views it changed", () => {
    const result = reduceFrame(emptyBookkeeping(), frame(1, [transaction(1, ["v:aa"])]));
    expect(result.resync).toBe(false);
    expect(result.bookkeeping.seq).toBe(1);
    expect(result.bookkeeping.revision).toBe(1);
    expect(result.dirty).toEqual(["v:aa"]);
    expect(result.bookkeeping.revisions[0]).toMatchObject({
      revision: 1,
      changed: 2,
      invalidated: 7,
      changed_views: ["v:aa"],
    });
  });

  it("counts executed work and validated steps without inventing a revision", () => {
    const first = reduceFrame(emptyBookkeeping(), frame(1, [transaction(1, ["v:aa"])]));
    const second = reduceFrame(first.bookkeeping, frame(2, [
      { kind: "committed", node: "n:1@0", view: "v:aa", changed: true, duration_ns: 5 },
      { kind: "committed", node: "n:2@0", view: "v:bb", changed: false, duration_ns: 5 },
      { kind: "validated", node: "n:3@0", deps: 2 },
      { kind: "notified", view: "v:aa", previous: 1, current: 2 },
    ] as EventsWire["events"]));
    const record = second.bookkeeping.revisions[0];
    expect(second.bookkeeping.revision).toBe(1);
    expect(record.executed).toBe(2);
    expect(record.validated).toBe(1);
    expect(record.notifications).toBe(1);
    expect(record.executed_nodes).toEqual(["n:1@0", "n:2@0"]);
    expect(record.cutoff_nodes).toEqual(["n:2@0"]);
  });

  it("reports a no-op transaction without publishing a revision", () => {
    const first = reduceFrame(emptyBookkeeping(), frame(1, [transaction(1, [])]));
    const second = reduceFrame(first.bookkeeping, frame(2, [transaction(2, [], false)]));
    expect(second.bookkeeping.revision).toBe(1);
    expect(second.bookkeeping.revisions).toHaveLength(1);
    expect(second.bookkeeping.lastNoop).toEqual({ label: "untitled", revision: 2 });
  });

  it("refuses a sequence gap instead of smoothing it over", () => {
    const first = reduceFrame(emptyBookkeeping(), frame(5, [transaction(1, [])]));
    const gap = reduceFrame(first.bookkeeping, frame(9, [transaction(2, [])]));
    expect(gap.resync).toBe(true);
    expect(gap.bookkeeping).toBe(first.bookkeeping);
    expect(gap.notice).toMatch(/gap/i);
  });

  it("ignores a frame it already applied", () => {
    const first = reduceFrame(emptyBookkeeping(), frame(3, [transaction(1, [])]));
    const duplicate = reduceFrame(first.bookkeeping, frame(3, [transaction(9, [])]));
    expect(duplicate.bookkeeping).toBe(first.bookkeeping);
    expect(duplicate.dirty).toEqual([]);
  });

  it("keeps revisions newest first and replaces rather than duplicates", () => {
    const one = upsertRevision([], { ...transaction(1, []), published: true } as Extract<
      EventsWire["events"][number],
      { kind: "transaction" }
    >);
    const two = upsertRevision(one, { ...transaction(2, []), published: true } as Extract<
      EventsWire["events"][number],
      { kind: "transaction" }
    >);
    expect(two.map((record) => record.revision)).toEqual([2, 1]);
    const again = upsertRevision(two, { ...transaction(2, []), published: true } as Extract<
      EventsWire["events"][number],
      { kind: "transaction" }
    >);
    expect(again.map((record) => record.revision)).toEqual([2, 1]);
  });

  it("does not mark a View dirty when a commit only cut off", () => {
    const seed = { ...emptyBookkeeping(), revisions: [knownRevision(1)], seq: 1, revision: 1 };
    const result = reduceFrame(seed, frame(2, [
      { kind: "committed", node: "n:1@0", view: "v:aa", changed: false, duration_ns: 5 },
    ] as EventsWire["events"]));
    expect(result.dirty).toEqual([]);
    expect(result.bookkeeping.revisions[0].cutoff_nodes).toEqual(["n:1@0"]);
  });
});
