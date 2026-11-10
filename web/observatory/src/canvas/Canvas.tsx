import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  activeRevision,
  follow,
  place,
  resetLayout,
  setCamera,
  toggleOverview,
  useStore,
} from "../state/store";
import { ViewCard } from "./ViewCard";
import { ComponentCard } from "./ComponentCard";
import { Minimap } from "./Minimap";
import { componentInks, inkForView, inkMap } from "./inks";
import {
  PHONE_BREAKPOINT,
  avoidOverlap,
  boardBounds,
  componentNode,
  assignPorts,
  edgeKey,
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
  // Edges take the ink of the component they belong to; the marker defs are
  // per ink so the arrowhead and port match the wire instead of staying green.
  const edgeInks = useMemo(
    () => [...new Set([...inks.map((entry) => entry.ink), "var(--faint)"])],
    [inks],
  );

  const collapsed = useMemo(
    () =>
      state.overview
        ? Object.fromEntries(state.views.map((view) => [view.view, true]))
        : state.collapsed,
    [state.overview, state.views, state.collapsed],
  );
  const board = useMemo(
    () => layoutBoard(state.views, state.components, state.renders, collapsed),
    [state.views, state.components, state.renders, collapsed],
  );

  // A drag overrides the computed slot; the envelope follows, so an edge to a
  // dragged card still reaches it.
  const placed = useMemo(() => {
    const merged = new Map(board.placement);
    for (const [node, at] of Object.entries(state.placement)) {
      if (merged.has(node)) merged.set(node, at);
    }
    // A position saved against an older layout may now sit on another plate.
    // Keep the automatic nodes still and walk each stored plate clear.
    for (const node of Object.keys(state.placement)) {
      const at = merged.get(node);
      if (!at || !board.sizes.has(node)) continue;
      merged.set(node, avoidOverlap(node, at, board.sizes, merged));
    }
    if (drag) merged.set(drag.node, drag.at);
    return merged;
  }, [board, state.placement, drag]);

  const bounds = useMemo(() => boardBounds(placed, board.sizes), [placed, board.sizes]);

  const ports = useMemo(() => assignPorts(board, placed), [board, placed]);

  const paths = useMemo(
    () =>
      board.edges.map((edge) => {
        const key = edgeKey(edge);
        return {
          edge,
          path: edgePath(edge, placed, board.sizes, { ends: ports.get(key) }),
        };
      }),
    [board, placed, ports],
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
    // The DAG runs downward. Fit its width, not its whole length: squeezing a
    // seven-level pipeline into 900px is what made every node unreadable. When
    // the whole board does fit at that scale, centre it; otherwise start at the
    // sources and let the reader pan down.
    const phone = box.width < PHONE_BREAKPOINT + 100;
    const side = phone ? 17 : 32;
    // The minimap owns the top-left corner. Fit inside the remaining window so
    // the first plate is not hidden underneath the map.
    const gutter = box.width > 900 ? 182 : 0;
    const usable = box.width - gutter - side * 2;
    const widthFit = usable / Math.max(bounds.width, 1);
    const zoom = Math.min(1.08, Math.max(MIN_ZOOM, widthFit));
    const fitsWidth = bounds.width * zoom <= usable;
    const fitsHeight = bounds.height * zoom <= box.height - 80;
    setCamera({
      x: fitsWidth
        ? gutter + side + (usable - bounds.width * zoom) / 2 - bounds.minX * zoom
        : gutter + side - bounds.minX * zoom,
      y: fitsHeight
        ? (box.height - bounds.height * zoom) / 2 - bounds.minY * zoom
        : 18 - bounds.minY * zoom,
      zoom,
    });
  }, [bounds, placed]);

  const navigate = useCallback(
    (point: { x: number; y: number }) => {
      touched.current = true;
      setCamera({
        ...state.camera,
        x: stage.width / 2 - point.x * state.camera.zoom,
        y: stage.height / 2 - point.y * state.camera.zoom,
      });
    },
    [stage, state.camera],
  );

  useEffect(() => {
    // Place the board once its cards exist, and again while renders settle —
    // until the reader moves it, which is theirs to keep.
    if (placed.size === 0) return;
    const signature = `${Math.round(bounds.width)}x${Math.round(bounds.height)}@${placed.size}@${Math.round(stage.width)}x${Math.round(stage.height)}`;
    if (fitted.current === signature) return;
    const first = fitted.current === "";
    fitted.current = signature;
    if (!first && touched.current) return;
    fit();
  }, [placed, bounds, fit, stage.width, stage.height]);

  useEffect(() => {
    const surfaceElement = surface.current;
    if (!surfaceElement) return;
    const onWheel = (event: WheelEvent) => {
      // A wheel over something that can scroll belongs to it. The native
      // listener on the surface runs before React's delegated handlers, so a
      // card cannot stop it from the inside; the check has to be here.
      const zooming = event.ctrlKey || event.metaKey;
      if (!zooming) {
        const target = event.target as HTMLElement | null;
        if (target?.closest(".minimap")) return;
        const scroller = target?.closest<HTMLElement>(".card-body, .editor-area");
        const body = target?.closest<HTMLElement>(".card-body");
        if (scroller && scroller.scrollHeight > scroller.clientHeight + 1) return;
        if (body && body.scrollHeight > body.clientHeight + 1) return;
      }
      event.preventDefault();
      touched.current = true;
      if (zooming) {
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
      if (drag) {
        place(
          drag.node,
          avoidOverlap(drag.node, drag.at, board.sizes, placed),
        );
      }
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

  const changedNodes = new Set<string>();
  for (const view of state.views) {
    if (reactionOf(view.view, view.state) === "changed") changedNodes.add(viewNode(view.view));
  }
  for (const component of state.components) {
    if (ranAt(component.component)) changedNodes.add(componentNode(component.component));
  }

  return (
    <div className="canvas">
      <div className="hud">
        <button type="button" className="hud-button" onClick={fit} title="Fit the board to the window width (f)">
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
        <button
          type="button"
          className="hud-button"
          aria-pressed={state.overview}
          onClick={() => {
            touched.current = false;
            fitted.current = "";
            toggleOverview();
          }}
          title="Fold every View to its header; open a detail for its content"
        >
          Overview
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
              {edgeInks.map((ink, index) => (
                <g key={ink}>
                  <marker
                    id={`edge-head-${index}`}
                    viewBox="0 0 10 10"
                    refX="9"
                    refY="5"
                    markerWidth="10"
                    markerHeight="10"
                    markerUnits="userSpaceOnUse"
                    orient="auto-start-reverse"
                  >
                    <path d="M1 1 L9 5 L1 9 Z" fill={ink} />
                  </marker>
                  <marker
                    id={`edge-read-${index}`}
                    viewBox="0 0 12 12"
                    refX="6"
                    refY="6"
                    markerWidth="12"
                    markerHeight="12"
                    markerUnits="userSpaceOnUse"
                    orient="auto-start-reverse"
                  >
                    <circle cx="6" cy="6" r="3.1" fill="var(--field)" stroke={ink} strokeWidth="1.4" />
                  </marker>
                  <marker
                    id={`edge-write-${index}`}
                    viewBox="0 0 12 12"
                    refX="6"
                    refY="6"
                    markerWidth="12"
                    markerHeight="12"
                    markerUnits="userSpaceOnUse"
                    orient="auto-start-reverse"
                  >
                    <rect x="2.6" y="2.6" width="6.8" height="6.8" fill={ink} />
                  </marker>
                </g>
              ))}
            </defs>
            {paths.map(({ edge, path }) => {
              if (!path) return null;
              const hot = ranAt(edge.component);
              const dim = spotlight !== null && spotlight !== edge.component;
              const ink = inksByKey.get(edge.component) ?? "var(--faint)";
              const marker = Math.max(0, edgeInks.indexOf(ink));
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
                    markerStart={
                      edge.kind === "reads"
                        ? `url(#edge-read-${marker})`
                        : `url(#edge-write-${marker})`
                    }
                    markerEnd={`url(#edge-head-${marker})`}
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
                producers={[
                  ...new Set(
                    state.components
                      .filter((component) => component.writes.includes(view.view))
                      .map((component) => component.name),
                  ),
                ]}
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

        <Minimap
          board={board}
          placed={placed}
          camera={state.camera}
          stage={stage}
          inks={inksByKey}
          changed={changedNodes}
          onNavigate={navigate}
        />

        {state.notice ? <p className="canvas-notice">{state.notice}</p> : null}
      </div>
    </div>
  );
}
