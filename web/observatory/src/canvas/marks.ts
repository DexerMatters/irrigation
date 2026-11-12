import { useEffect, type RefObject } from "react";

/**
 * The one-shot change animations.
 *
 * They live with the element they mark, not on a global "the revision changed"
 * effect. A revision event lands before the data it describes has been read, so
 * a surface-wide sweep fires while the DOM still holds the previous revision's
 * marks — the previous change flashes, and the new one never does. Each element
 * knows, from its own data, which revision it is annotated for; it animates when
 * that evidence arrives.
 */

export function stillMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/** A plate's border and halo, once, when the runtime reports it changed. */
export function usePlateFlash(
  ref: RefObject<HTMLElement | null>,
  active: boolean,
  token: string | null,
) {
  useEffect(() => {
    const element = ref.current;
    if (!element || !active || token === null || stillMotion()) return;
    element.animate(
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
  }, [ref, active, token]);
}

/**
 * Wipe the signal tint down every changed row inside `root`.
 *
 * `token` is the evidence the caller is showing: the shown revision and the
 * render that carries the marks. A null token means the rows are not annotated
 * as changed at the revision on screen, so there is nothing to flash.
 */
export function useRowFlash(
  root: RefObject<HTMLElement | null>,
  token: string | null,
) {
  useEffect(() => {
    const element = root.current;
    if (!element || token === null || stillMotion()) return;
    for (const row of element.querySelectorAll<HTMLElement>(".row-changed")) {
      row.animate(
        [
          { backgroundColor: "color-mix(in srgb, var(--signal) 18%, transparent)" },
          { backgroundColor: "color-mix(in srgb, var(--signal) 0%, transparent)" },
        ],
        { duration: 900, easing: "ease-out" },
      );
    }
  }, [root, token]);
}
