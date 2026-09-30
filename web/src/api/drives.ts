import { request, get, post, enc } from "@/api/client";
import { localizeDrive } from "@/api/names";
import type { Job, Drive } from "@/api/types";

/** Spaces */
export const drivesApi = {
  drives: () => get<Drive[]>("/drives").then((l) => l.map(localizeDrive)),
  /** `location_id` (administrators): the storage location of the new space; left out, the default location */
  createDrive: (name: string, quota_bytes?: number, source_path?: string, read_only?: boolean, location_id?: string) =>
    post<Drive>("/drives", { name, quota_bytes, source_path, read_only, location_id }),
  /** Scans a folder space for changes made on the server's folder; the finished task's result is a `ScanReport` */
  scanDrive: (id: string) => post<Job>(enc`/admin/drives/${id}/scan`),
  updateDrive: (id: string, req: { name?: string; quota_bytes?: number; read_only?: boolean }) => request<Drive>("PATCH", enc`/drives/${id}`, req),
  deleteDrive: (id: string) => request("DELETE", enc`/drives/${id}`),
  adminDrives: () => get<Drive[]>("/admin/drives").then((l) => l.map(localizeDrive)),
};
