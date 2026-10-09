/** File explorer actions: open, download, favorite, cut / copy / paste, new folder / text file, delete for good, keyboard shortcuts and drag-and-drop upload */
import { focusIsFree } from "@/lib/focus";
import { useEffect, type DragEvent } from "react";
import { toast } from "sonner";
import { api, privateSource, type Node } from "@/api";
import { keys } from "@/api/queryKeys";
import { triggerDownload } from "@/downloads";
import { setClipboard } from "@/lib/clipboard";
import { t } from "@/lib/i18n";
import { type FileChange, invalidateFiles, refreshFiles, renamed, rowsOf } from "@/lib/queries";
import { eachBatch, idsOf, type Picked } from "@/lib/span";
import { transferItems } from "@/lib/transfer";
import { type Origins, originsOf, toastWithUndo, undoLast } from "@/lib/undo";
import { confirm } from "@/lib/confirm";
import { carriesFiles, dropFiles, dropItems } from "@/lib/dnd";
import { filesFromDrop, uploadFiles } from "@/uploads";
import { runJob, waitForJob } from "@/lib/jobs";
import { reportShown } from "@/lib/errorReport";
import { type Item, isTyping } from "./types";
import { isPending, listRefreshed } from "./newItems";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";
import { errorMessage } from "@/lib/utils";
import { isMenuKey, menuPointOf, menusClosed, openMenuByKey } from "@/lib/contextMenus";
import { pressed } from "@/lib/style/keymap";

/** The most items a download or a ZIP file takes at once (the server's limit, which it words when there are more) */
const MAX_AT_ONCE = 10_000;

export function useExplorerActions(p: ExplorerProps, s: ExplorerState) {
  const { caps, qc, navigate, tabs, clip, canCreate, canUpload, selectedNodes, selectedIds, single, allFavorite, setSelected, setAnchor, dialog, setDialog, setDragging, setDetailsOpen } = s;
  /** Refresh (the menu, or Retry after an error): everything shown loads again, and new items go to their sorted places */
  const refresh = () => {
    listRefreshed();
    return invalidateFiles(qc);
  };
  /** After a change: what it touched loads again (lib/queries) */
  const changed = (change: FileChange) => refreshFiles(qc, change);
  /** The folders items are in (the folder shown, or each item's own in lists of several places) */
  const parentsOf = (ids: readonly string[]) => {
    const wanted = new Set(ids);
    return [p.folderId, ...p.items.filter((n) => wanted.has(n.id)).map((n) => n.parent_id)];
  };

  /** Like Windows: when the name exists, try "Name (2)", "Name (3)"… in turn */
  const uniqueName = (base: string, ext = "") => {
    const taken = new Set([...p.items, ...s.newItems.items.map((n) => n.item)].map((n) => n.name.toLowerCase()));
    for (let i = 1; ; i++) {
      const name = i === 1 ? `${base}${ext}` : `${base} (${i})${ext}`;
      if (!taken.has(name.toLowerCase())) return name;
    }
  };

  /** A new item shown where it was made gets its real id; the focus, lost with the row it was on, goes to it */
  const settleNew = (key: string, id: string) => {
    s.newItems.settle(key);
    s.replaceSelected(key, id);
    setTimeout(() => {
      if (!document.activeElement || document.activeElement === document.body) s.listNav.current?.show(id, true);
    });
  };

  /**
   * The Windows style (explorer/newItems): the new item shows at once at the end of the list, selected and being
   * renamed, while the server makes it. If making it fails, it goes and the reason is shown; a name typed meanwhile is
   * given once it is made (`renameItem`).
   */
  const createAtEnd = async (kind: "folder" | "file", folder: string, name: string) => {
    const made = kind === "folder" ? api.createFolder(folder, name).then((n) => n.id) : api.createEmptyFile(folder, name);
    // Handled below (and by a rename waiting for it)
    made.catch(() => undefined);
    const list = p.list;
    // A large folder not all loaded: after the items loaded, not in a part that isn't
    const at = list && !list.complete ? (list.index.get(list.loaded.at(-1)?.id ?? "") ?? -1) + 1 : Infinity;
    const now = Math.floor(Date.now() / 1000);
    const item = s.newItems.add(
      {
        parent_id: folder,
        kind,
        name,
        size: 0,
        mime: kind === "file" ? "text/plain" : "",
        created_at: now,
        updated_at: now,
        trashed_at: null,
        drive_id: p.folder?.drive_id ?? null,
        owner_name: s.me.display_name || s.me.username,
        is_favorite: false,
      },
      at,
      made,
    );
    setSelected(new Set([item.id]));
    setAnchor(item.id);
    // Being named from now on (the name box shows a moment later): it takes its id once that ends (`renameDone`)
    s.naming.current.add(item.id);
    let failed = false;
    // At once, so what is typed next goes into the name; chosen in a menu, once the menu has given the focus back
    if (!document.querySelector("[role=menu]")) setDialog({ t: "rename", node: item });
    else void menusClosed().then(() => !failed && setDialog({ t: "rename", node: item }));
    let id: string;
    try {
      id = await made;
    } catch (e) {
      failed = true;
      s.naming.current.delete(item.id);
      s.newItems.drop([item.id]);
      setDialog((d) => (d?.t === "rename" && d.node.id === item.id ? null : d));
      s.replaceSelected(item.id, null);
      toast.error(errorMessage(e, t("Couldn't create")));
      reportShown("create", e, folder);
      return;
    }
    s.newItems.made(item.id, id);
    void changed({ folders: [folder], contents: true, recent: kind === "file" });
    // Still being renamed: it takes its id when that ends (`renameDone`), so the name box isn't started again
    if (!s.naming.current.has(item.id)) settleNew(item.id, id);
  };

  /** Like Windows: create "New folder" or "New Text Document.txt" right away, select it and start inline renaming */
  const createNew = async (kind: "folder" | "file") => {
    if (!p.folderId || !(kind === "folder" ? canCreate : canUpload)) return;
    // Default names follow the UI language (like English Windows: New folder, New Text Document.txt)
    const name = kind === "folder" ? uniqueName(t("New folder")) : uniqueName(t("New Text Document"), ".txt");
    if (s.kit.newAtEnd) return createAtEnd(kind, p.folderId, name);
    try {
      const id = kind === "folder" ? (await api.createFolder(p.folderId, name)).id : await api.createEmptyFile(p.folderId, name);
      // Only after the list reloads does the new item have a place to edit its name; if it didn't reload, don't start
      // renaming a row that isn't there (that would leave the shortcuts turned off)
      await changed({ folders: [p.folderId], contents: true, recent: kind === "file" });
      // Chosen in a menu: the rename box takes the focus once the menu has given it back
      await menusClosed();
      // In the folder's own lists (not the navigation pane's list of its subfolders, which the file list doesn't show)
      const listed = qc.getQueriesData({ queryKey: keys.children(p.folderId), predicate: (q) => q.queryKey[2] !== "folders" }).some(([, d]) => rowsOf(d)?.some((n) => n.id === id));
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

  /** Renames an item in the list; a new one still being made is renamed once it is (if making it failed, that was said) */
  const renameItem = async (n: Item, name: string) => {
    const added = s.newItems.items.find((x) => x.item.id === n.id);
    let id = n.id;
    if (added && isPending(n.id)) {
      try {
        id = await added.made;
      } catch {
        return;
      }
    }
    const node = await api.rename(id, name);
    if (added) s.newItems.rename(n.id, node.name);
    void changed(renamed(node));
    if (name !== n.name)
      toastWithUndo(t('Renamed to "{name}"', { name }), {
        undo: async () => {
          const back = await api.rename(id, n.name);
          s.newItems.rename(id, back.name);
          void changed(renamed(back));
        },
        undoneText: t("Renamed back"),
        label: t("Undo rename"),
      });
  };

  /** Renaming ended (a new name, or Esc): a new item made meanwhile takes its id */
  const renameDone = () => {
    const d = s.dialogNow.current;
    setDialog(null);
    if (d?.t !== "rename" || !isPending(d.node.id)) return;
    s.naming.current.delete(d.node.id);
    const id = s.newItems.items.find((x) => x.item.id === d.node.id)?.id;
    if (id) settleNew(d.node.id, id);
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
    void triggerDownload(async (signal) => {
      const ids = picked.span ? await idsOf(picked, MAX_AT_ONCE) : picked.ids;
      signal?.throwIfAborted();
      return privateSource.downloadLink(ids, signal);
    });
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
    // Moved away: new items kept at the end go with them
    if (ok && mode === "move") s.newItems.drop(picked.ids);
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
    // Where the items are, for undoing a "Move here" (the Mac style's way of moving them)
    setClipboard({ mode: "copy", ids: selectedIds, span: s.span, count: s.count, origins: originsOf(selectedNodes, selectedIds, "") });
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

  /** The Mac style's "Move here": the items copied (or cut) are moved into this folder, with the same questions about names */
  const moveHere = async () => {
    if (!clip || !p.folderId) return;
    const picked: Picked = { ids: clip.ids, span: clip.span ?? null, count: clip.count ?? clip.ids.length };
    const moved = await transfer("move", picked, p.folderId, (n) => t("Moved {n} item|Moved {n} items", { n }), t("Couldn't move"), clip.origins ?? new Map());
    if (moved) setClipboard(null);
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
    s.newItems.drop(picked.ids);
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
      focusListAgain();
    } catch (e) {
      toast.error(errorMessage(e, t("Operation failed")));
      reportShown("delete", e);
      // Some may have gone to the trash, or been deleted, before it failed
      void changed({ folders: [...parents, picked.span?.folder], trash: true, contents: true, usage: true });
    }
  };

  /** Items that had the focus are gone (trashed, deleted): it goes back to the list, as in File Explorer, once the rows
   * are out, rather than nowhere (the dialog gives it back to a row that no longer exists) */
  const focusListAgain = () =>
    setTimeout(() => {
      if (focusIsFree(document.activeElement)) s.listNav.current?.focusStart();
    });

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
      void changed({ folders: [picked.span.folder], trash: true, contents: true, usage: true }).then(focusListAgain);
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
    void changed({ removed: picked.ids, usage: true }).then(focusListAgain);
    s.newItems.drop(picked.ids);
  };
  // Keyboard shortcuts, the style's (moving around, search and refresh are the address bar's: see Frame)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (dialog || isTyping(e.target) || document.querySelector("[role=dialog]")) return;
      // A focused button, tab, menu item or list row handles its own keys (Enter on a toolbar button mustn't also open the selected file)
      if (e.defaultPrevented || (e.target as HTMLElement | null)?.closest?.("button, a, select, [role=menu], [role=menuitem], [role=tab], [role=separator]")) return;
      const k = s.kit.keys;
      const mod = e.ctrlKey || e.metaKey;
      if (pressed(e, k.cut)) cut();
      else if (pressed(e, k.copy) && !window.getSelection()?.toString()) copy();
      else if (pressed(e, k.paste) && canPaste) {
        e.preventDefault();
        paste();
      } else if (pressed(e, k.moveHere) && canPaste) {
        e.preventDefault();
        void moveHere();
      } else if (pressed(e, k.selectAll)) {
        e.preventDefault();
        s.selectAll();
      } else if (pressed(e, k.details)) {
        e.preventDefault();
        setDetailsOpen(true);
      } else if (pressed(e, k.undo)) {
        // Take back the last move, rename or delete
        e.preventDefault();
        undoLast();
      } else if (pressed(e, k.deleteForever) && s.count && caps.del) {
        e.preventDefault();
        void deleteForever(s.picked);
      } else if (pressed(e, k.trash) && s.count && caps.del) {
        setDialog({ t: "trash", picked: s.picked });
      } else if (pressed(e, k.newFolder) && canCreate) {
        e.preventDefault();
        void createNew("folder");
      } else if (pressed(e, k.rename) && single && caps.write) {
        e.preventDefault();
        setDialog({ t: "rename", node: single });
      } else if (pressed(e, k.open) && single) {
        e.preventDefault();
        open(single, true);
      } else if (isMenuKey(e, k.menu)) {
        e.preventDefault();
        openMenu();
      } else if (pressed(e, k.clearSelection)) {
        setSelected(new Set());
      } else if (e.key.length === 1 && e.key !== " " && !pressed(e, k.shortcuts) && !mod && !e.altKey) {
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
    moveHere,
    dragProps,
    createNew,
    renameItem,
    renameDone,
  };
}

export type ExplorerActions = ReturnType<typeof useExplorerActions>;
