// Formats, plurals and sorting in each of the interface's languages. The language is decided when the modules load,
// so each test imports fresh copies after choosing it. Chinese and Japanese expectations come from Intl itself or from
// the dictionaries (no translations in the tests).
import { afterEach, describe, expect, test, vi } from "vitest";
import { formatCell, setFormatLocale } from "@/ooxml/core/numfmt";

type Lang = "en" | "zh-TW" | "zh-CN" | "ja";
const LANGS: Lang[] = ["en", "zh-TW", "zh-CN", "ja"];

/** Fresh copies of the i18n module, the formats and type-to-find in `lang` (with a dictionary of `dict`, if given) */
async function load(lang: Lang, dict?: Record<string, string>) {
  vi.resetModules();
  localStorage.setItem("tf-lang", lang);
  // Simplified Chinese and Japanese aren't offered yet
  localStorage.setItem("tf-lang-preview", "1");
  window.__TF_DICT__ = dict ? { LANG: lang, DICT: dict } : undefined;
  vi.spyOn(navigator, "languages", "get").mockReturnValue(["en-US"]);
  const i18n = await import("@/lib/i18n");
  await i18n.loadDictionary();
  return { i18n, utils: await import("@/lib/utils"), keys: await import("@/lib/keys") };
}

afterEach(() => {
  localStorage.clear();
  window.__TF_DICT__ = undefined;
  vi.restoreAllMocks();
  setFormatLocale(null);
});

/** Intl's locale for each language: English follows the browser (en-US here) */
const intl = (lang: Lang) => (lang === "en" ? "en-US" : lang);

describe("formats in each language", () => {
  test("numbers in texts, sizes and sizes as File Explorer lists them", async () => {
    for (const lang of LANGS) {
      const { i18n, utils } = await load(lang, {});
      expect(i18n.locale).toBe(intl(lang));
      expect(i18n.t("{n} files", { n: 12345 })).toBe(`${(12345).toLocaleString(intl(lang))} files`);
      expect(utils.formatBytes(1.5 * 1024 * 1024)).toBe(`${new Intl.NumberFormat(intl(lang), { minimumFractionDigits: 1 }).format(1.5)} MB`);
      expect(utils.formatBytes(512)).toBe("512 B");
      expect(utils.formatWinSize(5_000_000)).toBe(`${(4883).toLocaleString(intl(lang))} KB`);
    }
    const { utils } = await load("en");
    expect(utils.formatBytes(1.5 * 1024 * 1024)).toBe("1.5 MB");
    expect(utils.formatBytes(200 * 1024 * 1024)).toBe("200 MB");
    expect(utils.formatWinSize(5_000_000)).toBe("4,883 KB");
  });

  test("relative times, ages and lists follow Intl", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 9, 2, 14, 44));
    try {
      for (const lang of LANGS) {
        const { utils } = await load(lang, {});
        const now = Date.now() / 1000;
        expect(utils.formatTime(now - 5 * 60)).toBe(new Intl.RelativeTimeFormat(intl(lang), { numeric: "always" }).format(-5, "minute"));
        expect(utils.formatAge(3 * 3600)).toBe(new Intl.NumberFormat(intl(lang), { style: "unit", unit: "hour", unitDisplay: "short" }).format(3));
        expect(utils.formatList(["A", "B", "C"])).toBe(new Intl.ListFormat(intl(lang), { type: "conjunction" }).format(["A", "B", "C"]));
      }
      const { utils } = await load("en");
      const now = Date.now() / 1000;
      expect(utils.formatTime(now - 60)).toBe("1 minute ago");
      expect(utils.formatTime(now - 5 * 60)).toBe("5 minutes ago");
      expect(utils.formatAge(45)).toBe("45 sec");
      expect(utils.formatAge(5 * 60)).toBe("5 min");
      expect(utils.formatAge(3 * 86400)).toBe("3 days");
      expect(utils.formatList(["NAS", "S3"])).toBe("NAS and S3");
      expect(utils.formatList(["NAS", "S3", "SFTP"])).toBe("NAS, S3, and SFTP");
    } finally {
      vi.useRealTimers();
    }
  });

  test("a spreadsheet's month and weekday names and AM/PM follow the language, unless the format names a locale", () => {
    // 2026-10-02 15:00, a Friday
    const serial = (Date.UTC(2026, 9, 2, 15) - Date.UTC(1899, 11, 30)) / 86400000;
    for (const lang of ["en", "zh-Hant", "zh-Hans", "ja"]) {
      setFormatLocale(lang);
      const name = (options: Intl.DateTimeFormatOptions) => new Intl.DateTimeFormat(lang, { ...options, timeZone: "UTC" }).format(Date.UTC(2026, 9, 2, 15));
      const period = new Intl.DateTimeFormat(lang, { hour: "numeric", hour12: true, timeZone: "UTC" }).formatToParts(Date.UTC(2026, 9, 2, 15)).find((p) => p.type === "dayPeriod")!.value;
      expect(formatCell(serial, "d mmm").text).toBe(`2 ${name({ month: "short" })}`);
      expect(formatCell(serial, "dddd").text).toBe(name({ weekday: "long" }));
      expect(formatCell(serial, "ddd").text).toBe(name({ weekday: "short" }));
      expect(formatCell(serial, "h:mm AM/PM").text).toBe(`3:00 ${period}`);
      // A format for English keeps English names in any language
      expect(formatCell(serial, "[$-409]mmmm d, yyyy h AM/PM").text).toBe("October 2, 2026 3 PM");
    }
    setFormatLocale("en");
    expect(formatCell(serial, "dddd, mmmm d").text).toBe("Friday, October 2");
    expect(formatCell(serial, "d mmmmm").text).toBe("2 O");
  });
});

describe("plurals", () => {
  test("English picks the singular for 1 only, and the plural without a number", async () => {
    const { i18n } = await load("en");
    expect(i18n.t("{n} file|{n} files", { n: 1 })).toBe("1 file");
    expect(i18n.t("{n} file|{n} files", { n: 0 })).toBe("0 files");
    expect(i18n.t("{n} file|{n} files", { n: 2 })).toBe("2 files");
    expect(i18n.t("{n} file|{n} files", { n: 1.5 })).toBe("1.5 files");
    expect(i18n.t("{n} file|{n} files")).toBe("{n} files");
  });

  test("Chinese and Japanese have one form, a translation with several picks by the language's rules", async () => {
    for (const lang of ["zh-TW", "zh-CN", "ja"] as const) {
      expect(new Intl.PluralRules(lang).resolvedOptions().pluralCategories).toEqual(["other"]);
      // One form in the dictionary: used for every number
      const one = await load(lang, { "{n} file|{n} files": "#{n}" });
      expect(one.i18n.t("{n} file|{n} files", { n: 1 })).toBe(`#1`);
      expect(one.i18n.t("{n} file|{n} files", { n: 5 })).toBe(`#5`);
      // Missing: the English source, by English rules
      expect(one.i18n.t("{n} item|{n} items", { n: 1 })).toBe("1 item");
      expect(one.i18n.t("{n} item|{n} items", { n: 3 })).toBe("3 items");
    }
  });

  test("the server's counts pick plural forms too, also with digit grouping", async () => {
    const { i18n } = await load("en");
    expect(i18n.tServer("Deleted 1 file")).toBe("Deleted 1 file");
    const zh = await load("zh-TW", { "Deleted {n} file|Deleted {n} files": "#{n}", "My files": "@" });
    expect(zh.i18n.tServer("Deleted 1,234 files")).toBe("#1,234");
    expect(zh.i18n.tServer("Deleted 1 file")).toBe("#1");
  });
});

describe("lists from the server", () => {
  test("are joined the language's way", async () => {
    const dict = { "Password protected": "P", "accepts files": "A", "preview only": "V" };
    for (const lang of ["zh-TW", "zh-CN", "ja"] as const) {
      const { i18n } = await load(lang, dict);
      const separator = new Intl.ListFormat(lang, { type: "conjunction" }).formatToParts(["a", "b", "c"]).find((p) => p.type === "literal")!.value;
      expect(i18n.tServer("Password protected, accepts files, preview only")).toBe(["P", "A", "V"].join(separator));
    }
  });
});

describe("sorting and type-to-find", () => {
  test("names sort the language's way, numbers by value and letter case ignored", async () => {
    for (const lang of LANGS) {
      const { utils } = await load(lang);
      expect(["File 10", "file 2", "File 1"].sort(utils.nameCollator.compare)).toEqual(["File 1", "file 2", "File 10"]);
    }
  });

  test("typed text finds names ignoring case and accents, and in Japanese hiragana finds katakana", async () => {
    const en = await load("en");
    expect(en.keys.findByPrefix(["Zebra", "École", "eclair"], "ec", -1)).toBe(1);
    expect(en.keys.findByPrefix(["Zebra", "École", "eclair"], "EC", 1)).toBe(2);
    const ja = await load("ja");
    // "a" in hiragana finds a name that starts with "a" in katakana
    expect(ja.keys.findByPrefix(["\u30a2\u30eb\u30d0\u30e0", "\u30a4\u30f3\u30af"], "\u3042", -1)).toBe(0);
  });
});
