import { t } from "@/lib/i18n";
import type { Resolution } from "@/lib/conflicts";
import { request, get, post, enc, qs, toParams, responseError, send } from "@/api/client";
import { driveName, localizeLocated } from "@/api/names";
import type {
  Node,
  Job,
  SearchFilter,
  Located,
  FolderContents,
  NodeInfo,
  FoundPath,
  HistoryEntry,
  CursorPage,
  PositionedPage,
  SortKey,
  FileVersion,
  NameConflict,
  SortOrder,
} from "@/api/types";

export const SORT_KEYS = ["name", "updated", "created", "size", "type"] as const;

/** Folders and files: listing, changing, the trash, search, favorites, tasks and versions */
export const filesApi = {
  node: (id: string, signal?: AbortSignal) => get<NodeInfo>(enc`/nodes/${id}`, signal).then((n) => ({ ...n, drive: { ...n.drive, name: driveName(n.drive) } })),
  children: (id: string, sort?: SortKey, order?: SortOrder, foldersOnly?: boolean, signal?: AbortSignal) =>
    get<Node[]>(enc`/nodes/${id}/children` + qs({ sort, order, folders_only: foldersOnly ? "true" : undefined }), signal),
  childrenPage: (id: string, sort: SortKey, order: SortOrder, limit: number, after?: string, signal?: AbortSignal) =>
    get<CursorPage<Node>>(enc`/nodes/${id}/children` + qs({ sort, order, limit: String(limit), after }), signal),
  /** The part of a folder from `offset` on (a large folder is shown a part at a time), with how many items it has */
  childrenAt: (id: string, sort: SortKey, order: SortOrder, offset: number, limit: number, signal?: AbortSignal) =>
    get<PositionedPage<Node>>(enc`/nodes/${id}/children` + qs({ sort, order, limit: String(limit), offset: String(offset) }), signal),
  /** Where an item is in a folder's listing (null: not in it) */
  position: (id: string, item: string, sort: SortKey, order: SortOrder) => get<{ position: number | null; total: number }>(enc`/nodes/${id}/position` + qs({ item, sort, order })),
  /** The ids of items selected in a folder (all of them, or from one to another, less some), a batch at a time */
  selection: (id: string, req: { sort: SortKey; order: SortOrder; from?: string; to?: string; except: string[]; after?: string }) =>
    post<{ ids: string[]; next: string | null }>(enc`/nodes/${id}/select`, req),
  /** What a typed path names; `aliases` maps names as the UI language shows them to the ones paths use */
  findPath: (path: string, aliases: Record<string, string>) => post<FoundPath>("/nodes/find", { path, aliases }),
  createFolder: (parent_id: string, name: string) => post<Node>("/folders", { parent_id, name }),
  rename: (id: string, name: string) => request<Node>("PATCH", enc`/nodes/${id}`, { name }),
  /** `resolutions`: what to do with each item (by id) whose name the destination already has */
  /** Moving or copying to or from a folder on the server can take a while: follow the task (`waitForJob`) */
  move: (ids: string[], dest_id: string, resolutions?: Record<string, Resolution>) => post<Job>("/nodes/move", { ids, dest_id, resolutions }),
  copy: (ids: string[], dest_id: string, resolutions?: Record<string, Resolution>) => post<Job>("/nodes/copy", { ids, dest_id, resolutions }),
  /** Which names would clash: of `names` about to be uploaded to `dest_id`, of `ids` moved or copied there, or of `ids` restored from the trash (no `dest_id`) */
  conflicts: (req: { dest_id?: string; names?: string[]; ids?: string[] }) => post<NameConflict[]>("/nodes/conflicts", req),
  trash: (ids: string[]) => post("/nodes/trash", { ids }),
  /** The most recent entries about an item (and, for a folder, what's inside it) */
  history: (id: string, signal?: AbortSignal) => get<HistoryEntry[]>(enc`/nodes/${id}/activity`, signal),
  /** Size and number of items inside these folders (files among the ids hold nothing) */
  contents: (ids: string[], signal?: AbortSignal) => request<FolderContents>("POST", "/nodes/contents", { ids }, undefined, undefined, signal),
  /** With mine, only the items the person deleted */
  trashPage: (limit: number, after?: string, mine?: boolean, signal?: AbortSignal) =>
    get<CursorPage<Located>>(`/trash${qs({ limit: String(limit), after, mine: mine ? "true" : undefined })}`, signal).then((p) => ({ ...p, items: p.items.map(localizeLocated) })),
  restore: (ids: string[], resolutions?: Record<string, Resolution>) => post("/trash/restore", { ids, resolutions }),
  /** Deleting for good and emptying the trash answer with a task (see lib/jobs) */
  deleteForever: (ids: string[]) => post<Job>("/trash/delete", { ids }),
  emptyTrash: () => post<Job>("/trash/empty"),
  /** What Empty trash would delete: items per space */
  emptyTrashPreview: () => get<{ kind: string; name: string; items: number }[]>("/trash/empty"),
  /** Names containing `q`; at most 300 (`truncated` when there were more) */
  search: (q: string, f: SearchFilter = {}) =>
    get<{ items: Located[]; truncated: boolean }>(`/search${qs(toParams({ q, ...f }))}`).then((r) => ({ ...r, items: r.items.map(localizeLocated) })),
  recent: () => get<Located[]>("/recent").then((l) => l.map(localizeLocated)),
  favorites: (sort?: SortKey, order?: SortOrder) => get<Located[]>(`/favorites${qs({ sort, order })}`).then((l) => l.map(localizeLocated)),
  setFavorite: (ids: string[], favorite: boolean) => post("/nodes/favorite", { ids, favorite }),
  /** Packs items into a new ZIP file in `parentId`, on the server */
  compress: (ids: string[], parentId: string) => post<Job>("/archive/compress", { ids, parent_id: parentId, tz: new Date().getTimezoneOffset() }),
  /** Extracts a ZIP file into a new folder next to it, on the server */
  extract: (id: string) => post<Job>("/archive/extract", { id }),
  job: (id: string) => get<Job>(enc`/jobs/${id}`),
  /** Save from the online editor; with baseVersion (updated_at when the file was opened), returns 409 if someone else changed the file */
  saveContent: (id: string, content: BodyInit, baseVersion?: number) =>
    request<Node>("PUT", enc`/files/${id}/content`, undefined, content, baseVersion !== undefined ? { "X-Base-Version": String(baseVersion) } : undefined),
  /** A file's earlier versions, newest first */
  versions: (id: string, signal?: AbortSignal) => get<FileVersion[]>(enc`/files/${id}/versions`, signal),
  /** Where to open (preview) or download an earlier version */
  versionUrl: (id: string, version: string, download?: boolean) => enc`/api/files/${id}/versions/${version}/content` + (download ? "?download=1" : ""),
  /** Make an earlier version the file's content again (the current content becomes a version too) */
  restoreVersion: (id: string, version: string) => post<Node>(enc`/files/${id}/versions/${version}/restore`),
  /** Create an empty file (a zero-length tus upload completes immediately); returns the new node id */
  createEmptyFile: async (parentId: string, name: string) => {
    const b64 = (s: string) => btoa(String.fromCharCode(...new TextEncoder().encode(s)));
    const res = await send("/api/uploads", {
      method: "POST",
      headers: {
        "Tus-Resumable": "1.0.0",
        "Upload-Length": "0",
        "Upload-Metadata": `filename ${b64(name)},parentId ${b64(parentId)}`,
      },
    });
    if (!res.ok) throw await responseError(res, "/api/uploads", t("Couldn't create ({status})", { status: res.status }));
    return res.headers.get("X-Node-Id")!;
  },
};
