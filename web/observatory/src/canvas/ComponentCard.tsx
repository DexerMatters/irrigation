import { select, useStore } from "../state/store";
import type { ComponentWire, Placement } from "../state/types";
import { COMPONENT_BOX, componentNode, shortName } from "./layout";
import { useNodeDrag } from "./drag";

/**
 * One component on the board, drawn as a chip riding the wire between the Views
 * it reads and the Views it defines.
 *
 * It is not a plate of its own: the runtime's unit of work is "this reads those
 * Views and defines these", and the board already draws a line for each of those
 * relations. Putting the name on the junction is the honest picture — a root, a
 * general component and an invoked one are the same object here, and the family
 * is a word on the chip, not a different sort of box.
 */
export function ComponentCard({
  component,
  at,
  ink,
  revision,
  ran,
  onDrag,
}: {
  component: ComponentWire;
  at: Placement;
  ink: string;
  revision: number;
  /** This component executed at the displayed revision. */
  ran: boolean;
  onDrag: (node: string, at: Placement | null) => void;
}) {
  const state = useStore((snapshot) => snapshot);
  const node = componentNode(component.component);
  const drag = useNodeDrag(at, (next) => onDrag(node, next));
  const selected =
    state.selection?.kind === "component" && state.selection.key === component.component;
  const stale = component.suspect_at === revision && component.suspect_at > 0;
  const flow = `${component.reads.length}→${component.writes.length}`;

  return (
    <button
      type="button"
      className={[
        "component",
        ran ? "component-changed" : "",
        selected ? "component-selected" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      style={{
        left: at.x,
        top: at.y,
        width: COMPONENT_BOX.width,
        height: COMPONENT_BOX.height,
      }}
      data-blink={ran ? "1" : undefined}
      data-component={component.component}
      {...drag}
      onClick={(event) => {
        event.stopPropagation();
        void select({ kind: "component", key: component.component, label: component.name });
      }}
      title={`${component.name} · ${component.kind} component · reads ${component.reads.length}, defines ${component.writes.length}`}
    >
      <span className="chip-line">
        <span className="chip-port" style={{ background: ink }} aria-hidden="true" />
        <span className="chip-name">{shortName(component.name)}</span>
        <span className="chip-flow mono">{flow}</span>
      </span>
      <span className="chip-line chip-meta">
        <span className="chip-kind">{component.kind}</span>
        <span className={["chip-state", ran ? "ran" : "", stale ? "stale" : ""].filter(Boolean).join(" ")}>
          {ran ? `ran r${component.executed_at}` : `last r${component.executed_at}`}
        </span>
      </span>
    </button>
  );
}
