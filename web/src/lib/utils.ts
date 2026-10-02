export { cn } from "cn";
import { toast } from "sonner";
import { locale, t } from "@/lib/i18n";

/**
 * Whether a request failed because the server couldn't be reached: fetch() then throws a TypeError whose message
 * depends on the browser ("Failed to fetch", "NetworkError when attempting to fetch resource.", "Load failed")
 */
export function isNetworkError(e: unknown): boolean {
  return e instanceof TypeError && /fetch|network|load failed/i.test(e.message);
}

/** What a request that couldn't reach the server says, in place of the browser's own English message */
export const unreachable = () => t("Can't reach the server. Check your connection and try again.");

/** What went wrong, to show: an error's message, or `fallback` for anything else thrown */
export function errorMessage(e: unknown, fallback: string): string {
  if (isNetworkError(e)) return unreachable();
  return e instanceof Error ? e.message : fallback;
}

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

/**
 * Dates and times are written one way everywhere: numbers, in the order and with the clock of the interface's locale,
 * as File Explorer lists them. en-US: 10/2/2026 2:44 PM; en-GB: 02/10/2026 14:44; zh-TW: 2026/10/2, then the PM marker, then 2:44
 */
const DATE: Intl.DateTimeFormatOptions = { year: "numeric", month: "numeric", day: "numeric" };
const TIME: Intl.DateTimeFormatOptions = { hour: "numeric", minute: "2-digit" };

const formats = new Map<string, Intl.DateTimeFormat>();
/** A formatter for the interface's locale, made once per set of options (and time zone) */
function dateFormat(options: Intl.DateTimeFormatOptions, timeZone?: string): Intl.DateTimeFormat {
  const key = JSON.stringify([options, timeZone]);
  let f = formats.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(locale, { ...options, timeZone });
    formats.set(key, f);
  }
  return f;
}

/** No comma between the date and the time, as File Explorer writes them; the Chinese AM/PM marker set apart */
const tidy = (s: string) =>
  s
    .replace(/,(?= )/, "")
    .replace(/\s*(上午|下午)\s*/, " $1 ") // i18n-ignore: tidies the Chinese AM/PM markers in formatted dates
    .trim();

/** A date and time: 10/2/2026 2:44 PM. `timeZone` writes it as the clock there shows it (default: the browser's) */
export function formatDateTime(ts: number, timeZone?: string): string {
  return tidy(dateFormat({ ...DATE, ...TIME }, timeZone).format(new Date(ts * 1000)));
}

/** A date: 10/2/2026 */
export function formatDate(ts: number): string {
  return dateFormat(DATE).format(new Date(ts * 1000));
}

/** A time of day: 2:44 PM */
export function formatClock(ts: number): string {
  return tidy(dateFormat(TIME).format(new Date(ts * 1000)));
}

/** Windows File Explorer format: always shown in KB, e.g. 4,032 KB */
export function formatWinSize(n: number): string {
  return `${(n === 0 ? 0 : Math.max(1, Math.ceil(n / 1024))).toLocaleString(locale)} KB`;
}

/** A recent time in words ("Just now", "5 minutes ago", "Today 2:05 PM"), otherwise the date and time */
export function formatTime(ts: number): string {
  const d = new Date(ts * 1000);
  const diff = Date.now() / 1000 - ts;
  if (diff < 60) return t("Just now");
  if (diff < 3600) return t("{n} minute ago|{n} minutes ago", { n: Math.floor(diff / 60) });
  if (diff < 86400 && new Date().getDate() === d.getDate()) return t("Today {time}", { time: formatClock(ts) });
  return formatDateTime(ts);
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
