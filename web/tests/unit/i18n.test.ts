// The language is decided when the module loads, so each test imports a fresh copy after choosing it.
// Expected Chinese text is taken from the dictionary itself (no Chinese in the tests).
import { afterEach, describe, expect, test, vi } from "vitest";
import { ZH } from "@/lib/i18n/zh-TW";

async function load(lang: "en" | "zh-TW") {
  vi.resetModules();
  localStorage.setItem("tf-lang", lang);
  window.__TF_ZH__ = lang === "zh-TW" ? { ZH } : undefined;
  return import("@/lib/i18n");
}

const fill = (s: string, vars: Record<string, string>) => s.replace(/\{(\w+)\}/g, (_, k: string) => vars[k]);

afterEach(() => {
  localStorage.clear();
  window.__TF_ZH__ = undefined;
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
});
