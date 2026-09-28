import { useEffect, useMemo } from "react";
import { useInfiniteQuery, type InfiniteData, type QueryClient, type QueryKey } from "@tanstack/react-query";
import type { CursorPage } from "@/api";

/** A small first page shows a folder at once; the rest follows in larger pages */
export const FIRST_PAGE = 200;
const NEXT_PAGES = 2000;

type FetchPage<T> = (limit: number, after: string | undefined, signal: AbortSignal) => Promise<CursorPage<T>>;

/**
 * A folder or the trash, page by page: the first page shows right away, the others load one after another in the
 * background until the list is complete (so Select all covers everything), or until `enough` says the items loaded so
 * far will do. A refresh reloads the pages the same way. With `reuse`, pages already loaded aren't loaded again when
 * this starts using them.
 */
export function useAllPages<T extends { id: string }>(
  queryKey: QueryKey,
  fetchPage: FetchPage<T>,
  enabled = true,
  { enough, reuse = false }: { enough?: (items: T[]) => boolean; reuse?: boolean } = {},
) {
  const q = useInfiniteQuery({
    queryKey,
    queryFn: ({ pageParam, signal }) => fetchPage(pageParam ? NEXT_PAGES : FIRST_PAGE, pageParam ?? undefined, signal),
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.next,
    enabled,
    // Loaded pages stay as they are until a change marks them out of date
    ...(reuse ? { staleTime: Infinity } : {}),
  });
  const { hasNextPage, isFetchingNextPage, isError, fetchNextPage } = q;
  const items = useMemo(() => allItems(q.data), [q.data]);
  const done = !!enough?.(items);
  useEffect(() => {
    if (hasNextPage && !isFetchingNextPage && !isError && !done) void fetchNextPage();
  }, [hasNextPage, isFetchingNextPage, isError, done, fetchNextPage]);
  return { items, isLoading: q.isLoading, error: q.error, loadingMore: !!hasNextPage && !done, complete: !!q.data && !hasNextPage, refetch: q.refetch };
}

/** The items of every page loaded so far; an item renamed between two pages can come twice, so each is kept once */
export function allItems<T extends { id: string }>(data: InfiniteData<CursorPage<T>> | undefined): T[] {
  if (!data) return [];
  if (data.pages.length === 1) return data.pages[0].items;
  const seen = new Set<string>();
  return data.pages.flatMap((p) => p.items.filter((n) => !seen.has(n.id) && !!seen.add(n.id)));
}

/** The items loaded so far reach past the file `id`, so both files next to it are known (for Previous and Next) */
export function hasFileAfter(items: readonly { id: string; kind: string }[], id: string | undefined): boolean {
  if (!id) return false;
  const at = items.findIndex((n) => n.id === id);
  return at >= 0 && items.slice(at + 1).some((n) => n.kind === "file");
}

/**
 * A list with its first page replaced by a newer copy. The items the old first page had are kept after the new ones
 * (one pushed out of the first page would otherwise be in neither page), and so is where the next page starts.
 */
export function withFirstPage<T extends { id: string }>(data: InfiniteData<CursorPage<T>>, fresh: CursorPage<T>): InfiniteData<CursorPage<T>> {
  const [old, ...rest] = data.pages;
  if (!old) return data;
  const ids = new Set(fresh.items.map((n) => n.id));
  const items = [...fresh.items, ...old.items.filter((n) => !ids.has(n.id))];
  return { ...data, pages: [{ ...old, items }, ...rest] };
}

/**
 * Reload only the first page of the lists shown under `queryKey` (e.g. while files are uploaded into a big folder, rather
 * than every page again). A list that is loading already is left alone: its answer would overwrite this one.
 */
export async function refreshFirstPage<T extends { id: string }>(
  qc: QueryClient,
  queryKey: QueryKey,
  fetchFirst: (key: QueryKey, limit: number) => Promise<CursorPage<T>> | undefined,
) {
  const lists = qc.getQueryCache().findAll({ queryKey, type: "active" });
  await Promise.all(
    lists.map(async (query) => {
      const data = query.state.data as InfiniteData<CursorPage<T>> | undefined;
      if (!data?.pages || query.state.fetchStatus !== "idle") return;
      const request = fetchFirst(query.queryKey, FIRST_PAGE);
      if (!request) return;
      const fresh = await request.catch(() => undefined);
      if (!fresh || query.state.fetchStatus !== "idle") return;
      qc.setQueryData<InfiniteData<CursorPage<T>>>(query.queryKey, (d) => (d?.pages ? withFirstPage(d, fresh) : d));
    }),
  );
}
