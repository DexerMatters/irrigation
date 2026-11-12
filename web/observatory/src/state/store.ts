import { useSyncExternalStore } from "react";
import type {
  ComponentWire,
  EventsWire,
  HelloWire,
  NodeDetailWire,
  RevisionWire,
  Selection,
  StatsWire,
  ViewRender,
  ViewWire,
  WorkspaceWire,
} from "./types";
import { reduceFrame } from "./reduce";
import {
  activeRevisionOf,
  annotationRevisionOf,
  atNewestOf,
  shownRevisionOf,
} from "./revision";

/**
 * The page's state: what the host exposes, what each View currently draws, and
 * where the reader put things on the canvas. Everything else is derived.
 */

export type Placement = { x: number; y: number };
export type Camera = { x: number; y: number; zoom: number };

export type State = {
  connection: "connecting" | "live" | "reconnecting" | "paused" | "mismatched";
  level: string;
  runtime: string | null;
  seq: number;
  revision: number;
  /**
   * The revision the workspace headers describe.
   *
   * A revision event lands before the headers it is about: the frame says "r3
   * published", and the `views` array still describes r2 until the next read.
   * Marks must be drawn against this number, not `revision`, or a plate is
   * annotated with the change of the revision before the one on screen.
   */
  dataRevision: number;
  limits: Record<string, number>;
  historyFrom: number | null;
  views: ViewWire[];
  components: ComponentWire[];
  stats: StatsWire;
  renders: Record<string, ViewRender>;
  renderErrors: Record<string, string>;
  loading: Record<string, boolean>;
  revisions: RevisionWire[];
  selectedRevision: number | null;
  following: boolean;
  selection: Selection | null;
  detail: NodeDetailWire | null;
  dropped: number;
  lastNoop: { label: string; revision: number } | null;
  placement: Record<string, Placement>;
  camera: Camera;
  notice: string | null;
  /** Draft text per View, kept while the reader types. */
  drafts: Record<string, string>;
  /** The content version the draft was read from, so a host-side edit is seen. */
  draftVersion: Record<string, number>;
  /** True while the draft holds keystrokes the host has not acknowledged. */
  draftDirty: Record<string, boolean>;
  /** Last reply per View, replaced on the next edit. */
  replies: Record<string, { text: string; error: boolean }>;
  /** Views the reader folded down to their header. */
  collapsed: Record<string, boolean>;
  /** True when every plate is folded to its header for a structural overview. */
  overview: boolean;
  /**
   * A new number each time the board is to be tidied again.
   *
   * The board tidies once and then keeps its plan (see `settle`), so the reader
   * needs one word for "lay it out afresh": `Tidy` and `Overview` raise this, and
   * the board starts over from the tidy instead of the plan it was keeping.
   */
  layoutEpoch: number;
  /** Whether the structure minimap is shown over the board. */
  minimap: boolean;
};

/**
 * Where the reader put the cards. Keyed by node identity, so a stale layout
 * cannot pin a View that has been renamed or a component that has gone: the key
 * no longer names anything and the node falls back to the computed grid.
 */
const storageKey = `observatory:${location.host}:canvas:v2`;

type Stored = {
  placement?: Record<string, Placement>;
  camera?: Camera;
  collapsed?: Record<string, boolean>;
  overview?: boolean;
  minimap?: boolean;
};

function load(): Stored {
  try {
    const raw = localStorage.getItem(storageKey);
    return raw ? (JSON.parse(raw) as Stored) : {};
  } catch {
    return {};
  }
}

const stored = load();

const initial: State = {
  connection: "connecting",
  level: "nodes",
  runtime: null,
  seq: 0,
  revision: 0,
  dataRevision: 0,
  limits: {},
  historyFrom: null,
  views: [],
  components: [],
  stats: {
    nodes: 0,
    nodes_created: 0,
    revisions: 0,
    executions: 0,
    validations: 0,
    validation_attempts: 0,
    validation_failures: 0,
    execution_failures: 0,
    cutoffs: 0,
    suspensions: 0,
    demands: 0,
    notifications: 0,
    invalidations: 0,
    write_claims: 0,
    write_conflicts: 0,
    deferred_writes: 0,
    demand_retries: 0,
    reader_waits: 0,
    queued: 0,
    running: 0,
    waiting: 0,
  },
  renders: {},
  renderErrors: {},
  loading: {},
  revisions: [],
  selectedRevision: null,
  following: true,
  selection: null,
  detail: null,
  dropped: 0,
  lastNoop: null,
  placement: stored.placement ?? {},
  camera: stored.camera ?? { x: 40, y: 40, zoom: 1 },
  notice: null,
  drafts: {},
  draftVersion: {},
  draftDirty: {},
  replies: {},
  collapsed: stored.collapsed ?? {},
  overview: stored.overview ?? false,
  layoutEpoch: 0,
  minimap: stored.minimap ?? true,
};

let state: State = initial;
const listeners = new Set<() => void>();

// A read-only handle for browser checks and for anyone debugging the page.
(window as unknown as { __observatory?: () => State }).__observatory = () => state;

function set(patch: Partial<State>, persist = false) {
  state = { ...state, ...patch };
  if (persist) {
    try {
      localStorage.setItem(
        storageKey,
        JSON.stringify({
          placement: state.placement,
          camera: state.camera,
          collapsed: state.collapsed,
          overview: state.overview,
          minimap: state.minimap,
        }),
      );
    } catch {
      // A blocked store only loses layout, never data.
    }
  }
  for (const listener of listeners) listener();
}

export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useStore<T>(selector: (state: State) => T): T {
  return useSyncExternalStore(
    subscribe,
    () => selector(state),
    () => selector(initial),
  );
}

// -------------------------------------------------------------------- camera

export function setCamera(camera: Camera) {
  set({ camera }, true);
}

/**
 * Pins one board node where the reader dropped it.
 *
 * Keyed by node identity (`view:…` or `component:…`), so a View and a component
 * drag the same way. A drag reports its position while it moves and only lands
 * here when it ends, so storage is written once per gesture.
 */
export function place(node: string, at: Placement) {
  set({ placement: { ...state.placement, [node]: at } }, true);
}

/**
 * Forgets the dragged positions and lays the board out again.
 *
 * The camera goes back to the origin with them: this is the reader asking for
 * the board as it would be drawn from scratch, not a nudge of the one they have.
 */
export function resetLayout() {
  set(
    {
      placement: {},
      camera: { x: 40, y: 40, zoom: 1 },
      layoutEpoch: state.layoutEpoch + 1,
    },
    true,
  );
}

/** Fold every plate down to its header, or bring the content back. */
export function toggleOverview() {
  // The overview is a different board, not the same one with shorter plates: it
  // is tidied at the folded size so the whole DAG fits the viewport. The plan the
  // board was keeping is for the open plates and is not reused.
  set({ overview: !state.overview, layoutEpoch: state.layoutEpoch + 1 }, true);
}

/** Show or hide the structure minimap; the board gets the corner back. */
export function toggleMinimap() {
  set({ minimap: !state.minimap }, true);
}

/** Fold one View down to its header, or open it again. */
export function toggleCollapsed(view: string) {
  set(
    {
      collapsed: {
        ...state.collapsed,
        [view]: !state.collapsed[view],
      },
    },
    true,
  );
}

// --------------------------------------------------------------------- views

export function viewOf(key: string): ViewWire | undefined {
  return state.views.find((view) => view.view === key);
}

/*
 * Renders are read behind a cadence, not on every keystroke.
 *
 * A source edit changes most derived Views, so a live session re-reads almost
 * every render per commit. Firing that set on every keystroke (ten requests per
 * character, all competing with the stream with the workspace read) is what
 * made typing feel behind the host. The first change after a quiet moment is
 * read at once; a burst coalesces into at most one set every
 * `RENDER_CADENCE_MS`, always against the newest workspace. The plates' marks
 * do not wait for this: they come from the workspace, which is cheap.
 */
const RENDER_CADENCE_MS = 120;
let renderBatchTimer: ReturnType<typeof setTimeout> | null = null;
let lastRenderBatch = Number.NEGATIVE_INFINITY;

// Which cards are on screen. A render is only re-read for those: an off-screen
// plate cannot show it, and every read still holds the runtime's lock for the
// length of a projection. Panning a plate into view asks for it (see
// `setViewVisible`), so nothing stays stale where it can be seen.
const visibleViews = new Set<string>();

/** Report a plate entering or leaving the surface. */
export function setViewVisible(view: string, shown: boolean) {
  if (!shown) {
    visibleViews.delete(view);
    return;
  }
  if (visibleViews.has(view)) return;
  visibleViews.add(view);
  ensureRender(view);
}

function runRenderBatch() {
  renderBatchTimer = null;
  lastRenderBatch = performance.now();
  // The facts panel's View is read even when its plate is scrolled away.
  const selected =
    state.selection && "view" in state.selection ? state.selection.view : null;
  for (const view of state.views) {
    // A folded plate draws no body, so it has no render to refresh.
    if (state.overview || state.collapsed[view.view]) continue;
    if (
      visibleViews.size > 0 &&
      !visibleViews.has(view.view) &&
      view.view !== selected
    ) {
      continue;
    }
    ensureRender(view.view);
  }
}

/** Read whatever renders are stale, on the coalesced cadence. */
export function scheduleRenderRefresh() {
  if (renderBatchTimer !== null) return;
  const wait = RENDER_CADENCE_MS - (performance.now() - lastRenderBatch);
  if (wait <= 0) {
    runRenderBatch();
    return;
  }
  renderBatchTimer = setTimeout(runRenderBatch, wait);
}

// Views whose render was asked for while one was already on the wire. A render
// describes the runtime at the instant it was drawn, so a request that overlaps
// a demand can describe the revision *before* it: the queue is what gives the
// late reader a second look.
const renderQueue = new Set<string>();

export function ensureRender(view: string) {
  const wire = viewOf(view);
  if (!wire) return;
  const cached = state.renders[view];
  // A render is a value plus the row marks of the revision it was drawn at. A
  // keyed View keeps its content version across a value-only edit, so "newer
  // content version" alone would leave a plate showing the previous revision's
  // changed rows under the new revision's label. Re-read when the content moved
  // — or when the header says units moved but this drawing predates it; a View
  // whose units did not move keeps its drawing, and its rows are simply clean.
  const stale =
    cached !== undefined &&
    (cached.content_version < wire.content_version ||
      (cached.revision < state.dataRevision && wire.changed_units > 0));
  if (cached && !stale) return;
  if (state.loading[view]) {
    renderQueue.add(view);
    return;
  }
  set({ loading: { ...state.loading, [view]: true } });
  void fetch(`/api/v1/views/${view}/render`)
    .then((response) => (response.ok ? response.json() : Promise.reject(response.status)))
    .then((render: ViewRender) => {
      set({ loading: { ...state.loading, [view]: false } });
      const current = viewOf(view);
      // A render newer than the header is accepted; the next frame catches the
      // header up. An older one — in content or in revision — is retried once.
      if (
        current &&
        (render.content_version < current.content_version ||
          (render.revision < state.dataRevision && current.changed_units > 0))
      ) {
        ensureRender(view);
        return;
      }
      set({
        renders: { ...state.renders, [view]: render },
        renderErrors: { ...state.renderErrors, [view]: "" },
      });
      if (renderQueue.delete(view)) ensureRender(view);
    })
    .catch((error: unknown) => {
      set({
        loading: { ...state.loading, [view]: false },
        renderErrors: { ...state.renderErrors, [view]: String(error) },
      });
      if (renderQueue.delete(view)) ensureRender(view);
    });
}

// -------------------------------------------------------------------- editing

// A source View's text arrives when its plate first draws, and again whenever
// the host publishes a newer content version. The draft is the reader's once
// they type: a pending echo must never overwrite a keystroke, and a host-side
// edit must not silently replace unsaved text either.
const draftPending = new Set<string>();
// The last time this page published this View's text. Its own echo advances the
// content version, and re-reading the text we just sent would be one request
// per keystroke for no information.
const recentCommit = new Map<string, number>();
const SELF_ECHO_MS = 1000;

export function ensureDraft(view: string) {
  const wire = viewOf(view);
  if (!wire?.editable) return;
  // The reader owns text the host has not acknowledged; leave it alone.
  if (state.draftDirty[view]) return;
  if ((state.draftVersion[view] ?? -1) >= wire.content_version) return;
  if (performance.now() - (recentCommit.get(view) ?? Number.NEGATIVE_INFINITY) < SELF_ECHO_MS) {
    // This version is our own write coming back; the draft already holds it.
    set({ draftVersion: { ...state.draftVersion, [view]: wire.content_version } });
    return;
  }
  if (draftPending.has(view)) return;
  const version = wire.content_version;
  draftPending.add(view);
  void fetch(`/api/v1/views/${view}/text`)
    .then((response) => (response.ok ? response.json() : Promise.reject(response.status)))
    .then((payload: { text: string }) => {
      // A newer read landed, or the reader typed while this one was in flight.
      if (state.draftDirty[view]) return;
      if ((state.draftVersion[view] ?? -1) >= version) return;
      set({
        drafts: { ...state.drafts, [view]: payload.text },
        draftVersion: { ...state.draftVersion, [view]: version },
      });
    })
    .catch(() =>
      set({ notice: `${wire.name.split("::").pop() ?? "This View"} could not be read as text.` }),
    )
    .finally(() => draftPending.delete(view));
}

export function setDraft(view: string, text: string) {
  set({
    drafts: { ...state.drafts, [view]: text },
    draftDirty: { ...state.draftDirty, [view]: true },
  });
}

const inFlight = new Set<string>();
const queued = new Set<string>();

/**
 * The live loop, with no timer in it: a keystroke commits at once. If a commit
 * for the same View is already on the wire, one follow-up is queued, so typing
 * faster than the host parses still lands on the latest text.
 */
export function scheduleCommit(view: string) {
  if (inFlight.has(view)) {
    queued.add(view);
    return;
  }
  void runCommit(view);
}

async function runCommit(view: string) {
  inFlight.add(view);
  try {
    await commitText(view);
  } finally {
    inFlight.delete(view);
    if (queued.delete(view)) void runCommit(view);
  }
}

export async function commitText(view: string) {
  const text = state.drafts[view];
  if (text === undefined) return;
  try {
    const response = await fetch(`/api/v1/views/${view}/text`, {
      method: "POST",
      body: text,
    });
    const payload = (await response.json()) as {
      text?: string;
      reply?: string;
      error?: string;
      revision?: number;
      published?: boolean;
    };
    if (!response.ok) {
      set({
        replies: {
          ...state.replies,
          [view]: { text: payload.error ?? `rejected (${response.status})`, error: true },
        },
      });
      return;
    }
    // Never overwrite a newer draft the reader typed while the request was in
    // flight; the server's echo is only authoritative when nothing changed.
    const current = state.drafts[view];
    const settled = current === text;
    if (payload.revision) recentCommit.set(view, performance.now());
    set({
      drafts: settled ? { ...state.drafts, [view]: payload.text ?? text } : state.drafts,
      draftDirty: settled ? { ...state.draftDirty, [view]: false } : state.draftDirty,
      replies: {
        ...state.replies,
        [view]: {
          text: payload.published
            ? `${payload.reply ?? "committed"} · r${payload.revision}`
            : "no revision: this text changed nothing",
          error: false,
        },
      },
    });
    if (payload.revision) {
      follow();
      // The reply means the host has finished: read the headers now, which also
      // schedules the coalesced render read. No per-keystroke render storm.
      void refreshWorkspace();
    }
  } catch (problem) {
    set({
      replies: { ...state.replies, [view]: { text: String(problem), error: true } },
    });
  }
}

// ------------------------------------------------------------------ selection

export async function select(selection: Selection | null) {
  set({ selection, detail: null });
  if (!selection || selection.kind !== "node") return;
  try {
    const response = await fetch(`/api/v1/nodes/${selection.key}`);
    if (!response.ok) return;
    const detail = (await response.json()) as NodeDetailWire;
    if (state.selection === selection) set({ detail });
  } catch {
    // No detail is shown as no facts, never invented.
  }
}

export function follow() {
  set({ following: true, selectedRevision: null });
}

export function selectRevision(revision: number) {
  set({ following: false, selectedRevision: revision });
}

export function activeRevision(current: State): RevisionWire | null {
  return activeRevisionOf(current);
}

/** The revision the board is showing: the selected one, or the newest. */
export function shownRevision(current: State): number {
  return shownRevisionOf(current);
}

/** True when the board is reading the newest revision rather than a record. */
export function atNewest(current: State): boolean {
  return atNewestOf(current);
}

/** The revision the annotations on the plates describe. */
export function annotationRevision(current: State): number {
  return annotationRevisionOf(current);
}

// --------------------------------------------------------------------- frames

/// The wire protocol this page was built against.
///
/// The server documents its `PROTOCOL` as "bumped when a field changes meaning", which is only
/// useful if the reader checks it: a page that ignores the number renders a changed field as if it
/// were the old one, and every symptom is a plausible-looking wrong value.
export const PROTOCOL = 1;

export function applyHello(hello: HelloWire) {
  set({
    connection: hello.protocol === PROTOCOL ? "live" : "mismatched",
    level: hello.level,
    runtime: hello.runtime,
    limits: hello.limits,
    historyFrom: hello.history_from,
  });
}

export function applyWorkspace(workspace: WorkspaceWire, seq: number) {
  // A runtime that restarted begins its numbering again. That is not an old
  // read landing late: the journal, the renders and the selection all belong to
  // the previous run and have to go.
  const restarted = state.revision > 0 && workspace.revision + 1 < state.revision;
  set({
    connection: "live",
    seq,
    // Otherwise the readout never walks backwards because an old read landed.
    revision: restarted ? workspace.revision : Math.max(state.revision, workspace.revision),
    dataRevision: workspace.revision,
    ...(restarted
      ? { revisions: [], selectedRevision: null, following: true, selection: null, detail: null }
      : {}),
    runtime: workspace.runtime,
    level: workspace.level,
    views: workspace.views,
    components: workspace.components,
    stats: workspace.stats,
    dropped: workspace.dropped,
    lastNoop:
      workspace.last_noop && workspace.last_noop_revision
        ? { label: workspace.last_noop, revision: workspace.last_noop_revision }
        : null,
  });
  if (restarted) {
    // Drop drawings from the previous run so the new revision is re-read.
    const renders = { ...state.renders };
    for (const view of workspace.views) {
      const cached = renders[view.view];
      if (cached && cached.revision > workspace.revision) delete renders[view.view];
    }
    set({ renders });
  }
  scheduleRenderRefresh();
}

export function applyHistory(revisions: RevisionWire[]) {
  const known = new Set(state.revisions.map((record) => record.revision));
  const merged = [
    ...state.revisions,
    ...revisions.filter((record) => !known.has(record.revision)),
  ].sort((left, right) => right.revision - left.revision);
  set({ revisions: merged });
}

export function applyEvents(frame: EventsWire) {
  const result = reduceFrame(
    {
      seq: state.seq,
      revision: state.revision,
      revisions: state.revisions,
      lastNoop: state.lastNoop,
    },
    frame,
  );
  if (result.resync) {
    set({ notice: result.notice });
    void resync();
    return;
  }
  const advanced = result.bookkeeping.revision > state.revision;
  set({
    seq: result.bookkeeping.seq,
    revision: result.bookkeeping.revision,
    revisions: result.bookkeeping.revisions,
    lastNoop: result.bookkeeping.lastNoop,
    notice: result.notice,
  });
  if (advanced) {
    void refreshWorkspace();
  }
}

let workspaceInFlight = false;
let workspaceAgain = false;

/**
 * Refetches the headers as soon as a revision lands; no batching delay.
 *
 * A request that arrives while one is on the wire is not dropped: it asks for
 * one more read afterwards, because the second caller is usually the one that
 * knows the host has finished the work the first request was too early for.
 */
async function refreshWorkspace() {
  if (workspaceInFlight) {
    workspaceAgain = true;
    return;
  }
  workspaceInFlight = true;
  try {
    const response = await fetch("/api/v1/workspace");
    if (response.ok) {
      const workspace = (await response.json()) as WorkspaceWire;
      applyWorkspace(workspace, state.seq);
    }
  } catch {
    // The stream will bring the next frame anyway.
  } finally {
    workspaceInFlight = false;
    if (workspaceAgain) {
      workspaceAgain = false;
      void refreshWorkspace();
    }
  }
}

// ------------------------------------------------------------------ transport

let stream: AbortController | null = null;
let stopped = false;
let backoff = 500;

export function connect() {
  stopped = false;
  void loop();
}

export function disconnect() {
  stopped = true;
  stream?.abort();
  stream = null;
  set({ connection: "paused" });
}

async function loop(): Promise<void> {
  while (!stopped) {
    stream = new AbortController();
    try {
      const response = await fetch(`/api/v1/stream?window=64`, {
        signal: stream.signal,
        headers: { Accept: "text/event-stream" },
      });
      if (!response.ok || !response.body) throw new Error(`stream ${response.status}`);
      set({ connection: "live" });
      backoff = 500;
      await pump(response.body);
      if (stopped) return;
      set({ connection: "reconnecting" });
    } catch {
      if (stopped) return;
      set({ connection: "reconnecting" });
    }
    await sleep(backoff);
    backoff = Math.min(backoff * 2, 8_000);
  }
}

async function pump(body: ReadableStream<Uint8Array>): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  for (;;) {
    const { value, done } = await reader.read();
    if (done) return;
    buffer += decoder.decode(value, { stream: true });
    let boundary = buffer.indexOf("\n\n");
    while (boundary >= 0) {
      const chunk = buffer.slice(0, boundary);
      buffer = buffer.slice(boundary + 2);
      dispatch(chunk);
      boundary = buffer.indexOf("\n\n");
    }
  }
}

function dispatch(chunk: string) {
  let event = "message";
  let id: number | null = null;
  const data: string[] = [];
  for (const line of chunk.split("\n")) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("id:")) id = Number(line.slice(3).trim());
    else if (line.startsWith("data:")) data.push(line.slice(5).trim());
  }
  if (data.length === 0) return;
  const payload = data.join("\n");
  try {
    switch (event) {
      case "hello":
        applyHello(JSON.parse(payload) as HelloWire);
        break;
      case "workspace":
        applyWorkspace(JSON.parse(payload) as WorkspaceWire, id ?? state.seq);
        break;
      case "history":
        applyHistory(JSON.parse(payload) as RevisionWire[]);
        break;
      case "events":
        applyEvents(JSON.parse(payload) as EventsWire);
        break;
      default:
        break;
    }
  } catch {
    set({ notice: `Unreadable ${event} frame; waiting for a resync.` });
  }
}

async function resync() {
  try {
    const workspace = await fetch("/api/v1/workspace").then((response) => response.json());
    applyWorkspace(workspace as WorkspaceWire, state.seq + 1);
    const revisions = await fetch("/api/v1/revisions").then((response) => response.json());
    set({ revisions: revisions as RevisionWire[] });
  } catch {
    set({ notice: "Resync failed; the page is not current." });
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
