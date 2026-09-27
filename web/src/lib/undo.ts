import { toast } from "sonner";
import { api } from "@/api";
import { t } from "@/lib/i18n";

/** How long a message with an Undo button stays: long enough to read it and reach the button */
export const UNDO_MS = 8000;

/** The last action that can be undone, for Ctrl+Z; taking it back (either way) clears it */
let last: { run(): void; toast: string | number } | null = null;

/**
 * Success message with an Undo button. `undo` takes the action back; `undoneText` is shown once it has, and `after`
 * runs then (e.g. to refresh the lists). Taking it back can fail (e.g. the name is taken meanwhile): the reason is shown.
 * Ctrl+Z in the file list takes back the last of these actions too, also after the message has gone.
 */
export function toastWithUndo(message: string, opts: { undo(): Promise<unknown>; undoneText: string; after?(): void }) {
  const entry = {
    run: () => {
      if (last === entry) last = null;
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
  last = entry;
}

/** Take back the last move, rename or delete (Ctrl+Z); false when there's nothing to take back */
export function undoLast() {
  const entry = last;
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

/** Move items back to the folders they came from (they may come from several, e.g. in search results) */
export async function moveBack(origins: Origins) {
  const byParent = new Map<string, string[]>();
  for (const [id, parent] of origins) byParent.set(parent, [...(byParent.get(parent) ?? []), id]);
  for (const [parent, ids] of byParent) await api.move(ids, parent);
}
