import { useRef } from "react";
import { useStore } from "../state/store";
import type { Placement } from "../state/types";

/**
 * Drags one board node from its grip.
 *
 * The position is reported while the gesture runs and `null` when it ends, so
 * the board can show the move without writing storage on every pointer event
 * and persist exactly one final position. Only the grip carries these handlers;
 * the rest of a plate keeps its normal text selection, scrolling and controls.
 */
export function useNodeDrag(
  start: Placement,
  onDrag: (at: Placement | null) => void,
) {
  const zoom = useStore((snapshot) => snapshot.camera.zoom);
  const gesture = useRef<{ x: number; y: number; at: Placement } | null>(null);

  const onPointerDown = (event: React.PointerEvent<HTMLElement>) => {
    if (event.button !== 0) return;
    // This handler is attached to the grip only, so there is no control inside
    // to defer to: the press is the drag. Capture keeps the gesture on the grip
    // even when the pointer outruns it or crosses another plate.
    event.preventDefault();
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
