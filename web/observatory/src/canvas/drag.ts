import { useRef } from "react";
import { useStore } from "../state/store";
import type { Placement } from "../state/types";

/**
 * Drags one board node.
 *
 * The position is reported while the gesture runs and `null` when it ends, so
 * the board can show the move without writing storage on every pointer event
 * and persist exactly one final position. A control inside the card — an edit
 * button, a field chip — starts its own gesture instead of dragging the card.
 */
export function useNodeDrag(
  start: Placement,
  onDrag: (at: Placement | null) => void,
) {
  const zoom = useStore((snapshot) => snapshot.camera.zoom);
  const gesture = useRef<{ x: number; y: number; at: Placement } | null>(null);

  const onPointerDown = (event: React.PointerEvent<HTMLElement>) => {
    if (event.button !== 0) return;
    const target = event.target as HTMLElement;
    // A control inside the card starts its own gesture. The card itself may be a
    // button, so only a *different* button blocks the drag. The event must still
    // stop here: otherwise the board below treats the press as a pan and captures
    // the pointer, and the control never sees its click.
    if (target !== event.currentTarget && target.closest("button, textarea, input, a, select")) {
      event.stopPropagation();
      return;
    }
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    gesture.current = { x: event.clientX, y: event.clientY, at: start };
  };

  const onPointerMove = (event: React.PointerEvent<HTMLElement>) => {
    const current = gesture.current;
    if (!current) return;
    onDrag({
      x: Math.round(current.at.x + (event.clientX - current.x) / zoom),
      y: Math.round(current.at.y + (event.clientY - current.y) / zoom),
    });
  };

  const finish = () => {
    if (!gesture.current) return;
    gesture.current = null;
    onDrag(null);
  };

  return {
    onPointerDown,
    onPointerMove,
    onPointerUp: finish,
    onPointerCancel: finish,
  };
}
