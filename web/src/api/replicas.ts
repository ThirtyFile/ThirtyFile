import { request, get, post, enc, qs } from "@/api/client";
import { locationName } from "@/api/names";
import type { ReplicasOverview, ReplicaPolicyRequest, PromotePreflight } from "@/api/types";

/** Replicas of storage locations, and promoting one (administrators) */
export const replicasApi = {
  replicas: () =>
    get<ReplicasOverview>("/admin/replicas").then((o) => ({
      ...o,
      policies: o.policies.map((p) => ({
        ...p,
        source_name: locationName(p.source_location, p.source_name),
        targets: p.targets.map((t) => ({ ...t, name: locationName(t.location_id, t.name) })),
      })),
      unneeded: o.unneeded.map(([id, name, n, bytes]) => [id, locationName(id, name), n, bytes] as [string, string, number, number]),
    })),
  createReplicaPolicy: (req: ReplicaPolicyRequest) => post<{ id: string }>("/admin/replicas", req),
  updateReplicaPolicy: (id: string, req: ReplicaPolicyRequest) => request("PATCH", enc`/admin/replicas/${id}`, req),
  deleteReplicaPolicy: (id: string) => request("DELETE", enc`/admin/replicas/${id}`),
  syncReplicas: (id: string, location?: string) => post<{ jobs: string[] }>(enc`/admin/replicas/${id}/sync`, { location }),
  verifyReplicas: (id: string, location?: string) => post<{ jobs: string[] }>(enc`/admin/replicas/${id}/verify`, { location }),
  promotePreflight: (id: string, target: string) => get<PromotePreflight>(enc`/admin/replicas/${id}/promote` + qs({ target })),
  promoteReplica: (id: string, target: string, accept_missing: boolean) => post<{ moved: number; missing: number }>(enc`/admin/replicas/${id}/promote`, { target, accept_missing }),
  purgeReplicas: (location: string) => post<{ removed: number }>("/admin/replicas/purge", { location }),
  pauseReplicaJob: (id: string) => post(enc`/admin/replicas/jobs/${id}/pause`),
  resumeReplicaJob: (id: string) => post(enc`/admin/replicas/jobs/${id}/resume`),
  cancelReplicaJob: (id: string) => post(enc`/admin/replicas/jobs/${id}/cancel`),
};
