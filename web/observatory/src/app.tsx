import { useEffect } from "react";
import { Canvas } from "./canvas/Canvas";
import { Inspector } from "./components/Inspector";
import { Tape } from "./components/Tape";
import { connect, useStore } from "./state/store";

const statusWord = (connection: string): string =>
  connection === "live"
    ? "live"
    : connection === "paused"
      ? "paused"
      : connection === "connecting"
        ? "connecting"
        : "reconnecting";

/** The whole page: a spatial board, a thin tape, and facts on demand. */
export function App() {
  const state = useStore((snapshot) => snapshot);

  useEffect(() => {
    connect();
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target && /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)) return;
      if (event.key === "f") document.querySelector<HTMLButtonElement>(".hud-button")?.click();
      if (event.key === "Escape") (document.activeElement as HTMLElement | null)?.blur();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="app">
      <header className="header">
        <h1 className="wordmark">Observatory</h1>
        <span className="runtime mono">
          {state.runtime ? shortRuntime(state.runtime) : "no root"}
        </span>
        <span className="spacer" />
        <span className="status">
          <span className={state.connection === "live" ? "dot live" : "dot"} aria-hidden="true" />
          {statusWord(state.connection)}
        </span>
        <span className="rev">
          <span>rev</span>
          <span className="mono">{state.revision}</span>
        </span>
        <span className="rev">
          <span>views</span>
          <span className="mono">{state.views.length}</span>
        </span>
      </header>

      <div className={state.selection ? "stage stage-split" : "stage"}>
        <Canvas />
        <Inspector />
      </div>

      <Tape />
    </div>
  );
}

function shortRuntime(name: string): string {
  const parts = name.split("::");
  return parts.slice(-2).join("::");
}
