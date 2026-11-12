import type { RevisionWire } from "./types";

/**
 * Which revision the page is looking at, and which revision the data on screen
 * describes.
 *
 * Pure, and separate from the store because both facts are easy to get subtly
 * wrong and the symptom — a plate annotated with the previous revision's change
 * — looks exactly like a rendering bug. The stream announces a revision before
 * the workspace read that describes it has landed, so "the newest revision" and
 * "the revision the headers describe" are two numbers, not one.
 *
 * The board annotates against the *data* revision: while a new revision is in
 * flight the plates keep saying what they know (r21) instead of clearing every
 * mark until the read lands. That blank frame is what made live updates look
 * delayed even when the network was fast.
 */
export type RevisionState = {
  revision: number;
  dataRevision: number;
  selectedRevision: number | null;
  following: boolean;
  revisions: RevisionWire[];
};

/** The journal record on screen, if the history has it yet. */
export function activeRevisionOf(current: RevisionState): RevisionWire | null {
  const revision = current.selectedRevision ?? current.revision;
  if (current.revisions.length === 0) return null;
  return current.revisions.find((record) => record.revision === revision) ?? null;
}

/** The revision the board is showing: the selected one, or the newest. */
export function shownRevisionOf(current: RevisionState): number {
  return activeRevisionOf(current)?.revision ?? current.revision;
}

/** True when the board is reading the newest revision rather than a record. */
export function atNewestOf(current: RevisionState): boolean {
  return current.following || current.selectedRevision === current.revision;
}

/**
 * The revision the annotations on screen describe.
 *
 * Following the newest revision, that is the workspace read the headers came
 * from — the latest number the page actually has data for. An older selected
 * record is its own annotation revision. Marks are drawn against this number,
 * never against a revision whose data has not arrived.
 */
export function annotationRevisionOf(current: RevisionState): number {
  return atNewestOf(current) ? current.dataRevision : shownRevisionOf(current);
}
