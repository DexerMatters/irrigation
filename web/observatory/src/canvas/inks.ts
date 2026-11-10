import { displayName } from "./layout";
import type { ComponentWire } from "../state/types";

/**
 * One ink per component. The palette is narrow and earthy on purpose: on a
 * technical paper a handful of hues read as signals without turning the board
 * into a rainbow. Past eight components the oldest ink is reused, because a
 * board with twenty hues encodes nothing.
 */
const INKS = [
  "#0E5D5A",
  "#8A6206",
  "#5B3F73",
  "#8C4A32",
  "#3F5A73",
  "#4F6B2E",
  "#7A4A63",
  "#2F6076",
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
