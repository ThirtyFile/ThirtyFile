/**
 * The Columns view: a column for each folder level, from the top of the location down to the open folder, and the
 * folder selected in it opened in the next column to the right (a file selected shows a preview column there).
 *
 * The open folder's column is the explorer's own list: its selection, file operations, renaming and large-folder
 * loading are the ones every view has. The other columns show their folders a part at a time too (lib/windows); using
 * one of them (a click, the keys) opens its folder, with what was used selected there (lib/columns `arriveAt`). Each
 * column is a listbox named after its folder, and moving to another column is announced.
 */
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type DragEvent, type MouseEvent, type ReactNode } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api, privateSource, type Crumb, type Node, type PositionedPage } from "@/api";
import { keys } from "@/api/queryKeys";
import type { ExplorerProps } from "@/components/Explorer";
import type { ExplorerActions } from "@/components/explorer/actions";
import type { ExplorerState } from "@/components/explorer/state";
import { FileList, type FileListProps, type ListNav } from "@/components/FileList";
import { useSettled } from "@/lib/useSettled";
import { ItemError } from "@/components/ErrorState";
import { Resizer } from "@/components/Resizer";
import { Skeleton } from "@/components/ui/skeleton";
import {
  arriveAt,
  COLUMN_WIDTH,
  depthOf,
  keepTrail,
  keptTrail,
  MAX_COLUMN_WIDTH,
  MIN_COLUMN_WIDTH,
  openFrom,
  PREVIEW,
  PREVIEW_WIDTH,
  selectsOnArrival,
  trailFor,
  validWidths,
  withWidth,
  type Arrival,
  type ColumnWidths,
  type Trail,
} from "@/lib/columns";
import { carriesItems, dropEffect, dropItems, droppedIds, useFolderDrop } from "@/lib/dnd";
import { t } from "@/lib/i18n";
import { wantsCopy } from "@/lib/keys";
import { usePersisted } from "@/lib/session";
import { columnWidthsKey } from "@/lib/signOut";
import { cn } from "@/lib/utils";
import { useFolderWindows, windowsKey } from "@/lib/windows";
import { PreviewColumn } from "./PreviewColumn";

/** How long a folder stays selected before the next column loads it (holding an arrow key passes many) */
const SETTLE_MS = 150;

/** The key of the width of a list that isn't a folder's (search results, Recent…) */
const LIST = "list";

export function ColumnsView({ p, s, a, list }: { p: ExplorerProps; s: ExplorerState; a: ExplorerActions; list: FileListProps }) {
  const qc = useQueryClient();
  const [widths, setWidths] = usePersisted<ColumnWidths>(columnWidthsKey(s.me.id), {}, validWidths);
  const widthOf = (id: string) => widths[id] ?? (id === PREVIEW ? PREVIEW_WIDTH : COLUMN_WIDTH);
  const resize = (id: string, w: number | undefined) => setWidths(withWidth(widths, id, w));

  // The trail: kept from folder to folder (and while the page is open), and set again by each folder's path
  const [own, setOwn] = useState<Trail>(() => keptTrail() ?? []);
  const folderPage = p.trail !== undefined;
  const base = useMemo(() => (!folderPage ? [] : p.trail ? trailFor(own, p.trail) : own), [folderPage, own, p.trail]);
  /** The open folder's column (-1: none, while another folder loads, or in a list that isn't a folder) */
  const here = folderPage ? depthOf(base, p.folderId) : -1;
  const single = s.single;
  const opened: Crumb | null = single?.kind === "folder" ? { id: single.id, name: single.name } : null;
  // Selecting something else in the open folder's column replaces the columns to its right; nothing selected keeps them
  const trail = here >= 0 && s.count > 0 ? openFrom(base, here, opened) : base;
  useEffect(() => {
    if (!folderPage) return;
    if (trail !== own) setOwn(trail);
    keepTrail(trail);
  }, [folderPage, trail, own]);

  // The folder columns shown: up to the open folder, then the folder selected in it (and those kept beyond it). With
  // nothing selected, the columns beyond show only while what was open there is about to be selected again
  const keepBeyond = s.count > 0 || s.arriving() || selectsOnArrival(p.folderId);
  const shown: Trail = !folderPage ? (opened ? [opened] : []) : here < 0 || keepBeyond ? trail : trail.slice(0, here + 1);
  const preview = single?.kind === "file" && (here >= 0 || !folderPage) ? single : null;

  /** Opens the folder of another column, with `how` done there (lib/columns) */
  const go = (folder: string, how: Omit<Arrival, "folder"> = {}) => {
    arriveAt({ folder, ...how });
    s.navigate(`/files/${folder}`);
  };
  /**
   * The keys for the column before (-1) and the next one (1), on `item` in the column at `depth` of `shown` (in a list
   * that isn't a folder's: -1 for the list, its folder columns as if they started after it)
   */
  const column = (depth: number, dir: -1 | 1, item: Node) => {
    if (dir < 0) {
      if (depth > 0) go(shown[depth - 1].id, { select: shown[depth].id, focus: true });
      return;
    }
    if (item.kind !== "folder") return;
    // An empty folder has nothing to go to (as far as its column has loaded it)
    const sort = p.sort ?? { key: "name", order: "asc" };
    const first = qc.getQueryData<PositionedPage<Node>>([...windowsKey(item.id, sort.key, sort.order), 0]);
    if (first && first.total === 0) return;
    // Back into a folder whose columns were kept: what was open in it is selected again
    const next = trail[depth + 1]?.id === item.id ? trail[depth + 2]?.id : undefined;
    go(item.id, next ? { select: next, focus: true } : { first: true, focus: true });
  };

  // A folder that stays selected: its column loads, and what the page needs to open it is asked for beforehand
  const settled = useSettled(opened?.id, SETTLE_MS);
  useEffect(() => {
    if (settled) void qc.prefetchQuery({ queryKey: keys.node(settled), queryFn: ({ signal }) => api.node(settled, signal) });
  }, [qc, settled]);

  // The newest column is brought into view, as long as the open folder's stays in view too
  const box = useRef<HTMLDivElement>(null);
  const last = preview ? `preview:${preview.id}` : (shown.at(-1)?.id ?? "");
  useLayoutEffect(() => {
    const el = box.current;
    const scroller = el?.parentElement;
    const end = el?.lastElementChild;
    if (!el || !scroller || !end) return;
    const view = scroller.getBoundingClientRect();
    const over = end.getBoundingClientRect().right - view.right;
    if (over <= 0) return;
    const open = el.querySelector("[data-open-column]")?.getBoundingClientRect();
    scroller.scrollLeft += open ? Math.min(over, Math.max(0, open.left - view.left)) : over;
  }, [last, here]);

  // Moving to another column is announced (the list's own name is read too, once an item in it has the focus)
  const [said, setSaid] = useState("");
  const hereName = here >= 0 ? base[here].name : "";
  const announced = useRef<number | null>(null);
  useEffect(() => {
    if (here < 0) return;
    if (announced.current !== null && announced.current !== here) setSaid(t("Column {n}: {name}", { n: here + 1, name: hereName }));
    announced.current = here;
  }, [here, hereName]);

  /** A drag from a column doesn't select what it picks up: that would close the columns it may be dropped on */
  const dragging = useRef(false);

  const columns = folderPage
    ? shown.map((c, depth) => ({ folder: c, depth, open: depth === here }))
    : [{ folder: null, depth: -1, open: true }, ...shown.map((c) => ({ folder: c, depth: 0, open: false }))];

  return (
    <div ref={box} className="flex h-full w-max min-w-full">
      <div role="status" className="sr-only">
        {said}
      </div>
      {columns.map(({ folder, depth, open }) => {
        const key = folder?.id ?? LIST;
        return (
          <Column
            key={key}
            folder={folder}
            open={open}
            // The folder open in the next column
            next={open ? null : (shown[depth + 1]?.id ?? null)}
            label={folder?.name ?? p.crumbs.at(-1)?.label ?? t("Items")}
            p={p}
            s={s}
            a={a}
            list={list}
            // The folder just selected loads once it stays selected
            enabled={folder?.id !== opened?.id || settled === folder?.id}
            width={widthOf(key)}
            onResize={(w) => resize(key, w)}
            dragging={dragging}
            onGo={(how) => folder && go(folder.id, how)}
            onColumn={(dir, item) => column(depth, dir, item)}
            onOpenFolder={(item) => go(item.id, { first: true, focus: true })}
          />
        );
      })}
      {preview && (
        <ColumnBox width={widthOf(PREVIEW)} onResize={(w) => resize(PREVIEW, w)} label={t("Preview")} data-no-marquee>
          <PreviewColumn node={preview} s={s} a={a} />
        </ColumnBox>
      )}
    </div>
  );
}

/** A column: its width (with a handle to change it), and a box that scrolls what it holds */
function ColumnBox({
  width,
  onResize,
  label,
  children,
  className,
  tabStop = true,
  ...rest
}: {
  width: number;
  onResize(width: number | undefined): void;
  /** Names the handle */
  label: string;
  children: ReactNode;
  className?: string;
  /** Its handle is a Tab stop: the open folder's column's is, so Tab goes from its items to its handle */
  tabStop?: boolean;
} & Record<string, unknown>) {
  return (
    <div {...rest} className={cn("relative h-full shrink-0 border-r", className)} style={{ width }}>
      <div className="h-full overflow-y-auto">{children}</div>
      <Resizer
        width={width}
        onChange={onResize}
        onReset={() => onResize(undefined)}
        min={MIN_COLUMN_WIDTH}
        max={MAX_COLUMN_WIDTH}
        defaultWidth={width}
        edge="right"
        label={t('Resize the "{name}" column', { name: label })}
        untabbable={!tabStop}
      />
    </div>
  );
}

const loading = (rows: number) => (
  <div className="grid gap-1.5 p-2" aria-busy>
    {Array.from({ length: rows }, (_, i) => (
      <Skeleton key={i} className="h-5 w-full" />
    ))}
  </div>
);

/**
 * A folder's column. The open folder's (`open`) is the explorer's list. Another folder's shows the folder open in the
 * next column (`next`) selected, and using it opens its folder (`onGo`), with the item used selected there.
 */
function Column({
  folder,
  open,
  next,
  label,
  p,
  s,
  a,
  list,
  enabled,
  width,
  onResize,
  dragging,
  onGo,
  onColumn,
  onOpenFolder,
}: {
  /** None: a list that isn't a folder's (it is then the open column) */
  folder: Crumb | null;
  open: boolean;
  next: string | null;
  label: string;
  p: ExplorerProps;
  s: ExplorerState;
  a: ExplorerActions;
  list: FileListProps;
  enabled: boolean;
  width: number;
  onResize(width: number | undefined): void;
  dragging: { current: boolean };
  onGo(how: Omit<Arrival, "folder">): void;
  onColumn(dir: -1 | 1, item: Node): void;
  onOpenFolder(item: Node): void;
}) {
  const sort = p.sort ?? { key: "name" as const, order: "asc" as const };
  // The open folder's items are the explorer's; another folder's are loaded here, the same way
  const windows = useFolderWindows(folder?.id, sort.key, sort.order, enabled && !open && !!folder);
  const items = windows.list;
  const nav = useRef<ListNav | null>(null);
  const marked = useMemo(() => new Set(next ? [next] : []), [next]);
  // Items and files dropped on the empty space of another folder's column go into that folder
  const { dropping, dropProps } = useFolderDrop(!open && folder && s.caps.write ? folder : null, { onDropped: () => s.setSelected(new Set()) });
  const ownIds = useMemo(() => new Set(p.items.map((n) => n.id)), [p.items]);

  // The folder open in the next column is brought into view (in a large folder, its part loads)
  const ready = items.total >= 0;
  useEffect(() => {
    if (open || !next || !ready) return;
    if (items.index.has(next)) nav.current?.show(next, false);
    else
      void items
        .locate(next)
        .then((at) => at !== null && nav.current?.scrollTo(at))
        .catch(() => undefined);
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- once the column has loaded, and when another folder opens next to it
  }, [open, next, ready]);

  // Named after its folder, but not a list: a list box may hold only its items, not a message
  const empty = (
    <div role="group" aria-label={label} className="px-3 py-4 text-xs text-muted-foreground">
      {t("This folder is empty.")}
    </div>
  );
  let body: ReactNode;
  let boxProps: Record<string, unknown>;
  if (open) {
    if (p.loading) body = loading(6);
    else if (p.error) body = <ItemError error={p.error} kind="folder" onRetry={a.refresh} />;
    else
      body = (
        <FileList
          {...list}
          label={label}
          view="columns"
          groupBy="none"
          onSelect={(chosen, at, span) => !dragging.current && list.onSelect(chosen, at, span)}
          onColumn={onColumn}
          empty={empty}
        />
      );
    // Items dragged from another column onto the empty space of this one move here (files from the computer are the
    // explorer's: they upload here)
    boxProps =
      folder && s.caps.write
        ? {
            onDragOver: (e: DragEvent) => {
              if (!carriesItems(e.dataTransfer)) return;
              e.preventDefault();
              e.dataTransfer.dropEffect = dropEffect(e);
            },
            onDrop: (e: DragEvent) => {
              const ids = droppedIds(e.dataTransfer);
              if (!ids) return;
              e.preventDefault();
              e.stopPropagation();
              if (ids.every((id) => ownIds.has(id))) return;
              void dropItems(s.qc, ids, folder, wantsCopy(e)).then(() => s.setSelected(new Set()));
            },
          }
        : {};
    boxProps["data-open-column"] = true;
  } else {
    if (windows.error) body = <ItemError error={windows.error} kind="folder" onRetry={() => void windows.retry()} />;
    else if (!ready) body = loading(4);
    else
      body = (
        <FileList
          items={items.at}
          onShow={items.show}
          view="columns"
          source={privateSource}
          selected={marked}
          anchor={next}
          inactive
          label={label}
          navRef={nav}
          // A click or a key: this folder opens, with the item used selected
          onSelect={(chosen, at) => {
            if (dragging.current) return;
            const id = at ?? [...chosen][0];
            onGo(id ? { select: id, focus: true } : {});
          }}
          onOpen={(n, byKey) => (n.kind === "folder" ? onOpenFolder(n) : a.open(n, byKey))}
          onOpenInNewTab={list.onOpenInNewTab}
          onColumn={onColumn}
          dimmed={list.dimmed}
          onDropInto={list.onDropInto}
          onUploadInto={list.onUploadInto}
          empty={empty}
        />
      );
    boxProps = {
      ...dropProps,
      "data-drop-folder": true,
      // Holding the button down here doesn't select with a box: only the open folder's column does (useMarquee)
      "data-no-marquee": true,
      onClick: (e: MouseEvent) => {
        // The empty space of the column: its folder opens with nothing selected
        if (!(e.target as HTMLElement).closest("[data-node-id]")) onGo({});
      },
      // A right-click: the folder opens with the item selected, and the item's menu opens there
      onContextMenuCapture: (e: MouseEvent) => {
        e.preventDefault();
        e.stopPropagation();
        const id = (e.target as HTMLElement).closest<HTMLElement>("[data-node-id]")?.dataset.nodeId;
        onGo(id ? { select: id, focus: true, menuAt: { x: e.clientX, y: e.clientY } } : {});
      },
    };
  }
  return (
    <ColumnBox
      width={width}
      onResize={onResize}
      label={label}
      tabStop={open}
      className={cn(open ? "bg-background" : "bg-muted/20", dropping && "bg-brand/10")}
      onDragStartCapture={() => (dragging.current = true)}
      onDragEnd={() => (dragging.current = false)}
      onMouseDown={() => (dragging.current = false)}
      {...boxProps}
    >
      {body}
    </ColumnBox>
  );
}
