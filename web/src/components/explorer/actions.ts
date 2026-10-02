/** File explorer actions: open, download, favorite, cut / copy / paste, new folder / text file, delete for good, keyboard shortcuts and drag-and-drop upload */
import { useEffect, type DragEvent } from "react";
import { toast } from "sonner";
import { api, privateSource, type Node } from "@/api";
import { keys } from "@/api/queryKeys";
import { triggerDownload } from "@/downloads";
import { setClipboard } from "@/lib/clipboard";
import { t } from "@/lib/i18n";
import { type FileChange, invalidateFiles, refreshFiles, rowsOf } from "@/lib/queries";
import { eachBatch, idsOf, type Picked } from "@/lib/span";
import { transferItems } from "@/lib/transfer";
import { type Origins, originsOf, toastWithUndo, undoLast } from "@/lib/undo";
import { confirm } from "@/lib/confirm";
import { carriesFiles, dropFiles, dropItems } from "@/lib/dnd";
import { filesFromDrop, uploadFiles } from "@/uploads";
import { runJob, waitForJob } from "@/lib/jobs";
import { reportShown } from "@/lib/errorReport";
import { type Item, isTyping } from "./types";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";
import { errorMessage } from "@/lib/utils";
import { isMenuKey, menuPointOf, menusClosed, openMenuByKey } from "@/lib/contextMenus";

/** The most items a download or a ZIP file takes at once (the server's limit, which it words when there are more) */
const MAX_AT_ONCE = 10_000;

export function useExplorerActions(p: ExplorerProps, s: ExplorerState) {
  const { caps, qc, navigate, tabs, clip, canCreate, canUpload, selectedNodes, selectedIds, single, allFavorite, setSelected, setAnchor, dialog, setDialog, setDragging, setDetailsOpen } = s;
  /** Refresh (the menu, or Retry after an error): everything shown loads again */
  const refresh = () => invalidateFiles(qc);
  /** After a change: what it touched loads again (lib/queries) */
  const changed = (change: FileChange) => refreshFiles(qc, change);
  /** The folders items are in (the folder shown, or each item's own in lists of several places) */
  const parentsOf = (ids: readonly string[]) => {
    const wanted = new Set(ids);
    return [p.folderId, ...p.items.filter((n) => wanted.has(n.id)).map((n) => n.parent_id)];
  };

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
      await changed({ folders: [p.folderId], contents: true, recent: kind === "file" });
      // Chosen in a menu: the rename box takes the focus once the menu has given it back
      await menusClosed();
      // The folder's pages, and the folder tree's list of subfolders
      const listed = qc.getQueriesData({ queryKey: keys.children(p.folderId) }).some(([, d]) => rowsOf(d)?.some((n) => n.id === id));
      if (!listed) {
        // A large folder where it sorts into a part not loaded: go there; renaming starts once that part has loaded
        const at = p.list ? await p.list.locate(id).catch(() => null) : null;
        if (at === null || at === undefined) return;
        s.listNav.current?.scrollTo(at);
        s.renameWhenShown.current = { id, name };
        return;
      }
      setSelected(new Set([id]));
      setAnchor(id);
      setDialog({ t: "rename", node: { id, name } });
    } catch (e) {
      toast.error(errorMessage(e, t("Couldn't create")));
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

  // Multiple items or folders are zipped by the server while streaming; progress shows in the download panel at the bottom right.
  // A span of a large folder is asked of the server first (a download takes at most 10,000 items: more, and it says so)
  const download = (picked: Picked) => {
    if (!picked.count) return;
    void triggerDownload(async () => privateSource.downloadLink(picked.span ? await idsOf(picked, MAX_AT_ONCE) : picked.ids));
  };

  // A new ZIP file in this folder, or a new folder with a ZIP file's contents: made on the server, followed in a message
  const compress = (picked: Picked) => {
    const folder = p.folderId;
    if (folder && picked.count)
      void runJob(qc, async () => api.compress(picked.span ? await idsOf(picked, MAX_AT_ONCE) : picked.ids, folder), { folders: [folder], contents: true, usage: true });
  };
  const extract = (n: Item) => void runJob(qc, () => api.extract(n.id), { folders: [n.parent_id], contents: true, usage: true });

  const toggleFavorite = async () => {
    const picked = s.picked;
    try {
      await eachBatch(picked, allFavorite ? t("Removing from favorites…") : t("Adding to favorites…"), (ids) => api.setFavorite(ids, !allFavorite));
      toast.success(allFavorite ? t("Removed from favorites") : t("Added to favorites"));
      // The star shows at once in every list; only the list of favorites loads again (and the parts of a large folder
      // whose items were selected without being loaded)
      void changed({ updated: selectedNodes.map((n) => ({ id: n.id, is_favorite: !allFavorite })), favorites: true, folders: [picked.span?.folder] });
    } catch (e) {
      toast.error(errorMessage(e, t("Operation failed")));
      reportShown("favorite", e);
      if (picked.span) void changed({ folders: [picked.span.folder], favorites: true });
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
  const transfer = async (mode: "move" | "copy", picked: Picked, dest: string, done: (n: number) => string, fallback: string, known?: Origins) => {
    const ok = await transferItems(qc, mode, picked, dest, { done, fallback, origins: known, items: p.items });
    if (ok) setSelected(new Set());
    return ok;
  };
  const uploadInto = (dt: DataTransfer, folder: Node) => void dropFiles(dt, folder);

  /**
   * Shift+F10 or the Menu key with the focus outside the list's items (a row opens its own menu): the selected items'
   * menu, or with nothing selected, the menu of the empty space
   */
  const openMenu = () => {
    const area = s.area.current;
    if (!area) return;
    const row = () => area.querySelector<HTMLElement>("[data-node-id][aria-selected=true]");
    const at = s.count ? row() : null;
    if (at) {
      at.focus({ preventScroll: true });
      openMenuByKey(at, menuPointOf(at));
    } else if (s.count) {
      // The selected items are out of view: the first is brought into view, then its menu opens
      if (s.listNav.current?.focusStart())
        setTimeout(() => {
          const shown = row();
          if (shown) openMenuByKey(shown, menuPointOf(shown));
        }, 50);
    } else {
      const r = area.getBoundingClientRect();
      const back = document.activeElement instanceof HTMLElement && document.activeElement !== document.body ? document.activeElement : null;
      openMenuByKey(area, { x: r.left + 24, y: r.top + 24 }, back);
    }
  };

  const cut = () => {
    if (!s.count || !caps.write) return;
    setClipboard({ mode: "cut", ids: selectedIds, span: s.span, count: s.count, origins: originsOf(selectedNodes, selectedIds, "") });
    toast(t("{n} item cut. Go to the destination folder and select Paste to move it.|{n} items cut. Go to the destination folder and select Paste to move them.", { n: s.count }));
  };
  const copy = () => {
    if (!s.count) return;
    setClipboard({ mode: "copy", ids: selectedIds, span: s.span, count: s.count });
    toast(t("{n} item copied|{n} items copied", { n: s.count }));
  };
  const canPaste = !!clip && canCreate;
  const paste = async () => {
    if (!clip || !p.folderId) return;
    const picked: Picked = { ids: clip.ids, span: clip.span ?? null, count: clip.count ?? clip.ids.length };
    if (clip.mode === "cut") {
      const moved = await transfer("move", picked, p.folderId, (n) => t("Moved {n} item|Moved {n} items", { n }), t("Couldn't paste"), clip.origins ?? new Map());
      if (moved) setClipboard(null);
    } else {
      await transfer("copy", picked, p.folderId, (n) => t("Pasted {n} item|Pasted {n} items", { n }), t("Couldn't paste"));
    }
  };

  /** Shift+Delete: delete for good without going through the trash, after asking */
  const deleteForever = async (picked: Picked) => {
    const ok = await confirm({
      title: t("Permanently delete {n} item?|Permanently delete {n} items?", { n: picked.count }),
      description: t("Permanently deleted items can't be recovered."),
      confirmText: t("Delete permanently"),
      destructive: true,
      irreversible: true,
    });
    if (!ok) return;
    setSelected(new Set());
    const parents = parentsOf(picked.ids);
    try {
      await eachBatch(picked, t("Deleting permanently…"), async (ids) => {
        // Only items in the trash can be deleted for good: put them there first
        await api.trash(ids);
        const job = await api.deleteForever(ids);
        // The rows of what is picked one by one go at once; a span's folder loads again
        void changed(picked.span ? { folders: [picked.span.folder], trash: true, usage: true } : { removed: ids, usage: true });
        await waitForJob(job);
      });
      toast.success(t("Permanently deleted"));
    } catch (e) {
      toast.error(errorMessage(e, t("Operation failed")));
      reportShown("delete", e);
      // Some may have gone to the trash, or been deleted, before it failed
      void changed({ folders: [...parents, picked.span?.folder], trash: true, contents: true, usage: true });
    }
  };

  /** Move to the trash (after the dialog asked); `picked` one by one can be put back from the message */
  const trash = async (picked: Picked) => {
    const parents = parentsOf(picked.ids);
    try {
      await eachBatch(picked, t("Moving to the trash…"), (ids) => api.trash(ids));
    } catch (e) {
      toast.error(errorMessage(e, t("Operation failed")));
      reportShown("trash", e);
      void changed({ folders: [...parents, picked.span?.folder], trash: true, contents: true, usage: true });
      return;
    }
    setSelected(new Set());
    if (picked.span) {
      toast.success(t("Moved {n} item to trash|Moved {n} items to trash", { n: picked.count }));
      void changed({ folders: [picked.span.folder], trash: true, contents: true, usage: true });
      return;
    }
    // Restoring can fail, e.g. the original folder was deleted, a name conflict, or the space is full
    toastWithUndo(t("Moved to trash"), {
      undo: () => api.restore(picked.ids),
      undoneText: t("Restored"),
      label: t("Undo delete"),
      after: () => changed({ folders: parents, trash: true, contents: true, usage: true }),
    });
    // The rows go at once; the lists aren't loaded again for it
    void changed({ removed: picked.ids, usage: true });
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
        s.selectAll();
      } else if (e.altKey && e.key === "Enter") {
        setDetailsOpen(true);
      } else if (mod && !e.shiftKey && key === "z") {
        // Ctrl+Z: take back the last move, rename or delete
        e.preventDefault();
        undoLast();
      } else if (e.key === "Delete" && e.shiftKey && s.count && caps.del) {
        e.preventDefault();
        void deleteForever(s.picked);
      } else if (e.key === "Delete" && s.count && caps.del) {
        setDialog({ t: "trash", picked: s.picked });
      } else if (mod && e.shiftKey && key === "n" && canCreate) {
        // Ctrl+Shift+N: new folder (like Windows)
        e.preventDefault();
        void createNew("folder");
      } else if (e.key === "F2" && single && caps.write) {
        e.preventDefault();
        setDialog({ t: "rename", node: single });
      } else if (e.key === "Enter" && single) {
        open(single, true);
      } else if (isMenuKey(e)) {
        e.preventDefault();
        openMenu();
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

  return {
    refresh,
    changed,
    parentsOf,
    open,
    download,
    compress,
    extract,
    toggleFavorite,
    dropInto,
    uploadInto,
    transfer,
    trash,
    deleteForever,
    cut,
    copy,
    canPaste,
    paste,
    dragProps,
    createNew,
  };
}

export type ExplorerActions = ReturnType<typeof useExplorerActions>;
