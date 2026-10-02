import { request, get, post, enc, qs, toParams } from "@/api/client";
import type { Activity, ActivityFilter, ErrorEntry, ErrorFilter, Page, ShareAccess, ShareAccessFilter, LoginRecord, LoginFilter, LogSettings, LogStatus } from "@/api/types";

/** Activity, share link, sign-in and error logs, and how long they are kept */
export const logsApi = {
  activity: (f: ActivityFilter & { before?: number; limit?: number }) => get<Page<Activity>>(`/activity${qs(toParams(f))}`),
  /** The records to export (up to 100,000), which the page writes as CSV (components/logs/exportCsv.tsx) */
  activityExport: (f: ActivityFilter) => get<Activity[]>(`/activity/export${qs(toParams(f))}`),
  shareAccess: (f: ShareAccessFilter & { before?: number; limit?: number }) => get<Page<ShareAccess>>(`/share-access${qs(toParams(f))}`),
  loginLog: (f: LoginFilter & { before?: number; limit?: number }) => get<Page<LoginRecord>>(`/login-log${qs(toParams(f))}`),
  errorLog: (f: ErrorFilter & { before?: number; limit?: number }) => get<Page<ErrorEntry>>(`/admin/errors${qs(toParams(f))}`),
  errorLogExport: (f: ErrorFilter) => get<ErrorEntry[]>(`/admin/errors/export${qs(toParams(f))}`),
  loginLogExport: (f: LoginFilter) => get<LoginRecord[]>(`/login-log/export${qs(toParams(f))}`),
  logStatus: () => get<LogStatus>("/admin/logs"),
  updateLogSettings: (s: LogSettings) => request<LogStatus>("PUT", "/admin/logs", s),
  archiveLogsNow: () => post<LogStatus>("/admin/logs/archive"),
  logArchiveUrl: (id: number) => enc`/api/admin/logs/archives/${id}`,
  deleteLogArchive: (id: number) => request("DELETE", enc`/admin/logs/archives/${id}`),
};
