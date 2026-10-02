// Dates and times (lib/utils.ts) are written one way everywhere, in the order and with the clock of the interface's
// locale. The language is decided when the modules load, so each test imports fresh copies after choosing it.
import { afterEach, describe, expect, test, vi } from "vitest";
import { DICT as ZH } from "@/lib/i18n/zh-TW";

async function load(lang: "en" | "zh-TW" | "zh-CN" | "ja", browser = "en-US") {
  vi.resetModules();
  localStorage.setItem("tf-lang", lang);
  // Simplified Chinese and Japanese aren't offered yet
  localStorage.setItem("tf-lang-preview", "1");
  window.__TF_DICT__ = lang === "zh-TW" ? { LANG: "zh-TW", DICT: ZH } : undefined;
  vi.spyOn(navigator, "languages", "get").mockReturnValue([browser]);
  return import("@/lib/utils");
}

afterEach(() => {
  localStorage.clear();
  window.__TF_DICT__ = undefined;
  vi.restoreAllMocks();
});

// 2 October 2026, 14:44 in the test's own time zone
const at = new Date(2026, 9, 2, 14, 44).getTime() / 1000;

describe("dates and times", () => {
  test("en-US: month first and the 12-hour clock, in lists and dialogs alike", async () => {
    const u = await load("en");
    expect(u.formatDateTime(at)).toBe("10/2/2026 2:44 PM");
    expect(u.formatDate(at)).toBe("10/2/2026");
    expect(u.formatClock(at)).toBe("2:44 PM");
  });

  test("en-GB: day first and the 24-hour clock", async () => {
    const u = await load("en", "en-GB");
    expect(u.formatDateTime(at)).toBe("02/10/2026 14:44");
  });

  test("zh-TW: year first and the Chinese clock, one way everywhere", async () => {
    const u = await load("zh-TW");
    const expected = new Intl.DateTimeFormat("zh-TW", { year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "2-digit" }).format(new Date(at * 1000));
    // The Intl text, with the AM/PM marker set apart by spaces
    expect(u.formatDateTime(at).replace(/\s+/g, "")).toBe(expected.replace(/\s+/g, ""));
    expect(u.formatDateTime(at)).toMatch(/^2026\/10\/2 \S+ 2:44$/);
    expect(u.formatDate(at)).toBe("2026/10/2");
  });

  test("zh-CN and ja: year first and the 24-hour clock", async () => {
    for (const lang of ["zh-CN", "ja"] as const) {
      const u = await load(lang);
      expect(u.formatDateTime(at)).toBe("2026/10/2 14:44");
      expect(u.formatDate(at)).toBe("2026/10/2");
      expect(u.formatClock(at)).toBe("14:44");
    }
  });

  test("a time zone writes the time as the clock there shows it", async () => {
    const u = await load("en");
    expect(u.formatDateTime(Date.UTC(2026, 9, 2, 6, 30) / 1000, "Asia/Taipei")).toBe("10/2/2026 2:30 PM");
    expect(u.formatDateTime(Date.UTC(2026, 9, 2, 6, 30) / 1000, "UTC")).toBe("10/2/2026 6:30 AM");
  });
});
