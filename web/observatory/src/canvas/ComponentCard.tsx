import { useRef, type CSSProperties } from "react";
import { select, useStore } from "../state/store";
import type { ComponentWire, Placement } from "../state/types";
import { COMPONENT_BOX, componentNode, displayName } from "./layout";
import { useNodeDrag } from "./drag";
import { usePlateFlash } from "./marks";

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
  const chip = useRef<HTMLDivElement | null>(null);
  const node = componentNode(component.component);
  const drag = useNodeDrag(at, (next) => onDrag(node, next));
  const selected =
    state.selection?.kind === "component" && state.selection.key === component.component;
  const stale = component.suspect_at === revision && component.suspect_at > 0;
  const flow = `${component.reads.length}→${component.writes.length}`;

  const openFacts = () => {
    void select({ kind: "component", key: component.component, label: component.name });
  };
  usePlateFlash(chip, ran, ran ? String(component.executed_at) : null);

  return (
    <div
      ref={chip}
      className={[
        "component",
        ran ? "component-changed" : "",
        selected ? "component-selected" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      style={
        {
          left: at.x,
          top: at.y,
          width: COMPONENT_BOX.width,
          height: COMPONENT_BOX.height,
          "--component-ink": ink,
        } as CSSProperties
      }
      data-blink={ran ? "1" : undefined}
      data-component={component.component}
      onPointerDown={(event) => event.stopPropagation()}
    >
      <button
        type="button"
        className="component-grip"
        {...drag}
        aria-label={`Drag ${displayName(component.name)}`}
        title="Drag this component"
        onClick={(event) => event.stopPropagation()}
      >
        <GripIcon />
      </button>
      <button
        type="button"
        className="component-facts"
        onClick={(event) => {
          event.stopPropagation();
          openFacts();
        }}
        title={`${component.name} · ${component.kind} component · reads ${component.reads.length}, defines ${component.writes.length}`}
      >
        <span className="chip-line">
          <span className="chip-name">{displayName(component.name)}</span>
          <span className="chip-flow mono">{flow}</span>
        </span>
        <span className="chip-line chip-meta">
          <span className="chip-kind">{component.kind}</span>
          <span className={["chip-state", ran ? "ran" : "", stale ? "stale" : ""].filter(Boolean).join(" ")}>
            {ran
              ? `ran r${component.executed_at}`
              : component.executed_at > 0
                ? `last r${component.executed_at}`
                : "never ran"}
          </span>
        </span>
      </button>
    </div>
  );
}

function GripIcon() {
  return (
    <svg className="grip-icon" viewBox="0 0 10 14" width="10" height="14" aria-hidden="true">
      <g fill="currentColor">
        <circle cx="3" cy="3" r="1.1" />
        <circle cx="7" cy="3" r="1.1" />
        <circle cx="3" cy="7" r="1.1" />
        <circle cx="7" cy="7" r="1.1" />
        <circle cx="3" cy="11" r="1.1" />
        <circle cx="7" cy="11" r="1.1" />
      </g>
    </svg>
  );
}
