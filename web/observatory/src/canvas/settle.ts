import type { Placement } from "../state/types";
import { avoidOverlap, type Board, type Box, type NodeId } from "./layout";

/**
 * The board is tidied once, and after that it is a plan.
 *
 * `layoutBoard` answers "where would this graph go if it were drawn from
 * scratch?". That is the right answer on the pass that first draws the board and
 * the wrong answer on every pass after it: a runtime publishes on each keystroke,
 * every published revision re-reads a View, and re-answering the question moves
 * plates the reader is in the middle of reading. So the tidy is adopted once —
 * each plate keeps the slot and the box it was given — and later passes only
 * place newcomers.
 *
 * A plate is adopted only when its box is *measured*. A View draws its content
 * from a render that lands a round trip after the headers that created it, so a
 * box measured before that render is a guess; adopting guesses would freeze the
 * board at the shape it had before it drew anything. Until every box is real the
 * tidy is drawn afresh each pass (`settling`), which happens in the first few
 * hundred milliseconds and is what the camera waits for.
 *
 * A newcomer takes its tidy slot stepped clear of the plates already adopted.
 * Nothing that is already on the board moves to make room: an arrival rearranges
 * the reader's board no more than an edit does. The one deliberate exception is
 * a fold — the reader asking a plate to change its own body — which re-measures
 * that plate in its own slot.
 */

export type Plan = {
  placement: Map<NodeId, Placement>;
  sizes: Map<NodeId, Box>;
};

/** What a pass holds still, and what it needs to recognise it again. */
export type Settled = {
  plan: Plan;
  /** The fold each held plate was measured at: a flip measures it again. */
  folds: Map<NodeId, boolean>;
};

export type SettleInput = {
  /** This pass's tidy geometry, from `layoutBoard`. */
  tidy: Board;
  /** What the previous pass held still, or null on the pass that tidies. */
  kept: Settled | null;
  /** Whether a node is drawn folded; a folded plate has no body to measure. */
  folded: (node: NodeId) => boolean;
  /** Whether a node's box is real — its content has been read. */
  measured: (node: NodeId) => boolean;
  /** Plates the reader dropped. They are the reader's, not the layout's. */
  pins: Map<NodeId, Placement>;
};

export type SettleResult = {
  /** What to draw: held plates in their own slot, newcomers in theirs. */
  plan: Plan;
  /** What to hand back as `kept` on the next pass. */
  settled: Settled;
  /** True while some plate is still waiting for the box it will keep. */
  settling: boolean;
};

export function settleBoard(input: SettleInput): SettleResult {
  const { tidy, kept, folded, measured, pins } = input;

  // Every plate's box is real, so the tidy in hand can be adopted as it stands.
  let ready = true;
  for (const node of tidy.sizes.keys()) {
    if (!measured(node)) {
      ready = false;
      break;
    }
  }

  const placement = new Map<NodeId, Placement>();
  const sizes = new Map<NodeId, Box>();
  const folds = new Map<NodeId, boolean>();
  // The plates this pass holds still: what the next pass must find unchanged.
  const held = new Set<NodeId>();
  // Everything already on the board, a dropped plate included, is in the way of
  // whatever is placed after it.
  const occupied = new Map<NodeId, Placement>(pins);
  // The box each occupied plate really draws, which is not always the box a
  // fresh tidy would give it.
  const drawn = new Map<NodeId, Box>();
  for (const node of pins.keys()) {
    const box = tidy.sizes.get(node);
    if (box) drawn.set(node, box);
  }

  /** A wanted slot, walked clear of the plates already on the board. */
  const slot = (node: NodeId, wanted: Placement, box: Box): Placement => {
    drawn.set(node, box);
    return avoidOverlap(node, wanted, drawn, occupied);
  };

  /** Put a plate on the board; `keep` also makes the next pass respect it. */
  const put = (node: NodeId, at: Placement, box: Box, fold: boolean, keep: boolean) => {
    placement.set(node, at);
    sizes.set(node, box);
    folds.set(node, fold);
    drawn.set(node, box);
    if (!pins.has(node)) occupied.set(node, at);
    if (keep) held.add(node);
  };

  for (const [node, at] of kept?.plan.placement ?? []) {
    const box = kept?.plan.sizes.get(node);
    // Gone from the runtime: the slot is free and nobody remembers the node.
    if (!box || !tidy.sizes.has(node)) continue;
    const fold = folded(node);
    const was = kept?.folds.get(node);
    // The reader folded or opened this plate. That is a demand on the plate's
    // own body, not a new arrangement of the board: it is measured again where
    // it stands, and only steps aside if the new box would land on a neighbour.
    if (was !== undefined && was !== fold) {
      // A fold is measured from the same plate, so the tidy box is the one the
      // new fold asks for.
      const remeasured = tidy.sizes.get(node) ?? box;
      put(node, slot(node, at, remeasured), remeasured, fold, true);
      continue;
    }
    put(node, at, box, fold, true);
  }

  const rest = [...tidy.sizes.keys()].filter((node) => !placement.has(node));
  // Top-down and left to right: the tidy plan's own order, so a plate that has
  // to step around a held one stays on the side its neighbours are on.
  rest.sort((left, right) => {
    const a = tidy.placement.get(left);
    const b = tidy.placement.get(right);
    return (a?.y ?? 0) - (b?.y ?? 0) || (a?.x ?? 0) - (b?.x ?? 0);
  });

  for (const node of rest) {
    const at = tidy.placement.get(node);
    const box = tidy.sizes.get(node);
    if (!at || !box) continue;
    // A plate whose content has not been read keeps a guess for a box, so it is
    // not held still: the next pass may measure it.
    put(node, slot(node, at, box), box, folded(node), measured(node));
  }

  // Nothing is adopted before the first tidy is measured in full; after that
  // (and for a plate added later) the held set is exactly what the board keeps.
  // The plan already in hand is the witness: an empty one means this board has
  // never been tidied for real, so a measured plate cannot be held yet — the
  // tidy it was measured in is still moving.
  const adopted = kept !== null && kept.plan.placement.size > 0;
  const adopt = adopted || ready;
  const settledPlan: Plan = { placement: new Map(), sizes: new Map() };
  const settledFolds = new Map<NodeId, boolean>();
  if (adopt) {
    for (const node of held) {
      const at = placement.get(node);
      const box = sizes.get(node);
      if (!at || !box) continue;
      settledPlan.placement.set(node, at);
      settledPlan.sizes.set(node, box);
      settledFolds.set(node, folds.get(node) ?? false);
    }
  }

  return {
    plan: { placement, sizes },
    settled: { plan: settledPlan, folds: settledFolds },
    settling: !adopt || held.size < placement.size,
  };
}
