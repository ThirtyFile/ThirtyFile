import { useQuery } from "@tanstack/react-query";
import { api, type BackupJobState, type ReplicaJob, type ReplicaPolicy, type ReplicaTargetState } from "@/api";
import { t } from "@/lib/i18n";

/** A job of replicas that isn't over: it may still run, or be resumed */
export const replicaJobActive = (j: ReplicaJob) => j.state === "queued" || j.state === "running" || j.state === "paused" || j.state === "waiting" || j.state === "failed";

const busy = (s: BackupJobState) => s === "running" || s === "queued";

/**
 * Replica policies, how they are doing, and their jobs (Control panel › Replicas): refreshed every `interval` ms while
 * a job runs or waits, and every 15 seconds otherwise, as targets fall behind and catch up on their own.
 */
export function useReplicas(interval: number) {
  return useQuery({
    queryKey: ["replicas"],
    queryFn: api.replicas,
    refetchInterval: (query) => ((query.state.data?.jobs ?? []).some((j) => busy(j.state)) ? interval : 15_000),
  });
}

/** The job of a policy's target that isn't over, if any (the newest) */
export const targetJob = (p: ReplicaPolicy, location: string, jobs: ReplicaJob[]) =>
  jobs.find((j) => j.policy_id === p.id && j.location_id === location && replicaJobActive(j)) ?? null;

export const REPLICA_STATE_LABEL: Record<ReplicaTargetState, string> = {
  current: t("Current"),
  behind: t("Changes waiting"),
  syncing: t("Syncing"),
  initializing: t("First copy"),
  offline: t("Can't be reached"),
  failed: t("Failing"),
  corrupt: t("Damaged copies"),
  stale: t("Old primary, being checked"),
  paused: t("Paused"),
};

export const REPLICA_STATE_TONE: Record<ReplicaTargetState, string> = {
  current: "text-emerald-600 dark:text-emerald-400",
  behind: "text-brand",
  syncing: "text-brand",
  initializing: "text-brand",
  offline: "text-amber-600 dark:text-amber-400",
  failed: "text-destructive",
  corrupt: "text-destructive",
  stale: "text-amber-600 dark:text-amber-400",
  paused: "text-muted-foreground",
};

export const REPLICA_HEALTH_LABEL: Record<ReplicaPolicy["health"]["state"], string> = {
  ok: t("Current"),
  behind: t("Changes waiting"),
  degraded: t("Needs attention"),
  paused: t("Paused"),
};

export const REPLICA_HEALTH_TONE: Record<ReplicaPolicy["health"]["state"], string> = {
  ok: "text-emerald-600 dark:text-emerald-400",
  behind: "text-brand",
  degraded: "text-destructive",
  paused: "text-muted-foreground",
};

export const REPLICA_JOB_KIND_LABEL: Record<ReplicaJob["kind"], string> = {
  sync: t("Syncing"),
  verify: t("Checking"),
};
