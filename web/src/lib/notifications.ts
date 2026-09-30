import { driveName, type AppNotification, type NotificationKind, type SmtpSecurity } from "@/api";
import { ROLE_LABEL } from "@/lib/drives";
import { t } from "@/lib/i18n";
import { formatBytes, formatDateTime } from "@/lib/utils";

/** The kinds of notification, in the order the settings list them */
export const NOTIFICATION_KINDS: { kind: NotificationKind; label: string; desc: string }[] = [
  { kind: "shared", label: t("Shared with you"), desc: t("Someone shares a folder or file with you, or adds you to a space") },
  { kind: "space_full", label: t("Space almost full"), desc: t("A space you own or manage is 90% full") },
  { kind: "access_expiring", label: t("Access ending"), desc: t("Your access to something shared with you ends within 3 days") },
  { kind: "link_upload", label: t("Files received through a link"), desc: t("Someone uploads files through a link you made that accepts files") },
  { kind: "app_password", label: t("New app password"), desc: t("An app password is created for your account") },
  { kind: "sign_in_method", label: t("New sign-in method"), desc: t("A Microsoft, Google, GitHub or other account is linked to yours") },
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
    case "link_upload": {
      const count = d.count ?? 1;
      return count > 1
        ? { title: t("{n} files arrived in “{name}” through a link", { n: count, name }), detail: t("The last one: {file}", { file: d.file ?? "" }) }
        : { title: t("“{file}” arrived in “{name}” through a link", { file: d.file ?? "", name }), detail: "" };
    }
    case "app_password":
      return {
        title: t("An app password “{name}” was created for your account", { name }),
        detail: t("From {ip}. If you didn't create it, remove it under App passwords and change your password.", { ip: d.ip || "—" }),
      };
    case "sign_in_method":
      return {
        title: t("A {provider} account was linked to your account", { provider: d.label ?? "" }),
        detail: d.account
          ? t("{account}, from {ip}. If you didn't link it, unlink it under Sign-in methods and change your password.", { account: d.account, ip: d.ip || "—" })
          : t("From {ip}. If you didn't link it, unlink it under Sign-in methods and change your password.", { ip: d.ip || "—" }),
      };
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
