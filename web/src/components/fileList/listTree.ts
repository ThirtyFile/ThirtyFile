/**
 * Folders that expand in place in the List view (the Mac style; `disclosure` in components/style): an expanded folder's
 * items show under it, indented, and so on down. A large folder expanded loads a part at a time, as the list itself
 * does (lib/windows): its rows show where it has items, and the parts that come into view load.
 *
 * The rows are the list's items with each expanded folder's items after it, in one list by position, so selecting,
 * the keyboard, dragging and the file operations work on them as on any item.
 */
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { useQueries, useQueryClient } from "@tanstack/react-query";
import { api, type Node, type PositionedPage, type SortKey, type SortOrder } from "@/api";
import { assemble, WINDOW, windowsKey } from "@/lib/windows";
import type { Item } from "./layout";

/** Parts loaded ahead of those in view, each way */
const AHEAD = 1;

/** A row of the tree: its item (none until loaded), how deep it is, and where it is in its folder's list */
export interface TreeRow {
  item: Item | undefined;
  depth: number;
  /** The expanded folder it is in; null: the list itself */
  folder: string | null;
  pos: number;
}

/** What the list needs to show the tree: each row's depth, which folders are expanded, and expanding them */
export interface ListTreeView {
  /** A folder is expanded: the rows aren't only the list's own, so a range of rows is the rows loaded (not a span of the list) */
  expanded: boolean;
  depth(index: number): number;
  isOpen(id: string): boolean;
  /** Expands or collapses a folder */
  toggle(id: string, open: boolean): void;
}

/** A folder's items as far as they are loaded (-1 items: not known yet) */
export interface Branch {
  at: readonly (Item | undefined)[];
  total: number;
}

/**
 * The rows: `base` (the list's items, by position), with the items of each expanded folder in `open` after it. A
 * folder whose items aren't known yet has one row to show it is loading.
 */
export function flatten(base: readonly (Item | undefined)[], open: ReadonlySet<string>, branches: ReadonlyMap<string, Branch>): TreeRow[] {
  const rows: TreeRow[] = [];
  const walk = (items: readonly (Item | undefined)[], depth: number, folder: string | null, seen: ReadonlySet<string>) => {
    // Not forEach: the items not loaded are holes in the list, which it would pass over
    for (let pos = 0; pos < items.length; pos++) {
      const item = items[pos];
      rows.push({ item, depth, folder, pos });
      if (!item || item.kind !== "folder" || !open.has(item.id) || seen.has(item.id)) continue;
      const branch = branches.get(item.id);
      if (!branch || branch.total < 0) rows.push({ item: undefined, depth: depth + 1, folder: item.id, pos: 0 });
      else walk(branch.at, depth + 1, item.id, new Set([...seen, item.id]));
    }
  };
  walk(base, 0, null, new Set());
  return rows;
}

/**
 * The positions in view (rows `first` to `last`) for each list they show: the list itself (null) and each expanded
 * folder. The list keeps showing at least the folder row the rows in view are under.
 */
export function shownByFolder(rows: readonly TreeRow[], first: number, last: number): Map<string | null, [number, number]> {
  const out = new Map<string | null, [number, number]>();
  const add = (folder: string | null, pos: number) => {
    const r = out.get(folder);
    out.set(folder, r ? [Math.min(r[0], pos), Math.max(r[1], pos)] : [pos, pos]);
  };
  for (let i = Math.max(0, first); i <= last && i < rows.length; i++) add(rows[i].folder, rows[i].pos);
  if (!out.has(null) && rows.length) {
    // Rows in view all inside an expanded folder: the list's row above them
    for (let i = Math.min(first, rows.length - 1); i >= 0; i--)
      if (rows[i].folder === null) {
        add(null, rows[i].pos);
        break;
      }
  }
  return out;
}

/** The parts of a list of `total` items (unknown: -1) to load for the positions shown */
export function partsFor(total: number, shown: [number, number] | undefined): number[] {
  const parts = total >= 0 ? Math.max(1, Math.ceil(total / WINDOW)) : 1;
  const [a, b] = shown ? [Math.floor(shown[0] / WINDOW), Math.floor(shown[1] / WINDOW)] : [0, 0];
  const out: number[] = [];
  for (let w = Math.max(0, a - AHEAD); w <= Math.min(parts - 1, b + AHEAD); w++) out.push(w * WINDOW);
  return out;
}

/** The folders expanded in each list, kept while the page is open: going back to a list shows them expanded again */
const kept = new Map<string, string[]>();

/** Changes the version when a part of a folder's listing is added, changed or removed */
function useListingsVersion() {
  const qc = useQueryClient();
  const version = useRef(0);
  const subscribe = useCallback(
    (onChange: () => void) =>
      qc.getQueryCache().subscribe((e) => {
        if (e.type !== "added" && e.type !== "removed" && e.type !== "updated") return;
        const key = e.query.queryKey;
        if (key.length !== 6 || key[0] !== "children" || key[4] !== "at") return;
        version.current++;
        onChange();
      }),
    [qc],
  );
  return useSyncExternalStore(subscribe, () => version.current);
}

export interface ListTree {
  /** The tree is in use (the view has it, and a folder is expanded) */
  active: boolean;
  /** The rows' items, by position */
  rows: readonly (Item | undefined)[];
  /** The items of expanded folders that are loaded */
  children: readonly Item[];
  view: ListTreeView | undefined;
  /** The rows in view: the parts they need load */
  show(first: number, last: number): void;
  /** The rows under an item (its expanded folder's, and theirs) */
  under(id: string): readonly Item[];
}

/**
 * The list `base` (by position) with its expanded folders' items. `enabled`: the view has folders that expand; `place`
 * is the list and its order (expanded folders are kept for each); `show` is the list's own (a large folder's parts).
 */
export function useListTree(o: {
  enabled: boolean;
  place: string;
  base: readonly (Item | undefined)[];
  show?: (first: number, last: number) => void;
  sort?: { key: SortKey; order: SortOrder };
}): ListTree {
  const { enabled, place, base, show: baseShow } = o;
  const sort = o.sort ?? { key: "name" as SortKey, order: "asc" as SortOrder };
  const qc = useQueryClient();
  const [open, setOpen] = useState<readonly string[]>(() => kept.get(place) ?? []);
  const [openFor, setOpenFor] = useState(place);
  /** The positions in view of each expanded folder */
  const [shown, setShown] = useState<ReadonlyMap<string, [number, number]>>(new Map());
  if (openFor !== place) {
    setOpenFor(place);
    setOpen(kept.get(place) ?? []);
    setShown(new Map());
  }
  useEffect(() => void kept.set(place, [...open]), [place, open]);
  const openSet = useMemo(() => new Set(open), [open]);
  const active = enabled && open.length > 0;

  // Each expanded folder's items as cached; the folders shown are those whose parts are loaded
  const version = useListingsVersion();
  const branches = useMemo(() => {
    const out = new Map<string, Branch>();
    if (!active) return out;
    for (const id of open) {
      const key = windowsKey(id, sort.key, sort.order);
      const windows = qc
        .getQueryCache()
        .findAll({ queryKey: key })
        .filter((q) => q.queryKey.length === 6 && q.state.data)
        .map((q) => {
          const d = q.state.data as PositionedPage<Node>;
          return { start: q.queryKey[5] as number, items: d.items, total: d.total, at: q.state.dataUpdatedAt };
        });
      if (windows.length) out.set(id, assemble(windows));
    }
    return out;
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the version says when the cached parts changed
  }, [qc, active, open, sort.key, sort.order, version]);

  const rows = useMemo(() => (active ? flatten(base, openSet, branches) : null), [active, base, openSet, branches]);
  /** The expanded folders shown (not inside a collapsed one): their parts in view load */
  const visible = useMemo(() => {
    if (!rows) return [];
    const at = new Set<string>();
    for (const r of rows) if (r.item && openSet.has(r.item.id)) at.add(r.item.id);
    return [...at];
  }, [rows, openSet]);
  useQueries({
    queries: visible.flatMap((id) =>
      partsFor(branches.get(id)?.total ?? -1, shown.get(id)).map((start) => ({
        queryKey: [...windowsKey(id, sort.key, sort.order), start],
        queryFn: ({ signal }: { signal: AbortSignal }) => api.childrenAt(id, sort.key, sort.order, start, WINDOW, signal),
      })),
    ),
  });

  const show = useCallback(
    (first: number, last: number) => {
      if (!rows) return baseShow?.(first, last);
      const byFolder = shownByFolder(rows, first, last);
      const own = byFolder.get(null);
      if (own) baseShow?.(own[0], own[1]);
      setShown((cur) => {
        let changed = false;
        const next = new Map(cur);
        for (const [folder, range] of byFolder) {
          if (folder === null) continue;
          const was = cur.get(folder);
          // Only a change of part counts
          if (was && Math.floor(was[0] / WINDOW) === Math.floor(range[0] / WINDOW) && Math.floor(was[1] / WINDOW) === Math.floor(range[1] / WINDOW)) continue;
          next.set(folder, range);
          changed = true;
        }
        return changed ? next : cur;
      });
    },
    [rows, baseShow],
  );

  const toggle = useCallback((id: string, expand: boolean) => setOpen((now) => (expand ? (now.includes(id) ? now : [...now, id]) : now.filter((x) => x !== id))), []);
  const view = useMemo<ListTreeView | undefined>(
    () => (enabled ? { expanded: active, depth: (index) => rows?.[index]?.depth ?? 0, isOpen: (id) => active && openSet.has(id), toggle } : undefined),
    [enabled, active, rows, openSet, toggle],
  );
  const children = useMemo(() => (rows ? rows.flatMap((r) => (r.depth > 0 && r.item ? [r.item] : [])) : []), [rows]);
  const items = useMemo(() => (rows ? rows.map((r) => r.item) : base), [rows, base]);
  const under = useCallback(
    (id: string) => {
      if (!rows) return [];
      const at = rows.findIndex((r) => r.item?.id === id);
      if (at < 0) return [];
      const out: Item[] = [];
      for (let i = at + 1; i < rows.length && rows[i].depth > rows[at].depth; i++) if (rows[i].item) out.push(rows[i].item!);
      return out;
    },
    [rows],
  );
  return { active, rows: items, children, view, show, under };
}
