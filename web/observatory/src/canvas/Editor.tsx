import { useEffect, useRef } from "react";
import { commitText, ensureDraft, scheduleCommit, setDraft, useStore } from "../state/store";
import { displayName } from "./layout";
import type { ViewWire } from "../state/types";

/**
 * The in-place editor for a source View.
 *
 * A source plate is a text channel, so it is a text field all the time: there
 * is no "open the editor" step and no Done button to press. The text is read
 * once when the plate first draws, and every keystroke commits. The page does
 * not parse the host's language; it sends the text, shows the host's reply or
 * its error verbatim, and lets the stream carry the delta.
 */
export function Editor({ view, compact = false }: { view: ViewWire; compact?: boolean }) {
  const state = useStore((snapshot) => snapshot);
  const area = useRef<HTMLTextAreaElement | null>(null);
  const draft = state.drafts[view.view];
  const reply = state.replies[view.view];
  const loading = draft === undefined;
  const lines = (draft ?? "").split("\n").length;

  useEffect(() => {
    ensureDraft(view.view);
  }, [view.view, view.content_version]);

  return (
    <div className="editor">
      <textarea
        ref={area}
        className="editor-area mono"
        spellCheck={false}
        // The plate's height is the layout's, not the text's: the field fills
        // the body and scrolls instead of pushing the card past its band.
        rows={compact ? 4 : 6}
        value={draft ?? ""}
        placeholder={loading ? "reading the text…" : ""}
        readOnly={loading}
        aria-busy={loading}
        aria-label={`${displayName(view.name)} text`}
        onChange={(event) => {
          setDraft(view.view, event.target.value);
          scheduleCommit(view.view);
        }}
        onPointerDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
            event.preventDefault();
            void commitText(view.view);
          }
          if (event.key === "Escape") {
            event.preventDefault();
            event.currentTarget.blur();
          }
          // Typing must never pan the canvas or move the card.
          event.stopPropagation();
        }}
      />
      <div className="editor-foot">
        <span
          className={[
            "editor-reply",
            reply?.error ? "error" : reply ? "active" : "",
          ]
            .filter(Boolean)
            .join(" ")}
        >
          {reply ? reply.text : loading ? "editable source" : "edits commit as you type"}
        </span>
        <span className="editor-hint">
          {lines} {lines === 1 ? "line" : "lines"} · live
        </span>
      </div>
    </div>
  );
}
