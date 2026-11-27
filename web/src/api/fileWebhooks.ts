import { enc, get, post, request } from "@/api/client";
import { driveName } from "@/api/names";
import type { FileWebhookChange, FileWebhooksInfo } from "@/api/types";

const named = (info: FileWebhooksInfo) => ({ ...info, spaces: info.spaces.map((s) => ({ ...s, name: driveName(s) })) });

/** File event webhooks (administrators) */
export const fileWebhooksApi = {
  fileWebhooks: (signal?: AbortSignal) => get<FileWebhooksInfo>("/admin/file-webhooks", signal).then(named),
  createFileWebhook: (change: FileWebhookChange) => post<FileWebhooksInfo>("/admin/file-webhooks", change).then(named),
  /** `secret` left out: the one saved stays */
  updateFileWebhook: (id: string, change: FileWebhookChange) => request<FileWebhooksInfo>("PUT", enc`/admin/file-webhooks/${id}`, change).then(named),
  deleteFileWebhook: (id: string) => request<FileWebhooksInfo>("DELETE", enc`/admin/file-webhooks/${id}`).then(named),
  testFileWebhook: (id: string) => post<{ ok: boolean }>(enc`/admin/file-webhooks/${id}/test`),
};
