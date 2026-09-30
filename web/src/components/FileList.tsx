import { Fragment, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { Resizer } from "@/components/Resizer";
import type { FileSource, Node, SortKey, SortOrder } from "@/api";
import { typeLabel } from "@/components/FileIcon";
import type { Box, MeasureHits } from "@/components/useMarquee";
import { cn } from "@/lib/utils";
import { useMediaQuery } from "@/lib/focus";
import { COLUMN_WIDTH, MAX_COLUMN, MIN_COLUMN, MIN_NAME, columnShown, groupItems, columnsToHide, pageRows, setColumnWidth, useColumnPrefs, type ColumnId, type GroupBy } from "@/lib/listView";
import { carriesFiles, carriesItems, dropEffect, droppedIds, startDrag } from "@/lib/dnd";
import { t } from "@/lib/i18n";
import { findByPrefix, wantsCopy } from "@/lib/keys";
import { inSpan, spanCount, type ListSpan } from "@/lib/span";
import { type ViewMode, type Item, ROW, HEAD, GROUP_ROW, TILED, PAD, PHONE_GRID_W, GROUP_H, scrollParent, offsetIn, touching, evenLayout, groupedLayout } from "@/components/fileList/layout";
import { th, Head, type Handlers, type RowProps, COLUMN_CLASS, ListRow, Tile, GroupHeading, PlaceholderRow } from "@/components/fileList/rows";
import { listColumns, ColumnChoices } from "@/components/fileList/columns";

/** What the explorer's keyboard handling asks of the list */
export interface ListNav {
  /** A letter typed: go to the next item whose name starts with the letters typed in the last second */
  typeAhead(key: string): void;
  /** Scroll an item into view, and focus it */
  show(id: string, focus: boolean): void;
  /** Focus the item Tab reaches (the first selected, else the first item); false when there are no items */
  focusStart(): boolean;
  /** Scroll to a position (an item not loaded yet, whose part then loads) */
  scrollTo(index: number): void;
}

export interface FileListProps {
  /** In the order shown; a large folder has only some loaded (lib/windows): the others are empty until they load */
  items: readonly (Item | undefined)[];
  /** The positions in view (a large folder loads them) */
  onShow?(first: number, last: number): void;
  view: ViewMode;
  source: FileSource;
  selected: Set<string>;
  /** Selected besides `selected`: items from one to another, or all, in a large folder not all loaded (lib/span) */
  span?: ListSpan | null;
  /** A new selection; `span` is the span it has (none when left out) */
  onSelect(selected: Set<string>, anchor?: string, span?: ListSpan | null): void;
  /** Ctrl+A or the header's check box: select everything (the list may not have every item) */
  onSelectAll?(): void;
  /** Anchor of a range selection (Shift); stored by item id so it stays correct when the list changes */
  anchor: string | null;
  /** `byKey`: opened with Enter, so the keyboard carries on in what opens */
  onOpen(n: Item, byKey?: boolean): void;
  /** Middle click: open in a new tab */
  onOpenInNewTab?(n: Item): void;
  sort?: { key: SortKey; order: SortOrder };
  onSort?(key: SortKey): void;
  showLocation?: boolean;
  showOwner?: boolean;
  /** Show item checkboxes (in both views) */
  showCheckboxes?: boolean;
  /** A long press on an item also opens the context menu (on phones the selection bar takes its place) */
  touchMenu?: boolean;
  /** Cut items (not yet pasted) are shown semi-transparent */
  dimmed?: Set<string>;
  dateLabel?: string;
  dateOf?(n: Item): number;
  /** One more text column after the size (list view), e.g. who deleted each item in the trash */
  extraColumn?: { label: string; value(n: Item): string };
  /** Items in groups, each with a heading (see lib/listView.ts) */
  groupBy?: GroupBy;
  /** The groups the other way round (the list is sorted the other way by what it's grouped by) */
  groupReversed?: boolean;
  /** Allow dragging items into folders to move them (or copy them, with Ctrl) */
  onDropInto?(ids: string[], folder: Node, copy: boolean): void;
  /** Allow dropping files from the computer on folders to upload them there */
  onUploadInto?(dt: DataTransfer, folder: Node): void;
  empty?: ReactNode;
  /** Item being renamed inline */
  renamingId?: string | null;
  onRename?(item: Item, name: string): Promise<void>;
  onRenameDone?(): void;
  /** Accessible name of the list (defaults to "Items") */
  label?: string;
  /** Receives the list's geometry for marquee selection (useMarquee's `measure`): only the rows in view are rendered */
  measureRef?: RefObject<MeasureHits | null>;
  /** Receives the list's keyboard helpers */
  navRef?: RefObject<ListNav | null>;
}

const coarse = typeof window !== "undefined" && window.matchMedia("(pointer: coarse)").matches;
/** How long a finger rests on an item to select it, and how far it may move meanwhile */
const LONG_PRESS_MS = 500;
const LONG_PRESS_SLOP = 10;

export function FileList(p: FileListProps) {
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  /** The item with the keyboard focus: always rendered, so the focus isn't lost when it scrolls out of view */
  const [focusId, setFocusId] = useState<string | null>(null);
  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  /** Where the first row starts in the scroll container's content, the list's width, and the width it has in view */
  const [geo, setGeo] = useState({ top: 0, width: 0, room: 0 });
  const root = useRef<HTMLElement | null>(null);
  const head = useRef<HTMLTableSectionElement>(null);
  const pendingFocus = useRef<string | null>(null);
  const view = p.view;
  // Anything but an icon view (also a view saved by a later version) is Details
  const tile: (typeof TILED)[keyof typeof TILED] | null = view === "list" ? null : (TILED[view] ?? null);
  const grid = !!tile;
  const span = p.span ?? null;
  const selecting = p.selected.size > 0 || !!span;
  const prefs = useColumnPrefs();
  // Column widths apply from tablet width up; narrower, only the name and size show
  const wide = useMediaQuery("(min-width: 48rem)");
  const large = useMediaQuery("(min-width: 64rem)");

  // Where each loaded item is; a large folder has only some loaded
  const loaded = useMemo(() => {
    const at = new Map<string, number>();
    p.items.forEach((x, i) => x && at.set(x.id, i));
    return at;
  }, [p.items]);
  const complete = loaded.size === p.items.length;
  // Grouped, items are shown group by group: that's their order for the keyboard and Shift ranges too (only when every
  // item is loaded: the explorer loads them all when grouping)
  const dateOf = p.dateOf ?? ((x: Item) => x.updated_at);
  const groups = useMemo(
    () =>
      complete
        ? groupItems<Item>(p.items as Item[], p.groupBy ?? "none", { dateOf, typeOf: typeLabel, now: new Date(), reversed: p.groupReversed })
        : null,
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the date function is a new one on every render
    [p.items, p.groupBy, p.groupReversed, complete],
  );
  const items = useMemo(() => (groups ? groups.flatMap((g) => g.items) : p.items), [groups, p.items]);
  const n = items.length;

  const indexOf = useMemo(() => (groups ? new Map(items.map((x, i) => [x!.id, i])) : loaded), [groups, items, loaded]);
  const isSelected = (index: number, id: string) => p.selected.has(id) || inSpan(span, index, id);
  // The first selected item holds the Tab stop: found once per selection, not for every row
  const firstSelected = useMemo(() => {
    let first = span ? (span.from?.index ?? 0) : -1;
    for (const id of p.selected) {
      const i = indexOf.get(id);
      if (i !== undefined && (first < 0 || i < first)) first = i;
    }
    return Math.min(first, n - 1);
  }, [p.selected, indexOf, span, n]);
  const allSelected = useMemo(
    () =>
      n > 0 &&
      (span && !span.from && !span.to ? span.except.size === 0 : complete && p.selected.size >= n && items.every((x) => p.selected.has(x!.id))),
    [items, p.selected, n, span, complete],
  );

  // Phones: large icons a little narrower, three to a row rather than two with wide gaps
  const minTileW = tile && view === "grid" && !wide ? PHONE_GRID_W : tile?.w;
  const cols = tile ? Math.max(1, Math.floor((geo.width - 2 * PAD + tile.gap) / (minTileW! + tile.gap))) : 1;
  // The rows: in each group, its heading and then its items, `cols` to a row
  const layout = useMemo(
    () => (groups ? groupedLayout(groups, cols, tile ? tile.h + tile.gap : ROW, tile ? GROUP_H : GROUP_ROW) : evenLayout(n, cols, tile ? tile.h + tile.gap : ROW)),
    [groups, n, cols, tile],
  );
  const rowOf = (i: number) => layout.rowOf(i);
  const tabStop = firstSelected >= 0 ? firstSelected : 0;
  // Rows rendered even out of view: the Tab stop, the focused item and the one being renamed
  const pinned = [tabStop, focusId === null ? undefined : indexOf.get(focusId), p.renamingId ? indexOf.get(p.renamingId) : undefined]
    .filter((i): i is number => i !== undefined && i < n)
    .map(rowOf);

  // Row sizes are cached per key: new keys whenever the rows change
  // oxlint-disable-next-line react-hooks/exhaustive-deps -- the layout is what invalidates the keys
  const rowKey = useCallback((i: number) => i, [layout]);
  const v = useVirtualizer({
    count: layout.count,
    getScrollElement: () => scroller,
    estimateSize: (i) => layout.row(i).size,
    getItemKey: rowKey,
    overscan: grid ? 2 : 12,
    scrollMargin: geo.top,
    // Keep rows scrolled to by the keyboard clear of the sticky column headers
    scrollPaddingStart: grid ? PAD : HEAD,
    rangeExtractor: (range) => [...new Set([...defaultRangeExtractor(range), ...pinned])].sort((a, b) => a - b),
    initialRect: { width: 0, height: typeof window === "undefined" ? 800 : window.innerHeight },
  });

  // Find the scroll container, and where the rows start in it: measured before the first paint of a view, then when the
  // list changes size (not after every render: that reads the layout on every frame of a marquee drag)
  const measureGeo = useCallback(() => {
    const el = root.current;
    if (!el) return;
    const s = scrollParent(el);
    setScroller((cur) => (cur === s ? cur : s));
    const top = offsetIn(el, s).top + (head.current ? head.current.offsetHeight : PAD);
    const width = el.clientWidth;
    const room = s === document.documentElement ? window.innerWidth : s.clientWidth;
    setGeo((g) => (g.top === top && g.width === width && g.room === room ? g : { top, width, room }));
  }, []);
  const empty = n === 0;
  useLayoutEffect(measureGeo, [measureGeo, grid, empty]);
  useEffect(() => {
    const el = root.current;
    if (!el) return;
    const ro = new ResizeObserver(measureGeo);
    ro.observe(el);
    // The list can be wider than the space it has (Details view scrolling sideways): that space is watched too
    const s = scrollParent(el);
    if (s !== document.documentElement) ro.observe(s);
    return () => ro.disconnect();
  }, [measureGeo, grid, empty]);

  // Keyboard moves: focus the item once its row is rendered; a move to an item not loaded yet is made once it loads
  useLayoutEffect(() => {
    const nav = pendingNav.current;
    if (nav && items[nav.index]) {
      pendingNav.current = null;
      moveTo(nav.index, nav.mode);
    }
    const id = pendingFocus.current;
    const el = id && root.current?.querySelector<HTMLElement>(`[data-node-id="${CSS.escape(id)}"]`);
    if (el) {
      pendingFocus.current = null;
      el.focus({ preventScroll: true });
    }
  });

  // The positions in view (without the rows rendered around them), for a large folder to load them
  const range = v.range;
  const firstShown = range && layout.count ? layout.row(Math.min(range.startIndex, layout.count - 1)).start : 0;
  const lastShown = range && layout.count ? layout.row(Math.min(range.endIndex, layout.count - 1)).end - 1 : 0;
  const onShow = p.onShow;
  useEffect(() => onShow?.(firstShown, lastShown), [onShow, firstShown, lastShown]);

  // A new item is renamed right after it's created, wherever it sorts: bring it into view
  const renamingIndex = p.renamingId ? indexOf.get(p.renamingId) : undefined;
  const renamingShown = renamingIndex !== undefined;
  useEffect(() => {
    if (renamingIndex !== undefined) v.scrollToIndex(rowOf(renamingIndex));
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- only when renaming starts, not while the list reloads
  }, [p.renamingId, renamingShown]);

  const focusItem = (index: number) => {
    const item = items[index];
    v.scrollToIndex(rowOf(index));
    if (!item) return;
    setFocusId(item.id);
    const el = root.current?.querySelector<HTMLElement>(`[data-node-id="${CSS.escape(item.id)}"]`);
    if (el) el.focus({ preventScroll: true });
    else pendingFocus.current = item.id;
  };

  /**
   * The selection from the anchor to an item (Shift), with the items in `keep` (Ctrl+Shift). With items between them
   * not loaded, it is a span from one to the other (the server knows what is between them).
   */
  const rangeTo = (index: number, keep?: Set<string>): { selected: Set<string>; span: ListSpan | null } | null => {
    const anchorIndex = p.anchor === null ? -1 : (indexOf.get(p.anchor) ?? -1);
    if (anchorIndex < 0 || !items[index]) return null;
    const [lo, hi] = [Math.min(anchorIndex, index), Math.max(anchorIndex, index)];
    const next = new Set(keep);
    for (let i = lo; i <= hi; i++) {
      const item = items[i];
      if (!item) return { selected: new Set(keep), span: { from: { id: items[lo]!.id, index: lo }, to: { id: items[hi]!.id, index: hi }, except: new Set() } };
      next.add(item.id);
    }
    return { selected: next, span: null };
  };

  /** Ctrl+click or Ctrl+Space: the item in or out of the selection (in a span, it is left out of it, or back in) */
  const toggle = (index: number) => {
    const item = items[index];
    if (!item) return;
    const id = item.id;
    if (span && index >= (span.from?.index ?? 0) && index <= (span.to?.index ?? Infinity) && !p.selected.has(id)) {
      const except = new Set(span.except);
      if (except.has(id)) except.delete(id);
      else except.add(id);
      p.onSelect(p.selected, id, { ...span, except });
      return;
    }
    const next = new Set(p.selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    p.onSelect(next, id, span);
  };

  /** A keyboard move to an item: select it (or up to it, or only focus it); one not loaded yet is scrolled to, and waited for */
  const pendingNav = useRef<{ index: number; mode: "only" | "range" | "focus" } | null>(null);
  const moveTo = (index: number, mode: "only" | "range" | "focus") => {
    const item = items[index];
    if (!item) {
      pendingNav.current = { index, mode };
      v.scrollToIndex(rowOf(index));
      return;
    }
    if (mode !== "focus") {
      const range = mode === "range" ? rangeTo(index) : null;
      if (range) p.onSelect(range.selected, p.anchor!, range.span);
      else p.onSelect(new Set([item.id]), item.id, null);
    }
    focusItem(index);
  };

  /** The item below or above: in the same column of the next row of items (group headings are skipped), else the last or first item */
  const vertical = (index: number, dir: 1 | -1) => {
    const at = (r: number) => (r >= 0 && r < layout.count ? layout.row(r) : undefined);
    let r = rowOf(index) + dir;
    while (at(r)?.group) r += dir;
    const to = at(r);
    if (!to) return dir > 0 ? n - 1 : 0;
    return Math.min(to.start + index - layout.row(rowOf(index)).start, to.end - 1);
  };

  /** The item a page further down or up (PageDown, PageUp): as many rows as fit in view, less one */
  const page = (index: number, dir: 1 | -1) => {
    const view = scroller === document.documentElement || !scroller ? window.innerHeight : scroller.clientHeight;
    const rowHeight = tile ? tile.h + tile.gap : ROW;
    let at = index;
    for (let k = pageRows(view - (grid ? PAD : HEAD), rowHeight); k > 0; k--) {
      const next = vertical(at, dir);
      if (next === at) break;
      at = next;
    }
    return at;
  };

  /** Shift+F10 or the Menu key: open the context menu at the item, as right-clicking it does (browsers don't all do it for a focused row) */
  const keyMenu = useRef(0);
  const openMenu = (el: HTMLElement) => {
    const r = (el.querySelector("[data-drag-handle]") ?? el).getBoundingClientRect();
    keyMenu.current = Date.now() + 500;
    el.dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2, clientX: r.left + Math.min(r.width / 2, 100), clientY: r.top + r.height / 2 }));
    // A menu opened like a right-click leaves the focus on the row: move it into the menu, so the arrow keys go through its items
    setTimeout(() => {
      const menu = document.querySelector<HTMLElement>("[role=menu][data-open]");
      if (!menu) return;
      menu.focus();
      // Closed without doing anything that takes the focus (a dialog, the rename box): it goes back to the row, not the list around it
      const back = new MutationObserver(() => {
        if (menu.isConnected) return;
        back.disconnect();
        const at = document.activeElement;
        if (el.isConnected && (!at || at === document.body || !at.closest("[role=menu], [role=dialog], input, textarea"))) el.focus({ preventScroll: true });
      });
      back.observe(document.body, { childList: true, subtree: true });
    }, 30);
  };

  /**
   * Keyboard: arrows move the selection (Shift extends it, Ctrl moves only the focus), Space selects (toggles with Ctrl),
   * Home/End and PageUp/PageDown jump, Enter opens, Shift+F10 or the Menu key opens the context menu
   */
  const keyNav = (e: KeyboardEvent<HTMLElement>, index: number) => {
    // Keys typed in a control inside the row (its checkbox, the rename box) belong to that control
    const current = items[index];
    if (e.target !== e.currentTarget || !current || current.id === p.renamingId) return;
    if (e.key === "Enter" && !e.altKey && !e.repeat) {
      e.preventDefault();
      p.onOpen(current, true);
      return;
    }
    if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey)) {
      e.preventDefault();
      openMenu(e.currentTarget);
      return;
    }
    // Alt+arrows move around folders (handled by the address bar)
    if (e.altKey) return;
    let next: number | null = null;
    if (e.key === "ArrowDown") next = vertical(index, 1);
    else if (e.key === "ArrowUp") next = vertical(index, -1);
    else if (e.key === "ArrowRight" && grid) next = Math.min(n - 1, index + 1);
    else if (e.key === "ArrowLeft" && grid) next = Math.max(0, index - 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = n - 1;
    else if (e.key === "PageDown") next = page(index, 1);
    else if (e.key === "PageUp") next = page(index, -1);
    else if (e.key === " ") {
      e.preventDefault();
      if (e.ctrlKey || e.metaKey) toggle(index);
      else p.onSelect(new Set([current.id]), current.id, null);
      return;
    }
    if (next === null) return;
    e.preventDefault();
    // Ctrl moves the focus and leaves the selection alone, like File Explorer: Ctrl+Space then adds or removes the item.
    // An item not loaded yet (End in a large folder) is selected once its part has loaded.
    moveTo(next, (e.ctrlKey || e.metaKey) && !e.shiftKey ? "focus" : e.shiftKey ? "range" : "only");
  };

  /** Letters typed to find an item, and when the last one was typed */
  const typed = useRef({ text: "", at: 0 });
  const typeAhead = (key: string) => {
    const now = Date.now();
    const text = (now - typed.current.at < 1000 ? typed.current.text : "") + key.toLocaleLowerCase();
    typed.current = { text, at: now };
    const current = focusId ?? p.anchor;
    const at = current === null ? -1 : (indexOf.get(current) ?? -1);
    // The same letter again moves on to the next item starting with it; more letters narrow down from the current item
    const same = [...text].every((c) => c === text[0]);
    // In a large folder, among the items loaded
    const next = findByPrefix(
      Array.from({ length: n }, (_, i) => items[i]?.name ?? ""),
      same ? text[0] : text,
      same ? at : Math.max(at, 0) - 1,
    );
    if (next < 0) return;
    moveTo(next, "only");
  };
  const show = (id: string, focus: boolean) => {
    const index = indexOf.get(id);
    if (index === undefined) return;
    if (focus) focusItem(index);
    else v.scrollToIndex(rowOf(index));
  };
  const focusStart = () => {
    if (n === 0) return false;
    moveTo(tabStop, "focus");
    return true;
  };
  const scrollTo = (index: number) => index >= 0 && index < n && v.scrollToIndex(rowOf(index), { align: "center" });
  if (p.navRef) p.navRef.current = { typeAhead, show, focusStart, scrollTo };

  /** A finger resting on an item: selected once it has stayed long enough */
  const press = useRef<{ timer: ReturnType<typeof setTimeout>; x: number; y: number; done: boolean } | null>(null);
  /** The click that may follow a long press doesn't toggle the item again */
  const ignoreClick = useRef({ index: -1, until: 0 });
  const cancelPress = () => {
    if (press.current) clearTimeout(press.current.timer);
  };
  /** Long press (on every platform, iOS included): select the item, adding it when items are already selected */
  const longPress = (index: number) => {
    const id = items[index]!.id;
    if (!isSelected(index, id)) p.onSelect(selecting ? new Set(p.selected).add(id) : new Set([id]), id, selecting ? span : null);
    navigator.vibrate?.(15);
  };

  // Rows are rendered (and so used) only for items that are loaded
  const h = useRef<Handlers>(null!);
  h.current = {
    click: (e, index) => {
      const item = items[index]!;
      // Marquee selection prevents the default mousedown, so the row wouldn't get the focus: arrows continue from the clicked row
      (e.currentTarget as HTMLElement).focus({ preventScroll: true });
      if (ignoreClick.current.index === index && Date.now() < ignoreClick.current.until) return;
      if (coarse && !selecting && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
        p.onOpen(item);
        return;
      }
      const range = e.shiftKey ? rangeTo(index, e.ctrlKey || e.metaKey ? p.selected : undefined) : null;
      if (range) p.onSelect(range.selected, p.anchor!, range.span);
      else if (e.ctrlKey || e.metaKey || (coarse && selecting)) toggle(index);
      else p.onSelect(new Set([item.id]), item.id, null);
    },
    toggle,
    keyDown: keyNav,
    focused: setFocusId,
    // Right-clicking an unselected item selects only that item
    contextMenu: (e, index) => {
      // The browser's own menu event after Shift+F10 or the Menu key, which already opened the menu
      if (e.nativeEvent.isTrusted && Date.now() < keyMenu.current) {
        e.preventDefault();
        e.stopPropagation();
        return;
      }
      if (press.current) {
        // Android reports a long press as a right-click too: the long press handles it
        if (!p.touchMenu) {
          e.preventDefault();
          e.stopPropagation();
        }
        return;
      }
      const id = items[index]!.id;
      if (!isSelected(index, id)) p.onSelect(new Set([id]), id, null);
    },
    touchStart: (e, index) => {
      cancelPress();
      press.current = null;
      if (e.touches.length !== 1) return;
      // Without the menu, the context menu area mustn't start its own long press
      if (!p.touchMenu) e.stopPropagation();
      const { clientX: x, clientY: y } = e.touches[0];
      const timer = setTimeout(() => {
        if (press.current) press.current.done = true;
        longPress(index);
      }, LONG_PRESS_MS);
      press.current = { timer, x, y, done: false };
    },
    touchMove: (e) => {
      const at = press.current;
      const touch = e.touches[0];
      if (at && !at.done && touch && Math.hypot(touch.clientX - at.x, touch.clientY - at.y) > LONG_PRESS_SLOP) {
        cancelPress();
        press.current = null;
      }
    },
    touchEnd: (index) => {
      cancelPress();
      const ended = press.current;
      if (ended?.done) ignoreClick.current = { index, until: Date.now() + 800 };
      // Kept a moment for the right-click Android sends at the end of a long press
      setTimeout(() => {
        if (press.current === ended) press.current = null;
      }, 600);
    },
    dragStart: (e, index) => {
      const id = items[index]!.id;
      const chosen = isSelected(index, id);
      if (!chosen) p.onSelect(new Set([id]), id, null);
      // A span goes with the items picked one by one: where they are dropped asks the server for what it holds
      startDrag(
        e,
        chosen ? [...p.selected] : [id],
        items.filter((x): x is Item => !!x),
        chosen ? span : null,
      );
    },
    dragOver: (e, id) => {
      const items = carriesItems(e.dataTransfer) && !!p.onDropInto;
      if (!items && !(carriesFiles(e.dataTransfer) && p.onUploadInto)) return;
      e.preventDefault();
      // Files from the computer bubble on, so the list can tell they're held over a folder rather than the list itself
      if (items) e.stopPropagation();
      e.dataTransfer.dropEffect = dropEffect(e);
      setDropTarget(id);
    },
    dragLeave: (id) => setDropTarget((t) => (t === id ? null : t)),
    drop: (e, folder) => {
      setDropTarget(null);
      const ids = p.onDropInto && droppedIds(e.dataTransfer);
      if (ids) {
        e.preventDefault();
        e.stopPropagation();
        const rest = ids.filter((id) => id !== folder.id);
        if (rest.length) p.onDropInto!(rest, folder, wantsCopy(e));
      } else if (!carriesItems(e.dataTransfer) && carriesFiles(e.dataTransfer) && p.onUploadInto) {
        // Uploaded into this folder, not the one the list shows
        e.preventDefault();
        e.stopPropagation();
        p.onUploadInto(e.dataTransfer, folder);
      }
    },
    open: p.onOpen,
    // On touch screens a tap opens (or, while selecting, toggles): two quick taps mustn't open the item as well
    doubleClick: (item) => !coarse && p.onOpen(item),
    openInNewTab: p.onOpenInNewTab,
    dateOf: p.dateOf ?? ((x) => x.updated_at),
    extra: p.extraColumn?.value,
    rename: (item, name) => p.onRename!(item, name),
    renameDone: (item, byKey) => {
      p.onRenameDone?.();
      // Enter or Esc: the focus goes back to the item, as the rename box it was in goes away
      const index = indexOf.get(item.id);
      if (byKey && index !== undefined) focusItem(index);
    },
  };

  // Marquee selection finds the boxed items from the row geometry: most rows aren't in the DOM
  const measure: MeasureHits = (container) => {
    const el = root.current;
    const shown = items;
    if (!el) return () => [];
    const at = offsetIn(el, container);
    const top = at.top + (grid ? PAD : (head.current?.offsetHeight ?? HEAD));
    const gap = tile?.gap ?? 0;
    const tileW = tile ? (el.clientWidth - 2 * PAD - (cols - 1) * gap) / cols : 0;
    return function* (b: Box) {
      if (!grid && (b.x > at.left + at.width || b.x + b.w < at.left)) return;
      // Items not loaded yet can't be boxed
      for (let r = layout.rowAt(b.y - top); r < layout.count; r++) {
        const row = layout.row(r);
        const y = top + row.top;
        if (y > b.y + b.h) return;
        if (row.group || y + row.size - gap < b.y) continue;
        if (!tile) {
          const item = shown[row.start];
          if (item) yield item.id;
          continue;
        }
        const [c0, c1] = touching(at.left + PAD, tileW, tileW + gap, row.end - row.start, b.x, b.x + b.w);
        for (let c = c0; c <= c1; c++) {
          const item = shown[row.start + c];
          if (item) yield item.id;
        }
      }
    };
  };
  if (p.measureRef) p.measureRef.current = measure;

  // Details view: the columns shown (a key, so the rows only re-render when they change), and their widths
  const shownKey = listColumns(p)
    .filter((c) => columnShown(prefs, c.id))
    .map((c) => c.id)
    .join();
  const widthOf = (id: ColumnId) => prefs.widths[id] ?? COLUMN_WIDTH[id];
  // Too narrow for every column (e.g. with the details pane open): Type, then Size, make room before the list scrolls sideways
  const hide =
    wide && geo.room > 0
      ? columnsToHide(
          shownKey ? (shownKey.split(",") as ColumnId[]).filter((id) => large || id !== "location") : [],
          widthOf,
          (prefs.widths.name ?? MIN_NAME) + (p.showCheckboxes ? 30 : 0),
          geo.room,
        )
      : [];
  const fitKey = shownKey
    .split(",")
    .filter((id) => id && !hide.includes(id as ColumnId))
    .join();
  const shownIds = useMemo(() => (fitKey ? (fitKey.split(",") as ColumnId[]) : []), [fitKey]);

  if (n === 0) return <>{p.empty}</>;

  // Screen readers announce how many items are selected (the status bar isn't read out)
  const chosen = p.selected.size + (span ? spanCount(span, n) : 0);
  const status = (
    <div role="status" className="sr-only">
      {chosen > 0 ? t("{n} item selected|{n} items selected", { n: chosen }) : ""}
    </div>
  );
  const label = p.label ?? t("Items");

  // The rows to render, each with the gap before it: rows kept for the focus can be far from the ones in view
  let end = geo.top;
  const rows = v.getVirtualItems().map((r) => {
    const gap = r.start - end;
    end = r.end;
    return { row: r.index, gap };
  });
  const rest = v.getTotalSize() + geo.top - end;

  const row = (index: number): RowProps => {
    const item = items[index]!;
    return {
      item,
      index,
      h,
      selected: isSelected(index, item.id),
      tabStop: index === tabStop,
      dimmed: !!p.dimmed?.has(item.id),
      dropping: dropTarget === item.id,
      renaming: item.id === p.renamingId && !!p.onRename,
      // Not on touch screens: holding a finger on an item selects it rather than picking it up
      movable: !coarse && !!p.onDropInto && item.id !== p.renamingId,
      dropTarget: !!(p.onDropInto || p.onUploadInto) && item.kind === "folder",
    };
  };

  if (tile) {
    return (
      <>
        {status}
        <div ref={(el) => void (root.current = el)} role="listbox" aria-multiselectable aria-label={label} className="p-3">
          {rows.map(({ row: r, gap }) => {
            const at = layout.row(r);
            return (
              <Fragment key={r}>
                {gap > 0 && <div aria-hidden style={{ height: gap }} />}
                {at.group ? (
                  <div role="none" className="flex items-end px-1 pb-1.5" style={{ height: GROUP_H }}>
                    <GroupHeading group={at.group} className="h-auto" />
                  </div>
                ) : (
                  <div role="none" className="grid" style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))`, gap: tile.gap, marginBottom: tile.gap }}>
                    {Array.from({ length: at.end - at.start }, (_, k) => {
                      const i = at.start + k;
                      const item = items[i];
                      // Not loaded yet: its place, until its part loads
                      if (!item) return <div key={`#${i}`} aria-hidden className="animate-pulse rounded-md bg-muted/60" style={{ height: tile.h }} />;
                      return <Tile key={item.id} {...row(i)} view={view as Exclude<ViewMode, "list">} source={p.source} count={n} checkboxes={!!p.showCheckboxes} />;
                    })}
                  </div>
                )}
              </Fragment>
            );
          })}
          {rest > 0 && <div aria-hidden style={{ height: rest }} />}
        </div>
      </>
    );
  }

  const columns = listColumns(p).filter((c) => shownIds.includes(c.id));
  // The name takes the space left, until it's resized: then a blank column at the end takes it
  const nameWidth = wide ? prefs.widths.name : undefined;
  const filler = nameWidth !== undefined;
  // ...and the other columns can't squeeze it below its smallest width
  const minWidth =
    wide && !filler
      ? columns.filter((c) => large || c.id !== "location").reduce((sum, c) => sum + widthOf(c.id), MIN_NAME + (p.showCheckboxes ? 30 : 0))
      : undefined;
  const cellCount = 1 + columns.length + (p.showCheckboxes ? 1 : 0) + (filler ? 1 : 0);
  const spacer = (height: number) => (
    <tr aria-hidden>
      <td colSpan={cellCount} style={{ height, padding: 0 }} />
    </tr>
  );
  const resizer = (id: ColumnId | "name", label: string) => (
    <Resizer
      width={id === "name" ? (nameWidth ?? MIN_NAME) : widthOf(id)}
      measure={(handle) => handle.parentElement!.getBoundingClientRect().width}
      onChange={(w) => setColumnWidth(id, w)}
      onReset={() => setColumnWidth(id, undefined)}
      min={id === "name" ? MIN_NAME : MIN_COLUMN}
      max={MAX_COLUMN}
      defaultWidth={id === "name" ? MIN_NAME : COLUMN_WIDTH[id]}
      edge="right"
      label={t("Resize the \"{name}\" column", { name: label })}
    />
  );

  // role="grid": screen readers only report the selected state of rows in a grid, not in a plain table
  return (
    <>
      {status}
      <table
        ref={(el) => void (root.current = el)}
        role="grid"
        aria-multiselectable
        aria-label={label}
        aria-rowcount={layout.count + 1}
        className="w-full table-fixed border-collapse text-xs whitespace-nowrap select-none"
        style={minWidth === undefined ? undefined : { minWidth }}
      >
        <thead ref={head}>
          {/* Right-click the column headers to choose the columns */}
          <ContextMenu>
            <ContextMenuTrigger render={<tr role="row" aria-rowindex={1} />}>
              {p.showCheckboxes && (
                <th role="columnheader" className={cn(th, "w-[30px] px-[7px]")}>
                  <input
                    type="checkbox"
                    className="align-middle accent-brand"
                    aria-label={t("Select all")}
                    checked={allSelected}
                    ref={(el) => {
                      if (el) el.indeterminate = selecting && !allSelected;
                    }}
                    onChange={() =>
                      allSelected ? p.onSelect(new Set(), undefined, null) : p.onSelectAll ? p.onSelectAll() : p.onSelect(new Set(indexOf.keys()), undefined, null)
                    }
                  />
                </th>
              )}
              <Head sort={p.sort} onSort={p.onSort} k="name" label={t("Name")} className="pl-3" width={nameWidth} resize={resizer("name", t("Name"))} />
              {columns.map((c) => (
                <Head
                  key={c.id}
                  sort={p.sort}
                  onSort={p.onSort}
                  k={c.sort}
                  label={c.label}
                  className={COLUMN_CLASS[c.id]}
                  width={widthOf(c.id)}
                  resize={resizer(c.id, c.label)}
                />
              ))}
              {filler && <th aria-hidden className={th} />}
            </ContextMenuTrigger>
            <ContextMenuContent>
              <ColumnChoices columns={listColumns(p)} />
            </ContextMenuContent>
          </ContextMenu>
        </thead>
        <tbody>
          {rows.map(({ row: r, gap }) => {
            const at = layout.row(r);
            const item = items[at.start];
            return (
              <Fragment key={at.group ? `group:${at.group.key}` : (item?.id ?? `#${at.start}`)}>
                {gap > 0 && spacer(gap)}
                {at.group ? (
                  <tr role="row" aria-rowindex={r + 2}>
                    <td role="gridcell" colSpan={cellCount} className="px-3 pt-2" style={{ height: GROUP_ROW }}>
                      <GroupHeading group={at.group} />
                    </td>
                  </tr>
                ) : item ? (
                  <ListRow {...row(at.start)} checkboxes={!!p.showCheckboxes} columns={shownIds} filler={filler} ariaRow={r + 2} />
                ) : (
                  <PlaceholderRow cells={cellCount} ariaRow={r + 2} />
                )}
              </Fragment>
            );
          })}
          {rest > 0 && spacer(rest)}
        </tbody>
      </table>
    </>
  );
}
