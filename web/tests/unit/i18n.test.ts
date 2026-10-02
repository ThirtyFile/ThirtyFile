// The language is decided when the module loads, so each test imports a fresh copy after choosing it.
// Expected Chinese text is taken from the dictionary itself (no Chinese in the tests).
import { readdirSync } from "node:fs";
import { afterEach, describe, expect, test, vi } from "vitest";
import { DICT as ZH } from "@/lib/i18n/zh-TW";

async function load(lang: "en" | "zh-TW") {
  vi.resetModules();
  localStorage.setItem("tf-lang", lang);
  window.__TF_DICT__ = lang === "zh-TW" ? { LANG: "zh-TW", DICT: ZH } : undefined;
  return import("@/lib/i18n");
}

const fill = (s: string, vars: Record<string, string>) => s.replace(/\{(\w+)\}/g, (_, k: string) => vars[k]);

afterEach(() => {
  localStorage.clear();
  window.__TF_DICT__ = undefined;
});

describe("English", () => {
  test("t fills parameters and picks plurals", async () => {
    const { t, tServer } = await load("en");
    expect(t("{n} item selected|{n} items selected", { n: 1 })).toBe("1 item selected");
    expect(t("{n} item selected|{n} items selected", { n: 3 })).toBe("3 items selected");
    expect(t("Hello {who}", {})).toBe("Hello {who}");
    expect(tServer("Anything the server says")).toBe("Anything the server says");
    expect(tServer(null)).toBe("");
  });
});

describe("Traditional Chinese", () => {
  test("t and tc look up the dictionary and fall back to English", async () => {
    const { t, lang } = await load("zh-TW");
    expect(lang).toBe("zh-TW");
    expect(t("My files")).toBe(ZH["My files"]);
    expect(t("Not a dictionary entry {x}", { x: "1" })).toBe("Not a dictionary entry 1");
  });

  test("tServer translates exact messages", async () => {
    const { tServer } = await load("zh-TW");
    expect(tServer("My files")).toBe(ZH["My files"]);
    expect(tServer("Something new from the server")).toBe("Something new from the server");
  });

  test("tServer fills templates, translating known terms in the parameters", async () => {
    const { tServer } = await load("zh-TW");
    expect(tServer('"Report.docx" already exists')).toBe(fill(ZH['"{name}" already exists'], { name: "Report.docx" }));
    expect(tServer('Not enough storage space in "My files"')).toBe(fill(ZH['Not enough storage space in "{name}"'], { name: ZH["My files"] }));
  });

  test("tServer translates nested messages", async () => {
    const { tServer } = await load("zh-TW");
    const inner = 'The destination folder already contains "a"';
    expect(tServer(`Connection test failed: ${inner}`)).toBe(
      fill(ZH["Connection test failed: {error}"], { error: fill(ZH['The destination folder already contains "{name}"'], { name: "a" }) }),
    );
  });

  test("tServer translates a list of known items, such as a share link's options, whatever the order", async () => {
    const { tServer } = await load("zh-TW");
    const list = (...parts: string[]) => parts.join("、");
    expect(tServer("Expires 2026-05-01, accepts files, preview only")).toBe(list(fill(ZH["expires {date}"], { date: "2026-05-01" }), ZH["accepts files"], ZH["preview only"]));
    expect(tServer("Password protected, limited to 3 downloads, only accepts files")).toBe(
      list(ZH["Password protected"], fill(ZH["limited to {n} downloads"], { n: "3" }), ZH["only accepts files"]),
    );
    // A change to a link: each change, also those with a date or a number
    expect(tServer("/share/abc: changed the password, expires 2026-05-01, view and download")).toBe(
      fill(ZH["{username}: {changes}"], { username: "/share/abc", changes: list(ZH["changed the password"], fill(ZH["expires {date}"], { date: "2026-05-01" }), ZH["view and download"]) }),
    );
  });

  test("names ThirtyFile gives backups, copies and replica policies are shown translated, also inside messages", async () => {
    const { tServer, tMadeName } = await load("zh-TW");
    const backup = fill(ZH["Backup of {name}"], { name: ZH["Local disk"] });
    expect(tMadeName("Backup of Local disk")).toBe(backup);
    expect(tMadeName("Copy of NAS")).toBe(fill(ZH["Copy of {name}"], { name: "NAS" }));
    expect(tMadeName("Upload")).toBe("Upload");
    expect(tServer("Backup of Local disk: Local disk → NAS")).toBe(fill(ZH["{name}: {source} → {dest}"], { name: backup, source: ZH["Local disk"], dest: "NAS" }));
    expect(tServer("The backup “Backup of Local disk” can't reach its location")).toBe(fill(ZH["The backup “{name}” can't reach its location"], { name: backup }));
  });
});

describe("choosing the language", () => {
  test("browser language tags match the four languages leniently", async () => {
    const { matchLanguage } = await load("en");
    const cases: [string, string | undefined][] = [
      ["en-US", "en"],
      ["en", "en"],
      ["zh-TW", "zh-TW"],
      ["zh-Hant", "zh-TW"],
      ["zh-HK", "zh-TW"],
      ["zh-MO", "zh-TW"],
      ["zh-Hant-HK", "zh-TW"],
      ["zh", "zh-CN"],
      ["zh-CN", "zh-CN"],
      ["zh-Hans", "zh-CN"],
      ["zh-SG", "zh-CN"],
      ["zh-Hans-HK", "zh-CN"],
      ["zh_tw", "zh-TW"],
      ["ja", "ja"],
      ["ja-JP", "ja"],
      ["fr-FR", undefined],
      ["", undefined],
    ];
    for (const [tag, want] of cases) expect([tag, matchLanguage(tag)]).toEqual([tag, want]);
  });

  test("the browser's first language that can be used wins", async () => {
    const { browserLanguage } = await load("en");
    const all = () => true;
    expect(browserLanguage(["fr", "ja-JP", "en"], all)).toBe("ja");
    expect(browserLanguage(["zh-CN", "en"], all)).toBe("zh-CN");
    expect(browserLanguage(["fr", "de"], all)).toBe("en");
    expect(browserLanguage([], all)).toBe("en");
  });

  test("while Simplified Chinese and Japanese aren't ready, Chinese browsers get Traditional Chinese and Japanese ones the next language", async () => {
    const { browserLanguage } = await load("en");
    const ready = (l: string) => l === "en" || l === "zh-TW";
    expect(browserLanguage(["zh-CN"], ready)).toBe("zh-TW");
    expect(browserLanguage(["ja-JP"], ready)).toBe("en");
    expect(browserLanguage(["ja-JP", "zh-TW"], ready)).toBe("zh-TW");
  });

  test("the language the server says was picked comes first, then a choice kept here, then the browser, then the system default", async () => {
    const detected = async (languages: string[]) => {
      vi.resetModules();
      vi.stubGlobal("navigator", { ...navigator, languages });
      return (await import("@/lib/i18n")).lang;
    };
    try {
      // The system default only counts when none of the browser's languages fits
      window.__TF_DEFAULT_LANG__ = "zh-TW";
      expect(await detected(["en-GB"])).toBe("en");
      expect(await detected(["fr", "de"])).toBe("zh-TW");
      expect(await detected(["zh-HK", "en"])).toBe("zh-TW");
      localStorage.setItem("tf-lang", "en");
      expect(await detected(["zh-HK"])).toBe("en");
      // Saved with the account, chosen in this browser, or (signed in) the system default: the server says which
      window.__TF_LANG__ = "zh-TW";
      expect(await detected(["en"])).toBe("zh-TW");
      // …unless this page doesn't offer it
      window.__TF_LANG__ = "ja";
      expect(await detected(["fr"])).toBe("en");
    } finally {
      vi.unstubAllGlobals();
      window.__TF_DEFAULT_LANG__ = undefined;
      window.__TF_LANG__ = undefined;
    }
  });

  test("after signing in, the page switches once to the language picked for the person", async () => {
    const { languageToAdopt, adoptLanguage } = await load("en");
    const reload = vi.fn<() => void>();
    vi.stubGlobal("location", { ...window.location, reload });
    try {
      expect(languageToAdopt(null)).toBeUndefined();
      expect(languageToAdopt("en")).toBeUndefined();
      expect(languageToAdopt("ja")).toBeUndefined(); // not offered here
      expect(languageToAdopt("zh-TW")).toBe("zh-TW");
      adoptLanguage("zh-TW");
      expect(reload).toHaveBeenCalledTimes(1);
      // The page came back in another language (made elsewhere than by the server): it doesn't reload for it again
      expect(languageToAdopt("zh-TW")).toBeUndefined();
    } finally {
      vi.unstubAllGlobals();
      sessionStorage.clear();
    }
  });

  test("<html lang> names the script, so fonts draw the characters the language's way", async () => {
    vi.resetModules();
    localStorage.setItem("tf-lang-preview", "1");
    for (const [chosen, tag] of [
      ["en", "en"],
      ["zh-TW", "zh-Hant"],
      ["zh-CN", "zh-Hans"],
      ["ja", "ja"],
    ]) {
      vi.resetModules();
      localStorage.setItem("tf-lang", chosen);
      const { htmlLang } = await import("@/lib/i18n");
      expect(htmlLang).toBe(tag);
      expect(document.documentElement.lang).toBe(tag);
    }
  });
});

describe("dictionaries", () => {
  /** A fresh copy of the module with these settings */
  async function fresh(settings: { saved?: string; preview?: boolean; dict?: { LANG: string; DICT: Record<string, string> } }) {
    vi.resetModules();
    if (settings.saved) localStorage.setItem("tf-lang", settings.saved);
    if (settings.preview) localStorage.setItem("tf-lang-preview", "1");
    window.__TF_DICT__ = settings.dict;
    return import("@/lib/i18n");
  }

  test("every language has the same dictionary files", () => {
    const files = (lang: string) => readdirSync(`src/lib/i18n/${lang}`).sort();
    for (const lang of ["zh-CN", "ja"]) expect(files(lang)).toEqual(files("zh-TW"));
  });

  test("a language that isn't ready is neither offered nor used, unless previewing", async () => {
    const hidden = await fresh({ saved: "ja" });
    expect(hidden.LANGS.map((l) => l.id)).toEqual(hidden.LANGUAGES.filter((l) => l.ready).map((l) => l.id));
    expect(hidden.lang).not.toBe("ja");
    localStorage.clear();
    const shown = await fresh({ saved: "ja", preview: true });
    expect(shown.LANGS.map((l) => l.id)).toEqual(["en", "zh-TW", "zh-CN", "ja"]);
    expect(shown.lang).toBe("ja");
  });

  test("the page's own dictionary is loaded when the server added another one", async () => {
    const { DICT: JA } = await import("@/lib/i18n/ja");
    const i18n = await fresh({ saved: "ja", preview: true, dict: { LANG: "zh-TW", DICT: ZH } });
    expect(i18n.t("Request failed ({status})", { status: 500 })).toBe("Request failed (500)");
    await i18n.loadDictionary();
    expect(i18n.t("My files")).toBe(JA["My files"]);
  });

  test("the dictionary the server added is used at once", async () => {
    const i18n = await fresh({ saved: "zh-TW", dict: { LANG: "zh-TW", DICT: ZH } });
    expect(i18n.t("My files")).toBe(ZH["My files"]);
  });
});
