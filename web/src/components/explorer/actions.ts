/** File explorer actions: open, download, favorite, cut / copy / paste, new folder / text file, delete for good, keyboard shortcuts and drag-and-drop upload */
import { useEffect, type DragEvent } from "react";
import type { InfiniteData } from "@tanstack/react-query";
import { toast } from "sonner";
import { api, privateSource, triggerDownload, type CursorPage, type Node } from "@/api";
import { setClipboard } from "@/lib/clipboard";
import { t } from "@/lib/i18n";
import { allItems } from "@/lib/pages";
import { FOLDER_CONTENTS, invalidateFiles } from "@/lib/queries";
import { type Origins, moveBack, originsOf, toastWithUndo, undoLast } from "@/lib/undo";
import { confirm } from "@/components/confirm";
import { askBeforeTransfer } from "@/components/ConflictDialog";
import { carriesFiles, dropFiles, dropItems } from "@/lib/dnd";
import { filesFromDrop, uploadFiles } from "@/uploads";
import { runJob, waitForJob } from "@/lib/jobs";
import { reportShown } from "@/lib/errorReport";
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
  /** After adding, moving or removing items: what folders hold changes too */
  const refreshContents = () => invalidateFiles(qc, FOLDER_CONTENTS);

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
      await refreshContents();
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
      reportShown("create", e, p.folderId ?? undefined);
    }
  };

  // Folders and files both open in the current tab, so "Back" returns to the original location; a file already open in another tab switches there
  // A folder opened with Enter gets the focus in its list once it's shown (see state), so the keyboard carries on there
  const open = (n: Item, byKey = false) => {
    if (n.kind === "folder") {
      s.enteredByKey.current = byKey ? n.id : null;
      navigate(`/files/${n.id}`);
    } else tabs.openFile(`/view/${n.id}`);
  };

  // Multiple items or folders are zipped by the server while streaming; progress shows in the download panel at the bottom right
  const download = (ids: string[]) => ids.length && void triggerDownload(() => privateSource.downloadLink(ids));

  // A new ZIP file in this folder, or a new folder with a ZIP file's contents: made on the server, followed in a message
  const compress = (ids: string[]) => {
    const folder = p.folderId;
    if (folder && ids.length) void runJob(qc, () => api.compress(ids, folder));
  };
  const extract = (n: Item) => void runJob(qc, () => api.extract(n.id));

  const toggleFavorite = async () => {
    try {
      await api.setFavorite(selectedIds, !allFavorite);
      toast.success(allFavorite ? t("Removed from favorites") : t("Added to favorites"));
      refresh();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Operation failed"));
      reportShown("favorite", e);
    }
  };

  // Items dragged onto a folder in the list move there (or are copied, with Ctrl); files from the computer are uploaded there
  const dropInto = async (ids: string[], folder: Node, copy: boolean) => {
    await dropItems(qc, ids, folder, copy);
    setSelected(new Set());
  };

  /**
   * Moves or copies items to a folder, asking first what to do with names the folder already has (replace, skip or
   * keep both). `done` words the message for the number of items that went; a move can be undone from it. False when
   * it was cancelled or failed (the reason is shown).
   */
  const transfer = async (mode: "move" | "copy", ids: string[], dest: string, done: (n: number) => string, fallback: string, known?: Origins) => {
    try {
      const resolutions = await askBeforeTransfer(mode, ids, dest);
      if (!resolutions) return false;
      const sent = ids.filter((id) => resolutions[id] !== "skip");
      if (sent.length) {
        if (mode === "move") {
          const origins = new Map([...(known ?? originsOf(p.items, sent, dest))].filter(([id, parent]) => sent.includes(id) && parent !== dest));
          await waitForJob(await api.move(sent, dest, resolutions));
          if (origins.size) toastWithUndo(done(sent.length), { undo: () => moveBack(origins), undoneText: t("Moved back"), after: refreshContents });
          else toast.success(done(sent.length));
        } else {
          await waitForJob(await api.copy(sent, dest, resolutions));
          toast.success(done(sent.length));
        }
      }
      setSelected(new Set());
      refreshContents();
      return true;
    } catch (e) {
      toast.error(e instanceof Error ? e.message : fallback);
      reportShown(mode, e, dest);
      // What was done before it failed shows
      refreshContents();
      return false;
    }
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
    if (clip.mode === "cut") {
      const moved = await transfer("move", clip.ids, p.folderId, (n) => t("Moved {n} item|Moved {n} items", { n }), t("Couldn't paste"), clip.origins ?? new Map());
      if (moved) setClipboard(null);
    } else {
      await transfer("copy", clip.ids, p.folderId, (n) => t("Pasted {n} item|Pasted {n} items", { n }), t("Couldn't paste"));
    }
  };

  /** Shift+Delete: delete for good without going through the trash, after asking */
  const deleteForever = async (ids: string[]) => {
    const ok = await confirm({
      title: t("Permanently delete {n} item?|Permanently delete {n} items?", { n: ids.length }),
      description: t("Permanently deleted items can't be recovered."),
      confirmText: t("Delete permanently"),
      destructive: true,
      irreversible: true,
    });
    if (!ok) return;
    try {
      // Only items in the trash can be deleted for good: put them there first
      await api.trash(ids);
      await api.deleteForever(ids);
      toast.success(t("Permanently deleted"));
      setSelected(new Set());
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Operation failed"));
      reportShown("delete", e);
    }
    refreshContents();
  };

  // Keyboard shortcuts (moving around, search and refresh are the address bar's: see Frame)
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
      } else if (mod && !e.shiftKey && key === "z") {
        // Ctrl+Z: take back the last move, rename or delete
        e.preventDefault();
        undoLast();
      } else if (e.key === "Delete" && e.shiftKey && selectedNodes.length && caps.del) {
        e.preventDefault();
        void deleteForever(selectedIds);
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
        open(single, true);
      } else if (e.key === "Escape") {
        setSelected(new Set());
      } else if (e.key.length === 1 && e.key !== " " && e.key !== "?" && !mod && !e.altKey) {
        // Typing letters goes to the next item whose name starts with them
        e.preventDefault();
        s.listNav.current?.typeAhead(e.key);
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
          if (picked.length) void uploadFiles(picked, p.folderId!);
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

  return { refresh, refreshContents, open, download, compress, extract, toggleFavorite, dropInto, uploadInto, transfer, cut, copy, canPaste, paste, dragProps, createNew };
}

export type ExplorerActions = ReturnType<typeof useExplorerActions>;
