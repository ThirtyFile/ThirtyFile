import { enc, get, post, request, qs } from "@/api/client";

export interface S3Config {
  enabled: boolean;
  hostname: string;
  region: string;
  managers_publish: boolean;
}
export interface S3Key {
  id: string;
  access_key: string;
  description: string;
  writable: boolean;
  enabled: boolean;
  expires_at: number | null;
  last_used_at: number | null;
}
export interface S3Bucket {
  id: string;
  name: string;
  node_id: string;
  creator_id: number;
  enabled: boolean;
  created_at: number;
  last_used_at: number | null;
  creator: string;
  folder: string;
  active: boolean;
  keys: S3Key[];
}
export const s3Api = {
  s3Config: (signal?: AbortSignal) => get<S3Config>("/admin/s3", signal),
  s3Options: (signal?: AbortSignal) => get<S3Config>("/s3/options", signal),
  saveS3Config: (config: S3Config) => request<S3Config>("PUT", "/admin/s3", config),
  s3Buckets: (nodeId?: string, search?: string, signal?: AbortSignal) => get<{ can_publish: boolean; buckets: S3Bucket[] }>(`/s3/buckets${qs({ node_id: nodeId, search })}`, signal),
  publishS3Bucket: (nodeId: string, name: string) => post<S3Bucket>("/s3/buckets", { node_id: nodeId, name }),
  setS3Bucket: (id: string, enabled: boolean) => request("PATCH", enc`/s3/buckets/${id}`, { enabled }),
  deleteS3Bucket: (id: string) => request("DELETE", enc`/s3/buckets/${id}`),
  newS3Key: (id: string, description: string, writable: boolean, expiresAt: number | null) =>
    post<S3Key & { secret_key: string }>(enc`/s3/buckets/${id}/keys`, { description, writable, expires_at: expiresAt }),
  setS3Key: (id: string, key: string, enabled: boolean) => request("PATCH", enc`/s3/buckets/${id}/keys/${key}`, { enabled }),
  deleteS3Key: (id: string, key: string) => request("DELETE", enc`/s3/buckets/${id}/keys/${key}`),
};
