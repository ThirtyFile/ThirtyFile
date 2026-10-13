import { get, post, request, enc } from "@/api/client";

export interface SftpKey {
  id: string;
  name: string;
  fingerprint: string;
  created_at: number;
  last_used_at: number | null;
}
export interface SftpSettings {
  enabled: boolean;
  keys_enabled: boolean;
  address: string;
  fingerprint: string;
}
export const sftpApi = {
  sftpKeys: () => get<SftpKey[]>("/auth/sftp-keys"),
  addSftpKey: (data: { name: string; public_key: string; password?: string; code?: string }) => post<SftpKey>("/auth/sftp-keys", data),
  removeSftpKey: (id: string) => request("DELETE", enc`/auth/sftp-keys/${id}`),
  sftpSettings: () => get<SftpSettings>("/admin/sftp"),
  updateSftpSettings: (data: Pick<SftpSettings, "enabled" | "keys_enabled">) => request<SftpSettings>("PUT", "/admin/sftp", data),
};
