import { request, get, post, enc, qs } from "@/api/client";
import { localizeLocated } from "@/api/names";
import type { Located, SortKey, SortOrder, Tag, TagColor } from "@/api/types";

/** The signed-in person's own tags (only they see them) and the items they put them on */
export const tagsApi = {
  tags: () => get<Tag[]>("/tags"),
  createTag: (name: string, color: TagColor) => post<Tag>("/tags", { name, color }),
  updateTag: (id: number, change: { name?: string; color?: TagColor }) => request<Tag>("PATCH", enc`/tags/${id}`, change),
  deleteTag: (id: number) => request("DELETE", enc`/tags/${id}`),
  /** Adds and removes tags on up to 1,000 items (a larger selection goes a batch at a time: lib/span.ts) */
  applyTags: (ids: string[], add: number[], remove: number[]) => post("/nodes/tags", { ids, add, remove }),
  /** The items with a tag, where the person can still open them; at most 5,000 (`truncated` when there are more) */
  tagged: (id: number, sort?: SortKey, order?: SortOrder) =>
    get<{ items: Located[]; truncated: boolean }>(enc`/tags/${id}/items` + qs({ sort, order })).then((r) => ({ ...r, items: r.items.map(localizeLocated) })),
};
