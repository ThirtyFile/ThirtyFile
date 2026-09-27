import type { QueryClient } from "@tanstack/react-query";

/** Queries a change to files or folders can affect: lists, the opened item, space usage; not settings, users or branding */
const FILE_QUERIES = ["children", "node", "recent", "favorites", "search", "shared-with-me", "trash", "drives", "me", "access"];

/** Refetch after renaming, moving, copying, deleting, restoring or uploading; resolves when the lists have reloaded */
export function invalidateFiles(qc: QueryClient, ...more: string[]): Promise<unknown> {
  return Promise.all([...FILE_QUERIES, ...more].map((key) => qc.invalidateQueries({ queryKey: [key] })));
}
