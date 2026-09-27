/**
 * Dragging items onto folders: rows of the file list, folders in the tree, parts of the address bar and tabs showing a
 * folder all take them. Items dragged from the list move (with Ctrl, or Option on macOS, they're copied); files and
 * folders dragged in from the computer are uploaded into the folder they're dropped on.
 */
import { useState, type DragEvent } from "react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { api } from "@/api";
import { t } from "@/lib/i18n";
import { wantsCopy } from "@/lib/keys";
import { invalidateFiles } from "@/lib/queries";
import { moveBack, originsOf, toastWithUndo } from "@/lib/undo";
import { enqueue, filesFromDrop } from "@/uploads";

export const DRAG_MIME = "application/x-thirtyfile-nodes";

/** A folder that takes dropped items */
export interface DropFolder {
  /** Folder id, or the alias "root" (My files) or "shared" (All files) */
  id: string;
  name: string;
}

/** The folder a page shows, from its path: `/files` (My files), `/files/shared` (All files) or `/files/<id>` */
export function folderOfPath(path: string | undefined): string | null {
  const p = path?.split(/[?#]/)[0];
  if (p === "/files") return "root";
  const m = p && /^\/files\/([^/]+)$/.exec(p);
  return m ? decodeURIComponent(m[1]) : null;
}

/** The items being dragged from this page, with the folders they came from (for Undo) */
let dragged: { ids: string[]; items: readonly { id: string; parent_id: string | null }[] } | null = null;

/** Start dragging items of the list: they can be moved, or copied with Ctrl (Option) */
export function startDrag(e: DragEvent, ids: string[], items: readonly { id: string; parent_id: string | null }[]) {
  e.dataTransfer.setData(DRAG_MIME, JSON.stringify(ids));
  e.dataTransfer.effectAllowed = "copyMove";
  dragged = { ids, items };
  e.currentTarget.addEventListener("dragend", () => (dragged = null), { once: true });
}

export const carriesItems = (dt: DataTransfer) => dt.types.includes(DRAG_MIME);
export const carriesFiles = (dt: DataTransfer) => dt.types.includes("Files");

/** The dragged item ids; any page can set this type when dragging, so check what arrived */
export function droppedIds(dt: DataTransfer): string[] | null {
  const raw = dt.getData(DRAG_MIME);
  if (!raw) return null;
  try {
    const ids: unknown = JSON.parse(raw);
    return Array.isArray(ids) && ids.every((id) => typeof id === "string") ? ids : null;
  } catch {
    return null;
  }
}

/** Move or copy items into a folder; a move can be undone */
export async function dropItems(qc: QueryClient, ids: string[], folder: DropFolder, copy: boolean) {
  ids = ids.filter((id) => id !== folder.id);
  if (!ids.length) return;
  const refresh = () => invalidateFiles(qc);
  try {
    if (copy) {
      await api.copy(ids, folder.id);
      toast.success(t("Copied {n} item to \"{name}\"|Copied {n} items to \"{name}\"", { n: ids.length, name: folder.name }));
    } else {
      const origins = dragged ? originsOf(dragged.items, ids, folder.id) : new Map<string, string>();
      await api.move(ids, folder.id);
      const moved = t("Moved {n} item to \"{name}\"|Moved {n} items to \"{name}\"", { n: ids.length, name: folder.name });
      if (origins.size) toastWithUndo(moved, { undo: () => moveBack(origins), undoneText: t("Moved back"), after: refresh });
      else toast.success(moved);
    }
    refresh();
  } catch (e) {
    toast.error(e instanceof Error ? e.message : copy ? t("Couldn't copy") : t("Couldn't move"));
  }
}

/** Upload files and folders dropped from the computer into a folder */
export async function dropFiles(dt: DataTransfer, folder: DropFolder) {
  const picked = await filesFromDrop(dt);
  if (picked.length) enqueue(picked, folder.id);
}

/** What the pointer would do over a folder: copy with Ctrl (Option), otherwise move; files from the computer are copied */
export function dropEffect(e: DragEvent): "copy" | "move" {
  return carriesItems(e.dataTransfer) && !wantsCopy(e) ? "move" : "copy";
}

/**
 * Make an element a drop target for a folder (none when `folder` is null): `dropping` is true while items are held over
 * it. `onDropped` runs after items were dropped (e.g. to clear the selection).
 */
export function useFolderDrop(folder: DropFolder | null, opts: { onDropped?(): void } = {}) {
  const qc = useQueryClient();
  const [dropping, setDropping] = useState(false);
  if (!folder) return { dropping: false, dropProps: {} };
  const accepts = (dt: DataTransfer) => carriesItems(dt) || carriesFiles(dt);
  return {
    dropping,
    dropProps: {
      onDragOver: (e: DragEvent) => {
        if (!accepts(e.dataTransfer)) return;
        e.preventDefault();
        e.stopPropagation();
        e.dataTransfer.dropEffect = dropEffect(e);
        setDropping(true);
      },
      onDragLeave: (e: DragEvent) => {
        if (!e.currentTarget.contains(e.relatedTarget as globalThis.Node | null)) setDropping(false);
      },
      onDrop: (e: DragEvent) => {
        setDropping(false);
        if (!accepts(e.dataTransfer)) return;
        e.preventDefault();
        e.stopPropagation();
        const ids = droppedIds(e.dataTransfer);
        if (ids) void dropItems(qc, ids, folder, wantsCopy(e)).then(() => opts.onDropped?.());
        else if (carriesFiles(e.dataTransfer)) void dropFiles(e.dataTransfer, folder);
      },
    },
  };
}
