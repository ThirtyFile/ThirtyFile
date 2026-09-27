/** File explorer actions: open, download, favorite, cut / copy / paste, new folder / text file, keyboard shortcuts and drag-and-drop upload */
import { useEffect, type DragEvent } from "react";
import type { InfiniteData } from "@tanstack/react-query";
import { toast } from "sonner";
import { api, privateSource, triggerDownload, type CursorPage, type Node } from "@/api";
import { setClipboard } from "@/lib/clipboard";
import { t } from "@/lib/i18n";
import { allItems } from "@/lib/pages";
import { invalidateFiles } from "@/lib/queries";
import { moveBack, originsOf, toastWithUndo } from "@/lib/undo";
import { carriesFiles, dropFiles, dropItems } from "@/lib/dnd";
import { enqueue, filesFromDrop } from "@/uploads";
import { type Item, isTyping } from "./types";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";

export function useExplorerActions(p: ExplorerProps, s: ExplorerState) {
  const {
    caps,
    qc,
    navigate,
    tabs,
    clip,
    canCreate,
    canUpload,
    selectedNodes,
    selectedIds,
    single,
    allFavorite,
    setSelected,
    setAnchor,
    dialog,
    setDialog,
    setDragging,
    setDetailsOpen,
  } = s;
  const refresh = () => invalidateFiles(qc);

  /** Like Windows: when the name exists, try "Name (2)", "Name (3)"… in turn */
  const uniqueName = (base: string, ext = "") => {
    const taken = new Set(p.items.map((n) => n.name.toLowerCase()));
    for (let i = 1; ; i++) {
      const name = i === 1 ? `${base}${ext}` : `${base} (${i})${ext}`;
      if (!taken.has(name.toLowerCase())) return name;
    }
  };

  /** Like Windows: create "New folder" or "New Text Document.txt" right away, select it and start inline renaming */
  const createNew = async (kind: "folder" | "file") => {
    if (!p.folderId || !(kind === "folder" ? canCreate : canUpload)) return;
    try {
      // Default names follow the UI language (like English Windows: New folder, New Text Document.txt)
      const name = kind === "folder" ? uniqueName(t("New folder")) : uniqueName(t("New Text Document"), ".txt");
      const id = kind === "folder" ? (await api.createFolder(p.folderId, name)).id : await api.createEmptyFile(p.folderId, name);
      // Only after the list reloads does the new item have a place to edit its name; if it didn't reload, don't start
      // renaming a row that isn't there (that would leave the shortcuts turned off)
      await refresh();
      // The folder's pages, and the folder tree's list of subfolders
      const listed = qc
        .getQueriesData<Node[] | InfiniteData<CursorPage<Node>>>({ queryKey: ["children", p.folderId] })
        .some(([, d]) => (Array.isArray(d) ? d : allItems(d)).some((n) => n.id === id));
      if (!listed) return;
      setSelected(new Set([id]));
      setAnchor(id);
      setDialog({ t: "rename", node: { id, name } });
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Couldn't create"));
    }
  };

  // Folders and files both open in the current tab, so "Back" returns to the original location; a file already open in another tab switches there
  const open = (n: Item) => {
    if (n.kind === "folder") navigate(`/files/${n.id}`);
    else tabs.openFile(`/view/${n.id}`);
  };

  // Multiple items or folders are zipped by the server while streaming; progress shows in the download panel at the bottom right
  const download = (ids: string[]) => ids.length && void triggerDownload(() => privateSource.downloadLink(ids));

  const toggleFavorite = async () => {
    try {
      await api.setFavorite(selectedIds, !allFavorite);
      toast.success(allFavorite ? t("Removed from favorites") : t("Added to favorites"));
      refresh();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Operation failed"));
    }
  };

  // Items dragged onto a folder in the list move there (or are copied, with Ctrl); files from the computer are uploaded there
  const dropInto = async (ids: string[], folder: Node, copy: boolean) => {
    await dropItems(qc, ids, folder, copy);
    setSelected(new Set());
  };
  const uploadInto = (dt: DataTransfer, folder: Node) => void dropFiles(dt, folder);

  const cut = () => {
    if (!selectedIds.length || !caps.write) return;
    setClipboard({ mode: "cut", ids: selectedIds, origins: originsOf(selectedNodes, selectedIds, "") });
    toast(t("{n} item cut. Go to the destination folder and select Paste to move it.|{n} items cut. Go to the destination folder and select Paste to move them.", { n: selectedIds.length }));
  };
  const copy = () => {
    if (!selectedIds.length) return;
    setClipboard({ mode: "copy", ids: selectedIds });
    toast(t("{n} item copied|{n} items copied", { n: selectedIds.length }));
  };
  const canPaste = !!clip && canCreate;
  const paste = async () => {
    if (!clip || !p.folderId) return;
    try {
      if (clip.mode === "cut") {
        const dest = p.folderId;
        const origins = new Map([...(clip.origins ?? [])].filter(([, parent]) => parent !== dest));
        await api.move(clip.ids, dest);
        setClipboard(null);
        const moved = t("Moved {n} item|Moved {n} items", { n: clip.ids.length });
        if (origins.size) toastWithUndo(moved, { undo: () => moveBack(origins), undoneText: t("Moved back"), after: refresh });
        else toast.success(moved);
      } else {
        await api.copy(clip.ids, p.folderId);
        toast.success(t("Pasted {n} item|Pasted {n} items", { n: clip.ids.length }));
      }
      refresh();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Couldn't paste"));
    }
  };

  // Keyboard shortcuts
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (dialog || isTyping(e.target) || document.querySelector("[role=dialog]")) return;
      // A focused button, tab, menu item or list row handles its own keys (Enter on a toolbar button mustn't also open the selected file)
      if (e.defaultPrevented || (e.target as HTMLElement | null)?.closest?.("button, a, select, [role=menu], [role=menuitem], [role=tab], [role=separator]")) return;
      const mod = e.ctrlKey || e.metaKey;
      const key = e.key.toLowerCase();
      if (mod && key === "x") cut();
      else if (mod && key === "c" && !window.getSelection()?.toString()) copy();
      else if (mod && key === "v" && canPaste) {
        e.preventDefault();
        paste();
      } else if (mod && key === "a") {
        e.preventDefault();
        setSelected(new Set(p.items.map((n) => n.id)));
      } else if (e.altKey && e.key === "Enter") {
        setDetailsOpen(true);
      } else if (e.key === "Delete" && selectedNodes.length && caps.del) {
        setDialog({ t: "trash", ids: selectedIds });
      } else if (mod && e.shiftKey && key === "n" && canCreate) {
        // Ctrl+Shift+N: new folder (like Windows)
        e.preventDefault();
        void createNew("folder");
      } else if (e.key === "F2" && single && caps.write) {
        e.preventDefault();
        setDialog({ t: "rename", node: single });
      } else if (e.key === "Enter" && single) {
        open(single);
      } else if (e.key === "Escape") {
        setSelected(new Set());
      } else if (e.key === "Backspace" && p.upTo) {
        navigate(p.upTo);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // Upload files dragged in from the desktop (into the folder shown, or the folder row they're dropped on); when the storage service is offline, intercept and explain (otherwise the browser would open the dropped file)
  const dragProps = canUpload
    ? {
        onDragOver: (e: DragEvent) => {
          if (!carriesFiles(e.dataTransfer)) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = "copy";
          // Over a folder row, the files go into that folder: don't say they go into this one
          setDragging(!(e.target as HTMLElement).closest("[data-drop-folder]"));
        },
        onDragLeave: (e: DragEvent) => {
          if (!e.currentTarget.contains(e.relatedTarget as globalThis.Node | null)) setDragging(false);
        },
        // A folder row that takes the drop stops it there
        onDropCapture: () => setDragging(false),
        onDrop: async (e: DragEvent) => {
          if (!carriesFiles(e.dataTransfer)) return;
          e.preventDefault();
          setDragging(false);
          const picked = await filesFromDrop(e.dataTransfer);
          if (picked.length) enqueue(picked, p.folderId!);
        },
      }
    : canCreate && p.offline
      ? {
          onDragOver: (e: DragEvent) => {
            if (!e.dataTransfer.types.includes("Files")) return;
            e.preventDefault();
            e.dataTransfer.dropEffect = "none";
          },
          onDrop: (e: DragEvent) => {
            if (!e.dataTransfer.types.includes("Files")) return;
            e.preventDefault();
            toast.error(t("This space's storage service is offline. You can't upload right now."));
          },
        }
      : {};

  return { refresh, open, download, toggleFavorite, dropInto, uploadInto, cut, copy, canPaste, paste, dragProps, createNew };
}

export type ExplorerActions = ReturnType<typeof useExplorerActions>;
