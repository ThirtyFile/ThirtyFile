/** File explorer state: selection, view mode and grouping, dialogs, clipboard and permissions in the current folder */
import { useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";
import type { ListNav } from "@/components/FileList";
import type { ViewMode } from "@/components/fileList/layout";
import { useClipboard } from "@/lib/clipboard";
import { capsOf } from "@/lib/drives";
import { focusIsFree } from "@/lib/focus";
import type { GroupBy } from "@/lib/listView";
import { inSpan, spanCount, type FolderSpan, type ListSpan, type Picked } from "@/lib/span";
import { usePersisted, useMe } from "@/lib/session";
import { useUndoLabel } from "@/lib/undo";
import { useWindowsBehaviour } from "@/lib/windowsBehaviour";
import { useTabActions } from "@/tabs";
import type { DialogState } from "./types";
import type { ExplorerProps } from "../Explorer";

export function useExplorerState(p: ExplorerProps) {
  const me = useMe();
  // In a folder, available actions depend on the role; list pages (search, recent, etc.) mix several locations, so the server decides
  const caps = p.role ? capsOf(p.role, me, p.readOnly) : { write: me.can_write, del: me.can_delete, share: me.can_share, manage: false };
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tabs = useTabActions();
  const [view, setView] = usePersisted<ViewMode>("tf-view", "list");
  const [groupBy, setGroupBy] = usePersisted<GroupBy>("tf-group", "none");
  const [selected, setChosen] = useState<Set<string>>(new Set());
  /** A large folder: what is selected without being loaded (Select all, or Shift across parts not loaded; lib/span) */
  const [span, setSpan] = useState<FolderSpan | null>(null);
  const [anchor, setAnchor] = useState<string | null>(null);
  const [dialog, setDialog] = useState<DialogState | null>(null);
  const [dragging, setDragging] = useState(false);
  const [showCheckboxes, setShowCheckboxes] = usePersisted("tf-checkboxes", false);
  const [detailsOpen, setDetailsOpen] = usePersisted("tf-details-pane", false);
  const clip = useClipboard();
  const fileInput = useRef<HTMLInputElement>(null);
  const dirInput = useRef<HTMLInputElement>(null);
  const listNav = useRef<ListNav>(null);
  /** The area around the list, whose context menu is the one for empty space */
  const area = useRef<HTMLDivElement>(null);
  /** The File Explorer conventions followed (lib/windowsBehaviour) */
  const behaviour = useWindowsBehaviour();
  /** What Ctrl+Z would take back, as a menu names it ("Undo delete"); null when nothing */
  const undoLabel = useUndoLabel();

  // New folder and paste only touch the database; uploading and creating files need to write to the storage service, so they're disabled while offline
  const canCreate = !!p.folderId && caps.write;
  const canUpload = canCreate && !p.offline;
  const list = p.list;
  const total = list ? Math.max(0, list.total) : p.items.length;

  /** Selects items picked one by one (none: `span` is cleared too) */
  const setSelected = (next: Set<string>) => {
    setChosen(next);
    setSpan(null);
  };
  /** The file list's selection: with a span of a large folder, where and in which order it was made */
  const choose = (next: Set<string>, listSpan?: ListSpan | null) => {
    setChosen(next);
    setSpan(listSpan && p.folderId && p.sort ? { ...listSpan, folder: p.folderId, sort: p.sort.key, order: p.sort.order, count: spanCount(listSpan, total) } : null);
  };
  /** Ctrl+A: every item; in a large folder not all loaded, that is a span of the whole folder */
  const selectAll = () => {
    if (list && !list.complete && p.folderId && p.sort) {
      setChosen(new Set());
      setSpan({ folder: p.folderId, sort: p.sort.key, order: p.sort.order, except: new Set(), count: total });
    } else setSelected(new Set(p.items.map((n) => n.id)));
  };
  /**
   * Invert selection. In a large folder not all loaded: everything but the items picked (a span of the whole folder),
   * or, from a span of the whole folder, the items it left out. A span from one item to another can't be inverted.
   */
  const whole = !!span && !span.from && !span.to;
  const canInvert = !span || whole;
  const invert = () => {
    if (whole) setSelected(new Set(span.except));
    else if (list && !list.complete && p.folderId && p.sort) {
      setChosen(new Set());
      setSpan({ folder: p.folderId, sort: p.sort.key, order: p.sort.order, except: new Set(selected), count: total - selected.size });
    } else setSelected(new Set(p.items.filter((n) => !selected.has(n.id)).map((n) => n.id)));
  };

  // The items selected that are loaded (all of them, unless a span holds items not loaded)
  const selectedNodes = useMemo(() => p.items.filter((n) => selected.has(n.id) || (!!span && inSpan(span, list?.index.get(n.id) ?? -1, n.id))), [p.items, selected, span, list]);
  /** Items picked one by one; the span's aren't all known here (see `picked`) */
  const selectedIds = span ? [...selected] : selectedNodes.map((n) => n.id);
  const count = span ? selected.size + spanCount(span, total) : selectedNodes.length;
  /** What the commands work on */
  const picked: Picked = { ids: selectedIds, span, count };
  const single = !span && selectedNodes.length === 1 ? selectedNodes[0] : null;
  const allFavorite = selectedNodes.length > 0 && selectedNodes.every((n) => n.is_favorite);

  // Clear the selection when switching folders
  const place = `${p.folderId}|${p.crumbs.map((c) => c.label).join("/")}`;
  /** The folder shown before this one (the folder id is unknown for a moment while the next folder loads) */
  const lastFolder = useRef(p.folderId);
  const cameFrom = useRef<string | undefined>(undefined);
  useEffect(() => {
    setSelected(new Set());
    setAnchor(null);
    if (p.folderId && p.folderId !== lastFolder.current) {
      cameFrom.current = lastFolder.current;
      lastFolder.current = p.folderId;
    }
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the folder id is part of the place
  }, [place]);
  // A span is a part of the folder in one order: sorted another way, it would be other items
  const order = `${p.sort?.key}|${p.sort?.order}`;
  useEffect(() => setSpan(null), [order]);
  // Going up (or back) to the folder holding the one shown before selects that one, like File Explorer; the focus
  // goes to it too when it was lost with the list (Alt+Up, Backspace), not when it's on a button that was clicked
  const locating = useRef<string | null>(null);
  useEffect(() => {
    const id = cameFrom.current;
    if (!id) return;
    if (!p.items.some((n) => n.id === id)) {
      // A large folder: go to where it is; it is selected once its part has loaded
      if (list && !list.complete && list.total >= 0 && locating.current !== id) {
        locating.current = id;
        void list
          .locate(id)
          .then((at) => (at === null ? (cameFrom.current = undefined) : listNav.current?.scrollTo(at)))
          .catch(() => undefined);
      }
      return;
    }
    cameFrom.current = undefined;
    setSelected(new Set([id]));
    setAnchor(id);
    const focus = !document.activeElement || document.activeElement === document.body;
    setTimeout(() => listNav.current?.show(id, focus));
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- runs as items load; the list is read as it is then
  }, [p.items]);
  /** An item just made in a part of a large folder not loaded: renamed once that part has loaded (actions' createNew) */
  const renameWhenShown = useRef<{ id: string; name: string } | null>(null);
  useEffect(() => {
    const wanted = renameWhenShown.current;
    if (!wanted || !p.items.some((n) => n.id === wanted.id)) return;
    renameWhenShown.current = null;
    setSelected(new Set([wanted.id]));
    setAnchor(wanted.id);
    setDialog({ t: "rename", node: wanted });
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- when items load
  }, [p.items]);
  /** A folder opened with Enter: once its items are shown, the focus goes to the first so the arrows carry on there */
  const enteredByKey = useRef<string | null>(null);
  useEffect(() => {
    const id = enteredByKey.current;
    if (!id || p.loading || p.folderId !== id) return;
    enteredByKey.current = null;
    // An empty folder has nothing to focus: Backspace and Alt+Up still work from the page
    setTimeout(() => focusIsFree(document.activeElement) && listNav.current?.focusStart());
  }, [p.items, p.loading, p.folderId]);

  return {
    me,
    caps,
    qc,
    navigate,
    tabs,
    clip,
    fileInput,
    dirInput,
    listNav,
    area,
    behaviour,
    undoLabel,
    enteredByKey,
    renameWhenShown,
    canCreate,
    canUpload,
    selectedNodes,
    selectedIds,
    picked,
    count,
    total,
    span,
    choose,
    selectAll,
    invert,
    canInvert,
    whole,
    single,
    allFavorite,
    view,
    setView,
    groupBy,
    setGroupBy,
    selected,
    setSelected,
    anchor,
    setAnchor,
    dialog,
    setDialog,
    dragging,
    setDragging,
    showCheckboxes,
    setShowCheckboxes,
    detailsOpen,
    setDetailsOpen,
  };
}

export type ExplorerState = ReturnType<typeof useExplorerState>;
