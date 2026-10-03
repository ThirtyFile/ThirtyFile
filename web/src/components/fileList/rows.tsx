//! The file list's column headers, rows (Details view) and items (icon views)

import { memo, type DragEvent, type KeyboardEvent, type MouseEvent, type ReactNode, type RefObject, type FocusEvent, type TouchEvent } from "react";
import { ChevronDownIcon, ChevronRightIcon, ChevronUpIcon } from "lucide-react";
import type { FileSource, SortKey } from "@/api";
import { FileIcon, typeLabel, typeTitle } from "@/components/FileIcon";
import { cn, formatDateTime, formatWinSize } from "@/lib/utils";
import type { ColumnId, Group } from "@/lib/listView";
import { InlineRename } from "@/components/InlineRename";
import { t } from "@/lib/i18n";
import { type ViewMode, type Item, TILED } from "@/components/fileList/layout";
import { Thumb } from "@/components/fileList/thumbs";
import { ItemMarks, hasMarks } from "@/components/fileList/marks";
import { TagNames } from "@/components/tags";
import type { FileListProps } from "@/components/FileList";

export const th =
  "group/th relative sticky top-0 z-[1] h-[30px] border-b bg-background px-2 text-left font-normal text-muted-foreground after:absolute after:top-1.5 after:right-0 after:bottom-1.5 after:w-px after:bg-border last:after:hidden";

/** Sortable column header (defined at module level: defining it inside the component would rebuild the whole header row on every render) */
export function Head({
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
export interface Handlers {
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
  /** A folder's triangle, in a list whose folders expand in place: expands it, or collapses it */
  expand(item: Item, open: boolean): void;
}
export type HandlersRef = RefObject<Handlers>;

/** The state a row shows; a row re-renders only when one of these changes */
export interface RowProps {
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

export function rowProps({ item, index, h, selected, tabStop, dropTarget }: RowProps) {
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
export const dragHandle = ({ h, index, movable }: RowProps) => ({
  draggable: movable,
  onDragStart: (e: DragEvent) => h.current.dragStart(e, index),
});

export function renameBox(r: RowProps, multiline?: boolean) {
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

export const td = "h-7 px-2 truncate";

/** Where each column shows: narrow screens keep the name and size */
export const COLUMN_CLASS: Record<ColumnId, string> = {
  location: "max-lg:hidden",
  date: "max-md:hidden",
  created: "max-md:hidden",
  type: "max-md:hidden",
  size: "",
  owner: "max-md:hidden",
  tags: "max-md:hidden",
  extra: "max-md:hidden",
};

/** Whether COLUMN_CLASS shows a column: `wide` at least 48rem (md), `large` at least 64rem (lg) */
export function fitsScreen(id: ColumnId, wide: boolean, large: boolean) {
  return id === "size" || (id === "location" ? large : wide);
}

export function Cell({ id, item, h }: { id: ColumnId; item: Item; h: HandlersRef }) {
  const muted = cn(td, "text-muted-foreground", COLUMN_CLASS[id]);
  switch (id) {
    case "location":
      return (
        <td role="gridcell" className={muted} title={item.location}>
          {item.location}
        </td>
      );
    case "date":
      return (
        <td role="gridcell" className={muted}>
          {formatDateTime(h.current.dateOf(item))}
        </td>
      );
    case "created":
      return (
        <td role="gridcell" className={muted}>
          {formatDateTime(item.created_at)}
        </td>
      );
    case "type":
      return (
        <td role="gridcell" className={muted} title={typeTitle(item)}>
          {typeLabel(item)}
        </td>
      );
    case "size":
      return (
        <td role="gridcell" className={cn(muted, "pr-3 text-right tabular-nums")}>
          {item.kind === "folder" ? "" : formatWinSize(item.size)}
        </td>
      );
    case "owner":
      return (
        <td role="gridcell" className={muted}>
          {item.owner_name || "—"}
        </td>
      );
    case "tags":
      return (
        <td role="gridcell" className={muted}>
          {!!item.tags?.length && <TagNames ids={item.tags} className="flex-nowrap overflow-hidden" />}
        </td>
      );
    case "extra":
      return (
        <td role="gridcell" className={muted}>
          {h.current.extra?.(item)}
        </td>
      );
  }
}

/** How far in each level of a list whose folders expand in place is indented */
export const LEVEL_INDENT = 16;

export const ListRow = memo(function ListRow(
  r: RowProps & {
    checkboxes: boolean;
    columns: ColumnId[];
    filler: boolean;
    ariaRow: number;
    /** A list whose folders expand in place: how deep the row is (0: the list's own items) */
    level?: number;
    /** ...and whether the folder is expanded (none for a file) */
    expanded?: boolean;
  },
) {
  const { item } = r;
  const tree = r.level !== undefined;
  return (
    <tr
      {...rowProps(r)}
      role="row"
      aria-rowindex={r.ariaRow}
      aria-level={tree ? r.level! + 1 : undefined}
      aria-expanded={r.expanded}
      className={cn(
        // Hovered, selected and focused, it looks as the style says (tf-row, style.css)
        "tf-row cursor-default",
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
      <td role="gridcell" className={cn(td, tree ? "pl-1" : "pl-3")} style={tree ? { paddingLeft: 4 + r.level! * LEVEL_INDENT } : undefined}>
        <div data-drag-handle {...dragHandle(r)} className={cn("flex w-fit max-w-full min-w-0 items-center", tree ? "gap-1" : "gap-2")} title={item.name}>
          {/* The triangle is for the mouse (the keyboard uses → and ←): hidden from screen readers, which hear the row expanded or not */}
          {tree && (
            <span
              aria-hidden
              data-disclosure
              className={cn("flex size-4 shrink-0 items-center justify-center text-muted-foreground hover:text-foreground", r.expanded === undefined && "invisible")}
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation();
                if (r.expanded !== undefined) r.h.current.expand(item, !r.expanded);
              }}
              onDoubleClick={(e) => e.stopPropagation()}
            >
              <ChevronRightIcon className={cn("size-3.5 transition-transform", r.expanded && "rotate-90")} />
            </span>
          )}
          <FileIcon node={item} className={cn("size-4 shrink-0", tree && "mr-1")} />
          {r.renaming ? (
            renameBox(r)
          ) : (
            <>
              <span data-name className="truncate">
                {item.name}
              </span>
              <ItemMarks item={item} />
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

export const Tile = memo(function Tile(r: RowProps & { view: Exclude<ViewMode, "list">; source: FileSource; count: number; checkboxes: boolean }) {
  const { item, view } = r;
  // The star and the tag dots
  const marks = hasMarks(item) && <ItemMarks item={item} large />;
  /** A row of small icons and names: File Explorer's List, and a column of the Columns view */
  const line = view === "compact" || view === "columns";
  // Space selects with the keyboard, so the check box isn't a Tab stop of its own
  const checkbox = r.checkboxes && (
    <input
      type="checkbox"
      tabIndex={-1}
      className={cn("size-4 shrink-0 accent-brand", !line && "absolute top-1.5 left-1.5")}
      aria-label={t("Select {name}", { name: item.name })}
      checked={r.selected}
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      onChange={() => r.h.current.toggle(r.index)}
    />
  );
  let body: ReactNode;
  if (line) {
    // List: a small icon and the name, the items side by side in columns. Columns: the same, one to a row, and folders
    // with an arrow (they open in the next column)
    body = (
      <>
        {checkbox}
        <FileIcon node={item} className="size-4" />
        {r.renaming ? (
          renameBox(r)
        ) : (
          <span data-name className="truncate">
            {item.name}
          </span>
        )}
        {marks}
        {view === "columns" && item.kind === "folder" && <ChevronRightIcon aria-hidden className="ml-auto size-3.5 shrink-0 text-muted-foreground" />}
      </>
    );
  } else if (view === "gallery") {
    // The Gallery view's strip: the picture alone; the name is what the item is called, and shows on hover
    body = (
      <>
        <Thumb node={item} source={r.source} className="max-h-full max-w-full rounded-sm" iconClass="size-9" />
        <span data-name className="sr-only">
          {item.name}
        </span>
        {marks && <span className="absolute top-0.5 right-0.5 flex">{marks}</span>}
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
          {r.renaming ? (
            renameBox(r)
          ) : (
            <span data-name className="truncate text-[13px]">
              {item.name}
            </span>
          )}
          <span className="truncate text-xs text-muted-foreground">{typeLabel(item)}</span>
          {item.kind === "file" && <span className="truncate text-xs text-muted-foreground tabular-nums">{formatWinSize(item.size)}</span>}
        </div>
        {marks && <span className="absolute top-1.5 right-1.5 flex">{marks}</span>}
        {checkbox}
      </>
    );
  } else {
    // Large and medium icons: the picture above the name
    const medium = view === "medium";
    body = (
      <>
        <div className={cn("tf-thumb flex items-center justify-center", medium ? "h-14" : "mx-auto h-[88px] w-(--tf-thumb-w) max-w-full")}>
          <Thumb node={item} source={r.source} className={cn("w-full rounded", medium ? "max-h-14" : "max-h-[88px]")} iconClass={medium ? "size-8" : "size-(--tf-grid-icon)"} />
        </div>
        {r.renaming ? (
          renameBox(r, true)
        ) : (
          <span data-name className="tf-name mx-auto mt-1.5 line-clamp-2 w-(--tf-name-w) max-w-full text-(length:--tf-name-text) leading-[18px] break-all">
            {item.name}
          </span>
        )}
        {marks && <span className="absolute top-1.5 right-1.5 flex">{marks}</span>}
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
      data-line={line || undefined}
      data-strip={view === "gallery" || undefined}
      className={cn(
        // Hovered, selected and focused, it looks as the style says (tf-item, style.css)
        "tf-item relative min-w-0 select-none",
        line
          ? "flex items-center gap-2 px-1.5 text-(length:--tf-list-text) leading-(--tf-list-leading)"
          : view === "tiles"
            ? "flex items-center gap-2.5 p-2"
            : view === "gallery"
              ? "flex items-center justify-center p-1"
              : "text-center",
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

/** A group's heading: its name, how many items it holds, and a line */
export function GroupHeading({ group, className }: { group: Group<Item>; className?: string }) {
  return (
    <div className={cn("flex h-full items-center gap-2 text-[13px] whitespace-nowrap", className)}>
      <span className="font-medium">{group.label}</span>
      <span className="text-xs text-muted-foreground">({group.items.length})</span>
      <span className="h-px flex-1 bg-border" />
    </div>
  );
}

/** A row whose item isn't loaded yet (a large folder loads it as it comes into view) */
export function PlaceholderRow({ cells, ariaRow }: { cells: number; ariaRow: number }) {
  return (
    <tr role="row" aria-rowindex={ariaRow} aria-busy>
      <td role="gridcell" colSpan={cells} className="h-7 px-3">
        <span className="block h-3 w-1/3 animate-pulse rounded bg-muted" />
      </td>
    </tr>
  );
}
