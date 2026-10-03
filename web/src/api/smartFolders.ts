import { request, get, post, enc, qs } from "@/api/client";
import { localizeLocated } from "@/api/names";
import type { CursorPage, Located, PositionedPage, SmartFolder, SmartQuery, SortKey, SortOrder } from "@/api/types";

/** The signed-in person's own smart folders (only they see them), and what each lists: like a folder's items, a part at a time */
export const smartFoldersApi = {
  smartFolders: () => get<SmartFolder[]>("/smart-folders"),
  smartFolder: (id: number, signal?: AbortSignal) => get<SmartFolder>(enc`/smart-folders/${id}`, signal),
  createSmartFolder: (name: string, query: SmartQuery) => post<SmartFolder>("/smart-folders", { name, query }),
  updateSmartFolder: (id: number, change: { name?: string; query?: SmartQuery }) => request<SmartFolder>("PATCH", enc`/smart-folders/${id}`, change),
  deleteSmartFolder: (id: number) => request("DELETE", enc`/smart-folders/${id}`),
  /** What a smart folder lists, page by page (`after`: the `next` of the page before) */
  smartItemsPage: (id: number, sort: SortKey, order: SortOrder, limit: number, after?: string, signal?: AbortSignal) =>
    get<CursorPage<Located>>(enc`/smart-folders/${id}/items` + qs({ sort, order, limit: String(limit), after }), signal).then((p) => ({ ...p, items: p.items.map(localizeLocated) })),
  /** The part of what a smart folder lists from `offset` on, with how many items it lists */
  smartItemsAt: (id: number, sort: SortKey, order: SortOrder, offset: number, limit: number, signal?: AbortSignal) =>
    get<PositionedPage<Located>>(enc`/smart-folders/${id}/items` + qs({ sort, order, limit: String(limit), offset: String(offset) }), signal).then((p) => ({
      ...p,
      items: p.items.map(localizeLocated),
    })),
  /** Where an item is in what a smart folder lists (null: not in it) */
  smartPosition: (id: number, item: string, sort: SortKey, order: SortOrder) =>
    get<{ position: number | null; total: number }>(enc`/smart-folders/${id}/position` + qs({ item, sort, order })),
  /** The ids of items selected in a smart folder (all of them, or from one to another, less some), a batch at a time */
  smartSelection: (id: number, req: { sort: SortKey; order: SortOrder; from?: string; to?: string; except: string[]; after?: string }) =>
    post<{ ids: string[]; next: string | null }>(enc`/smart-folders/${id}/select`, req),
};
