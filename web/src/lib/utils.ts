export { cn } from "cn";
import { toast } from "sonner";
import { locale, t } from "@/lib/i18n";

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

const dateFmt = new Intl.DateTimeFormat(locale, {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
});

const winDateFmt = new Intl.DateTimeFormat(locale, {
  year: "numeric",
  month: "numeric",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
  // English follows the region's clock (en-GB: 24-hour)
  hour12: locale === "zh-TW" ? true : undefined,
});

/**
 * Windows File Explorer format: 9/3/2026 10:15 PM, 03/09/2026 22:15 in en-GB
 * (zh-TW: 2026/9/3 with the Chinese PM marker before 10:15)
 */
export function formatWinDate(ts: number): string {
  const s = winDateFmt.format(new Date(ts * 1000));
  return locale === "zh-TW" ? s.replace(/\s*(上午|下午)\s*/, " $1 ") : s.replace(",", ""); // i18n-ignore: tidies the Chinese AM/PM markers in formatted dates
}

/** Windows File Explorer format: always shown in KB, e.g. 4,032 KB */
export function formatWinSize(n: number): string {
  return `${(n === 0 ? 0 : Math.max(1, Math.ceil(n / 1024))).toLocaleString(locale)} KB`;
}

/** 2026/09/25 14:48 */
export function formatDateTime(ts: number): string {
  return dateFmt.format(new Date(ts * 1000));
}

/** A recent time in words ("Just now", "5 minutes ago", "Today 14:05"), otherwise the date */
export function formatTime(ts: number): string {
  const d = new Date(ts * 1000);
  const diff = Date.now() / 1000 - ts;
  if (diff < 60) return t("Just now");
  if (diff < 3600) return t("{n} minute ago|{n} minutes ago", { n: Math.floor(diff / 60) });
  if (diff < 86400 && new Date().getDate() === d.getDate()) return t("Today {time}", { time: d.toTimeString().slice(0, 5) });
  return dateFmt.format(d);
}

export function formatDate(ts: number): string {
  return new Intl.DateTimeFormat(locale, { year: "numeric", month: "2-digit", day: "2-digit" }).format(new Date(ts * 1000));
}

/** Sorts names the way File Explorer does: "File 2" before "File 10", letter case ignored (the server sorts the same way) */
export const nameCollator = new Intl.Collator(locale, { numeric: true, sensitivity: "base" });

export function extOf(name: string): string {
  const i = name.lastIndexOf(".");
  return i > 0 ? name.slice(i + 1).toLowerCase() : "";
}

/**
 * Puts text on the clipboard; false when the browser refused. Text that is still coming (a link the server is making)
 * is passed as a promise, and this is called right in the click: browsers such as Safari only allow writing to the
 * clipboard during the click itself, not once an answer has come back.
 */
export async function copyText(text: string | Promise<string>): Promise<boolean> {
  if (typeof text !== "string") {
    // A clipboard item can wait for its text, and keeps the right to write it that the click gave
    if (typeof ClipboardItem !== "undefined" && navigator.clipboard?.write) {
      try {
        await navigator.clipboard.write([new ClipboardItem({ "text/plain": text.then((s) => new Blob([s], { type: "text/plain" })) })]);
        return true;
      } catch {
        // Refused, or the text couldn't be made: try again below with the text itself
      }
    }
    try {
      text = await text;
    } catch {
      return false;
    }
  }
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Fallback when the clipboard API isn't available over http
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    let ok = false;
    try {
      ok = document.execCommand("copy");
    } catch {
      // Not allowed either
    }
    ta.remove();
    return ok;
  }
}

/** Copies text, then says it was copied (`done`), or that the browser refused */
export async function copyAndSay(text: string, done = t("Copied")) {
  if (await copyText(text)) toast.success(done);
  else toast.error(t("Couldn't copy"));
}

/** How long removed items stay in the trash, for the texts that explain it */
export function trashHint(days: number) {
  return days > 0
    ? t("Removed items stay in the trash for {n} day and can be restored until then.|Removed items stay in the trash for {n} days and can be restored until then.", { n: days })
    : t("Removed items stay in the trash and can be restored until it's emptied.");
}
