import { t } from "@/lib/i18n";

/** "Showing n records…" as one translated sentence per situation, rather than fragments joined together */
export function shownCount(n: number, more: boolean, filtered: boolean): string {
  if (more && filtered) return t("Showing {n} record, more available (filtered)|Showing {n} records, more available (filtered)", { n });
  if (more) return t("Showing {n} record, more available|Showing {n} records, more available", { n });
  if (filtered) return t("Showing {n} record (filtered)|Showing {n} records (filtered)", { n });
  return t("Showing {n} record|Showing {n} records", { n });
}
