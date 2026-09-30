import { request, get, post, enc, qs, toParams } from "@/api/client";
import type { Activity, ActivityFilter, ErrorEntry, ErrorFilter, Page, ShareAccess, ShareAccessFilter, LoginRecord, LoginFilter, LogSettings, LogStatus } from "@/api/types";

/** Activity, share link, sign-in and error logs, and how long they are kept */
export const logsApi = {
  activity: (f: ActivityFilter & { before?: number; limit?: number }) => get<Page<Activity>>(`/activity${qs(toParams(f))}`),
  /** CSV export URL (tz: browser time zone; exported times are shown in local time) */
  activityExportUrl: (f: ActivityFilter) => `/api/activity/export${qs(toParams({ ...f, tz: new Date().getTimezoneOffset() }))}`,
  shareAccess: (f: ShareAccessFilter & { before?: number; limit?: number }) => get<Page<ShareAccess>>(`/share-access${qs(toParams(f))}`),
  loginLog: (f: LoginFilter & { before?: number; limit?: number }) => get<Page<LoginRecord>>(`/login-log${qs(toParams(f))}`),
  errorLog: (f: ErrorFilter & { before?: number; limit?: number }) => get<Page<ErrorEntry>>(`/admin/errors${qs(toParams(f))}`),
  errorLogExportUrl: (f: ErrorFilter) => `/api/admin/errors/export${qs(toParams({ ...f, tz: new Date().getTimezoneOffset() }))}`,
  loginLogExportUrl: (f: LoginFilter) => `/api/login-log/export${qs(toParams({ ...f, tz: new Date().getTimezoneOffset() }))}`,
  logStatus: () => get<LogStatus>("/admin/logs"),
  updateLogSettings: (s: LogSettings) => request<LogStatus>("PUT", "/admin/logs", s),
  archiveLogsNow: () => post<LogStatus>("/admin/logs/archive"),
  logArchiveUrl: (id: number) => enc`/api/admin/logs/archives/${id}`,
  deleteLogArchive: (id: number) => request("DELETE", enc`/admin/logs/archives/${id}`),
};
