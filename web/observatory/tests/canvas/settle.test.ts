import { describe, expect, it } from "vitest";
import type { Placement } from "../../src/state/types";
import { type Board, componentNode, viewNode } from "../../src/canvas/layout";
import { type Settled, settleBoard } from "../../src/canvas/settle";

/** A board stated as `id, x, y, width, height` — the parts a test cares about. */
type Spot = [string, number, number, number, number];

function board(spots: Spot[]): Board {
  const placement = new Map<string, Placement>();
  const sizes = new Map<string, { width: number; height: number }>();
  for (const [id, x, y, width, height] of spots) {
    placement.set(id, { x, y });
    sizes.set(id, { width, height });
  }
  return { placement, sizes, edges: [], ranks: new Map() };
}

const A = viewNode("v:a");
const B = viewNode("v:b");
const C = componentNode("c:1");

function settle(
  tidy: Board,
  kept: Settled | null,
  measured: (node: string) => boolean = () => true,
  folded: (node: string) => boolean = () => false,
  pins = new Map<string, Placement>(),
) {
  return settleBoard({ tidy, kept, folded, measured, pins });
}

function overlaps(left: Placement, right: Placement, size: { width: number; height: number }) {
  return (
    left.x < right.x + size.width &&
    left.x + size.width > right.x &&
    left.y < right.y + size.height &&
    left.y + size.height > right.y
  );
}

describe("settleBoard", () => {
  it("adopts the first tidy whole, once every box is measured", () => {
    const tidy = board([
      [A, 0, 0, 316, 200],
      [C, 100, 244, 180, 44],
      [B, 0, 332, 316, 120],
    ]);
    const result = settle(tidy, null);
    for (const node of [A, C, B]) {
      expect(result.plan.placement.get(node)).toEqual(tidy.placement.get(node));
      expect(result.settled.plan.placement.get(node)).toEqual(tidy.placement.get(node));
    }
    expect(result.settling).toBe(false);
  });

  it("keeps the board it adopted when the tidy says something else", () => {
    const tidy = board([
      [A, 0, 0, 316, 200],
      [B, 0, 244, 316, 120],
    ]);
    const first = settle(tidy, null);
    // The next revision re-measures A, so a fresh tidy would move both plates.
    const again = settle(board([
      [A, 600, 0, 316, 320],
      [B, 600, 364, 316, 120],
    ]), first.settled);
    expect(again.plan.placement.get(A)).toEqual({ x: 0, y: 0 });
    expect(again.plan.placement.get(B)).toEqual({ x: 0, y: 244 });
    expect(again.plan.sizes.get(A)).toEqual({ width: 316, height: 200 });
    expect(again.settling).toBe(false);
  });

  it("does not wait for a render that never lands to tidy", () => {
    const tidy = board([[A, 0, 0, 316, 200]]);
    const unmeasured = settle(tidy, null, (node) => node !== A);
    // The plate is drawn where the tidy put it, but nothing is adopted: the box
    // is a guess until the render arrives.
    expect(unmeasured.plan.placement.get(A)).toEqual({ x: 0, y: 0 });
    expect(unmeasured.settled.plan.placement.has(A)).toBe(false);
    expect(unmeasured.settling).toBe(true);
  });

  it("adopts the whole board once the last plate is measured", () => {
    const tidy = board([[A, 0, 0, 316, 200]]);
    const waiting = settle(tidy, null, () => false);
    expect(waiting.settling).toBe(true);
    const measured = settle(tidy, waiting.settled, () => true);
    expect(measured.settling).toBe(false);
    // The guess was never adopted, so the real box is the one that sticks.
    expect(measured.settled.plan.sizes.get(A)).toEqual({ width: 316, height: 200 });
  });

  it("does not adopt half of a board that is still guessing", () => {
    const tidy = board([
      [A, 0, 0, 316, 200],
      [B, 0, 244, 316, 120],
    ]);
    const waiting = settle(tidy, null, (node) => node !== B);
    const next = settle(tidy, waiting.settled, (node) => node !== B);
    // A is measured, but the tidy it sits in still moves with B's box; holding A
    // there would be adopting the shape of a guess.
    expect(next.settled.plan.placement.has(A)).toBe(false);
    expect(next.settling).toBe(true);
  });

  it("places a newcomer clear of the plates already there, without moving them", () => {
    const tidy = board([[A, 0, 0, 316, 200]]);
    const first = settle(tidy, null);
    // A third plate lands where a fresh tidy would put it on top of A.
    const arriving = settle(board([
      [A, 0, 0, 316, 200],
      [B, 20, 20, 316, 120],
    ]), first.settled);
    const a = arriving.plan.placement.get(A);
    const b = arriving.plan.placement.get(B);
    const size = arriving.plan.sizes.get(B);
    expect(a).toEqual({ x: 0, y: 0 });
    expect(b && size ? overlaps(a ?? { x: 0, y: 0 }, b, size) : true).toBe(false);
    expect(arriving.settled.plan.placement.has(B)).toBe(true);
  });

  it("measures a folded plate again in the slot it already has", () => {
    const tidy = board([[A, 0, 0, 316, 200]]);
    const first = settle(tidy, null);
    // The reader folds A: the tidy now wants a header box, and a fresh layout
    // would put it somewhere else entirely.
    const folded = settle(board([[A, 500, 900, 316, 35]]), first.settled, () => true, (node) => node === A);
    expect(folded.plan.placement.get(A)).toEqual({ x: 0, y: 0 });
    expect(folded.plan.sizes.get(A)).toEqual({ width: 316, height: 35 });
    // Opening it again restores the measured body, in the same slot.
    const opened = settle(board([[A, 500, 900, 316, 240]]), folded.settled, () => true, () => false);
    expect(opened.plan.placement.get(A)).toEqual({ x: 0, y: 0 });
    expect(opened.plan.sizes.get(A)).toEqual({ width: 316, height: 240 });
  });

  it("forgets a plate the runtime no longer reports", () => {
    const first = settle(board([
      [A, 0, 0, 316, 200],
      [B, 0, 244, 316, 120],
    ]), null);
    const shrunk = settle(board([[A, 0, 0, 316, 200]]), first.settled);
    expect(shrunk.plan.placement.has(B)).toBe(false);
    expect(shrunk.settled.plan.placement.has(B)).toBe(false);
  });

  it("keeps the tidy off the plates the reader dropped", () => {
    const pin = new Map([[A, { x: 0, y: 0 }]]);
    const tidy = board([
      [A, 0, 0, 316, 200],
      [B, 10, 10, 316, 120],
    ]);
    const result = settle(tidy, null, () => true, () => false, pin);
    const b = result.plan.placement.get(B);
    const size = result.plan.sizes.get(B);
    expect(b && size ? overlaps({ x: 0, y: 0 }, b, size) : true).toBe(false);
  });
});
