import { enc, get, post, request, qs } from "@/api/client";

export interface FtpConfig {
  enabled: boolean;
  managers_publish: boolean;
  allow_private_plaintext: boolean;
}
export interface FtpStatus extends FtpConfig {
  address: string;
  passive_ports: string;
  passive_host: string;
  tls_ready: boolean;
  listening: boolean;
  error: string | null;
}
export interface FtpChange {
  name: string;
  enabled: boolean;
  allow_list: boolean;
  allow_read: boolean;
  expires_at: number | null;
}
export interface FtpFolder extends FtpChange {
  id: string;
  node_id: string;
  username: string;
  creator_id: number;
  creator: string;
  folder_name: string;
  active: boolean;
  last_used_at: number | null;
}
export interface FtpCreated extends Pick<FtpFolder, "id" | "node_id" | "name" | "username"> {
  password: string;
}
export interface FtpConfirm {
  password?: string;
  code?: string;
}
export const ftpApi = {
  ftpConfig: (signal?: AbortSignal) => get<FtpStatus>("/admin/ftp", signal),
  saveFtpConfig: (config: FtpConfig) => request<FtpStatus>("PUT", "/admin/ftp", config),
  ftpFolders: (nodeId?: string, signal?: AbortSignal) => get<{ can_publish: boolean; folders: FtpFolder[] }>(`/ftp/folders${qs({ node_id: nodeId })}`, signal),
  createFtpFolder: (node_id: string, config: FtpChange, identity: FtpConfirm) => post<FtpCreated>("/ftp/folders", { node_id, ...config, ...identity }),
  saveFtpFolder: (id: string, config: FtpChange, identity: FtpConfirm) => request("PATCH", enc`/ftp/folders/${id}`, { ...config, ...identity }),
  rotateFtpPassword: (id: string, identity: FtpConfirm) => post<FtpCreated>(enc`/ftp/folders/${id}/password`, identity),
  deleteFtpFolder: (id: string, identity: FtpConfirm) => request("DELETE", enc`/ftp/folders/${id}`, identity),
};
