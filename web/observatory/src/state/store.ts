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

/**
 * The page's state: what the host exposes, what each View currently draws, and
 * where the reader put things on the canvas. Everything else is derived.
 */

export type Placement = { x: number; y: number };
export type Camera = { x: number; y: number; zoom: number };

export type State = {
  connection: "connecting" | "live" | "reconnecting" | "paused";
  level: string;
  runtime: string | null;
  seq: number;
  revision: number;
  announced: number;
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
  /** The View open for in-place text editing. */
  editing: string | null;
  /** Draft text per View, kept while the reader types. */
  drafts: Record<string, string>;
  /** Last reply per View, replaced on the next edit. */
  replies: Record<string, { text: string; error: boolean }>;
  /** Views the reader folded down to their header. */
  collapsed: Record<string, boolean>;
  /** True when every plate is folded to its header for a structural overview. */
  overview: boolean;
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
  announced: 0,
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
  editing: null,
  drafts: {},
  replies: {},
  collapsed: stored.collapsed ?? {},
  overview: stored.overview ?? false,
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

export function resetLayout() {
  set({ placement: {}, camera: { x: 40, y: 40, zoom: 1 } }, true);
}

/** Fold every plate down to its header, or bring the content back. */
export function toggleOverview() {
  set({ overview: !state.overview }, true);
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

/**
 * Re-reads every plate's render at once.
 *
 * Called after a commit the host has finished: one parallel round trip, no
 * waiting for the header fetch to say which View moved. The header refresh
 * follows and only corrects the labels.
 */
export async function refreshRenders() {
  for (const view of state.views) {
    ensureRender(view.view, true);
  }
}

// Views whose render was asked for while one was already on the wire. A render
// describes the runtime at the instant it was drawn, so a request that overlaps
// a demand can describe the revision *before* it: the queue is what gives the
// late reader a second look.
const renderQueue = new Set<string>();

export function ensureRender(view: string, force = false) {
  const wire = viewOf(view);
  if (!wire) return;
  const cached = state.renders[view];
  if (!force && cached && cached.content_version >= wire.content_version) return;
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
      // header up. An older one is retried once.
      if (current && render.content_version < current.content_version) {
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

export function beginEdit(view: string) {
  const wire = viewOf(view);
  if (!wire?.editable || state.editing === view) return;
  if (state.drafts[view] !== undefined) {
    set({ editing: view });
    return;
  }
  void fetch(`/api/v1/views/${view}/text`)
    .then((response) => (response.ok ? response.json() : Promise.reject(response.status)))
    .then((payload: { text: string }) => {
      set({
        editing: view,
        drafts: { ...state.drafts, [view]: payload.text },
      });
    })
    .catch(() => set({ notice: "This View's text could not be read." }));
}

export function endEdit(view: string) {
  if (state.editing !== view) return;
  // Committing on close means the last keystroke is never lost.
  const draft = state.drafts[view];
  const wire = viewOf(view);
  if (draft !== undefined && wire) void commitText(view);
  set({ editing: null });
}

export function setDraft(view: string, text: string) {
  set({ drafts: { ...state.drafts, [view]: text } });
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
    set({
      drafts:
        current === text ? { ...state.drafts, [view]: payload.text ?? text } : state.drafts,
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
      // The transaction is published before the host demands what it changed, so
      // the frame that announced this revision can describe the last one. The
      // reply means the host has finished: read the plates and the headers at
      // once, in parallel, instead of waiting for one to unlock the other.
      void refreshRenders();
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
  const revision = current.selectedRevision ?? current.revision;
  if (current.revisions.length === 0) return null;
  return current.revisions.find((record) => record.revision === revision) ?? null;
}

// --------------------------------------------------------------------- frames

export function applyHello(hello: HelloWire) {
  set({
    connection: "live",
    level: hello.level,
    runtime: hello.runtime,
    limits: hello.limits,
    historyFrom: hello.history_from,
  });
}

export function applyWorkspace(workspace: WorkspaceWire, seq: number) {
  set({
    connection: "live",
    seq,
    revision: workspace.revision,
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
  for (const view of workspace.views) ensureRender(view.view);
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
    for (const view of result.dirty) ensureRender(view);
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
