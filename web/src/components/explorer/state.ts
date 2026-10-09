/** File explorer state: selection, view mode and grouping, dialogs, clipboard and permissions in the current folder */
import { useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";
import type { ListNav } from "@/components/FileList";
import { useClipboard } from "@/lib/clipboard";
import { capsOf } from "@/lib/drives";
import { takeArrival } from "@/lib/columns";
import { openMenuByKey } from "@/lib/contextMenus";
import { focusIsFree } from "@/lib/focus";
import { useDetailsPane } from "@/lib/detailsPane";
import type { GroupBy } from "@/lib/listView";
import { inSpan, smartListing, spanCount, type FolderSpan, type ListSpan, type Picked } from "@/lib/span";
import { usePersisted, useMe } from "@/lib/session";
import { useUndoLabel } from "@/lib/undo";
import { useStyleKit, useView } from "@/components/style";
import { useTabActions } from "@/tabs";
import type { DialogState } from "./types";
import { arrange, notLoaded, useNewItems } from "./newItems";
import { useListTree } from "@/components/fileList/listTree";
import type { ExplorerProps } from "../Explorer";

export function useExplorerState(p: ExplorerProps) {
  const me = useMe();
  // In a folder, available actions depend on the role; list pages (search, recent, etc.) mix several locations, so the server decides
  const caps = p.role ? capsOf(p.role, me, p.readOnly) : { write: me.can_write, del: me.can_delete, share: me.can_share, manage: false };
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tabs = useTabActions();
  /** The parts of the interface style in use (components/style) */
  const kit = useStyleKit();
  /** A view kept from before that the style doesn't offer (or not on this screen): the style's own */
  const [view, setView] = useView();
  const [groupBy, setGroupBy] = usePersisted<GroupBy>("tf-group", "none");
  const [selected, setChosen] = useState<Set<string>>(new Set());
  /** A large folder: what is selected without being loaded (Select all, or Shift across parts not loaded; lib/span) */
  const [span, setSpan] = useState<FolderSpan | null>(null);
  const [anchor, setAnchor] = useState<string | null>(null);
  const [dialog, setDialog] = useState<DialogState | null>(null);
  const [dragging, setDragging] = useState(false);
  const [showCheckboxes, setShowCheckboxes] = usePersisted("tf-checkboxes", false);
  const [detailsOpen, setDetailsOpen] = useDetailsPane();
  const clip = useClipboard();
  const fileInput = useRef<HTMLInputElement>(null);
  const dirInput = useRef<HTMLInputElement>(null);
  const listNav = useRef<ListNav>(null);
  /** The area around the list, whose context menu is the one for empty space */
  const area = useRef<HTMLDivElement>(null);
  /** What Ctrl+Z would take back, as a menu names it ("Undo delete"); null when nothing */
  const undoLabel = useUndoLabel();

  // New folder and paste only touch the database; uploading and creating files need to write to the storage service, so they're disabled while offline
  const canCreate = !!p.folderId && caps.write;
  const canUpload = canCreate && !p.offline;
  const list = p.list;
  const total = list ? Math.max(0, list.total) : p.items.length;
  /** What a span of a large list is selected in: the folder, or the smart folder */
  const listing = p.folderId ?? (p.smartFolder !== undefined ? smartListing(p.smartFolder) : undefined);

  // New items stay where they were made until a refresh, a change of sort or leaving the folder (explorer/newItems)
  const dialogNow = useRef(dialog);
  dialogNow.current = dialog;
  /** New items being named (from when they are made until the name box closes): they keep their place on a refresh */
  const naming = useRef(new Set<string>());
  const newItems = useNewItems(`${p.folderId}|${p.sort?.key}|${p.sort?.order}`, (n) => naming.current.has(n.item.id));
  const loadedById = useMemo(() => new Map(p.items.map((n) => [n.id, n])), [p.items]);
  const { see } = newItems;
  useEffect(() => see(loadedById), [see, loadedById]);
  /** The list's own items, by position */
  const own = useMemo(
    () => (kit.newAtEnd ? arrange(p.list?.at ?? p.items, newItems.items, loadedById) : (p.list?.at ?? p.items)),
    [kit.newAtEnd, p.list, p.items, newItems.items, loadedById],
  );
  // Folders that expand in place (the style's List view of a folder, not grouped): their items show under them
  // (components/fileList/listTree). Not in lists of several places (search results…), which may hold them already
  const tree = useListTree({
    enabled: kit.disclosure && view === "list" && groupBy === "none" && !!p.folderId,
    place: `${p.folderId}|${p.sort?.key}|${p.sort?.order}`,
    base: own,
    show: p.list?.show,
    sort: p.sort,
  });
  /** What the list shows, by position */
  const shown = tree.rows;
  /** The items known here: those loaded (in expanded folders too), and new ones that aren't (items being made aren't: nothing can be done with them yet) */
  const items = useMemo(() => {
    const more = [...notLoaded(newItems.items, loadedById), ...tree.children];
    return more.length ? [...p.items, ...more] : p.items;
  }, [p.items, newItems.items, loadedById, tree.children]);

  /** Selects items picked one by one (none: `span` is cleared too) */
  const setSelected = (next: Set<string>) => {
    setChosen(next);
    setSpan(null);
  };
  /** An item selected is now another (a new item that got its id), or none (null) */
  const replaceSelected = (from: string, to: string | null) => {
    setChosen((now) => (now.has(from) ? new Set([...now].flatMap((id) => (id !== from ? [id] : to ? [to] : []))) : now));
    setAnchor((a) => (a === from ? to : a));
  };
  /** The file list's selection: with a span of a large folder, where and in which order it was made */
  const choose = (next: Set<string>, listSpan?: ListSpan | null) => {
    setChosen(next);
    setSpan(listSpan && listing && p.sort ? { ...listSpan, folder: listing, sort: p.sort.key, order: p.sort.order, count: spanCount(listSpan, total) } : null);
  };
  /** Ctrl+A: every item; in a large folder not all loaded, that is a span of the whole folder */
  const selectAll = () => {
    if (list && !list.complete && listing && p.sort) {
      setChosen(new Set());
      setSpan({ folder: listing, sort: p.sort.key, order: p.sort.order, except: new Set(), count: total });
    } else setSelected(new Set(items.map((n) => n.id)));
  };
  /**
   * Invert selection. In a large folder not all loaded: everything but the items picked (a span of the whole folder),
   * or, from a span of the whole folder, the items it left out. A span from one item to another can't be inverted.
   */
  const whole = !!span && !span.from && !span.to;
  const canInvert = !span || whole;
  const invert = () => {
    if (whole) setSelected(new Set(span.except));
    else if (list && !list.complete && listing && p.sort) {
      setChosen(new Set());
      setSpan({ folder: listing, sort: p.sort.key, order: p.sort.order, except: new Set(selected), count: total - selected.size });
    } else setSelected(new Set(items.filter((n) => !selected.has(n.id)).map((n) => n.id)));
  };

  // The items selected that are loaded (all of them, unless a span holds items not loaded)
  const selectedNodes = useMemo(() => items.filter((n) => selected.has(n.id) || (!!span && inSpan(span, list?.index.get(n.id) ?? -1, n.id))), [items, selected, span, list]);
  /** Items picked one by one; the span's aren't all known here (see `picked`) */
  const selectedIds = span ? [...selected] : selectedNodes.map((n) => n.id);
  const count = span ? selected.size + spanCount(span, total) : selectedNodes.length;
  /** What the commands work on */
  const picked: Picked = { ids: selectedIds, span, count };
  const single = !span && selectedNodes.length === 1 ? selectedNodes[0] : null;
  const allFavorite = selectedNodes.length > 0 && selectedNodes.every((n) => n.is_favorite);

  // Clear the selection when switching folders: a folder by its id (renaming the folder shown doesn't leave it), the
  // other pages (Recent, a search) by what they show
  const place = p.folderId ?? `|${p.crumbs.map((c) => c.label).join("/")}`;
  /** The folder shown before this one (the folder id is unknown for a moment while the next folder loads) */
  const lastFolder = useRef(p.folderId);
  const cameFrom = useRef<string | undefined>(undefined);
  /** The Columns view went to another column: on arriving, its first item is selected (the folder's id), or a menu opens */
  const selectFirst = useRef<string | null>(null);
  const focusOnArrival = useRef(false);
  const menuOnArrival = useRef<{ x: number; y: number } | null>(null);
  useEffect(() => {
    setSelected(new Set());
    setAnchor(null);
    // A name box of the folder left can't be typed in any more: the shortcuts, off while it is open, come back
    setDialog((d) => (d?.t === "rename" ? null : d));
    renameWhenShown.current = null;
    // The Columns view says what to select in the folder it went to (lib/columns), in place of the folder came from
    const arrived = p.folderId ? takeArrival(p.folderId) : null;
    if (arrived) {
      cameFrom.current = arrived.select;
      selectFirst.current = arrived.first ? p.folderId! : null;
      focusOnArrival.current = !!arrived.focus;
      menuOnArrival.current = arrived.menuAt ?? null;
    } else if (p.folderId && p.folderId !== lastFolder.current) {
      cameFrom.current = lastFolder.current;
      selectFirst.current = null;
      focusOnArrival.current = false;
      menuOnArrival.current = null;
    }
    if (p.folderId) lastFolder.current = p.folderId;
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the folder id is part of the place
  }, [place]);
  // A span is a part of the folder in one order: sorted another way, it would be other items
  const order = `${p.sort?.key}|${p.sort?.order}`;
  useEffect(() => setSpan(null), [order]);
  // Going up (or back) to the folder holding the one shown before selects that one, like File Explorer; the focus
  // goes to it too when it was lost with the list (Alt+Up, Backspace), not when it's on a button that was clicked
  const locating = useRef<string | null>(null);
  useEffect(() => {
    const first = selectFirst.current;
    if (first && first === p.folderId && !p.loading) {
      selectFirst.current = null;
      const item = (p.list?.at ?? p.items)[0];
      if (item) arrive(item.id);
      return;
    }
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
    arrive(id);
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- runs as items load; the list is read as it is then
  }, [p.items, p.loading]);
  /** Selects the item arrived at, and shows it */
  const arrive = (id: string) => {
    setSelected(new Set([id]));
    setAnchor(id);
    const focus = focusOnArrival.current || !document.activeElement || document.activeElement === document.body;
    const menuAt = menuOnArrival.current;
    focusOnArrival.current = false;
    menuOnArrival.current = null;
    setTimeout(() => {
      listNav.current?.show(id, focus);
      const row = menuAt && area.current?.querySelector<HTMLElement>(`[data-node-id="${CSS.escape(id)}"]`);
      if (row) openMenuByKey(row, menuAt);
    });
  };
  /** Something is still to be selected on arriving in this folder (the folder came from, or the first item) */
  const arriving = () => !!cameFrom.current || !!selectFirst.current;
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

  /** The tree's folders expand and collapse; collapsing one with items selected inside it selects it instead */
  const listTree = tree.view && {
    ...tree.view,
    toggle: (id: string, open: boolean) => {
      if (!open) {
        const inside = new Set(tree.under(id).map((n) => n.id));
        if ([...selected].some((x) => inside.has(x))) {
          setSelected(new Set([id]));
          setAnchor(id);
        }
      }
      tree.view!.toggle(id, open);
    },
  };

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
    kit,
    newItems,
    dialogNow,
    naming,
    replaceSelected,
    shown,
    /** The positions in view: a large folder (and a large folder expanded in it) loads them */
    onShow: tree.show,
    listTree,
    items,
    undoLabel,
    enteredByKey,
    renameWhenShown,
    arriving,
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
