import type { AlertParams } from "@/api";
import { t } from "@/lib/i18n";

/** What an alert is about, as the emails say it: the same sentences, in the page's language */
export function alertText(kind: string, p: AlertParams): string {
  const location = p.location ?? "";
  const percent = p.percent ?? 0;
  switch (kind) {
    case "usage_disk":
      return t("The storage location “{location}” is {percent}% full", { location, percent });
    case "data_disk":
      return t("The disk of the data folder is {percent}% full", { percent });
    case "usage_errors":
      return t("The storage location “{location}” fails {percent}% of its operations", { location, percent });
    case "offline":
      return t("The storage location “{location}” can't be reached", { location });
    case "sign_ins_ip":
      return t("{count} failed sign-ins from {ip} in the last {minutes} minutes", { count: p.count ?? 0, ip: p.ip ?? "", minutes: p.minutes ?? 0 });
    case "sign_ins_user":
      return t("{count} failed sign-ins for the account “{username}” in the last {minutes} minutes", { count: p.count ?? 0, username: p.username ?? "", minutes: p.minutes ?? 0 });
    case "certificate":
      return t("The FTP certificate ends in {days} days", { days: p.days ?? 0 });
    case "backup":
      return t("The backup “{name}” needs attention", { name: p.database ? t("Database") : (p.name ?? "") });
    case "replica":
      return t("The replicas “{name}” need attention", { name: p.name ?? "" });
    default:
      return kind;
  }
}
