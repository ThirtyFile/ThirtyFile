import {
  Fragment,
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type DragEvent,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
  type RefObject,
  type FocusEvent,
  type TouchEvent,
} from "react";
import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";
import { ChevronDownIcon, ChevronUpIcon, StarIcon } from "lucide-react";
import type { FileSource, Node, SortKey, SortOrder } from "@/api";
import { FileIcon, canBrowserThumbnail, canThumbnail, typeLabel, typeTitle } from "@/components/FileIcon";
import { browserThumb, knownThumb } from "@/lib/thumbs";
import type { Box, MeasureHits } from "@/components/useMarquee";
import { cn, formatWinDate, formatWinSize } from "@/lib/utils";
import { InlineRename } from "@/components/InlineRename";
import { carriesFiles, carriesItems, dropEffect, droppedIds, startDrag } from "@/lib/dnd";
import { t, tc } from "@/lib/i18n";
import { findByPrefix, wantsCopy } from "@/lib/keys";

export type ViewMode = "list" | "grid";

/** What the explorer's keyboard handling asks of the list */
export interface ListNav {
  /** A letter typed: go to the next item whose name starts with the letters typed in the last second */
  typeAhead(key: string): void;
}

type Item = Node & { location?: string };

export interface FileListProps {
  items: Item[];
  view: ViewMode;
  source: FileSource;
  selected: Set<string>;
  onSelect(selected: Set<string>, anchor?: string): void;
  /** Anchor of a range selection (Shift); stored by item id so it stays correct when the list changes */
  anchor: string | null;
  onOpen(n: Item): void;
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

/** Details view: height of a row (its cells are h-7) and of the column headers */
const ROW = 28;
const HEAD = 30;
/** Icon view: tiles of one size (at least TILE_W wide), so rows are placed and hit-tested by their index */
const TILE_W = 116;
const TILE_H = 148;
const GAP = 8;
const PAD = 12;

export function Thumb({ node, source, className, iconClass }: { node: Node; source: FileSource; className?: string; iconClass?: string }) {
  const [failed, setFailed] = useState(false);
  if (canBrowserThumbnail(node)) return <BrowserThumb node={node} source={source} className={className} iconClass={iconClass} />;
  if (canThumbnail(node) && !failed) {
    return (
      <img
        src={source.thumbUrl(node)}
        loading="lazy"
        draggable={false}
        onError={() => setFailed(true)}
        className={cn("object-contain", className)}
        alt=""
      />
    );
  }
  return <FileIcon node={node} className={iconClass} />;
}

/** A PDF's or video's thumbnail: the server's, or made here once the item is on screen (lib/thumbs.ts); the icon until then */
function BrowserThumb({ node, source, className, iconClass }: { node: Node; source: FileSource; className?: string; iconClass?: string }) {
  const [url, setUrl] = useState<string | null | undefined>(() => knownThumb(node, source));
  const box = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    const known = knownThumb(node, source);
    setUrl(known);
    if (known !== undefined || !box.current) return;
    let job: ReturnType<typeof browserThumb> | null = null;
    let cancelled = false;
    const seen = new IntersectionObserver(
      (entries) => {
        if (job || !entries.some((e) => e.isIntersecting)) return;
        job = browserThumb(node, source);
        void job.promise.then((u) => !cancelled && setUrl(u));
      },
      { rootMargin: "200px" },
    );
    seen.observe(box.current);
    return () => {
      cancelled = true;
      seen.disconnect();
      job?.release();
    };
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the node object is new after every refresh of the list: its id and date say when the file changed
  }, [node.id, node.updated_at, source]);
  if (url) return <img src={url} draggable={false} onError={() => setUrl(null)} className={cn("object-contain", className)} alt="" />;
  return (
    <span ref={box} className="inline-flex shrink-0">
      <FileIcon node={node} className={iconClass} />
    </span>
  );
}

const th =
  "group/th relative sticky top-0 z-[1] h-[30px] border-b bg-background px-2 text-left font-normal text-muted-foreground after:absolute after:top-1.5 after:right-0 after:bottom-1.5 after:w-px after:bg-border last:after:hidden";

/** Sortable column header (defined at module level: defining it inside the component would rebuild the whole header row on every render) */
function Head({
  k,
  label,
  className,
  sort,
  onSort,
}: {
  k?: SortKey;
  label: string;
  className?: string;
  sort: FileListProps["sort"];
  onSort: FileListProps["onSort"];
}) {
  const active = !!k && sort?.key === k;
  return (
    <th
      role="columnheader"
      className={cn(th, k && onSort && "hover:bg-muted", className)}
      aria-sort={active ? (sort!.order === "asc" ? "ascending" : "descending") : undefined}
    >
      {/* Windows 11 shows the sort direction arrow above the column header */}
      {active &&
        (sort!.order === "asc" ? (
          <ChevronUpIcon className="absolute -top-0.5 left-1/2 size-3 -translate-x-1/2" />
        ) : (
          <ChevronDownIcon className="absolute -top-0.5 left-1/2 size-3 -translate-x-1/2" />
        ))}
      {k && onSort ? (
        <button type="button" onClick={() => onSort(k)} className="flex h-full w-full items-center hover:text-foreground">
          {label}
        </button>
      ) : (
        label
      )}
    </th>
  );
}

/** What rows do when used; rows are memoised, so they reach the list's current state through a ref */
interface Handlers {
  click(e: MouseEvent, index: number): void;
  toggle(index: number): void;
  keyDown(e: KeyboardEvent<HTMLElement>, index: number): void;
  focused(id: string): void;
  contextMenu(e: MouseEvent, index: number): void;
  touchStart(e: TouchEvent, index: number): void;
  touchMove(e: TouchEvent): void;
  touchEnd(index: number): void;
  dragStart(e: DragEvent, index: number): void;
  dragOver(e: DragEvent, id: string): void;
  dragLeave(id: string): void;
  drop(e: DragEvent, folder: Item): void;
  open(n: Item): void;
  doubleClick(n: Item): void;
  openInNewTab?(n: Item): void;
  dateOf(n: Item): number;
  extra?(n: Item): string;
  rename(item: Item, name: string): Promise<void>;
  renameDone(): void;
}
type HandlersRef = RefObject<Handlers>;

/** The state a row shows; a row re-renders only when one of these changes */
interface RowProps {
  item: Item;
  index: number;
  h: HandlersRef;
  selected: boolean;
  /** Reached with Tab (the first selected item, else the first item); arrows move between the others */
  tabStop: boolean;
  dimmed: boolean;
  dropping: boolean;
  renaming: boolean;
  /** Drag to move or copy (turned off while renaming) */
  movable: boolean;
  /** Folders take dropped items and files */
  dropTarget: boolean;
}

function rowProps({ item, index, h, selected, tabStop, dropTarget }: RowProps) {
  return {
    "data-node-id": item.id,
    "aria-selected": selected,
    tabIndex: tabStop ? 0 : -1,
    onKeyDown: (e: KeyboardEvent<HTMLElement>) => h.current.keyDown(e, index),
    onFocus: (e: FocusEvent) => e.target === e.currentTarget && h.current.focused(item.id),
    onClick: (e: MouseEvent) => h.current.click(e, index),
    onDoubleClick: () => h.current.doubleClick(item),
    onMouseDown: (e: MouseEvent) => {
      if (e.button === 1 && h.current.openInNewTab) e.preventDefault();
    },
    onAuxClick: (e: MouseEvent) => {
      if (e.button === 1 && h.current.openInNewTab) {
        e.preventDefault();
        h.current.openInNewTab(item);
      }
    },
    onContextMenu: (e: MouseEvent) => h.current.contextMenu(e, index),
    onTouchStart: (e: TouchEvent) => h.current.touchStart(e, index),
    onTouchMove: (e: TouchEvent) => h.current.touchMove(e),
    onTouchEnd: () => h.current.touchEnd(index),
    onTouchCancel: () => h.current.touchEnd(index),
    ...(dropTarget
      ? {
          "data-drop-folder": true,
          onDragOver: (e: DragEvent) => h.current.dragOver(e, item.id),
          onDragLeave: () => h.current.dragLeave(item.id),
          onDrop: (e: DragEvent) => h.current.drop(e, item),
        }
      : {}),
  };
}

// Drag to move or copy: icon view drags the whole item; list view drags only the name, dragging from other columns marquee-selects (like Windows)
const dragHandle = ({ h, index, movable }: RowProps) => ({
  draggable: movable,
  onDragStart: (e: DragEvent) => h.current.dragStart(e, index),
});

function renameBox(r: RowProps, multiline?: boolean) {
  return (
    <InlineRename
      initial={r.item.name}
      multiline={multiline}
      selectAll={r.item.kind === "folder"}
      onSubmit={(name) => r.h.current.rename(r.item, name)}
      onDone={() => r.h.current.renameDone()}
      className={multiline ? "mt-1.5" : undefined}
    />
  );
}

const td = "h-7 px-2 truncate";

const ListRow = memo(function ListRow(r: RowProps & { checkboxes: boolean; location: boolean; owner: boolean; extra: boolean }) {
  const { item } = r;
  return (
    <tr
      {...rowProps(r)}
      role="row"
      aria-rowindex={r.index + 2}
      className={cn(
        "cursor-default outline-none hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset aria-selected:bg-selection aria-selected:text-accent-foreground aria-selected:shadow-[inset_3px_0_0_var(--color-brand)]",
        r.dropping && "bg-brand/15",
        r.dimmed && "opacity-50",
      )}
    >
      {r.checkboxes && (
        <td role="gridcell" className={cn(td, "px-[7px]")}>
          <input
            type="checkbox"
            className="align-middle accent-brand"
            aria-label={t("Select {name}", { name: item.name })}
            checked={r.selected}
            onClick={(e) => e.stopPropagation()}
            onChange={() => r.h.current.toggle(r.index)}
          />
        </td>
      )}
      <td role="gridcell" className={cn(td, "pl-3")}>
        <div data-drag-handle {...dragHandle(r)} className="flex w-fit max-w-full min-w-0 items-center gap-2" title={item.name}>
          <FileIcon node={item} className="size-4 shrink-0" />
          {r.renaming ? (
            renameBox(r)
          ) : (
            <>
              <span className="truncate">{item.name}</span>
              {item.is_favorite && <StarIcon className="size-[11px] shrink-0 fill-amber-400 text-amber-400" aria-label={tc("state", "Favorite")} />}
            </>
          )}
        </div>
      </td>
      {r.location && (
        <td role="gridcell" className={cn(td, "text-muted-foreground max-lg:hidden")} title={item.location}>
          {item.location}
        </td>
      )}
      <td role="gridcell" className={cn(td, "text-muted-foreground max-md:hidden")}>{formatWinDate(r.h.current.dateOf(item))}</td>
      <td role="gridcell" className={cn(td, "text-muted-foreground max-md:hidden")} title={typeTitle(item)}>
        {typeLabel(item)}
      </td>
      <td role="gridcell" className={cn(td, "pr-3 text-right text-muted-foreground tabular-nums")}>{item.kind === "folder" ? "" : formatWinSize(item.size)}</td>
      {r.owner && <td role="gridcell" className={cn(td, "text-muted-foreground max-md:hidden")}>{item.owner_name}</td>}
      {r.extra && <td role="gridcell" className={cn(td, "text-muted-foreground max-md:hidden")}>{r.h.current.extra?.(item)}</td>}
    </tr>
  );
});

const Tile = memo(function Tile(r: RowProps & { source: FileSource; count: number; checkboxes: boolean }) {
  const { item } = r;
  return (
    <div
      {...rowProps(r)}
      {...dragHandle(r)}
      role="option"
      aria-posinset={r.index + 1}
      aria-setsize={r.count}
      title={item.name}
      className={cn(
        "relative min-w-0 rounded-md border border-transparent p-2 text-center outline-none select-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring aria-selected:border-brand aria-selected:bg-selection",
        r.dropping && "border-brand bg-brand/10",
        r.dimmed && "opacity-50",
        r.renaming && "z-[1]",
      )}
      style={{ height: TILE_H }}
    >
      <div className="flex h-[88px] items-center justify-center">
        <Thumb node={item} source={r.source} className="max-h-[88px] w-full rounded" iconClass="size-[42px]" />
      </div>
      {r.renaming ? renameBox(r, true) : <span className="line-clamp-2 pt-1.5 text-xs leading-[18px] break-all">{item.name}</span>}
      {item.is_favorite && <StarIcon className="absolute top-1.5 right-1.5 size-3 fill-amber-400 text-amber-400" />}
      {/* Space selects with the keyboard, so the check box isn't a Tab stop of its own */}
      {r.checkboxes && (
        <input
          type="checkbox"
          tabIndex={-1}
          className="absolute top-1.5 left-1.5 size-4 accent-brand"
          aria-label={t("Select {name}", { name: item.name })}
          checked={r.selected}
          onClick={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          onChange={() => r.h.current.toggle(r.index)}
        />
      )}
    </div>
  );
});

/** The nearest scrolling ancestor, which the list is virtualised against */
function scrollParent(el: HTMLElement): HTMLElement {
  for (let p = el.parentElement; p; p = p.parentElement) {
    const o = getComputedStyle(p).overflowY;
    if (o === "auto" || o === "scroll") return p;
  }
  return document.documentElement;
}

/** An element's position in a scroll container's content */
function offsetIn(el: Element, container: HTMLElement) {
  const r = el.getBoundingClientRect();
  const c = container === document.documentElement ? { top: 0, left: 0 } : container.getBoundingClientRect();
  return { top: r.top - c.top + container.scrollTop, left: r.left - c.left + container.scrollLeft, width: r.width };
}

/** First and last of `count` boxes (`size` long, `stride` apart from `start`) that touch the span lo–hi */
function touching(start: number, size: number, stride: number, count: number, lo: number, hi: number): [number, number] {
  return [Math.max(0, Math.ceil((lo - start - size) / stride)), Math.min(count - 1, Math.floor((hi - start) / stride))];
}

export function FileList(p: FileListProps) {
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  /** The item with the keyboard focus: always rendered, so the focus isn't lost when it scrolls out of view */
  const [focusId, setFocusId] = useState<string | null>(null);
  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  /** Where the first row starts in the scroll container's content, and the list's width */
  const [geo, setGeo] = useState({ top: 0, width: 0 });
  const root = useRef<HTMLElement | null>(null);
  const head = useRef<HTMLTableSectionElement>(null);
  const pendingFocus = useRef<string | null>(null);
  const grid = p.view === "grid";
  const n = p.items.length;
  const selecting = p.selected.size > 0;

  const indexOf = useMemo(() => new Map(p.items.map((x, i) => [x.id, i])), [p.items]);
  // The first selected item holds the Tab stop: found once per selection, not for every row
  const firstSelected = useMemo(() => {
    let first = -1;
    for (const id of p.selected) {
      const i = indexOf.get(id);
      if (i !== undefined && (first < 0 || i < first)) first = i;
    }
    return first;
  }, [p.selected, indexOf]);
  const allSelected = useMemo(() => n > 0 && p.selected.size >= n && p.items.every((x) => p.selected.has(x.id)), [p.items, p.selected, n]);

  const cols = grid ? Math.max(1, Math.floor((geo.width - 2 * PAD + GAP) / (TILE_W + GAP))) : 1;
  const stride = grid ? TILE_H + GAP : ROW;
  const rowOf = (i: number) => Math.floor(i / cols);
  const tabStop = firstSelected >= 0 ? firstSelected : 0;
  // Rows rendered even out of view: the Tab stop, the focused item and the one being renamed
  const pinned = [tabStop, focusId === null ? undefined : indexOf.get(focusId), p.renamingId ? indexOf.get(p.renamingId) : undefined]
    .filter((i): i is number => i !== undefined && i < n)
    .map(rowOf);

  // Row sizes are cached per key: new keys when the view changes
  const rowKey = useCallback((i: number) => (grid ? -1 - i : i), [grid]);
  const v = useVirtualizer({
    count: Math.ceil(n / cols),
    getScrollElement: () => scroller,
    estimateSize: () => stride,
    getItemKey: rowKey,
    overscan: grid ? 2 : 12,
    scrollMargin: geo.top,
    // Keep rows scrolled to by the keyboard clear of the sticky column headers
    scrollPaddingStart: grid ? PAD : HEAD,
    rangeExtractor: (range) => [...new Set([...defaultRangeExtractor(range), ...pinned])].sort((a, b) => a - b),
    initialRect: { width: 0, height: typeof window === "undefined" ? 800 : window.innerHeight },
  });

  // Find the scroll container, and where the rows start in it (measured after every render and on resize)
  const measureGeo = useCallback(() => {
    const el = root.current;
    if (!el) return;
    const s = scrollParent(el);
    setScroller((cur) => (cur === s ? cur : s));
    const top = offsetIn(el, s).top + (head.current ? head.current.offsetHeight : PAD);
    const width = el.clientWidth;
    setGeo((g) => (g.top === top && g.width === width ? g : { top, width }));
  }, []);
  useLayoutEffect(measureGeo);
  const empty = n === 0;
  useEffect(() => {
    const el = root.current;
    if (!el) return;
    const ro = new ResizeObserver(measureGeo);
    ro.observe(el);
    return () => ro.disconnect();
  }, [measureGeo, grid, empty]);

  // Keyboard moves: focus the item once its row is rendered
  useLayoutEffect(() => {
    const id = pendingFocus.current;
    const el = id && root.current?.querySelector<HTMLElement>(`[data-node-id="${CSS.escape(id)}"]`);
    if (el) {
      pendingFocus.current = null;
      el.focus({ preventScroll: true });
    }
  });

  // A new item is renamed right after it's created, wherever it sorts: bring it into view
  const renamingIndex = p.renamingId ? indexOf.get(p.renamingId) : undefined;
  const renamingShown = renamingIndex !== undefined;
  useEffect(() => {
    if (renamingIndex !== undefined) v.scrollToIndex(Math.floor(renamingIndex / cols));
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- only when renaming starts, not while the list reloads
  }, [p.renamingId, renamingShown]);

  const focusItem = (index: number) => {
    const id = p.items[index].id;
    v.scrollToIndex(rowOf(index));
    setFocusId(id);
    const el = root.current?.querySelector<HTMLElement>(`[data-node-id="${CSS.escape(id)}"]`);
    if (el) el.focus({ preventScroll: true });
    else pendingFocus.current = id;
  };

  const rangeTo = (index: number, keep?: Set<string>) => {
    const anchorIndex = p.anchor === null ? -1 : (indexOf.get(p.anchor) ?? -1);
    if (anchorIndex < 0) return null;
    const next = new Set(keep);
    for (let i = Math.min(anchorIndex, index); i <= Math.max(anchorIndex, index); i++) next.add(p.items[i].id);
    return next;
  };

  const toggle = (index: number) => {
    const id = p.items[index].id;
    const next = new Set(p.selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    p.onSelect(next, id);
  };

  /** Keyboard: arrows move the selection (Shift extends it), Space selects (toggles with Ctrl), Home/End jump, Enter opens */
  const keyNav = (e: KeyboardEvent<HTMLElement>, index: number) => {
    // Keys typed in a control inside the row (its checkbox, the rename box) belong to that control
    if (e.target !== e.currentTarget || p.items[index].id === p.renamingId) return;
    if (e.key === "Enter" && !e.altKey && !e.repeat) {
      e.preventDefault();
      p.onOpen(p.items[index]);
      return;
    }
    // Alt+arrows move around folders (handled by the address bar)
    if (e.altKey) return;
    let next: number | null = null;
    if (e.key === "ArrowDown") next = Math.min(n - 1, index + cols);
    else if (e.key === "ArrowUp") next = Math.max(0, index - cols);
    else if (e.key === "ArrowRight" && grid) next = Math.min(n - 1, index + 1);
    else if (e.key === "ArrowLeft" && grid) next = Math.max(0, index - 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = n - 1;
    else if (e.key === " ") {
      e.preventDefault();
      if (e.ctrlKey || e.metaKey) toggle(index);
      else p.onSelect(new Set([p.items[index].id]), p.items[index].id);
      return;
    }
    if (next === null) return;
    e.preventDefault();
    const item = p.items[next];
    const range = e.shiftKey ? rangeTo(next) : null;
    if (range) p.onSelect(range, p.anchor!);
    else p.onSelect(new Set([item.id]), item.id);
    focusItem(next);
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
    const next = findByPrefix(
      p.items.map((x) => x.name),
      same ? text[0] : text,
      same ? at : Math.max(at, 0) - 1,
    );
    if (next < 0) return;
    p.onSelect(new Set([p.items[next].id]), p.items[next].id);
    focusItem(next);
  };
  if (p.navRef) p.navRef.current = { typeAhead };

  /** A finger resting on an item: selected once it has stayed long enough */
  const press = useRef<{ timer: ReturnType<typeof setTimeout>; x: number; y: number; done: boolean } | null>(null);
  /** The click that may follow a long press doesn't toggle the item again */
  const ignoreClick = useRef({ index: -1, until: 0 });
  const cancelPress = () => {
    if (press.current) clearTimeout(press.current.timer);
  };
  /** Long press (on every platform, iOS included): select the item, adding it when items are already selected */
  const longPress = (index: number) => {
    const id = p.items[index].id;
    if (!p.selected.has(id)) p.onSelect(selecting ? new Set(p.selected).add(id) : new Set([id]), id);
    navigator.vibrate?.(15);
  };

  const h = useRef<Handlers>(null!);
  h.current = {
    click: (e, index) => {
      const item = p.items[index];
      // Marquee selection prevents the default mousedown, so the row wouldn't get the focus: arrows continue from the clicked row
      (e.currentTarget as HTMLElement).focus({ preventScroll: true });
      if (ignoreClick.current.index === index && Date.now() < ignoreClick.current.until) return;
      if (coarse && !selecting && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
        p.onOpen(item);
        return;
      }
      const range = e.shiftKey ? rangeTo(index, e.ctrlKey || e.metaKey ? p.selected : undefined) : null;
      if (range) p.onSelect(range, p.anchor!);
      else if (e.ctrlKey || e.metaKey || (coarse && selecting)) toggle(index);
      else p.onSelect(new Set([item.id]), item.id);
    },
    toggle,
    keyDown: keyNav,
    focused: setFocusId,
    // Right-clicking an unselected item selects only that item
    contextMenu: (e, index) => {
      if (press.current) {
        // Android reports a long press as a right-click too: the long press handles it
        if (!p.touchMenu) {
          e.preventDefault();
          e.stopPropagation();
        }
        return;
      }
      if (!p.selected.has(p.items[index].id)) p.onSelect(new Set([p.items[index].id]), p.items[index].id);
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
      const id = p.items[index].id;
      const ids = p.selected.has(id) ? [...p.selected] : [id];
      if (!p.selected.has(id)) p.onSelect(new Set([id]), id);
      startDrag(e, ids, p.items);
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
    renameDone: () => p.onRenameDone?.(),
  };

  // Marquee selection finds the boxed items from the row geometry: most rows aren't in the DOM
  const measure: MeasureHits = (container) => {
    const el = root.current;
    const items = p.items;
    if (!el) return () => [];
    const at = offsetIn(el, container);
    const width = el.clientWidth;
    if (!grid) {
      const top = at.top + (head.current?.offsetHeight ?? HEAD);
      return function* (b: Box) {
        if (b.x > at.left + at.width || b.x + b.w < at.left) return;
        const [first, last] = touching(top, ROW, ROW, items.length, b.y, b.y + b.h);
        for (let i = first; i <= last; i++) yield items[i].id;
      };
    }
    const perRow = Math.max(1, Math.floor((width - 2 * PAD + GAP) / (TILE_W + GAP)));
    const tileW = (width - 2 * PAD - (perRow - 1) * GAP) / perRow;
    return function* (b: Box) {
      const [r0, r1] = touching(at.top + PAD, TILE_H, TILE_H + GAP, Math.ceil(items.length / perRow), b.y, b.y + b.h);
      const [c0, c1] = touching(at.left + PAD, tileW, tileW + GAP, perRow, b.x, b.x + b.w);
      for (let r = r0; r <= r1; r++) for (let c = c0; c <= c1 && r * perRow + c < items.length; c++) yield items[r * perRow + c].id;
    };
  };
  if (p.measureRef) p.measureRef.current = measure;

  if (n === 0) return <>{p.empty}</>;

  // Screen readers announce how many items are selected (the status bar isn't read out)
  const status = (
    <div role="status" className="sr-only">
      {p.selected.size > 0 ? t("{n} item selected|{n} items selected", { n: p.selected.size }) : ""}
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
    const item = p.items[index];
    return {
      item,
      index,
      h,
      selected: p.selected.has(item.id),
      tabStop: index === tabStop,
      dimmed: !!p.dimmed?.has(item.id),
      dropping: dropTarget === item.id,
      renaming: item.id === p.renamingId && !!p.onRename,
      // Not on touch screens: holding a finger on an item selects it rather than picking it up
      movable: !coarse && !!p.onDropInto && item.id !== p.renamingId,
      dropTarget: !!(p.onDropInto || p.onUploadInto) && item.kind === "folder",
    };
  };

  if (grid) {
    return (
      <>
        {status}
        <div ref={(el) => void (root.current = el)} role="listbox" aria-multiselectable aria-label={label} className="p-3">
          {rows.map(({ row: r, gap }) => (
            <Fragment key={r}>
              {gap > 0 && <div aria-hidden style={{ height: gap }} />}
              <div role="none" className="grid gap-2" style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))`, marginBottom: GAP }}>
                {p.items.slice(r * cols, (r + 1) * cols).map((item, k) => (
                  <Tile key={item.id} {...row(r * cols + k)} source={p.source} count={n} checkboxes={!!p.showCheckboxes} />
                ))}
              </div>
            </Fragment>
          ))}
          {rest > 0 && <div aria-hidden style={{ height: rest }} />}
        </div>
      </>
    );
  }

  const columns = 4 + (p.showCheckboxes ? 1 : 0) + (p.showLocation ? 1 : 0) + (p.showOwner ? 1 : 0) + (p.extraColumn ? 1 : 0);
  const spacer = (height: number) => (
    <tr aria-hidden>
      <td colSpan={columns} style={{ height, padding: 0 }} />
    </tr>
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
        aria-rowcount={n + 1}
        className="w-full table-fixed border-collapse text-xs whitespace-nowrap select-none"
      >
        <thead ref={head}>
          <tr role="row" aria-rowindex={1}>
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
                  onChange={() => p.onSelect(allSelected ? new Set() : new Set(p.items.map((x) => x.id)))}
                />
              </th>
            )}
            <Head sort={p.sort} onSort={p.onSort} k="name" label={t("Name")} className="pl-3" />
            {p.showLocation && <Head sort={p.sort} onSort={p.onSort} label={t("Location")} className="w-[220px] max-lg:hidden" />}
            <Head sort={p.sort} onSort={p.onSort} k="updated" label={p.dateLabel ?? t("Date modified")} className="w-[170px] max-md:hidden" />
            <Head sort={p.sort} onSort={p.onSort} k="type" label={t("Type")} className="w-[120px] max-md:hidden" />
            <Head sort={p.sort} onSort={p.onSort} k="size" label={t("Size")} className="w-[100px]" />
            {p.showOwner && <Head sort={p.sort} onSort={p.onSort} label={t("Uploaded by")} className="w-[110px] max-md:hidden" />}
            {p.extraColumn && <Head sort={p.sort} onSort={p.onSort} label={p.extraColumn.label} className="w-[110px] max-md:hidden" />}
          </tr>
        </thead>
        <tbody>
          {rows.map(({ row: i, gap }) => (
            <Fragment key={p.items[i].id}>
              {gap > 0 && spacer(gap)}
              <ListRow {...row(i)} checkboxes={!!p.showCheckboxes} location={!!p.showLocation} owner={!!p.showOwner} extra={!!p.extraColumn} />
            </Fragment>
          ))}
          {rest > 0 && spacer(rest)}
        </tbody>
      </table>
    </>
  );
}
