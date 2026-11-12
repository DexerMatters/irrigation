import { displayName } from "./layout";
import type { ComponentWire } from "../state/types";

/**
 * One ink per component. The palette is narrow and earthy on purpose: on a
 * technical paper a handful of hues read as signals without turning the board
 * into a rainbow. The tones are lifted for a dark ground — the wire, the legend
 * dot and the chip tint all have to be readable at 1px — and past eight
 * components the oldest ink is reused, because a board with twenty hues encodes
 * nothing.
 */
const INKS = [
  "#3E9C8F",
  "#B98A3A",
  "#8D74B5",
  "#C0704F",
  "#5B87AE",
  "#7E9C4C",
  "#B06F8C",
  "#4E9BB0",
];

export type ComponentInk = {
  key: string;
  label: string;
  short: string;
  ink: string;
  reads: number;
  writes: number;
};

/** Every component the runtime reports, each with a stable ink. */
export function componentInks(components: ComponentWire[]): ComponentInk[] {
  return components.map((component, index) => ({
    key: component.component,
    label: component.name,
    short: displayName(component.name),
    ink: INKS[index % INKS.length],
    reads: component.reads.length,
    writes: component.writes.length,
  }));
}

export function inkMap(inks: ComponentInk[]): Map<string, string> {
  return new Map(inks.map((entry) => [entry.key, entry.ink]));
}

/**
 * The ink of the component that defines a View.
 *
 * One rule for every View: the colour is the component that writes it. A source
 * View has no such component — the host owns it — so it gets the neutral ink and
 * no borrowed identity from whoever happens to read it.
 */
export function inkForView(
  inks: ComponentInk[],
  components: ComponentWire[],
  key: string,
): string {
  const producer = components.find((component) => component.writes.includes(key));
  if (!producer) return "var(--faint)";
  return inks.find((entry) => entry.key === producer.component)?.ink ?? "var(--faint)";
}
