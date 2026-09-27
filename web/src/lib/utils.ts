export { cn } from "cn";
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
  hour12: true,
});

/** Windows File Explorer format: 9/3/2026 10:15 PM (zh-TW: 2026/9/3 with the Chinese PM marker before 10:15) */
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

export async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Fallback when the clipboard API isn't available over http
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
}
