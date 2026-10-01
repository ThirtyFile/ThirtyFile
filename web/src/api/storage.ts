import type { UsageHistory, UsageOverview, UsageRange, UsageThresholds, UsageWork } from "@/lib/usage";
import { request, get, post, enc, qs } from "@/api/client";
import { driveName, locationName } from "@/api/names";
import type { StorageKind, StorageConfig, StorageLocation, LocationSpace, LocationTestStep, LocationPage, UnusedJob, MovesList, SpaceMove } from "@/api/types";

/** Whether a move isn't over: the space is being moved, or waits to be */
export const moveActive = (m: SpaceMove) => m.state === "queued" || m.state === "running" || m.state === "paused" || m.state === "failed";

/** Storage locations, their usage, and moves of spaces between them (administrators) */
export const storageApi = {
  storageLocations: () => get<StorageLocation[]>("/admin/storage").then((l) => l.map((x) => ({ ...x, name: locationName(x.id, x.name) }))),
  /** Control panel › Storage usage: the latest samples, the last hour's operations and alerts */
  usageOverview: (signal?: AbortSignal) =>
    get<UsageOverview>("/admin/usage", signal).then((o) => ({ ...o, locations: o.locations.map((l) => ({ ...l, name: locationName(l.id, l.name) })) })),
  /** A location's history ('' for all of them together) over a range, for one kind of work or all */
  usageHistory: (q: { location: string; range: UsageRange; work: UsageWork | "all" }, signal?: AbortSignal) =>
    get<UsageHistory>("/admin/usage/history" + qs({ location: q.location, range: q.range, work: q.work }), signal),
  setUsageThresholds: (t: UsageThresholds) => request<UsageThresholds>("PUT", "/admin/usage/thresholds", t),
  createStorage: (req: { name: string; kind: StorageKind; config: StorageConfig }) => post<{ id: string }>("/admin/storage", req),
  updateStorage: (id: string, req: { name?: string; config?: StorageConfig }) => request("PATCH", enc`/admin/storage/${id}`, req),
  deleteStorage: (id: string) => request("DELETE", enc`/admin/storage/${id}`),
  testStorage: (req: { id?: string; kind: StorageKind; config: StorageConfig }) => post<{ ok: boolean; region?: string; host_key?: string }>("/admin/storage/test", req),
  testExistingStorage: (id: string) => post(enc`/admin/storage/${id}/test`),
  testStorageSteps: (id: string) => post<{ ok: boolean; steps: LocationTestStep[] }>(enc`/admin/storage/${id}/test-steps`),
  browseStorage: (id: string, path: string, after?: string) => get<LocationPage>(enc`/admin/storage/${id}/browse` + qs({ path, after })),
  storageDownloadUrl: (id: string, path: string) => enc`/api/admin/storage/${id}/download` + qs({ path }),
  unusedContent: (id: string) => get<UnusedJob | null>(enc`/admin/storage/${id}/unused`),
  findUnusedContent: (id: string) => post<UnusedJob>(enc`/admin/storage/${id}/unused`),
  removeUnusedContent: (id: string, scanId: string) => post<UnusedJob>(enc`/admin/storage/${id}/unused/remove`, { scan_id: scanId }),
  setDefaultStorage: (id: string) => post(enc`/admin/storage/${id}/default`),
  storageLocationSpaces: (id: string) => get<LocationSpace[]>(enc`/admin/storage/${id}/spaces`).then((l) => l.map((s) => ({ ...s, name: driveName(s) }))),
  moves: () =>
    get<MovesList>("/admin/moves").then((l) => ({
      ...l,
      moves: l.moves.map((m) => ({
        ...m,
        space_name: driveName({ kind: m.space_kind, name: m.space_name }),
        from_name: m.from_location ? locationName(m.from_location, m.from_name) : m.from_name,
        to_name: locationName(m.to_location, m.to_name),
      })),
    })),
  startMoves: (drive_ids: string[], location_id: string) => post<{ ids: string[] }>("/admin/moves", { drive_ids, location_id }),
  pauseMove: (id: string) => post(enc`/admin/moves/${id}/pause`),
  resumeMove: (id: string) => post(enc`/admin/moves/${id}/resume`),
  cancelMove: (id: string) => post(enc`/admin/moves/${id}/cancel`),
  setMoveConcurrency: (concurrency: number) => request("PUT", "/admin/moves/settings", { concurrency }),
};
