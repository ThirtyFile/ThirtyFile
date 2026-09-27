import { useState, type DragEvent, type KeyboardEvent, type MouseEvent, type ReactNode } from "react";
import { ChevronDownIcon, ChevronUpIcon, StarIcon } from "lucide-react";
import type { FileSource, Node, SortKey, SortOrder } from "@/api";
import { FileIcon, canThumbnail, typeLabel, typeTitle } from "@/components/FileIcon";
import { cn, formatWinDate, formatWinSize } from "@/lib/utils";
import { InlineRename } from "@/components/InlineRename";
import { t, tc } from "@/lib/i18n";

export type ViewMode = "list" | "grid";
export const DRAG_MIME = "application/x-thirtyfile-nodes";

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
  /** Show item checkboxes */
  showCheckboxes?: boolean;
  /** Cut items (not yet pasted) are shown semi-transparent */
  dimmed?: Set<string>;
  dateLabel?: string;
  dateOf?(n: Item): number;
  /** Allow dragging items into folders to move them */
  onMoveInto?(ids: string[], folder: Node): void;
  empty?: ReactNode;
  /** Item being renamed inline */
  renamingId?: string | null;
  onRename?(item: Item, name: string): Promise<void>;
  onRenameDone?(): void;
  /** Accessible name of the list (defaults to "Items") */
  label?: string;
}

const coarse = typeof window !== "undefined" && window.matchMedia("(pointer: coarse)").matches;

export function Thumb({ node, source, className, iconClass }: { node: Node; source: FileSource; className?: string; iconClass?: string }) {
  const [failed, setFailed] = useState(false);
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

export function FileList(p: FileListProps) {
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const selecting = p.selected.size > 0;

  const click = (e: MouseEvent, index: number) => {
    const item = p.items[index];
    // Marquee selection prevents the default mousedown, so the row wouldn't get the focus: arrows continue from the clicked row
    (e.currentTarget as HTMLElement).focus({ preventScroll: true });
    if (coarse && !selecting && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
      p.onOpen(item);
      return;
    }
    const anchorIndex = p.anchor === null ? -1 : p.items.findIndex((x) => x.id === p.anchor);
    if (e.shiftKey && anchorIndex >= 0) {
      const [a, b] = [Math.min(anchorIndex, index), Math.max(anchorIndex, index)];
      const next = new Set(e.ctrlKey || e.metaKey ? p.selected : []);
      for (let i = a; i <= b; i++) next.add(p.items[i].id);
      p.onSelect(next, p.anchor!);
    } else if (e.ctrlKey || e.metaKey || (coarse && selecting)) {
      toggle(index);
    } else {
      p.onSelect(new Set([item.id]), item.id);
    }
  };

  const toggle = (index: number) => {
    const id = p.items[index].id;
    const next = new Set(p.selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    p.onSelect(next, id);
  };

  const contextMenu = (index: number) => {
    // Right-clicking an unselected item selects only that item
    if (!p.selected.has(p.items[index].id)) p.onSelect(new Set([p.items[index].id]), p.items[index].id);
  };

  const dragStart = (e: DragEvent, index: number) => {
    const id = p.items[index].id;
    const ids = p.selected.has(id) ? [...p.selected] : [id];
    if (!p.selected.has(id)) p.onSelect(new Set([id]), id);
    e.dataTransfer.setData(DRAG_MIME, JSON.stringify(ids));
    e.dataTransfer.effectAllowed = "move";
  };

  // Drag to move: icon view drags the whole item; list view drags only the name, dragging from other columns marquee-selects (like Windows)
  const dragHandle = (item: Item, i: number) => ({
    draggable: !!p.onMoveInto && item.id !== p.renamingId,
    onDragStart: (e: DragEvent) => dragStart(e, i),
  });

  /** Keyboard: arrows move the selection (Shift extends it), Space selects (toggles with Ctrl), Home/End jump, Enter opens */
  const keyNav = (e: KeyboardEvent<HTMLElement>, index: number) => {
    // Keys typed in a control inside the row (its checkbox, the rename box) belong to that control
    if (e.target !== e.currentTarget) return;
    const el = e.currentTarget;
    const parent = el.parentElement;
    if (!parent) return;
    // Columns of the icon view, as laid out by the browser
    const perRow = p.view === "grid" ? Math.max(1, getComputedStyle(parent).gridTemplateColumns.split(" ").filter(Boolean).length) : 1;
    if (e.key === "Enter" && !e.altKey && !e.repeat) {
      e.preventDefault();
      p.onOpen(p.items[index]);
      return;
    }
    let next: number | null = null;
    if (e.key === "ArrowDown") next = Math.min(p.items.length - 1, index + perRow);
    else if (e.key === "ArrowUp") next = Math.max(0, index - perRow);
    else if (e.key === "ArrowRight" && p.view === "grid") next = Math.min(p.items.length - 1, index + 1);
    else if (e.key === "ArrowLeft" && p.view === "grid") next = Math.max(0, index - 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = p.items.length - 1;
    else if (e.key === " ") {
      e.preventDefault();
      if (e.ctrlKey || e.metaKey) toggle(index);
      else p.onSelect(new Set([p.items[index].id]), p.items[index].id);
      return;
    }
    if (next === null) return;
    e.preventDefault();
    const item = p.items[next];
    const anchorIndex = p.anchor === null ? -1 : p.items.findIndex((x) => x.id === p.anchor);
    if (e.shiftKey && anchorIndex >= 0) {
      const [a, b] = [Math.min(anchorIndex, next), Math.max(anchorIndex, next)];
      const range = new Set<string>();
      for (let k = a; k <= b; k++) range.add(p.items[k].id);
      p.onSelect(range, p.anchor!);
    } else {
      p.onSelect(new Set([item.id]), item.id);
    }
    parent.querySelector<HTMLElement>(`[data-node-id="${CSS.escape(item.id)}"]`)?.focus();
  };

  const rowProps = (item: Item, i: number) => ({
    "data-node-id": item.id,
    "aria-selected": p.selected.has(item.id),
    // One item is reachable with Tab (the first selected one, else the first item); arrows move between the others
    tabIndex: p.selected.has(item.id) ? (p.items.findIndex((x) => p.selected.has(x.id)) === i ? 0 : -1) : p.selected.size === 0 && i === 0 ? 0 : -1,
    onKeyDown: (e: KeyboardEvent<HTMLElement>) => {
      if (item.id !== p.renamingId) keyNav(e, i);
    },
    onClick: (e: MouseEvent) => click(e, i),
    onDoubleClick: () => p.onOpen(item),
    onMouseDown: (e: MouseEvent) => {
      if (e.button === 1 && p.onOpenInNewTab) e.preventDefault();
    },
    onAuxClick: (e: MouseEvent) => {
      if (e.button === 1 && p.onOpenInNewTab) {
        e.preventDefault();
        p.onOpenInNewTab(item);
      }
    },
    onContextMenu: () => contextMenu(i),
    ...(p.onMoveInto && item.kind === "folder"
      ? {
          onDragOver: (e: DragEvent) => {
            if (!e.dataTransfer.types.includes(DRAG_MIME)) return;
            e.preventDefault();
            e.stopPropagation();
            e.dataTransfer.dropEffect = "move";
            setDropTarget(item.id);
          },
          onDragLeave: () => setDropTarget((t) => (t === item.id ? null : t)),
          onDrop: (e: DragEvent) => {
            setDropTarget(null);
            const raw = e.dataTransfer.getData(DRAG_MIME);
            if (!raw) return;
            e.preventDefault();
            e.stopPropagation();
            // Any page can set this type when dragging, so check what arrived
            let dropped: unknown;
            try {
              dropped = JSON.parse(raw);
            } catch {
              return;
            }
            if (!Array.isArray(dropped) || !dropped.every((id) => typeof id === "string")) return;
            const ids = (dropped as string[]).filter((id) => id !== item.id);
            if (ids.length) p.onMoveInto!(ids, item);
          },
        }
      : {}),
  });

  if (p.items.length === 0) return <>{p.empty}</>;

  // Screen readers announce how many items are selected (the status bar isn't read out)
  const status = (
    <div role="status" className="sr-only">
      {p.selected.size > 0 ? t("{n} item selected|{n} items selected", { n: p.selected.size }) : ""}
    </div>
  );
  const label = p.label ?? t("Items");

  const dateOf = p.dateOf ?? ((n: Item) => n.updated_at);
  const renaming = (item: Item) => item.id === p.renamingId && !!p.onRename;
  const renameBox = (item: Item, multiline?: boolean) => (
    <InlineRename
      initial={item.name}
      multiline={multiline}
      selectAll={item.kind === "folder"}
      onSubmit={(name) => p.onRename!(item, name)}
      onDone={() => p.onRenameDone?.()}
      className={multiline ? "mt-1.5" : undefined}
    />
  );
  const star = (item: Item) => item.is_favorite && <StarIcon className="size-[11px] shrink-0 fill-amber-400 text-amber-400" aria-label={tc("state", "Favorite")} />;

  if (p.view === "grid") {
    return (
      <>
        {status}
        <div role="listbox" aria-multiselectable aria-label={label} className="grid grid-cols-[repeat(auto-fill,minmax(116px,1fr))] gap-2 p-3">
          {p.items.map((item, i) => (
            <div
              key={item.id}
              {...rowProps(item, i)}
              {...dragHandle(item, i)}
              role="option"
              title={item.name}
              className={cn(
                "relative min-w-0 rounded-md border border-transparent p-2 text-center outline-none select-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring aria-selected:border-brand aria-selected:bg-selection",
                dropTarget === item.id && "border-brand bg-brand/10",
                p.dimmed?.has(item.id) && "opacity-50",
              )}
            >
              <div className="flex h-[88px] items-center justify-center">
                <Thumb node={item} source={p.source} className="max-h-[88px] w-full rounded" iconClass="size-[42px]" />
              </div>
              {renaming(item) ? renameBox(item, true) : <span className="line-clamp-2 pt-1.5 text-xs leading-[18px] break-all">{item.name}</span>}
              {item.is_favorite && <StarIcon className="absolute top-1.5 right-1.5 size-3 fill-amber-400 text-amber-400" />}
            </div>
          ))}
        </div>
      </>
    );
  }

  const allSelected = p.items.length > 0 && p.items.every((n) => p.selected.has(n.id));
  const td = "h-7 px-2 truncate";

  // role="grid": screen readers only report the selected state of rows in a grid, not in a plain table
  return (
    <>
      {status}
      <table
        role="grid"
        aria-multiselectable
        aria-label={label}
        aria-rowcount={p.items.length + 1}
        className="w-full table-fixed border-collapse text-xs whitespace-nowrap select-none"
      >
        <thead>
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
                  onChange={() => p.onSelect(allSelected ? new Set() : new Set(p.items.map((n) => n.id)))}
                />
              </th>
            )}
            <Head sort={p.sort} onSort={p.onSort} k="name" label={t("Name")} className="pl-3" />
            {p.showLocation && <Head sort={p.sort} onSort={p.onSort} label={t("Location")} className="w-[220px] max-lg:hidden" />}
            <Head sort={p.sort} onSort={p.onSort} k="updated" label={p.dateLabel ?? t("Date modified")} className="w-[170px] max-md:hidden" />
            <Head sort={p.sort} onSort={p.onSort} k="type" label={t("Type")} className="w-[120px] max-md:hidden" />
            <Head sort={p.sort} onSort={p.onSort} k="size" label={t("Size")} className="w-[100px]" />
            {p.showOwner && <Head sort={p.sort} onSort={p.onSort} label={t("Uploaded by")} className="w-[110px] max-md:hidden" />}
          </tr>
        </thead>
        <tbody>
          {p.items.map((item, i) => (
            <tr
              key={item.id}
              {...rowProps(item, i)}
              role="row"
              aria-rowindex={i + 2}
              className={cn(
                "cursor-default outline-none hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset aria-selected:bg-selection aria-selected:text-accent-foreground aria-selected:shadow-[inset_3px_0_0_var(--color-brand)]",
                dropTarget === item.id && "bg-brand/15",
                p.dimmed?.has(item.id) && "opacity-50",
              )}
            >
              {p.showCheckboxes && (
                <td role="gridcell" className={cn(td, "px-[7px]")}>
                  <input
                    type="checkbox"
                    className="align-middle accent-brand"
                    aria-label={t("Select {name}", { name: item.name })}
                    checked={p.selected.has(item.id)}
                    onClick={(e) => e.stopPropagation()}
                    onChange={() => toggle(i)}
                  />
                </td>
              )}
              <td role="gridcell" className={cn(td, "pl-3")}>
                <div data-drag-handle {...dragHandle(item, i)} className="flex w-fit max-w-full min-w-0 items-center gap-2" title={item.name}>
                  <FileIcon node={item} className="size-4 shrink-0" />
                  {renaming(item) ? (
                    renameBox(item)
                  ) : (
                    <>
                      <span className="truncate">{item.name}</span>
                      {star(item)}
                    </>
                  )}
                </div>
              </td>
              {p.showLocation && (
                <td role="gridcell" className={cn(td, "text-muted-foreground max-lg:hidden")} title={item.location}>
                  {item.location}
                </td>
              )}
              <td role="gridcell" className={cn(td, "text-muted-foreground max-md:hidden")}>{formatWinDate(dateOf(item))}</td>
              <td role="gridcell" className={cn(td, "text-muted-foreground max-md:hidden")} title={typeTitle(item)}>
                {typeLabel(item)}
              </td>
              <td role="gridcell" className={cn(td, "pr-3 text-right text-muted-foreground tabular-nums")}>{item.kind === "folder" ? "" : formatWinSize(item.size)}</td>
              {p.showOwner && <td role="gridcell" className={cn(td, "text-muted-foreground max-md:hidden")}>{item.owner_name}</td>}
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}
