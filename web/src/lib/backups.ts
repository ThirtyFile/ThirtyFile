import { useEffect, useRef } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, backupJobActive, type BackupJob, type BackupJobState, type BackupSet, type BackupsOverview } from "@/api";
import { keys } from "@/api/queryKeys";
import { invalidateFiles } from "@/lib/queries";
import { t } from "@/lib/i18n";

/** A job changing state between two refreshes of the list */
export interface BackupJobChange {
  job: BackupJob;
  from: BackupJobState;
}

const busy = (s: BackupJobState) => s === "running" || s === "queued";

/**
 * Copies and their jobs (Control panel › Backups), refreshed every `interval` ms while a job runs or waits, and every
 * `paused` ms while one is only paused or stopped. When a job stops running, the storage locations and spaces are
 * refreshed (a restore adds files, deleting a copy frees room). `onChange` is told of every change of state.
 */
export function useBackups(interval: number, paused: number | false = false, onChange?: (changes: BackupJobChange[]) => void) {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: keys.backups(),
    queryFn: api.backups,
    refetchInterval: (query) => {
      const jobs = query.state.data?.jobs ?? [];
      return jobs.some((j) => busy(j.state)) ? interval : jobs.some(backupJobActive) ? paused : false;
    },
  });
  const seen = useRef<{ data: BackupsOverview; states: Map<string, BackupJobState> } | null>(null);
  const report = useRef(onChange);
  useEffect(() => {
    report.current = onChange;
  });
  useEffect(() => {
    const data = q.data;
    if (!data || seen.current?.data === data) return;
    const before = seen.current?.states;
    seen.current = { data, states: new Map(data.jobs.map((j) => [j.id, j.state])) };
    if (!before) return;
    const changes = data.jobs.flatMap((j) => {
      const from = before.get(j.id);
      return from && from !== j.state ? [{ job: j, from }] : [];
    });
    if (!changes.length) return;
    if (changes.some((c) => busy(c.from) && !busy(c.job.state))) void invalidateFiles(qc, "admin-drives", "storage-locations");
    report.current?.(changes);
  }, [q.data, qc]);
  return q;
}

/** The copy's most recent complete snapshot: what it can be restored from */
export const completeSnapshot = (s: BackupSet) => s.snapshots.find((n) => n.state === "complete") ?? null;

/** The job of a copy that isn't over, if any (the newest) */
export const activeJob = (s: BackupSet, jobs: BackupJob[]) => jobs.find((j) => j.set_id === s.id && backupJobActive(j)) ?? null;

export const BACKUP_JOB_KIND_LABEL: Record<BackupJob["kind"], string> = {
  snapshot: t("Copying"),
  restore: t("Restoring"),
  verify: t("Checking"),
  remove: t("Deleting"),
};

export const BACKUP_JOB_STATE_LABEL: Record<BackupJobState, string> = {
  queued: t("Waiting"),
  running: t("Running"),
  paused: t("Paused"),
  waiting: t("Waiting for the location"),
  failed: t("Stopped by an error"),
  done: t("Done"),
  cancelled: t("Cancelled"),
};

/** How a space of a copy is named: personal spaces by their owner, as they are all called "My files" */
export const backupSpaceLabel = (s: { kind: string; name: string; owner: string }) => (s.kind === "personal" && s.owner ? `${s.name} · ${s.owner}` : s.name);
