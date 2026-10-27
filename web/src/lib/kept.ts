//! What keeps an item from being deleted for good (a legal hold, or the retention period of its space), as the pages say it

import { toast } from "sonner";
import type { Job, Kept, KeptItem } from "@/api";
import { t } from "@/lib/i18n";
import { formatDate } from "@/lib/utils";

/** "On legal hold", or "Held until 1/5/2027" */
export function keptText(k: Kept): string {
  if (k.hold) return t("On legal hold");
  return k.until ? t("Held until {date}", { date: formatDate(k.until) }) : "";
}

/** The longer sentence: with the reason of a hold */
export function keptReason(k: Kept): string {
  if (k.hold) return t("On legal hold: {reason}", { reason: k.hold });
  return k.until ? t("Held until {date}: its space keeps files for a period after their last change", { date: formatDate(k.until) }) : "";
}

/** The items a deletion left in the trash, from the finished task's report */
export function keptOf(job: Pick<Job, "result">): KeptItem[] {
  const result = job.result as { kept?: KeptItem[] } | null | undefined;
  return Array.isArray(result?.kept) ? result.kept : [];
}

/** Says which items stayed in the trash, and why */
export function keptToast(kept: KeptItem[]) {
  toast.warning(t("{n} item is held and stays in the trash|{n} items are held and stay in the trash", { n: kept.length }), {
    description: kept
      .slice(0, 5)
      .map((k) => `${k.name}: ${keptText(k)}`)
      .concat(kept.length > 5 ? [t("and {n} more", { n: kept.length - 5 })] : [])
      .join("\n"),
    duration: 10000,
  });
}
