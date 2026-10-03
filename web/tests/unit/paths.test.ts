// The language is decided when the module loads, so each test imports a fresh copy after choosing it.
// Expected Chinese text is taken from the dictionary itself (no Chinese in the tests).
import { afterEach, describe, expect, test, vi } from "vitest";
import { DICT as ZH } from "@/lib/i18n/zh-TW";

async function load(lang: "en" | "zh-TW") {
  vi.resetModules();
  localStorage.setItem("tf-lang", lang);
  window.__TF_DICT__ = lang === "zh-TW" ? { LANG: "zh-TW", DICT: ZH } : undefined;
  return import("@/lib/paths");
}

afterEach(() => {
  localStorage.clear();
  window.__TF_DICT__ = undefined;
});

const info = (location: string[] | null, kind: "personal" | "company" | "team", via_share = false) => ({
  location,
  via_share,
  drive: { id: "d", name: "x", kind, root_id: "r" },
});

describe("pathOf", () => {
  test("joins the location, with the system's names in the UI language", async () => {
    const en = await load("en");
    expect(en.pathOf(info(["My files", "Reports"], "personal"))).toBe("/My files/Reports");
    expect(en.pathOf(info(null, "team"))).toBeUndefined();
    expect(en.pathOf(undefined)).toBeUndefined();

    const zh = await load("zh-TW");
    expect(zh.pathOf(info(["My files", "Reports"], "personal"))).toBe(`/${ZH["My files"]}/Reports`);
    expect(zh.pathOf(info(["All files"], "company"))).toBe(`/${ZH["All files"]}`);
    expect(zh.pathOf(info(["Shared with me", "Plans", "Q1"], "personal", true))).toBe(`/${ZH["Shared with me"]}/Plans/Q1`);
    // Spaces people named stay as they are, also when named like the system's ones
    expect(zh.pathOf(info(["My files (2)", "a"], "team"))).toBe("/My files (2)/a");
    expect(zh.pathOf(info(["My files"], "team"))).toBe("/My files");
  });
});

describe("pathAliases", () => {
  test("maps the names as shown to the names paths use", async () => {
    expect((await load("en")).pathAliases()).toEqual({});
    expect((await load("zh-TW")).pathAliases()).toEqual({
      [ZH["My files"]]: "My files",
      [ZH["All files"]]: "All files",
      [ZH["Shared with me"]]: "Shared with me",
    });
  });
});

describe("urlOf and appLink", () => {
  test.each(["en", "zh-TW"] as const)("Control panel paths round-trip in %s without accepting arbitrary routes", async (lang) => {
    const { controlPanelPath } = await load(lang);
    const { CONTROL_PANEL_ITEMS } = await import("@/admin/controlPanel");
    const root = lang === "en" ? "Control panel" : ZH["Control panel"];
    expect(controlPanelPath("/admin", CONTROL_PANEL_ITEMS)).toBe("/admin");
    for (const item of CONTROL_PANEL_ITEMS) {
      expect(controlPanelPath(`/admin/${item.key}`, CONTROL_PANEL_ITEMS)).toBe(item.to);
      expect(controlPanelPath(`/${root}/${item.title}`, CONTROL_PANEL_ITEMS)).toBe(item.to);
    }
    expect(controlPanelPath("/admin/not-a-page", CONTROL_PANEL_ITEMS)).toBeNull();
    expect(controlPanelPath("/admin/storage/anything", CONTROL_PANEL_ITEMS)).toBeNull();
    expect(controlPanelPath("/My files/admin/storage", CONTROL_PANEL_ITEMS)).toBeNull();
    expect(controlPanelPath("//admin/storage", CONTROL_PANEL_ITEMS)).toBeNull();
  });
  test("a found path opens its page", async () => {
    const { urlOf } = await load("en");
    expect(urlOf({ place: "spaces", id: null, path: [] })).toBe("/drives");
    expect(urlOf({ place: "shared", id: null, path: ["Shared with me"] })).toBe("/shared-with-me");
    expect(urlOf({ place: "folder", id: "f1", path: ["My files", "a"] })).toBe("/files/f1");
    expect(urlOf({ place: "file", id: "n1", path: ["My files", "a.txt"] })).toBe("/view/n1");
  });

  test("links to this site's pages are followed, other text is a path", async () => {
    const { appLink } = await load("en");
    const origin = "https://files.example.com";
    expect(appLink(" https://files.example.com/files/abc?x=1 ", origin)).toBe("/files/abc?x=1");
    expect(appLink("https://files.example.com/dav/My%20files/", origin)).toBeNull();
    expect(appLink("https://elsewhere.example.com/files/abc", origin)).toBeNull();
    expect(appLink("/My files/Reports", origin)).toBeNull();
    expect(appLink("My files", origin)).toBeNull();
  });
});
