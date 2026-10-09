/**
 * A large folder a part at a time. The list knows how many items the folder has, and loads only the parts ("windows"
 * of WINDOW items, asked for by their position) that are in view, with one more each way; scrolling or jumping (End, a
 * drag of the scroll bar) loads the parts it reaches, not everything before them. At most KEEP parts are kept per
 * folder, the ones nearest where the list is, so a very large folder never fills the page's memory. A folder, sort or
 * order left cancels the parts still loading for it. A smart folder lists the same way (`WindowSource`).
 */
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { useQueries, useQueryClient, type Query, type QueryKey } from "@tanstack/react-query";
import { api, type Node, type PositionedPage, type SortKey, type SortOrder } from "@/api";
import { keys } from "@/api/queryKeys";

/** Items per part */
export const WINDOW = 500;
/** Parts kept per folder listing: 10,000 items */
export const KEEP = 20;
/** Parts loaded ahead of those in view, each way */
const AHEAD = 1;

/** A large folder's items as far as they are loaded */
export interface SparseList<T> {
  /** Items by their position; empty where not loaded (as long as the folder: `total`) */
  at: readonly (T | undefined)[];
  /** How many items the folder has; -1 before the first part is loaded */
  total: number;
  /** The items loaded, in their order */
  loaded: T[];
  /** Where each loaded item is */
  index: ReadonlyMap<string, number>;
  /** Every item is loaded */
  complete: boolean;
  /** The positions shown: their parts load, with one more each way */
  show(first: number, last: number): void;
  /** Where an item is: at once when it is loaded, otherwise asked of the server; null when it isn't in the folder */
  locate(id: string): Promise<number | null>;
}

/** A part of a folder as cached: where it starts, its items, how many items the folder had, and when it was loaded */
export interface LoadedWindow<T> {
  start: number;
  items: readonly T[];
  total: number;
  at: number;
}

export interface Assembled<T> {
  at: (T | undefined)[];
  total: number;
  loaded: T[];
  /** Where each loaded item is */
  index: Map<string, number>;
  /** Parts loaded when the folder had another number of items: their positions are out of date */
  outdated: number[];
}

/**
 * The items of the parts loaded, by position. The newest part says how many items there are; parts loaded when there
 * were more or fewer are left out (items have moved since), and an item found twice (it moved between two parts loaded
 * at different times) is shown once, where the newer part has it.
 */
export function assemble<T extends { id: string }>(windows: readonly LoadedWindow<T>[]): Assembled<T> {
  if (!windows.length) return { at: [], total: -1, loaded: [], index: new Map(), outdated: [] };
  const newest = windows.reduce((a, b) => (b.at > a.at ? b : a));
  const total = newest.total;
  const current = windows.filter((w) => w.total === total).sort((a, b) => b.at - a.at);
  const at: (T | undefined)[] = new Array(total);
  const index = new Map<string, number>();
  for (const w of current) {
    w.items.forEach((n, k) => {
      const i = w.start + k;
      if (i >= total || at[i] !== undefined || index.has(n.id)) return;
      at[i] = n;
      index.set(n.id, i);
    });
  }
  const loaded = [...index.values()].sort((a, b) => a - b).map((i) => at[i]!);
  return { at, total, loaded, index, outdated: windows.filter((w) => w.total !== total).map((w) => w.start) };
}

/**
 * The files just before and after the one at `at` (folders are passed over), for Previous and Next; none where the list
 * isn't loaded that far yet
 */
export function neighbours<T extends { kind: string }>(items: readonly (T | undefined)[], at: number | null): { prev?: T; next?: T } {
  if (at === null || at < 0) return {};
  const find = (from: number, step: 1 | -1) => {
    for (let i = from; i >= 0 && i < items.length; i += step) {
      const n = items[i];
      if (!n) return undefined;
      if (n.kind === "file") return n;
    }
    return undefined;
  };
  return { prev: find(at - 1, -1), next: find(at + 1, 1) };
}

/** The parts to keep of those cached: the first (where the list starts), and the others nearest the parts shown */
export function toKeep(starts: readonly number[], shown: [number, number], keep = KEEP): Set<number> {
  const mid = ((shown[0] + shown[1]) / 2) * WINDOW;
  const byNearness = starts.filter((s) => s !== 0).sort((a, b) => Math.abs(a - mid) - Math.abs(b - mid));
  return new Set([0, ...byNearness.slice(0, keep - 1)].filter((s) => starts.includes(s)));
}

/** The cache key of a folder's parts; the part's start follows it */
export const windowsKey = (folder: string | undefined, sort: SortKey, order: SortOrder): QueryKey => keys.childrenAt(folder, sort, order);

const startOf = (q: Query) => q.queryKey[5] as number;

/** Whether `key` begins with `prefix` (whose parts are plain values: ids, sort keys): compared part by part, since the
 * cache tells every subscriber of every change while files arrive */
export function startsWith(key: QueryKey, prefix: QueryKey): boolean {
  if (key.length < prefix.length) return false;
  for (let i = 0; i < prefix.length; i++) if (key[i] !== prefix[i]) return false;
  return true;
}

/** Changes the version when a part of this listing is added, changed or removed */
function useCacheVersion(key: QueryKey) {
  const qc = useQueryClient();
  const version = useRef(0);
  const text = JSON.stringify(key);
  // The key as it is (undefined parts too), for the listing `text` names
  const current = useRef(key);
  current.current = key;
  const subscribe = useCallback(
    (onChange: () => void) => {
      const prefix = current.current;
      return qc.getQueryCache().subscribe((e) => {
        if (e.type !== "added" && e.type !== "removed" && e.type !== "updated") return;
        if (!startsWith(e.query.queryKey, prefix)) return;
        version.current++;
        onChange();
      });
    },
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- `text` stands for the key
    [qc, text],
  );
  return useSyncExternalStore(subscribe, () => version.current);
}

/** Where a list's parts come from: a folder's items, or what a smart folder holds */
export interface WindowSource<T> {
  /** The cache key of the parts (5 parts long); the part's start follows it */
  key: QueryKey;
  /** The part from `start` on, with how many items the list has */
  part(start: number, limit: number, signal: AbortSignal): Promise<PositionedPage<T>>;
  /** Where an item is in the list (null: not in it) */
  position(id: string): Promise<number | null>;
}

/** A folder's items, a part at a time (see the top of this file) */
export function useFolderWindows(folder: string | undefined, sort: SortKey, order: SortOrder, enabled = true) {
  const source = useMemo<WindowSource<Node>>(
    () => ({
      key: windowsKey(folder, sort, order),
      part: (start, limit, signal) => api.childrenAt(folder!, sort, order, start, limit, signal),
      position: async (id) => (folder ? (await api.position(folder, id, sort, order)).position : null),
    }),
    [folder, sort, order],
  );
  return useWindows(source, enabled && !!folder);
}

/** A list's items, a part at a time (see the top of this file) */
export function useWindows<T extends { id: string }>(source: WindowSource<T>, enabled = true) {
  const qc = useQueryClient();
  const key = source.key;
  const text = JSON.stringify(key);
  /** The parts in view, by number */
  const [shown, setShown] = useState<[number, number]>([0, 0]);
  const [shownFor, setShownFor] = useState(text);
  if (shownFor !== text) {
    // Another folder or order starts at its top
    setShownFor(text);
    setShown([0, 0]);
  }
  const version = useCacheVersion(key);
  const built = useMemo(
    () =>
      assemble(
        qc
          .getQueryCache()
          .findAll({ queryKey: key })
          .filter((q) => q.queryKey.length === 6 && q.state.data)
          .map((q) => {
            const d = q.state.data as PositionedPage<T>;
            return { start: startOf(q), items: d.items, total: d.total, at: q.state.dataUpdatedAt };
          }),
      ),
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the version says when the cached parts changed
    [qc, text, version],
  );
  const parts = built.total >= 0 ? Math.max(1, Math.ceil(built.total / WINDOW)) : 1;
  const first = Math.min(parts - 1, Math.max(0, shown[0] - AHEAD));
  const last = Math.min(parts - 1, shown[1] + AHEAD);
  const starts: number[] = [];
  for (let w = first; w <= last; w++) starts.push(w * WINDOW);
  const results = useQueries({
    queries: starts.map((start) => ({
      queryKey: [...key, start],
      queryFn: ({ signal }: { signal: AbortSignal }) => source.part(start, WINDOW, signal),
      enabled,
    })),
  });

  // Parts loaded before the number of items changed: those in view load again, the others are forgotten
  const outdated = built.outdated.join();
  useEffect(() => {
    if (!outdated) return;
    const stale = new Set(outdated.split(",").map(Number));
    const predicate = (q: Query) => q.queryKey.length === 6 && stale.has(startOf(q));
    qc.removeQueries({ queryKey: key, predicate, type: "inactive" });
    void qc.invalidateQueries({ queryKey: key, predicate });
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the key is the text
  }, [qc, text, outdated]);

  // At most KEEP parts are kept: those farthest from the ones shown go
  useEffect(() => {
    const cached = qc
      .getQueryCache()
      .findAll({ queryKey: key })
      .filter((q) => q.queryKey.length === 6);
    if (cached.length <= KEEP) return;
    const keep = toKeep(cached.map(startOf), shown);
    for (const q of cached) if (!keep.has(startOf(q)) && !q.getObserversCount()) qc.removeQueries({ queryKey: q.queryKey, exact: true });
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the key is the text
  }, [qc, text, version, shown]);

  const show = useCallback((firstItem: number, lastItem: number) => {
    const next: [number, number] = [Math.floor(Math.max(0, firstItem) / WINDOW), Math.floor(Math.max(0, lastItem) / WINDOW)];
    setShown((cur) => (cur[0] === next[0] && cur[1] === next[1] ? cur : next));
  }, []);
  const index = built.index;
  const locate = useCallback(
    async (id: string) => {
      const at = index.get(id);
      if (at !== undefined) return at;
      return source.position(id);
    },
    [index, source],
  );
  const firstPart = results[0];
  const list: SparseList<T> = {
    at: built.at,
    total: built.total,
    loaded: built.loaded,
    index: built.index,
    complete: built.total >= 0 && built.loaded.length >= built.total,
    show,
    locate,
  };
  return {
    list,
    isLoading: built.total < 0 && !!firstPart?.isLoading,
    /** The folder couldn't be listed (its first part failed) */
    error: built.total < 0 ? (firstPart?.error ?? null) : null,
    /** A part in view couldn't be loaded: retrying loads it again (in its place, so nothing shows twice) */
    partError: built.total >= 0 ? (results.find((r) => r.error)?.error ?? null) : null,
    loadingMore: results.some((r) => r.isFetching),
    retry: () => qc.invalidateQueries({ queryKey: key, predicate: (q) => q.queryKey.length === 6 && (!!q.state.error || q.getObserversCount() > 0) }),
  };
}
