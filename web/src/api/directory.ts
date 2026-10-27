import { get, post, request } from "@/api/client";
import type { DirectoryInfo, DirectoryReport, DirectorySettings, DirectoryStep } from "@/api/types";
type Confirmation = { my_password?: string; my_code?: string };

/** Directory sign-in (LDAP, Active Directory) */
export const directoryApi = {
  directorySettings: (signal?: AbortSignal) => get<DirectoryInfo>("/admin/directory", signal),
  /** Saving asks who the administrator is; a blank password keeps the saved one */
  saveDirectory: (s: DirectorySettings & Confirmation) => request<DirectoryInfo>("PUT", "/admin/directory", s),
  /** Tries the settings as typed: connect, the service account, the person, their groups */
  testDirectory: (s: DirectorySettings, username: string) => post<{ steps: DirectoryStep[] }>("/admin/directory/test", { ...s, username }),
  syncDirectory: () => post<DirectoryReport>("/admin/directory/sync"),
};
