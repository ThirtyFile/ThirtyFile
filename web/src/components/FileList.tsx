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
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuCheckboxItem, DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Resizer } from "@/components/Resizer";
import type { FileSource, Node, SortKey, SortOrder } from "@/api";
import { FileIcon, canBrowserThumbnail, canThumbnail, typeLabel, typeTitle } from "@/components/FileIcon";
import { browserThumb, knownThumb } from "@/lib/thumbs";
import type { Box, MeasureHits } from "@/components/useMarquee";
import { cn, formatWinDate, formatWinSize } from "@/lib/utils";
import { useMediaQuery } from "@/lib/focus";
import {
  COLUMN_WIDTH,
  MAX_COLUMN,
  MIN_COLUMN,
  MIN_NAME,
  columnShown,
  groupItems,
  columnsToHide,
  pageRows,
  resetColumns,
  setColumnWidth,
  showColumn,
  useColumnPrefs,
  type ColumnId,
  type Group,
  type GroupBy,
} from "@/lib/listView";
import { InlineRename } from "@/components/InlineRename";
import { carriesFiles, carriesItems, dropEffect, droppedIds, startDrag } from "@/lib/dnd";
import { t, tc } from "@/lib/i18n";
import { findByPrefix, wantsCopy } from "@/lib/keys";

/** "list" is Details and "grid" Large icons (the names they were saved under); "compact" is File Explorer's List */
export type ViewMode = "list" | "grid" | "medium" | "compact" | "tiles";

/** What the explorer's keyboard handling asks of the list */
export interface ListNav {
  /** A letter typed: go to the next item whose name starts with the letters typed in the last second */
  typeAhead(key: string): void;
  /** Scroll an item into view, and focus it */
  show(id: string, focus: boolean): void;
  /** Focus the item Tab reaches (the first selected, else the first item); false when there are no items */
  focusStart(): boolean;
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

/** Details view: height of a row (its cells are h-7), of the column headers and of a group's heading */
const ROW = 28;
const HEAD = 30;
const GROUP_ROW = 32;
/** Icon views: items of one size (at least `w` wide, `h` high, `gap` apart), so rows are placed and hit-tested by their index */
const TILED: Record<Exclude<ViewMode, "list">, { w: number; h: number; gap: number }> = {
  grid: { w: 116, h: 148, gap: 8 },
  medium: { w: 88, h: 112, gap: 6 },
  tiles: { w: 250, h: 72, gap: 6 },
  compact: { w: 220, h: 26, gap: 2 },
};
const PAD = 12;
/** Icon views: height of a group's heading, with the space above it */
const GROUP_H = 36;

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
  width,
  resize,
}: {
  k?: SortKey;
  label: string;
  className?: string;
  sort: FileListProps["sort"];
  onSort: FileListProps["onSort"];
  width?: number;
  /** The handle that resizes the column */
  resize?: ReactNode;
}) {
  const active = !!k && sort?.key === k;
  return (
    <th
      role="columnheader"
      className={cn(th, k && onSort && "hover:bg-muted", className)}
      style={width === undefined ? undefined : { width }}
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
        <button type="button" onClick={() => onSort(k)} className="flex h-full w-full min-w-0 items-center hover:text-foreground">
          <span className="truncate">{label}</span>
        </button>
      ) : (
        <span className="block truncate">{label}</span>
      )}
      {resize}
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
  renameDone(item: Item, byKey: boolean): void;
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
      onDone={(byKey) => r.h.current.renameDone(r.item, byKey)}
      className={multiline ? "mt-1.5" : undefined}
    />
  );
}

const td = "h-7 px-2 truncate";

/** Where each column shows: narrow screens keep the name and size */
const COLUMN_CLASS: Record<ColumnId, string> = {
  location: "max-lg:hidden",
  date: "max-md:hidden",
  created: "max-md:hidden",
  type: "max-md:hidden",
  size: "",
  owner: "max-md:hidden",
  extra: "max-md:hidden",
};

function Cell({ id, item, h }: { id: ColumnId; item: Item; h: HandlersRef }) {
  const muted = cn(td, "text-muted-foreground", COLUMN_CLASS[id]);
  switch (id) {
    case "location":
      return (
        <td role="gridcell" className={muted} title={item.location}>
          {item.location}
        </td>
      );
    case "date":
      return <td role="gridcell" className={muted}>{formatWinDate(h.current.dateOf(item))}</td>;
    case "created":
      return <td role="gridcell" className={muted}>{formatWinDate(item.created_at)}</td>;
    case "type":
      return (
        <td role="gridcell" className={muted} title={typeTitle(item)}>
          {typeLabel(item)}
        </td>
      );
    case "size":
      return <td role="gridcell" className={cn(muted, "pr-3 text-right tabular-nums")}>{item.kind === "folder" ? "" : formatWinSize(item.size)}</td>;
    case "owner":
      return <td role="gridcell" className={muted}>{item.owner_name}</td>;
    case "extra":
      return <td role="gridcell" className={muted}>{h.current.extra?.(item)}</td>;
  }
}

const ListRow = memo(function ListRow(r: RowProps & { checkboxes: boolean; columns: ColumnId[]; filler: boolean; ariaRow: number }) {
  const { item } = r;
  return (
    <tr
      {...rowProps(r)}
      role="row"
      aria-rowindex={r.ariaRow}
      className={cn(
        "cursor-default outline-none hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset aria-selected:bg-selection aria-selected:text-accent-foreground aria-selected:shadow-[inset_3px_0_0_var(--color-brand)]",
        r.dropping && "bg-brand/15",
        r.dimmed && "opacity-50",
      )}
    >
      {r.checkboxes && (
        <td role="gridcell" className={cn(td, "px-[7px]")}>
          {/* Space selects with the keyboard, so the check box isn't a Tab stop of its own */}
          <input
            type="checkbox"
            tabIndex={-1}
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
      {r.columns.map((id) => (
        <Cell key={id} id={id} item={item} h={r.h} />
      ))}
      {r.filler && <td aria-hidden />}
    </tr>
  );
});

const Tile = memo(function Tile(r: RowProps & { view: Exclude<ViewMode, "list">; source: FileSource; count: number; checkboxes: boolean }) {
  const { item, view } = r;
  const star = item.is_favorite && <StarIcon className="size-3 shrink-0 fill-amber-400 text-amber-400" aria-label={tc("state", "Favorite")} />;
  // Space selects with the keyboard, so the check box isn't a Tab stop of its own
  const checkbox = r.checkboxes && (
    <input
      type="checkbox"
      tabIndex={-1}
      className={cn("size-4 shrink-0 accent-brand", view !== "compact" && "absolute top-1.5 left-1.5")}
      aria-label={t("Select {name}", { name: item.name })}
      checked={r.selected}
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      onChange={() => r.h.current.toggle(r.index)}
    />
  );
  let body: ReactNode;
  if (view === "compact") {
    // List: a small icon and the name, the items side by side in columns
    body = (
      <>
        {checkbox}
        <FileIcon node={item} className="size-4" />
        {r.renaming ? renameBox(r) : <span className="truncate">{item.name}</span>}
        {star}
      </>
    );
  } else if (view === "tiles") {
    // Tiles: a medium picture, with the name, type and size beside it
    body = (
      <>
        <div className="flex size-12 shrink-0 items-center justify-center">
          <Thumb node={item} source={r.source} className="max-h-12 max-w-12 rounded" iconClass="size-8" />
        </div>
        <div className="grid min-w-0 flex-1 text-left leading-[18px]">
          {r.renaming ? renameBox(r) : <span className="truncate text-[13px]">{item.name}</span>}
          <span className="truncate text-xs text-muted-foreground">{typeLabel(item)}</span>
          {item.kind === "file" && <span className="truncate text-xs text-muted-foreground tabular-nums">{formatWinSize(item.size)}</span>}
        </div>
        {star && <span className="absolute top-1.5 right-1.5 flex">{star}</span>}
        {checkbox}
      </>
    );
  } else {
    // Large and medium icons: the picture above the name
    const medium = view === "medium";
    body = (
      <>
        <div className={cn("flex items-center justify-center", medium ? "h-14" : "h-[88px]")}>
          <Thumb node={item} source={r.source} className={cn("w-full rounded", medium ? "max-h-14" : "max-h-[88px]")} iconClass={medium ? "size-8" : "size-[42px]"} />
        </div>
        {r.renaming ? renameBox(r, true) : <span className="line-clamp-2 pt-1.5 text-xs leading-[18px] break-all">{item.name}</span>}
        {star && <span className="absolute top-1.5 right-1.5 flex">{star}</span>}
        {checkbox}
      </>
    );
  }
  return (
    <div
      {...rowProps(r)}
      {...dragHandle(r)}
      role="option"
      aria-posinset={r.index + 1}
      aria-setsize={r.count}
      title={item.name}
      className={cn(
        "relative min-w-0 rounded-md border border-transparent outline-none select-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring aria-selected:border-brand aria-selected:bg-selection",
        view === "compact" ? "flex items-center gap-2 px-1.5 text-xs" : view === "tiles" ? "flex items-center gap-2.5 p-2" : "text-center",
        view === "grid" ? "p-2" : view === "medium" && "p-1.5",
        r.dropping && "border-brand bg-brand/10",
        r.dimmed && "opacity-50",
        r.renaming && "z-[1]",
      )}
      style={{ height: TILED[view].h }}
    >
      {body}
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

export interface ListColumn {
  id: ColumnId;
  label: string;
  /** Clicking the header sorts by it */
  sort?: SortKey;
}

/** The Details view's columns a list can show besides the name, in order (shown or not, see lib/listView.ts) */
export function listColumns(p: Pick<FileListProps, "showLocation" | "showOwner" | "extraColumn" | "dateLabel">): ListColumn[] {
  const out: ListColumn[] = [];
  if (p.showLocation) out.push({ id: "location", label: t("Location") });
  out.push({ id: "date", label: p.dateLabel ?? t("Date modified"), sort: "updated" });
  out.push({ id: "created", label: t("Date created"), sort: "created" });
  out.push({ id: "type", label: t("Type"), sort: "type" });
  out.push({ id: "size", label: t("Size"), sort: "size" });
  if (p.showOwner) out.push({ id: "owner", label: t("Uploaded by") });
  if (p.extraColumn) out.push({ id: "extra", label: p.extraColumn.label });
  return out;
}

/** Menu items choosing the columns (the column headers' context menu, and View › Columns) */
export function ColumnChoices({ columns }: { columns: ListColumn[] }) {
  const prefs = useColumnPrefs();
  return (
    <>
      <DropdownMenuCheckboxItem checked disabled>
        {t("Name")}
      </DropdownMenuCheckboxItem>
      {columns.map((c) => (
        <DropdownMenuCheckboxItem key={c.id} checked={columnShown(prefs, c.id)} onCheckedChange={(on) => showColumn(c.id, on)} closeOnClick>
          {c.label}
        </DropdownMenuCheckboxItem>
      ))}
      <DropdownMenuSeparator />
      <DropdownMenuItem onClick={resetColumns}>{t("Restore default columns")}</DropdownMenuItem>
    </>
  );
}

/** A group's heading: its name, how many items it holds, and a line */
function GroupHeading({ group, className }: { group: Group<Item>; className?: string }) {
  return (
    <div className={cn("flex h-full items-center gap-2 text-[13px] whitespace-nowrap", className)}>
      <span className="font-medium">{group.label}</span>
      <span className="text-xs text-muted-foreground">({group.items.length})</span>
      <span className="h-px flex-1 bg-border" />
    </div>
  );
}

/** A row of the layout: a group's heading, or items (one in Details, a row of them in the icon views) */
interface LayoutRow {
  /** The items it holds, from `start` up to `end` (in the order shown) */
  start: number;
  end: number;
  /** Height, with the gap below it */
  size: number;
  /** Where it starts, from the top of the first row */
  top: number;
  group?: Group<Item>;
}

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
  const selecting = p.selected.size > 0;
  const prefs = useColumnPrefs();
  // Column widths apply from tablet width up; narrower, only the name and size show
  const wide = useMediaQuery("(min-width: 48rem)");
  const large = useMediaQuery("(min-width: 64rem)");

  // Grouped, items are shown group by group: that's their order for the keyboard and Shift ranges too
  const dateOf = p.dateOf ?? ((x: Item) => x.updated_at);
  const groups = useMemo(
    () => groupItems<Item>(p.items, p.groupBy ?? "none", { dateOf, typeOf: typeLabel, now: new Date(), reversed: p.groupReversed }),
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the date function is a new one on every render
    [p.items, p.groupBy, p.groupReversed],
  );
  const items = useMemo(() => (groups ? groups.flatMap((g) => g.items) : p.items), [groups, p.items]);
  const n = items.length;

  const indexOf = useMemo(() => new Map(items.map((x, i) => [x.id, i])), [items]);
  // The first selected item holds the Tab stop: found once per selection, not for every row
  const firstSelected = useMemo(() => {
    let first = -1;
    for (const id of p.selected) {
      const i = indexOf.get(id);
      if (i !== undefined && (first < 0 || i < first)) first = i;
    }
    return first;
  }, [p.selected, indexOf]);
  const allSelected = useMemo(() => n > 0 && p.selected.size >= n && items.every((x) => p.selected.has(x.id)), [items, p.selected, n]);

  const cols = tile ? Math.max(1, Math.floor((geo.width - 2 * PAD + tile.gap) / (tile.w + tile.gap))) : 1;
  // The rows: in each group, its heading and then its items, `cols` to a row
  const layout = useMemo(() => {
    const rows: LayoutRow[] = [];
    const rowOf: number[] = new Array(n);
    let top = 0;
    const add = (r: Omit<LayoutRow, "top">) => {
      rows.push({ ...r, top });
      top += r.size;
    };
    let start = 0;
    for (const group of groups ?? [undefined]) {
      const end = group ? start + group.items.length : n;
      if (group) add({ start, end: start, size: tile ? GROUP_H : GROUP_ROW, group });
      for (let i = start; i < end; i += cols) {
        const last = Math.min(i + cols, end);
        for (let k = i; k < last; k++) rowOf[k] = rows.length;
        add({ start: i, end: last, size: tile ? tile.h + tile.gap : ROW });
      }
      start = end;
    }
    return { rows, rowOf };
  }, [groups, n, cols, tile]);
  const rowOf = (i: number) => layout.rowOf[i];
  const tabStop = firstSelected >= 0 ? firstSelected : 0;
  // Rows rendered even out of view: the Tab stop, the focused item and the one being renamed
  const pinned = [tabStop, focusId === null ? undefined : indexOf.get(focusId), p.renamingId ? indexOf.get(p.renamingId) : undefined]
    .filter((i): i is number => i !== undefined && i < n)
    .map(rowOf);

  // Row sizes are cached per key: new keys whenever the rows change
  // oxlint-disable-next-line react-hooks/exhaustive-deps -- the layout is what invalidates the keys
  const rowKey = useCallback((i: number) => i, [layout]);
  const v = useVirtualizer({
    count: layout.rows.length,
    getScrollElement: () => scroller,
    estimateSize: (i) => layout.rows[i].size,
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
    if (renamingIndex !== undefined) v.scrollToIndex(rowOf(renamingIndex));
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- only when renaming starts, not while the list reloads
  }, [p.renamingId, renamingShown]);

  const focusItem = (index: number) => {
    const id = items[index].id;
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
    for (let i = Math.min(anchorIndex, index); i <= Math.max(anchorIndex, index); i++) next.add(items[i].id);
    return next;
  };

  const toggle = (index: number) => {
    const id = items[index].id;
    const next = new Set(p.selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    p.onSelect(next, id);
  };

  /** The item below or above: in the same column of the next row of items (group headings are skipped), else the last or first item */
  const vertical = (index: number, dir: 1 | -1) => {
    const { rows } = layout;
    let r = rowOf(index) + dir;
    while (rows[r]?.group) r += dir;
    const to = rows[r];
    if (!to) return dir > 0 ? n - 1 : 0;
    return Math.min(to.start + index - rows[rowOf(index)].start, to.end - 1);
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
    if (e.target !== e.currentTarget || items[index].id === p.renamingId) return;
    if (e.key === "Enter" && !e.altKey && !e.repeat) {
      e.preventDefault();
      p.onOpen(items[index], true);
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
      else p.onSelect(new Set([items[index].id]), items[index].id);
      return;
    }
    if (next === null) return;
    e.preventDefault();
    // Ctrl moves the focus and leaves the selection alone, like File Explorer: Ctrl+Space then adds or removes the item
    if ((e.ctrlKey || e.metaKey) && !e.shiftKey) {
      focusItem(next);
      return;
    }
    const item = items[next];
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
      items.map((x) => x.name),
      same ? text[0] : text,
      same ? at : Math.max(at, 0) - 1,
    );
    if (next < 0) return;
    p.onSelect(new Set([items[next].id]), items[next].id);
    focusItem(next);
  };
  const show = (id: string, focus: boolean) => {
    const index = indexOf.get(id);
    if (index === undefined) return;
    if (focus) focusItem(index);
    else v.scrollToIndex(rowOf(index));
  };
  const focusStart = () => {
    if (n === 0) return false;
    focusItem(tabStop);
    return true;
  };
  if (p.navRef) p.navRef.current = { typeAhead, show, focusStart };

  /** A finger resting on an item: selected once it has stayed long enough */
  const press = useRef<{ timer: ReturnType<typeof setTimeout>; x: number; y: number; done: boolean } | null>(null);
  /** The click that may follow a long press doesn't toggle the item again */
  const ignoreClick = useRef({ index: -1, until: 0 });
  const cancelPress = () => {
    if (press.current) clearTimeout(press.current.timer);
  };
  /** Long press (on every platform, iOS included): select the item, adding it when items are already selected */
  const longPress = (index: number) => {
    const id = items[index].id;
    if (!p.selected.has(id)) p.onSelect(selecting ? new Set(p.selected).add(id) : new Set([id]), id);
    navigator.vibrate?.(15);
  };

  const h = useRef<Handlers>(null!);
  h.current = {
    click: (e, index) => {
      const item = items[index];
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
      if (!p.selected.has(items[index].id)) p.onSelect(new Set([items[index].id]), items[index].id);
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
      const id = items[index].id;
      const ids = p.selected.has(id) ? [...p.selected] : [id];
      if (!p.selected.has(id)) p.onSelect(new Set([id]), id);
      startDrag(e, ids, items);
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
    const { rows } = layout;
    const shown = items;
    if (!el) return () => [];
    const at = offsetIn(el, container);
    const top = at.top + (grid ? PAD : (head.current?.offsetHeight ?? HEAD));
    const gap = tile?.gap ?? 0;
    const tileW = tile ? (el.clientWidth - 2 * PAD - (cols - 1) * gap) / cols : 0;
    return function* (b: Box) {
      if (!grid && (b.x > at.left + at.width || b.x + b.w < at.left)) return;
      for (const row of rows) {
        const y = top + row.top;
        if (y > b.y + b.h) return;
        if (row.group || y + row.size - gap < b.y) continue;
        if (!tile) {
          yield shown[row.start].id;
          continue;
        }
        const [c0, c1] = touching(at.left + PAD, tileW, tileW + gap, row.end - row.start, b.x, b.x + b.w);
        for (let c = c0; c <= c1; c++) yield shown[row.start + c].id;
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
    const item = items[index];
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

  if (tile) {
    return (
      <>
        {status}
        <div ref={(el) => void (root.current = el)} role="listbox" aria-multiselectable aria-label={label} className="p-3">
          {rows.map(({ row: r, gap }) => {
            const at = layout.rows[r];
            return (
              <Fragment key={r}>
                {gap > 0 && <div aria-hidden style={{ height: gap }} />}
                {at.group ? (
                  <div role="none" className="flex items-end px-1 pb-1.5" style={{ height: GROUP_H }}>
                    <GroupHeading group={at.group} className="h-auto" />
                  </div>
                ) : (
                  <div role="none" className="grid" style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))`, gap: tile.gap, marginBottom: tile.gap }}>
                    {items.slice(at.start, at.end).map((item, k) => (
                      <Tile key={item.id} {...row(at.start + k)} view={view as Exclude<ViewMode, "list">} source={p.source} count={n} checkboxes={!!p.showCheckboxes} />
                    ))}
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
        aria-rowcount={layout.rows.length + 1}
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
                    onChange={() => p.onSelect(allSelected ? new Set() : new Set(items.map((x) => x.id)))}
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
            const at = layout.rows[r];
            return (
              <Fragment key={at.group ? `group:${at.group.key}` : items[at.start].id}>
                {gap > 0 && spacer(gap)}
                {at.group ? (
                  <tr role="row" aria-rowindex={r + 2}>
                    <td role="gridcell" colSpan={cellCount} className="px-3 pt-2" style={{ height: GROUP_ROW }}>
                      <GroupHeading group={at.group} />
                    </td>
                  </tr>
                ) : (
                  <ListRow {...row(at.start)} checkboxes={!!p.showCheckboxes} columns={shownIds} filler={filler} ariaRow={r + 2} />
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
