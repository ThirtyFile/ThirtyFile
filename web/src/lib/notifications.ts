import { driveName, type AppNotification, type NotificationKind, type SmtpSecurity } from "@/api";
import { ROLE_LABEL } from "@/lib/drives";
import { t } from "@/lib/i18n";
import { formatBytes, formatDateTime } from "@/lib/utils";

/** The kinds of notification, in the order the settings list them */
export const NOTIFICATION_KINDS: { kind: NotificationKind; label: string; desc: string }[] = [
  { kind: "shared", label: t("Shared with you"), desc: t("Someone shares a folder or file with you, or adds you to a space") },
  { kind: "space_full", label: t("Space almost full"), desc: t("A space you own or manage is 90% full") },
  { kind: "access_expiring", label: t("Access ending"), desc: t("Your access to something shared with you ends within 3 days") },
];

/** The name as the app shows it elsewhere: spaces the system named itself are translated */
function shownName(n: AppNotification) {
  const name = n.data.name ?? "";
  const space = n.kind === "space_full" || n.data.item === "space";
  return space && n.data.drive_kind ? driveName({ kind: n.data.drive_kind, name }) : name;
}

/** The heading and the line below it for a notification */
export function notificationText(n: AppNotification): { title: string; detail: string } {
  const name = shownName(n);
  const d = n.data;
  const role = d.role ? ROLE_LABEL[d.role] : "";
  const ends = d.expires_at ? formatDateTime(d.expires_at) : "";
  switch (n.kind) {
    case "shared": {
      const by = d.by || t("Someone");
      const title =
        d.item === "space" ? t("{by} added you to the space “{name}”", { by, name }) : t("{by} shared “{name}” with you", { by, name });
      const detail = ends ? t("Role: {role} · Until {time}", { role, time: ends }) : t("Role: {role}", { role });
      return { title, detail };
    }
    case "space_full":
      return {
        title: t("The space “{name}” is almost full", { name }),
        detail: t("{used} of {quota} used ({percent}%)", { used: formatBytes(d.used ?? 0), quota: formatBytes(d.quota ?? 0), percent: d.percent ?? 0 }),
      };
    case "access_expiring":
      return { title: t("Your access to “{name}” ends soon", { name }), detail: t("{role} until {time}", { role, time: ends }) };
    default:
      return { title: name, detail: "" };
  }
}

/** Where opening a notification goes: the folder or space, or the file's viewer */
export function notificationLink(n: AppNotification): string | null {
  if (!n.node_id) return null;
  return n.data.item === "file" ? `/view/${encodeURIComponent(n.node_id)}` : `/files/${encodeURIComponent(n.node_id)}`;
}

/** The number on the bell: 99+ beyond 99 */
export function unreadBadge(unread: number): string {
  return unread > 99 ? "99+" : String(unread);
}

/** The usual port for each kind of email encryption */
export const SMTP_PORT: Record<SmtpSecurity, number> = { starttls: 587, tls: 465, none: 25 };

/** The port after choosing another encryption: its usual port, unless a port of its own had been entered */
export function portForSecurity(port: number, security: SmtpSecurity): number {
  return !port || Object.values(SMTP_PORT).includes(port) ? SMTP_PORT[security] : port;
}
