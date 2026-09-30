import { createStore, useStore } from "@/lib/store";

/** Unsaved text editor content; kept when switching tabs and coming back */
export interface TextDraft {
  kind: "text";
  text: string;
  /** Original content when editing started; if someone else changed the file meanwhile, the draft is kept but saving is refused */
  base: string;
  /** Version (updated_at) the draft is based on, sent when saving so the server can refuse to overwrite a newer version */
  version?: number;
}

/**
 * Marker for a workbook with unsaved changes: the changes themselves stay in the spreadsheet's session
 * (`sheet/session.ts`); the marker only flags the tab and warns before closing
 */
export interface SheetDraft {
  kind: "sheet";
  /** Version (updated_at) the session was loaded from */
  base: number;
}

export type Draft = TextDraft | SheetDraft;

const drafts = new Map<string, Draft>();
const discardListeners = new Set<(nodeId: string) => void>();
/** Counts the times a file got or lost its draft */
const version = createStore(0);

/** The draft of the given kind; a draft of another kind (e.g. a workbook's marker for a text editor) is never returned */
export function getDraft<K extends Draft["kind"]>(nodeId: string, kind: K): Extract<Draft, { kind: K }> | undefined {
  const d = drafts.get(nodeId);
  return d?.kind === kind ? (d as Extract<Draft, { kind: K }>) : undefined;
}

export function setDraft(nodeId: string, draft: Draft | null) {
  const had = drafts.has(nodeId);
  if (draft) drafts.set(nodeId, draft);
  else drafts.delete(nodeId);
  if (had !== !!draft) version.set(version.get() + 1);
  if (had && !draft) discardListeners.forEach((l) => l(nodeId));
}

/**
 * A text save finished: `saved` is the text that was sent, now stored as `version`. Edits typed while the save was on
 * its way (`current` differs) stay a draft, based on the saved text and version, so the next save doesn't conflict
 * with this one; with no such edits the draft is gone.
 */
export function textSaved(nodeId: string, saved: string, current: string, version: number) {
  setDraft(nodeId, current === saved ? null : { kind: "text", text: current, base: saved, version });
}

/** Notify when a draft is removed (saved or discarded; the spreadsheet uses this to release discarded sessions) */
export function onDraftRemoved(listener: (nodeId: string) => void) {
  discardListeners.add(listener);
}

/** Whether the file has unsaved changes of any kind (for the tab marker and confirming before closing) */
export function hasDraft(nodeId: string) {
  return drafts.has(nodeId);
}

/** Redraw the unsaved-content markers on tabs */
export function useDraftsVersion() {
  return useStore(version);
}

// While there's unsaved content, warn before closing or reloading the browser
window.addEventListener("beforeunload", (e) => {
  if (drafts.size > 0) e.preventDefault();
});
