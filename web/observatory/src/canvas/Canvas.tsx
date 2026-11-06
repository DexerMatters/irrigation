import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  activeRevision,
  follow,
  place,
  resetLayout,
  setCamera,
  useStore,
} from "../state/store";
import { ViewCard } from "./ViewCard";
import { ComponentCard } from "./ComponentCard";
import { componentInks, inkForView, inkMap } from "./inks";
import {
  PHONE_BREAKPOINT,
  boardBounds,
  componentNode,
  edgePath,
  layoutBoard,
  viewNode,
} from "./layout";
import type { Placement } from "../state/types";

const MIN_ZOOM = 0.14;
const MAX_ZOOM = 2.2;

export type Drag = { node: string; at: Placement } | null;

/** What one revision did to one View, as far as the page can say. */
export type Reaction = "changed" | "cutoff" | null;

/**
 * The board: one zoomable, movable surface holding every View and component and
 * the edges between them. Wheel pans, ctrl/⌘ + wheel zooms at the pointer, and a
 * card drags itself. A drag is held here while it happens and written once when
 * it ends, so a gesture never touches storage.
 */
export function Canvas() {
  const state = useStore((snapshot) => snapshot);
  const surface = useRef<HTMLDivElement | null>(null);
  const pan = useRef<{ x: number; y: number; camera: { x: number; y: number } } | null>(null);
  // Until the reader moves the board, it re-fits as renders change card sizes.
  const touched = useRef(false);
  const fitted = useRef("");
  const [drag, setDrag] = useState<Drag>(null);
  const [spotlight, setSpotlight] = useState<string | null>(null);
  const [stage, setStage] = useState({ width: 1280, height: 720 });

  const record = activeRevision(state);
  const shownRevision = record?.revision ?? state.revision;
  const followingNewest = state.following || state.selectedRevision === state.revision;
  const journaled = useMemo(() => new Set(record?.changed_views ?? []), [record]);

  /**
   * What the running revision did to one View. The header state is the
   * runtime's own verdict for the newest revision — changed content, or a run
   * that produced the same value (`cutoff`). For an older revision the journal
   * is the only witness, and it names the Views its transaction staged.
   */
  const reactionOf = (view: string, stateWord: string): Reaction =>
    followingNewest
      ? stateWord === "changed"
        ? "changed"
        : stateWord === "cutoff"
          ? "cutoff"
          : null
      : journaled.has(view)
        ? "changed"
        : null;

  const inks = useMemo(() => componentInks(state.components), [state.components]);
  const inksByKey = useMemo(() => inkMap(inks), [inks]);

  const board = useMemo(
    () => layoutBoard(state.views, state.components, state.renders, stage),
    [state.views, state.components, state.renders, stage],
  );

  // A drag overrides the computed slot; the envelope follows, so an edge to a
  // dragged card still reaches it.
  const placed = useMemo(() => {
    const merged = new Map(board.placement);
    for (const [node, at] of Object.entries(state.placement)) {
      if (merged.has(node)) merged.set(node, at);
    }
    if (drag) merged.set(drag.node, drag.at);
    return merged;
  }, [board, state.placement, drag]);

  const bounds = useMemo(() => boardBounds(placed, board.sizes), [placed, board.sizes]);

  const paths = useMemo(
    () =>
      board.edges.map((edge) => ({
        edge,
        path: edgePath(edge, placed, board.sizes),
      })),
    [board, placed],
  );

  useEffect(() => {
    // The board folds for the surface it is shown on, so the layout follows the
    // window rather than a fixed assumption about it.
    const element = surface.current;
    if (!element) return;
    const measure = () => {
      const box = element.getBoundingClientRect();
      if (box.width > 0 && box.height > 0) {
        setStage((current) =>
          Math.abs(current.width - box.width) < 8 && Math.abs(current.height - box.height) < 8
            ? current
            : { width: box.width, height: box.height },
        );
      }
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const zoomAt = useCallback(
    (factor: number, clientX: number, clientY: number) => {
      const box = surface.current?.getBoundingClientRect();
      if (!box) return;
      const { camera } = state;
      const zoom = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, camera.zoom * factor));
      const pointerX = clientX - box.left;
      const pointerY = clientY - box.top;
      // Keep the world point under the pointer fixed.
      const worldX = (pointerX - camera.x) / camera.zoom;
      const worldY = (pointerY - camera.y) / camera.zoom;
      setCamera({ x: pointerX - worldX * zoom, y: pointerY - worldY * zoom, zoom });
      touched.current = true;
    },
    [state],
  );

  const fit = useCallback(() => {
    const box = surface.current?.getBoundingClientRect();
    if (!box || placed.size === 0) return;
    // Narrow surfaces fit the width and let the board run past the bottom: a
    // plate you can read and scroll beats a whole graph you cannot.
    const fitWidth = box.width < PHONE_BREAKPOINT + 100;
    const zoom = Math.min(
      1.1,
      Math.max(
        MIN_ZOOM,
        fitWidth
          ? (box.width - 40) / Math.max(bounds.width, 1)
          : Math.min(
              (box.width - 64) / Math.max(bounds.width, 1),
              (box.height - 96) / Math.max(bounds.height, 1),
            ),
      ),
    );
    setCamera({
      x: (box.width - bounds.width * zoom) / 2 - bounds.minX * zoom,
      y: fitWidth ? 16 - bounds.minY * zoom : (box.height - bounds.height * zoom) / 2 - bounds.minY * zoom,
      zoom,
    });
  }, [bounds, placed]);

  useEffect(() => {
    // Place the board once its cards exist, and again while renders settle —
    // until the reader moves it, which is theirs to keep.
    if (placed.size === 0) return;
    const signature = `${Math.round(bounds.width)}x${Math.round(bounds.height)}@${placed.size}`;
    if (fitted.current === signature) return;
    const first = fitted.current === "";
    fitted.current = signature;
    if (!first && touched.current) return;
    fit();
  }, [placed, bounds, fit]);

  useEffect(() => {
    const surfaceElement = surface.current;
    if (!surfaceElement) return;
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      touched.current = true;
      if (event.ctrlKey || event.metaKey) {
        zoomAt(event.deltaY < 0 ? 1.08 : 1 / 1.08, event.clientX, event.clientY);
      } else {
        setCamera({
          ...state.camera,
          x: state.camera.x - event.deltaX,
          y: state.camera.y - event.deltaY,
        });
      }
    };
    surfaceElement.addEventListener("wheel", onWheel, { passive: false });
    return () => surfaceElement.removeEventListener("wheel", onWheel);
  }, [state.camera, zoomAt]);

  useEffect(() => {
    // The blink is a one-shot animation per revision: it marks the units the
    // runtime says changed, and nothing else.
    const surfaceElement = surface.current;
    if (!surfaceElement || shownRevision === 0) return;
    const targets = surfaceElement.querySelectorAll<HTMLElement>("[data-blink='1']");
    for (const target of targets) {
      target.animate(
        [
          {
            boxShadow: "0 0 0 0 color-mix(in srgb, var(--signal) 50%, transparent)",
            borderColor: "var(--signal)",
          },
          {
            boxShadow: "0 0 0 7px color-mix(in srgb, var(--signal) 0%, transparent)",
            borderColor: "var(--line)",
          },
        ],
        { duration: 760, easing: "cubic-bezier(0.2, 0.7, 0.3, 1)" },
      );
    }
    for (const row of surfaceElement.querySelectorAll<HTMLElement>(".row-changed")) {
      row.animate(
        [
          { backgroundColor: "color-mix(in srgb, var(--signal) 18%, transparent)" },
          { backgroundColor: "color-mix(in srgb, var(--signal) 0%, transparent)" },
        ],
        { duration: 900, easing: "ease-out" },
      );
    }
  }, [shownRevision]);

  const onPointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    pan.current = {
      x: event.clientX,
      y: event.clientY,
      camera: { x: state.camera.x, y: state.camera.y },
    };
  };

  const onPointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const current = pan.current;
    if (!current) return;
    touched.current = true;
    setCamera({
      ...state.camera,
      x: current.camera.x + (event.clientX - current.x),
      y: current.camera.y + (event.clientY - current.y),
    });
  };

  const onPointerUp = () => {
    pan.current = null;
  };

  // A card drag is reported here while it happens and persisted on release.
  const onNodeDrag = (node: string, at: Placement | null) => {
    if (at === null) {
      if (drag) place(drag.node, drag.at);
      setDrag(null);
      return;
    }
    touched.current = true;
    setDrag({ node, at });
  };

  const ranAt = (component: string): boolean => {
    if (shownRevision === 0) return false;
    const wire = state.components.find((item) => item.component === component);
    return wire?.executed_at === shownRevision;
  };

  return (
    <div className="canvas">
      <div className="hud">
        <button type="button" className="hud-button" onClick={fit} title="Fit the board (f)">
          Fit
        </button>
        <button
          type="button"
          className="hud-button"
          onClick={() => zoomAt(1.15, innerWidth / 2, innerHeight / 2)}
          aria-label="Zoom in"
        >
          +
        </button>
        <button
          type="button"
          className="hud-button"
          onClick={() => zoomAt(1 / 1.15, innerWidth / 2, innerHeight / 2)}
          aria-label="Zoom out"
        >
          −
        </button>
        <button
          type="button"
          className="hud-button hud-zoom"
          onClick={() => {
            touched.current = true;
            setCamera({ ...state.camera, zoom: 1 });
          }}
          title="Reset zoom to 100%"
        >
          {Math.round(state.camera.zoom * 100)}%
        </button>
        <button
          type="button"
          className="hud-button"
          onClick={() => {
            touched.current = false;
            fitted.current = "";
            resetLayout();
          }}
          title="Forget the dragged positions and lay the board out again"
        >
          Tidy
        </button>
        <span className="hud-sep" />
        <span className="legend" aria-label="Components">
          {inks.map((entry) => (
            <button
              key={entry.key}
              type="button"
              className="legend-item"
              aria-pressed={spotlight === entry.key}
              onPointerEnter={() => setSpotlight(entry.key)}
              onPointerLeave={() => setSpotlight(null)}
              onClick={() => setSpotlight(spotlight === entry.key ? null : entry.key)}
              title={`${entry.label}: reads ${entry.reads}, defines ${entry.writes} View(s)`}
            >
              <span className="legend-ink" style={{ background: entry.ink }} aria-hidden="true" />
              {entry.short}
              <span className="legend-count mono">
                {entry.reads}→{entry.writes}
              </span>
            </button>
          ))}
        </span>
        <span className="hud-sep" />
        {state.following ? (
          <span className="hud-note">following r{state.revision}</span>
        ) : (
          <button type="button" className="hud-button" onClick={follow}>
            Jump to r{state.revision}
          </button>
        )}
        {record && !followingNewest ? (
          <span className="hud-note">showing r{record.revision}</span>
        ) : null}
        <span style={{ flex: 1 }} />
        <span className="hud-note hud-hint">drag · ⌘/ctrl + wheel</span>
      </div>

      <div
        className="surface"
        ref={surface}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        role="application"
        aria-label="View canvas"
      >
        <div
          className="world"
          style={{
            transform: `translate(${state.camera.x}px, ${state.camera.y}px) scale(${state.camera.zoom})`,
          }}
        >
          <svg
            className="links"
            width={Math.max(0, bounds.maxX + 40)}
            height={Math.max(0, bounds.maxY + 40)}
            aria-hidden="true"
          >
            {/*
              The two ends say what the edge is and which way it runs: a port at
              the tail — hollow where a View is observed, solid where a component
              publishes — and an arrow at the head, pointing at whoever receives.
            */}
            <defs>
              <marker
                id="edge-head"
                viewBox="0 0 10 10"
                refX="9"
                refY="5"
                markerWidth="10"
                markerHeight="10"
                markerUnits="userSpaceOnUse"
                orient="auto-start-reverse"
              >
                <path d="M1 1 L9 5 L1 9 Z" className="edge-arrow" />
              </marker>
              <marker
                id="edge-read"
                viewBox="0 0 12 12"
                refX="6"
                refY="6"
                markerWidth="12"
                markerHeight="12"
                markerUnits="userSpaceOnUse"
                orient="auto-start-reverse"
              >
                <circle cx="6" cy="6" r="3.1" className="edge-port-read" />
              </marker>
              <marker
                id="edge-write"
                viewBox="0 0 12 12"
                refX="6"
                refY="6"
                markerWidth="12"
                markerHeight="12"
                markerUnits="userSpaceOnUse"
                orient="auto-start-reverse"
              >
                <rect x="2.6" y="2.6" width="6.8" height="6.8" className="edge-port-write" />
              </marker>
            </defs>
            {paths.map(({ edge, path }) => {
              if (!path) return null;
              const hot = ranAt(edge.component);
              const dim = spotlight !== null && spotlight !== edge.component;
              // An edge carries the relation, not the identity: the plate and its
              // legend chip already name the component, and a board of coloured
              // curves reads as a chart of nothing.
              const ink = "var(--signal)";
              return (
                <g
                  key={`${edge.from}->${edge.to}-${edge.kind}`}
                  className={["link", edge.kind, hot ? "hot" : "", dim ? "dim" : ""]
                    .filter(Boolean)
                    .join(" ")}
                  onPointerEnter={() => setSpotlight(edge.component)}
                  onPointerLeave={() => setSpotlight(null)}
                >
                  <title>{`${edge.label} ${edge.kind === "reads" ? "reads" : "defines"} this View · r${shownRevision}`}</title>
                  <path
                    className="edge"
                    d={path}
                    stroke={ink}
                    markerStart={edge.kind === "reads" ? "url(#edge-read)" : "url(#edge-write)"}
                    markerEnd="url(#edge-head)"
                  />
                </g>
              );
            })}
          </svg>

          {state.views.map((view) => {
            const at = placed.get(viewNode(view.view));
            if (!at) return null;
            return (
              <ViewCard
                key={view.view}
                wire={view}
                at={at}
                box={board.sizes.get(viewNode(view.view)) ?? { width: 316, height: 220 }}
                revision={shownRevision}
                followingNewest={followingNewest}
                reaction={reactionOf(view.view, view.state)}
                ink={inkForView(inks, state.components, view.view)}
                producer={
                  state.components.find((component) => component.writes.includes(view.view))
                    ?.name ?? null
                }
                onDrag={onNodeDrag}
              />
            );
          })}

          {state.components.map((component) => {
            const at = placed.get(componentNode(component.component));
            if (!at) return null;
            return (
              <ComponentCard
                key={component.component}
                component={component}
                at={at}
                ink={inksByKey.get(component.component) ?? "var(--faint)"}
                revision={shownRevision}
                ran={ranAt(component.component)}
                onDrag={onNodeDrag}
              />
            );
          })}
        </div>

        {state.notice ? <p className="canvas-notice">{state.notice}</p> : null}
      </div>
    </div>
  );
}
