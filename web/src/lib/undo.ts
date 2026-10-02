import { toast } from "sonner";
import { api } from "@/api";
import { t } from "@/lib/i18n";
import { waitForJob } from "@/lib/jobs";
import type { FileChange } from "@/lib/queries";
import { createStore, useStore } from "@/lib/store";

/** How long a message with an Undo button stays: long enough to read it and reach the button */
export const UNDO_MS = 8000;

interface Undoable {
  run(): void;
  toast: string | number;
  /** What taking it back is called in a menu, e.g. "Undo delete" */
  label: string;
}

/** The last action that can be undone, for Ctrl+Z and the menus; taking it back (either way) clears it */
const last = createStore<Undoable | null>(null);

/**
 * Success message with an Undo button. `undo` takes the action back; `undoneText` is shown once it has, and `after`
 * runs then (e.g. to refresh the lists). Taking it back can fail (e.g. the name is taken meanwhile): the reason is shown.
 * Ctrl+Z in the file list takes back the last of these actions too, also after the message has gone; a menu offers it
 * as `label` (e.g. "Undo delete").
 */
export function toastWithUndo(message: string, opts: { undo(): Promise<unknown>; undoneText: string; label: string; after?(): void }) {
  const entry: Undoable = {
    label: opts.label,
    run: () => {
      if (last.get() === entry) last.set(null);
      opts
        .undo()
        .then(() => {
          toast.success(opts.undoneText);
          opts.after?.();
        })
        .catch((e: Error) => toast.error(e.message));
    },
    toast: toast.success(message, { duration: UNDO_MS, action: { label: t("Undo"), onClick: () => entry.run() } }),
  };
  last.set(entry);
}

/** What the last action that can be taken back is called in a menu ("Undo delete"), or null when there is none */
export function useUndoLabel() {
  return useStore(last)?.label ?? null;
}

/** Take back the last move, rename or delete (Ctrl+Z); false when there's nothing to take back */
export function undoLast() {
  const entry = last.get();
  if (!entry) return false;
  toast.dismiss(entry.toast);
  entry.run();
  return true;
}

/** Where each item was before a move (item id → parent folder id), for moving it back */
export type Origins = Map<string, string>;

export function originsOf(items: readonly { id: string; parent_id: string | null }[], ids: readonly string[], dest: string): Origins {
  const wanted = new Set(ids);
  // Items already in the destination don't move, so there's nothing to take back for them
  return new Map(items.filter((n) => wanted.has(n.id) && n.parent_id && n.parent_id !== dest).map((n) => [n.id, n.parent_id!]));
}

function byParent(origins: Origins) {
  const out = new Map<string, string[]>();
  for (const [id, parent] of origins) out.set(parent, [...(out.get(parent) ?? []), id]);
  return out;
}

/** Move items back to the folders they came from (they may come from several, e.g. in search results) */
export async function moveBack(origins: Origins) {
  for (const [parent, ids] of byParent(origins)) await waitForJob(await api.move(ids, parent));
}

/** What moving items back changed (lib/queries) */
export function movedBack(origins: Origins): FileChange {
  return { moved: [...byParent(origins)].map(([to, ids]) => ({ ids, to })), usage: true };
}
