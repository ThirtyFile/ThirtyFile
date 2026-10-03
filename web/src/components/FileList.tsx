import { Fragment, useEffect, useMemo, useRef, useState, type ReactNode, type RefObject } from "react";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { Resizer } from "@/components/Resizer";
import type { FileSource, Node, SortKey, SortOrder } from "@/api";
import { typeLabel } from "@/components/FileIcon";
import type { Box, MeasureHits } from "@/components/useMarquee";
import { cn } from "@/lib/utils";
import { useMediaQuery } from "@/lib/focus";
import { COLUMN_WIDTH, MAX_COLUMN, MIN_COLUMN, MIN_NAME, columnShown, groupItems, columnsToHide, setColumnWidth, useColumnPrefs, type ColumnId, type GroupBy } from "@/lib/listView";
import { carriesFiles, carriesItems, dropEffect, droppedIds, startDrag } from "@/lib/dnd";
import { t } from "@/lib/i18n";
import { wantsCopy } from "@/lib/keys";
import { inSpan, spanCount, type ListSpan } from "@/lib/span";
import { type ViewMode, type Item, HEAD, GROUP_ROW, GROUP_H, offsetIn, touching } from "@/components/fileList/layout";
import { useListLayout } from "@/components/fileList/useListLayout";
import { th, Head, type Handlers, type RowProps, COLUMN_CLASS, fitsScreen, ListRow, Tile, GroupHeading, PlaceholderRow } from "@/components/fileList/rows";
import { listColumns, ColumnChoices } from "@/components/fileList/columns";
import { useListKeyboard } from "@/components/fileList/useListKeyboard";
import { useClickToRename } from "@/lib/clickToRename";
import type { ListTreeView } from "@/components/fileList/listTree";

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
  /**
   * Clicking the name of the item already selected on its own starts renaming it, as in File Explorer (the Windows
   * style; lib/clickToRename): left out where items can't be renamed
   */
  onClickRename?(item: Item): void;
  /** Accessible name of the list (defaults to "Items") */
  label?: string;
  /** Receives the list's geometry for marquee selection (useMarquee's `measure`): only the rows in view are rendered */
  measureRef?: RefObject<MeasureHits | null>;
  /** Receives the list's keyboard helpers */
  navRef?: RefObject<ListNav | null>;
  /** A column of the Columns view: the style's keys for the column before (-1) and the next one (1), on the item with the focus */
  onColumn?(dir: -1 | 1, item: Item): void;
  /** The List view of a style whose folders expand in place (components/fileList/listTree): `items` are its rows */
  tree?: ListTreeView;
  /**
   * A column of the Columns view that isn't the open folder's: what it shows selected is the folder open in the next
   * column, shown less strongly and not announced
   */
  inactive?: boolean;
}

const coarse = typeof matchMedia === "function" && matchMedia("(pointer: coarse)").matches;
/** How long a finger rests on an item to select it, and how far it may move meanwhile */
const LONG_PRESS_MS = 500;
const LONG_PRESS_SLOP = 10;

export function FileList(p: FileListProps) {
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  /** The item with the keyboard focus: always rendered, so the focus isn't lost when it scrolls out of view */
  const [focusId, setFocusId] = useState<string | null>(null);
  const view = p.view;
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
    () => (complete ? groupItems<Item>(p.items as Item[], p.groupBy ?? "none", { dateOf, typeOf: typeLabel, now: new Date(), reversed: p.groupReversed }) : null),
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
    () => n > 0 && (span && !span.from && !span.to ? span.except.size === 0 : complete && p.selected.size >= n && items.every((x) => p.selected.has(x!.id))),
    [items, p.selected, n, span, complete],
  );

  const tabStop = firstSelected >= 0 ? firstSelected : 0;
  // Rows rendered even out of view: the Tab stop, the focused item and the one being renamed
  const { root, head, scroller, geo, tile, grid, pad, cols, layout, rowOf, v, across } = useListLayout({
    view,
    wide,
    groups,
    n,
    keep: [tabStop, focusId === null ? undefined : indexOf.get(focusId), p.renamingId ? indexOf.get(p.renamingId) : undefined],
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

  const { focusItem, rangeTo, toggle, keyNav } = useListKeyboard({
    p,
    items,
    n,
    indexOf,
    span,
    layout,
    rowOf,
    v,
    root,
    grid,
    pad,
    across: grid && view !== "columns",
    sideways: across,
    tile,
    scroller,
    focusId,
    setFocusId,
    tabStop,
  });

  // Click the name of the item selected on its own to rename it (not on touch screens, where a long press selects)
  const onClickRename = p.onClickRename;
  useClickToRename({
    enabled: !!onClickRename && !!p.onRename && !coarse,
    root,
    itemOf: (el) => el.closest<HTMLElement>("[data-node-id]")?.dataset.nodeId ?? null,
    selectedAlone: (id) => !span && p.selected.size === 1 && p.selected.has(id) && p.renamingId !== id,
    canRename: () => true,
    start: (id) => {
      const at = indexOf.get(id);
      const item = at === undefined ? undefined : items[at];
      if (item) onClickRename?.(item);
    },
  });

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
    expand: (item, open) => p.tree?.toggle(item.id, open),
  };

  // Marquee selection finds the boxed items from the row geometry: most rows aren't in the DOM
  const measure: MeasureHits = (container) => {
    const el = root.current;
    const shown = items;
    // A strip laid out across isn't selected with a box (its view marks it `data-no-marquee`)
    if (!el || across) return () => [];
    const at = offsetIn(el, container);
    const top = at.top + (grid ? pad : (head.current?.offsetHeight ?? HEAD));
    const gap = tile?.gap ?? 0;
    const tileW = tile ? (el.clientWidth - 2 * pad - (cols - 1) * gap) / cols : 0;
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
        const [c0, c1] = touching(at.left + pad, tileW, tileW + gap, row.end - row.start, b.x, b.x + b.w);
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
  const status = !p.inactive && (
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
      // Another column of the Columns view isn't a Tab stop: Tab goes to the open folder's, and the arrows to the others
      tabStop: index === tabStop && !p.inactive,
      dimmed: !!p.dimmed?.has(item.id),
      dropping: dropTarget === item.id,
      renaming: item.id === p.renamingId && !!p.onRename,
      // Not on touch screens: holding a finger on an item selects it rather than picking it up
      movable: !coarse && !!p.onDropInto && item.id !== p.renamingId,
      dropTarget: !!(p.onDropInto || p.onUploadInto) && item.kind === "folder",
    };
  };

  if (tile && across) {
    // Across (the Gallery view's strip): the items side by side, with the room of those not rendered before and after
    return (
      <>
        {status}
        <div
          ref={(el) => void (root.current = el)}
          role="listbox"
          aria-multiselectable
          aria-orientation="horizontal"
          aria-label={label}
          className="flex h-full w-max items-center"
          style={{ paddingInline: pad }}
        >
          {rows.map(({ row: r, gap }) => {
            const i = layout.row(r).start;
            const item = items[i];
            return (
              <Fragment key={item?.id ?? `#${i}`}>
                {gap > 0 && <div aria-hidden className="shrink-0" style={{ width: gap }} />}
                <div role="none" className="shrink-0" style={{ width: tile.w, marginRight: tile.gap }}>
                  {item ? (
                    <Tile {...row(i)} view={view as Exclude<ViewMode, "list">} source={p.source} count={n} checkboxes={false} />
                  ) : (
                    // Not loaded yet: its place, until its part loads
                    <div aria-hidden className="animate-pulse rounded-md bg-muted/60" style={{ height: tile.h }} />
                  )}
                </div>
              </Fragment>
            );
          })}
          {rest > 0 && <div aria-hidden className="shrink-0" style={{ width: rest }} />}
        </div>
      </>
    );
  }

  if (tile) {
    return (
      <>
        {status}
        <div ref={(el) => void (root.current = el)} role="listbox" aria-multiselectable aria-label={label} className={cn(view === "columns" ? "p-1" : "p-3", p.inactive && "tf-idle")}>
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
  const minWidth = wide && !filler ? columns.filter((c) => large || c.id !== "location").reduce((sum, c) => sum + widthOf(c.id), MIN_NAME + (p.showCheckboxes ? 30 : 0)) : undefined;
  // Cells spanning the row span the columns shown: counting those the screen width hides (COLUMN_CLASS) would add
  // empty columns to the table, which take their share of the name's width
  const cellCount = 1 + columns.filter((c) => fitsScreen(c.id, wide, large)).length + (p.showCheckboxes ? 1 : 0) + (filler ? 1 : 0);
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
      label={t('Resize the "{name}" column', { name: label })}
    />
  );

  // role="grid": screen readers only report the selected state of rows in a grid, not in a plain table (a treegrid when
  // folders expand in place: rows say how deep they are, and whether they are expanded)
  return (
    <>
      {status}
      <table
        ref={(el) => void (root.current = el)}
        role={p.tree ? "treegrid" : "grid"}
        aria-multiselectable
        aria-label={label}
        aria-rowcount={layout.count + 1}
        className="w-full table-fixed border-collapse text-(length:--tf-list-text) leading-(--tf-list-leading) whitespace-nowrap select-none"
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
                    onChange={() => (allSelected ? p.onSelect(new Set(), undefined, null) : p.onSelectAll ? p.onSelectAll() : p.onSelect(new Set(indexOf.keys()), undefined, null))}
                  />
                </th>
              )}
              <Head sort={p.sort} onSort={p.onSort} k="name" label={t("Name")} className="pl-3" width={nameWidth} resize={resizer("name", t("Name"))} />
              {columns.map((c) => (
                <Head key={c.id} sort={p.sort} onSort={p.onSort} k={c.sort} label={c.label} className={COLUMN_CLASS[c.id]} width={widthOf(c.id)} resize={resizer(c.id, c.label)} />
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
                  <ListRow
                    {...row(at.start)}
                    checkboxes={!!p.showCheckboxes}
                    columns={shownIds}
                    filler={filler}
                    ariaRow={r + 2}
                    level={p.tree?.depth(at.start)}
                    expanded={p.tree && item.kind === "folder" ? p.tree.isOpen(item.id) : undefined}
                  />
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
