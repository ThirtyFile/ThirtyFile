import type { QueryClient } from "@tanstack/react-query";

/** Queries a change to files or folders can affect: lists, the opened item, space usage; not settings, users or branding */
const FILE_QUERIES = ["children", "node", "recent", "favorites", "search", "shared-with-me", "trash", "drives", "me", "access"];

/**
 * What folders hold, counted through every level (the details pane): only changes that add, move or remove items
 * refresh it (pass it to invalidateFiles), not a rename or a new favorite
 */
export const FOLDER_CONTENTS = "folder-contents";

/** Refetch after renaming, moving, copying, deleting, restoring or uploading; resolves when the lists have reloaded */
export function invalidateFiles(qc: QueryClient, ...more: string[]): Promise<unknown> {
  return Promise.all([...FILE_QUERIES, ...more].map((key) => qc.invalidateQueries({ queryKey: [key] })));
}
