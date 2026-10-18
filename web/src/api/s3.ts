import { enc, get, post, request, qs } from "@/api/client";

export interface S3Config {
  enabled: boolean;
  hostname: string;
  region: string;
  managers_publish: boolean;
}
export interface S3Key {
  id: string;
  bucket_id: string;
  access_key: string;
  description: string;
  writable: boolean;
  enabled: boolean;
  expires_at: number | null;
  created_at: number;
  last_used_at: number | null;
}
/** A key just made: its Secret Key is shown this once */
export type NewS3Key = S3Key & { secret_key: string };
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
/** A bucket just published, without what the list adds to it */
export type NewS3Bucket = Omit<S3Bucket, "creator" | "folder" | "active" | "keys">;
/** Who is acting, asked again (`useConfirmIdentity`) */
type Confirm = { password?: string; code?: string };

export const s3Api = {
  s3Config: (signal?: AbortSignal) => get<S3Config>("/admin/s3", signal),
  s3Options: (signal?: AbortSignal) => get<S3Config>("/s3/options", signal),
  saveS3Config: (config: S3Config) => request<S3Config>("PUT", "/admin/s3", config),
  s3Buckets: (nodeId?: string, search?: string, signal?: AbortSignal) => get<{ can_publish: boolean; buckets: S3Bucket[] }>(`/s3/buckets${qs({ node_id: nodeId, search })}`, signal),
  /** `confirm`: the person's password (and code), asked again, as for other credentials */
  publishS3Bucket: (nodeId: string, name: string, confirm: Confirm) => post<NewS3Bucket>("/s3/buckets", { node_id: nodeId, name, ...confirm }),
  setS3Bucket: (id: string, enabled: boolean) => request("PATCH", enc`/s3/buckets/${id}`, { enabled }),
  deleteS3Bucket: (id: string) => request("DELETE", enc`/s3/buckets/${id}`),
  newS3Key: (id: string, description: string, writable: boolean, expiresAt: number | null, confirm: Confirm) =>
    post<NewS3Key>(enc`/s3/buckets/${id}/keys`, { description, writable, expires_at: expiresAt, ...confirm }),
  setS3Key: (id: string, key: string, enabled: boolean) => request("PATCH", enc`/s3/buckets/${id}/keys/${key}`, { enabled }),
  deleteS3Key: (id: string, key: string) => request("DELETE", enc`/s3/buckets/${id}/keys/${key}`),
};
