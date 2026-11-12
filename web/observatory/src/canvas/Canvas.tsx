import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  activeRevision,
  annotationRevision,
  atNewest,
  follow,
  place,
  resetLayout,
  setCamera,
  toggleMinimap,
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
  type Board,
  type NodeId,
} from "./layout";
import { settleBoard, type Settled } from "./settle";
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
 *
 * The board is tidied once, when its plates have all been measured, and then it
 * is the reader's: `settle` keeps the plan, and nothing here re-derives it. Tidy
 * and Overview are the two commands that ask for a new one.
 */
export function Canvas() {
  const state = useStore((snapshot) => snapshot);
  const surface = useRef<HTMLDivElement | null>(null);
  const pan = useRef<{ x: number; y: number; camera: { x: number; y: number } } | null>(null);
  // The camera is framed for the board it was given and then left alone: a plate
  // arriving, a plate growing, the facts panel opening are not reasons to move a
  // surface somebody is reading. Only these startup passes may re-frame.
  const framing = useRef<"none" | "provisional" | "final">("none");
  // Until the reader moves the board, those passes are theirs to spend.
  const touched = useRef(false);
  const [drag, setDrag] = useState<Drag>(null);
  const [spotlight, setSpotlight] = useState<string | null>(null);
  const [stage, setStage] = useState({ width: 1280, height: 720 });
  // The legend scrolls sideways inside a one-line bar. A fade says "there is
  // more" only when there is: it is measured, not assumed.
  const legend = useRef<HTMLSpanElement | null>(null);
  const [legendOverflow, setLegendOverflow] = useState(false);
  // Handlers read the camera through a ref: a pan changes state on every
  // pointer move, and re-subscribing the wheel and key listeners per frame is
  // both wasteful and a way to lose an event mid-gesture.
  const cameraRef = useRef(state.camera);
  cameraRef.current = state.camera;

  const record = activeRevision(state);
  const followingNewest = atNewest(state);
  // The headers and renders on screen describe this revision. Following the
  // newest, it is the last workspace read — not the newest number announced —
  // so a plate keeps its truthful marks instead of blanking for a round trip.
  const revision = annotationRevision(state);
  const journaled = useMemo(() => new Set(record?.changed_views ?? []), [record]);
  const sameAtRevision = useMemo(() => new Set(record?.cutoff_views ?? []), [record]);

  /**
   * What the running revision did to one View. The header state is the
   * runtime's own verdict for the newest revision — changed content, or a run
   * that produced the same value (`cutoff`). For an older revision the journal
   * is the only witness, and it names the Views its transaction staged.
   */
  const reactionOf = (view: string, stateWord: string): Reaction => {
    // The live header describes the data revision, and so does the plate: the
    // annotation and the data always come from the same read.
    if (followingNewest) {
      return stateWord === "changed" ? "changed" : stateWord === "cutoff" ? "cutoff" : null;
    }
    // An older revision has no live header: the journal names what it staged
    // and what ran without moving.
    if (journaled.has(view)) return "changed";
    return sameAtRevision.has(view) ? "cutoff" : null;
  };

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

  // The board tidies once and then keeps the plan it was given. `board` above is
  // what a tidy would say now; `plan` below is what the board is actually drawn
  // from, so a plate that is already placed moves only when the reader says so.
  const kept = useRef<{ epoch: number; settled: Settled } | null>(null);

  const foldOf = useCallback(
    (node: NodeId) =>
      node.startsWith("view:") && Boolean(collapsed[node.slice("view:".length)]),
    [collapsed],
  );

  // A plate's box is real once its content has been read. A component chip
  // carries no data, and a folded plate draws no body, so both are measured as
  // soon as they exist.
  const measuredOf = useCallback(
    (node: NodeId) => {
      if (!node.startsWith("view:")) return true;
      const key = node.slice("view:".length);
      if (collapsed[key]) return true;
      return Boolean(state.renders[key]) || Boolean(state.renderErrors[key]);
    },
    [collapsed, state.renders, state.renderErrors],
  );

  const pins = useMemo(
    () => new Map(Object.entries(state.placement)),
    [state.placement],
  );

  // A memo returning a different object is not the board re-tidying: the plan is
  // handed forward through `kept`, and only a new `layoutEpoch` — Tidy, or
  // Overview — drops it and tidies again.
  const laidOut = useMemo(() => {
    const previous =
      kept.current?.epoch === state.layoutEpoch ? kept.current.settled : null;
    const next = settleBoard({
      tidy: board,
      kept: previous,
      folded: foldOf,
      measured: measuredOf,
      pins,
    });
    kept.current = { epoch: state.layoutEpoch, settled: next.settled };
    return next;
  }, [board, foldOf, measuredOf, pins, state.layoutEpoch]);

  const plan = useMemo<Board>(
    () => ({
      placement: laidOut.plan.placement,
      sizes: laidOut.plan.sizes,
      edges: board.edges,
      ranks: board.ranks,
    }),
    [laidOut, board],
  );

  // A drag overrides the plan's slot; the envelope follows, so an edge to a
  // dragged card still reaches it.
  const placed = useMemo(() => {
    const merged = new Map(plan.placement);
    for (const [node, at] of Object.entries(state.placement)) {
      if (merged.has(node)) merged.set(node, at);
    }
    if (drag) merged.set(drag.node, drag.at);
    return merged;
  }, [plan, state.placement, drag]);

  const bounds = useMemo(() => boardBounds(placed, plan.sizes), [placed, plan.sizes]);

  const ports = useMemo(() => assignPorts(plan, placed), [plan, placed]);

  const paths = useMemo(
    () =>
      plan.edges.map((edge) => {
        const key = edgeKey(edge);
        return {
          edge,
          path: edgePath(edge, placed, plan.sizes, { ends: ports.get(key) }),
        };
      }),
    [plan, placed, ports],
  );

  useEffect(() => {
    // How big the surface is, so a fit knows the window it is fitting into and
    // the HUD can tell a phone from a desk. The board itself does not re-arrange
    // for it: a plate keeps the slot and box it was given.
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

  const zoomAt = useCallback((factor: number, clientX: number, clientY: number) => {
    const box = surface.current?.getBoundingClientRect();
    if (!box) return;
    const camera = cameraRef.current;
    const zoom = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, camera.zoom * factor));
    const pointerX = clientX - box.left;
    const pointerY = clientY - box.top;
    // Keep the world point under the pointer fixed.
    const worldX = (pointerX - camera.x) / camera.zoom;
    const worldY = (pointerY - camera.y) / camera.zoom;
    setCamera({ x: pointerX - worldX * zoom, y: pointerY - worldY * zoom, zoom });
    touched.current = true;
  }, []);

  // A zoom from a control happens around the middle of the board, not the
  // middle of the window: with the facts panel open those are two places.
  const zoomAtCentre = useCallback(
    (factor: number) => {
      const box = surface.current?.getBoundingClientRect();
      if (!box) return;
      zoomAt(factor, box.left + box.width / 2, box.top + box.height / 2);
    },
    [zoomAt],
  );

  // The whole structure in the corner, and the board's fit has to leave it a
  // clear window; with the map hidden the board takes the full width.
  const showMap = state.minimap && stage.width >= 900;

  const frame = useCallback(
    (mode: "width" | "board") => {
      const box = surface.current?.getBoundingClientRect();
      if (!box || placed.size === 0) return;
      // The DAG runs downward. Fitting the width keeps every node readable;
      // fitting the whole board is the overview a long pipeline needs — but it
      // is a choice, not the default, because squeezing seven levels into 900px
      // is what made every node unreadable.
      const phone = box.width < PHONE_BREAKPOINT + 100;
      const side = phone ? 17 : 32;
      const gutter = showMap ? 182 : 0;
      const usable = Math.max(1, box.width - gutter - side * 2);
      const room = Math.max(120, box.height - (phone ? 44 : 80));
      const widthFit = usable / Math.max(bounds.width, 1);
      const heightFit = room / Math.max(bounds.height, 1);
      const zoom = Math.min(
        1.08,
        Math.max(MIN_ZOOM, mode === "board" ? Math.min(widthFit, heightFit) : widthFit),
      );
      const fitsWidth = bounds.width * zoom <= usable;
      const fitsHeight = bounds.height * zoom <= room;
      setCamera({
        x: fitsWidth
          ? gutter + side + (usable - bounds.width * zoom) / 2 - bounds.minX * zoom
          : gutter + side - bounds.minX * zoom,
        y: fitsHeight
          ? (box.height - bounds.height * zoom) / 2 - bounds.minY * zoom
          : 18 - bounds.minY * zoom,
        zoom,
      });
    },
    [bounds, placed, showMap],
  );

  const fit = useCallback(() => frame("width"), [frame]);
  const fitAll = useCallback(() => frame("board"), [frame]);

  const navigate = useCallback(
    (point: { x: number; y: number }) => {
      touched.current = true;
      const zoom = cameraRef.current.zoom;
      setCamera({
        ...cameraRef.current,
        x: stage.width / 2 - point.x * zoom,
        y: stage.height / 2 - point.y * zoom,
      });
    },
    [stage.width, stage.height],
  );

  useEffect(() => {
    const element = legend.current;
    if (!element) return;
    const measure = () => setLegendOverflow(element.scrollWidth > element.clientWidth + 1);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [inks.length, stage.width]);

  useEffect(() => {
    // The board's own keys. They are declared here because the actions are the
    // camera's: a page-level listener would have to click a button to reach it.
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      if (target && /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)) return;
      const run = (action: () => void, retidy = false) => {
        event.preventDefault();
        touched.current = false;
        // A fit from the keys is the reader framing the board themselves, so
        // nothing re-frames after it; `o` changes the board instead, which is
        // tidied and framed again from scratch.
        framing.current = retidy ? "none" : "final";
        action();
      };
      switch (event.key) {
        case "f":
          run(fit);
          break;
        case "a":
          run(fitAll);
          break;
        case "o":
          run(toggleOverview, true);
          break;
        case "m":
          run(toggleMinimap);
          break;
        case "0":
          run(() => zoomAtCentre(1 / cameraRef.current.zoom));
          break;
        default:
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [fit, fitAll, zoomAtCentre]);

  useEffect(() => {
    // The board is framed twice at most: as soon as its plates exist, and once
    // when the last box has been measured — the first frame would be chained to
    // boxes that are still guesses. After that the camera is the reader's, and a
    // plate arriving, a wire changing, the facts panel opening or the window
    // resizing are not reasons to move a surface somebody is reading. `Fit` and
    // `All` still frame it on request, and a re-tidy frames the board it builds.
    if (placed.size === 0) return;
    if (framing.current === "final") return;
    if (framing.current === "provisional" && laidOut.settling) return;
    // The reader has already moved this board; the startup frame is theirs to
    // keep, not ours to correct.
    if (framing.current !== "none" && touched.current) return;
    fit();
    framing.current = laidOut.settling ? "provisional" : "final";
  }, [placed, fit, laidOut.settling]);

  useEffect(() => {
    // The surface clips rather than scrolls, so a focused plate or row that is
    // off-screen would otherwise be focused somewhere the reader cannot see.
    // Tab pans the camera just enough to bring it back.
    //
    // A press focuses whatever it lands on too, and that is not the reader asking
    // to be taken anywhere: panning for it would move the plate out from under
    // the pointer between the press and the release, so the click would land on
    // whatever had slid into its place. Only focus reached without a press is
    // worth following.
    const element = surface.current;
    if (!element) return;
    let pressed = false;
    let released: ReturnType<typeof setTimeout> | null = null;
    const onPress = () => {
      pressed = true;
      // The press's focus lands in this task, before the timer can run.
      if (released) clearTimeout(released);
      released = setTimeout(() => {
        pressed = false;
      }, 0);
    };
    const onFocusIn = (event: FocusEvent) => {
      if (pressed) return;
      const target = event.target as HTMLElement | null;
      const node = target?.closest<HTMLElement>(".card, .component");
      if (!node) return;
      const box = element.getBoundingClientRect();
      const rect = node.getBoundingClientRect();
      const pad = 24;
      const dx =
        rect.left < box.left + pad
          ? box.left + pad - rect.left
          : rect.right > box.right - pad
            ? box.right - pad - rect.right
            : 0;
      const dy =
        rect.top < box.top + pad
          ? box.top + pad - rect.top
          : rect.bottom > box.bottom - pad
            ? box.bottom - pad - rect.bottom
            : 0;
      if (dx === 0 && dy === 0) return;
      const camera = cameraRef.current;
      touched.current = true;
      setCamera({ ...camera, x: camera.x + dx, y: camera.y + dy });
    };
    window.addEventListener("pointerdown", onPress, true);
    element.addEventListener("focusin", onFocusIn);
    return () => {
      window.removeEventListener("pointerdown", onPress, true);
      element.removeEventListener("focusin", onFocusIn);
      if (released) clearTimeout(released);
    };
  }, []);

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
        // A pointer over a text field is reading or typing, not panning.
        if (target?.closest("textarea, input, select, .inspector")) return;
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
        const camera = cameraRef.current;
        setCamera({ ...camera, x: camera.x - event.deltaX, y: camera.y - event.deltaY });
      }
    };
    surfaceElement.addEventListener("wheel", onWheel, { passive: false });
    return () => surfaceElement.removeEventListener("wheel", onWheel);
  }, [zoomAt]);

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
          avoidOverlap(drag.node, drag.at, plan.sizes, placed),
        );
      }
      setDrag(null);
      return;
    }
    touched.current = true;
    setDrag({ node, at });
  };

  const ranAt = (component: string): boolean => {
    if (!followingNewest || revision === 0) return false;
    const wire = state.components.find((item) => item.component === component);
    return wire?.executed_at === revision;
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
        <button type="button" className="hud-button" onClick={fit} title="Fit the board to the width (f)">
          Fit
        </button>
        <button
          type="button"
          className="hud-button"
          onClick={fitAll}
          title="Fit the whole board in the window (a)"
        >
          All
        </button>
        <button
          type="button"
          className="hud-button"
          onClick={() => zoomAtCentre(1.15)}
          aria-label="Zoom in"
        >
          +
        </button>
        <button
          type="button"
          className="hud-button"
          onClick={() => zoomAtCentre(1 / 1.15)}
          aria-label="Zoom out"
        >
          −
        </button>
        <button
          type="button"
          className="hud-button hud-zoom"
          onClick={() => {
            touched.current = true;
            zoomAtCentre(1 / cameraRef.current.zoom);
          }}
          title="Reset zoom to 100% (0)"
        >
          {Math.round(state.camera.zoom * 100)}%
        </button>
        <button
          type="button"
          className="hud-button"
          onClick={() => {
            touched.current = false;
            framing.current = "none";
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
            framing.current = "none";
            toggleOverview();
          }}
          title="Fold every View to its header; open a detail for its content (o)"
        >
          Overview
        </button>
        {/* The map is not drawn on a narrow stage; offering the toggle there
            would switch something the reader cannot see. */}
        {stage.width >= 900 ? (
          <button
            type="button"
            className="hud-button"
            aria-pressed={state.minimap}
            onClick={toggleMinimap}
            title="Show or hide the structure minimap (m)"
          >
            Map
          </button>
        ) : null}
        <span className="hud-sep" />
        <span
          ref={legend}
          className={["legend", legendOverflow ? "legend-overflow" : ""].filter(Boolean).join(" ")}
          aria-label="Components"
        >
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
        <span className="spacer" />
        <span className="hud-note hud-hint">drag to pan · ⌘/ctrl + wheel to zoom</span>
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
                  <title>{`${edge.label} ${edge.kind === "reads" ? "reads" : "defines"} this View · r${revision}`}</title>
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
                box={plan.sizes.get(viewNode(view.view)) ?? { width: 316, height: 220 }}
                revision={revision}
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
                revision={revision}
                ran={ranAt(component.component)}
                onDrag={onNodeDrag}
              />
            );
          })}
        </div>

        {state.views.length === 0 ? (
          <div className="canvas-empty">
            <p>
              {state.connection === "live"
                ? "This runtime reports no Views yet."
                : state.connection === "mismatched"
                  ? "This page was built against a different wire protocol."
                  : "connecting to the runtime…"}
            </p>
          </div>
        ) : null}

        {showMap ? (
          <Minimap
            board={plan}
            placed={placed}
            camera={state.camera}
            stage={stage}
            inks={inksByKey}
            changed={changedNodes}
            onNavigate={navigate}
          />
        ) : null}

        {state.notice ? <p className="canvas-notice">{state.notice}</p> : null}
      </div>
    </div>
  );
}
