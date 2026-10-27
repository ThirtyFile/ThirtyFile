import { get, post, enc, qs } from "@/api/client";
import type { HeldItem, Hold, HoldList, HoldPlace, NodeRetention } from "@/api/types";

/** Legal holds and what keeps an item from being deleted for good */
export const holdsApi = {
  /** Administrators: the holds (`state`: active, released or all), and the spaces with a retention period */
  holds: (state: "active" | "released" | "all", q: string, signal?: AbortSignal) => get<HoldList>(`/admin/holds${qs({ state, q: q || undefined })}`, signal),
  placeHold: (req: HoldPlace) => post<Hold>("/admin/holds", req),
  releaseHold: (id: string) => post(enc`/admin/holds/${id}/release`),
  /** The files a hold covers (the first ones, with `more` when there are others) */
  holdItems: (id: string, signal?: AbortSignal) => get<{ items: HeldItem[]; more: boolean }>(enc`/admin/holds/${id}/items`, signal),
  holdExportUrl: (id: string) => enc`/api/admin/holds/${id}/export`,
  nodeRetention: (id: string, signal?: AbortSignal) => get<NodeRetention>(enc`/nodes/${id}/retention`, signal),
};
