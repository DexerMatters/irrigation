import { displayName } from "./layout";
import { useEffect, useRef } from "react";
import { commitText, scheduleCommit, setDraft, useStore } from "../state/store";
import type { ViewWire } from "../state/types";

/**
 * The in-place editor for a source View.
 *
 * The page does not parse the host's language. It sends the text, shows the
 * host's reply or its error, and lets the stream carry the delta. The editor's
 * value is the draft while the reader is typing, so a server echo can never
 * steal a keystroke.
 */
export function Editor({ view }: { view: ViewWire }) {
  const state = useStore((snapshot) => snapshot);
  const area = useRef<HTMLTextAreaElement | null>(null);
  const draft = state.drafts[view.view] ?? "";
  const reply = state.replies[view.view];
  const lines = draft.split("\n").length;

  useEffect(() => {
    if (area.current) area.current.focus();
  }, [view.view]);

  return (
    <div className="editor" onPointerDown={(event) => event.stopPropagation()}>
      <textarea
        ref={area}
        className="editor-area mono"
        spellCheck={false}
        rows={Math.min(14, Math.max(4, lines))}
        value={draft}
        aria-label={`${short(view.name)} text`}
        onChange={(event) => {
          setDraft(view.view, event.target.value);
          scheduleCommit(view.view);
        }}
        onKeyDown={(event) => {
          if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
            event.preventDefault();
            void commitText(view.view);
          }
          if (event.key === "Escape") {
            event.preventDefault();
            event.currentTarget.blur();
          }
          // Editing must not pan the canvas or move the card.
          event.stopPropagation();
        }}
      />
      <div className="editor-foot">
        <span className={`editor-reply ${reply?.error ? "error" : ""}`}>
          {reply ? reply.text : "edits commit as you type"}
        </span>
        <span className="editor-hint">
          {lines} {lines === 1 ? "line" : "lines"} · live
        </span>
      </div>
    </div>
  );
}

export function short(name: string): string {
  return displayName(name);
}
