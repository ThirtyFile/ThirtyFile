import { useEffect, useMemo } from "react";
import { useInfiniteQuery, type InfiniteData, type QueryKey } from "@tanstack/react-query";
import type { CursorPage } from "@/api";

/** A small first page shows a folder at once; the rest follows in larger pages */
const FIRST_PAGE = 200;
const NEXT_PAGES = 2000;

/**
 * A folder or the trash, page by page: the first page shows right away, the others load one after another in the
 * background until the list is complete (so Select all covers everything). A refresh reloads the pages the same way.
 */
export function useAllPages<T extends { id: string }>(queryKey: QueryKey, fetchPage: (limit: number, after?: string) => Promise<CursorPage<T>>, enabled = true) {
  const q = useInfiniteQuery({
    queryKey,
    queryFn: ({ pageParam }) => fetchPage(pageParam ? NEXT_PAGES : FIRST_PAGE, pageParam ?? undefined),
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.next,
    enabled,
  });
  const { hasNextPage, isFetchingNextPage, isError, fetchNextPage } = q;
  useEffect(() => {
    if (hasNextPage && !isFetchingNextPage && !isError) void fetchNextPage();
  }, [hasNextPage, isFetchingNextPage, isError, fetchNextPage]);
  const items = useMemo(() => allItems(q.data), [q.data]);
  return { items, isLoading: q.isLoading, error: q.error, loadingMore: !!hasNextPage, refetch: q.refetch };
}

/** The items of every page loaded so far; an item renamed between two pages can come twice, so each is kept once */
export function allItems<T extends { id: string }>(data: InfiniteData<CursorPage<T>> | undefined): T[] {
  if (!data) return [];
  if (data.pages.length === 1) return data.pages[0].items;
  const seen = new Set<string>();
  return data.pages.flatMap((p) => p.items.filter((n) => !seen.has(n.id) && !!seen.add(n.id)));
}
