import { useSyncExternalStore } from "react";

/** Unsaved text editor content; kept when switching tabs and coming back */
interface Draft {
  text: string;
  /** Original content when editing started; if someone else changed the file meanwhile, the draft is kept but saving is refused */
  base: string;
  /** Version (updated_at) the draft is based on, sent when saving so the server can refuse to overwrite a newer version */
  version?: number;
}

const drafts = new Map<string, Draft>();
const listeners = new Set<() => void>();
const discardListeners = new Set<(nodeId: string) => void>();
let version = 0;

function emit() {
  version++;
  listeners.forEach((l) => l());
}

export function getDraft(nodeId: string) {
  return drafts.get(nodeId);
}

export function setDraft(nodeId: string, draft: Draft | null) {
  const had = drafts.has(nodeId);
  if (draft) drafts.set(nodeId, draft);
  else drafts.delete(nodeId);
  if (had !== !!draft) emit();
  if (had && !draft) discardListeners.forEach((l) => l(nodeId));
}

/** Notify when a draft is removed (saved or discarded; the spreadsheet uses this to release discarded sessions) */
export function onDraftRemoved(listener: (nodeId: string) => void) {
  discardListeners.add(listener);
}

export function hasDraft(nodeId: string) {
  return drafts.has(nodeId);
}

/** Redraw the unsaved-content markers on tabs */
export function useDraftsVersion() {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => {
        listeners.delete(l);
      };
    },
    () => version,
  );
}

// While there's unsaved content, warn before closing or reloading the browser
window.addEventListener("beforeunload", (e) => {
  if (drafts.size > 0) e.preventDefault();
});
