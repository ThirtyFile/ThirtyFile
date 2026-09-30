import { request, get, post, enc, qs } from "@/api/client";
import type { NotificationKind, AppNotification, NotificationPrefs, NotificationSettings } from "@/api/types";

/** Notifications under the bell, and how each kind is sent */
export const notificationsApi = {
  /** Also tells the server the time zone, for the times in emails */
  notifications: () => get<{ items: AppNotification[]; unread: number }>(`/notifications${qs({ tz: String(new Date().getTimezoneOffset()) })}`),
  /** Without ids: all of them */
  markNotificationsRead: (ids?: number[]) => post("/notifications/read", { ids }),
  deleteNotification: (id: number) => request("DELETE", enc`/notifications/${id}`),
  clearNotifications: () => request("DELETE", "/notifications"),
  notificationSettings: () => get<NotificationSettings>("/notifications/settings"),
  /** Changing the email address takes the current password (and a two-factor code when it is on) */
  updateNotificationSettings: (req: { email?: string; kinds?: Partial<Record<NotificationKind, NotificationPrefs>>; password?: string; code?: string }) =>
    request<NotificationSettings>("PUT", "/notifications/settings", req),
};
