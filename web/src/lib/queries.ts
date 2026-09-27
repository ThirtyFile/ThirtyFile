import type { QueryClient } from "@tanstack/react-query";

/** Queries a change to files or folders can affect: lists, the opened item, space usage; not settings, users or branding */
const FILE_QUERIES = ["children", "node", "recent", "favorites", "search", "shared-with-me", "trash", "drives", "me", "access"];

/** Refetch after renaming, moving, copying, deleting, restoring or uploading */
export function invalidateFiles(qc: QueryClient, ...more: string[]) {
  for (const key of [...FILE_QUERIES, ...more]) qc.invalidateQueries({ queryKey: [key] });
}
