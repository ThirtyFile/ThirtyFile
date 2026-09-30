import { request, get, post, enc, qs } from "@/api/client";
import { driveName, locationName } from "@/api/names";
import type { BackupSchedule, BackupPolicySettings, BackupsOverview, CopyPreview, RestorePreview, RestoreRequest, SnapshotItem, BackupJob } from "@/api/types";

/** Whether a backup job isn't over */
export const backupJobActive = (j: BackupJob) => j.state === "queued" || j.state === "running" || j.state === "paused" || j.state === "waiting" || j.state === "failed";

/** Backups and copies of storage locations, and restoring from them (administrators) */
export const backupsApi = {
  backups: () =>
    get<BackupsOverview>("/admin/backups").then((o) => ({
      ...o,
      sets: o.sets.map((s) => ({
        ...s,
        source_name: s.source_location ? locationName(s.source_location, s.source_name) : s.source_name,
        dest_name: locationName(s.dest_location, s.dest_name),
        snapshots: s.snapshots.map((n) => ({ ...n, spaces: n.spaces.map((sp) => ({ ...sp, name: driveName(sp) })) })),
      })),
    })),
  copyPreview: (source: string, dest: string) =>
    post<CopyPreview>("/admin/backups/copies/preview", { source, dest }).then((p) => ({
      ...p,
      spaces: p.spaces.map((s) => ({ ...s, name: driveName(s) })),
    })),
  startCopy: (source: string, dest: string, name: string) => post<{ set_id: string; job_id: string }>("/admin/backups/copies", { source, dest, name }),
  pauseBackupJob: (id: string) => post(enc`/admin/backups/jobs/${id}/pause`),
  resumeBackupJob: (id: string) => post(enc`/admin/backups/jobs/${id}/resume`),
  cancelBackupJob: (id: string) => post(enc`/admin/backups/jobs/${id}/cancel`),
  verifyBackup: (id: string) => post<{ job_id: string }>(enc`/admin/backups/sets/${id}/verify`),
  deleteBackup: (id: string) => request("DELETE", enc`/admin/backups/sets/${id}`),
  restorePreview: (snapshot: string, req: RestoreRequest) =>
    post<RestorePreview>(enc`/admin/backups/snapshots/${snapshot}/restore/preview`, req).then((p) => ({
      ...p,
      space: { ...p.space, name: driveName(p.space) },
      targets: p.targets.map((t) => ({ ...t, name: driveName(t) })),
    })),
  restoreBackup: (snapshot: string, req: RestoreRequest) => post<{ job_id: string }>(enc`/admin/backups/snapshots/${snapshot}/restore`, req),
  browseSnapshot: (snapshot: string, space: string, folder?: string | null) =>
    get<{ path: [string, string][]; items: SnapshotItem[] }>(enc`/admin/backups/snapshots/${snapshot}/browse` + qs({ space, folder: folder ?? undefined })),
  createBackupPolicy: (req: Partial<BackupPolicySettings> & { name: string; source: string; dest: string }) =>
    post<{ set_id: string }>("/admin/backups/policies", req),
  updateBackupPolicy: (id: string, req: Partial<BackupPolicySettings> & { name?: string }) => request("PATCH", enc`/admin/backups/policies/${id}`, req),
  runBackupPolicy: (id: string) => post<{ job_id: string | null }>(enc`/admin/backups/policies/${id}/run`),
  backupNextRuns: (schedule: BackupSchedule, tz: string) => post<number[]>("/admin/backups/policies/next-runs", { schedule, tz }),
  importBackups: (location: string) => post<{ found: number; added: string[] }>("/admin/backups/import", { location }),
};
