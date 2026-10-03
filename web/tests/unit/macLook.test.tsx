// The Mac style's look (components/style/mac/art): its tokens against the ones every style sets (src/style.css), in
// both themes and with enough contrast; its icons for every kind and format of file; its symbols; and its assets
// loading only with the style
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, test } from "vitest";
import { FileIcon, type FileCategory } from "@/components/FileIcon";
import { StyleKitContext } from "@/components/style";
import { macKit } from "@/components/style/mac";
import { glyphColor, inkOn, macArtFor } from "@/components/style/mac/art/documents";
import { MAC_SYMBOLS } from "@/components/style/mac/art/symbols";
import { loadMacArt, macArt } from "@/components/style/mac/loadArt";
import { macSymbols } from "@/components/style/mac/look";
import { windowsKit } from "@/components/style/windows";
import { FILE_TYPES } from "@/lib/fileTypes";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// ───────────── Tokens ─────────────

const src = join(process.cwd(), "src");
const read = (path: string) => readFileSync(join(src, path), "utf8");

/** The custom properties of every rule of a stylesheet whose selector is exactly `selector` */
function block(css: string, selector: string): Map<string, string> {
  const out = new Map<string, string>();
  const body = css.replace(/\/\*[\s\S]*?\*\//g, "");
  for (const m of body.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    if (m[1].trim() !== selector) continue;
    for (const d of m[2].matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) out.set(d[1], d[2].trim());
  }
  return out;
}

const shared = read("style.css");
const mac = read("components/style/mac/art/mac.css");
/** The tokens of the style layer that the Windows style sets (the shared stylesheet's) */
const windowsTokens = [...block(shared, ":root").keys()].filter((k) => k.startsWith("--tf-"));
const palette = { light: block(shared, ":root"), dark: new Map([...block(shared, ":root"), ...block(shared, ".dark")]) };
const macLight = block(mac, ':root[data-style="mac"]');
const macDark = block(mac, ':root.dark[data-style="mac"]');
/** Every token as the Mac style has it in a theme: its own over the shared palette */
const theme = { light: new Map([...palette.light, ...macLight]), dark: new Map([...palette.dark, ...macLight, ...macDark]) };

/** Sizes are set once; the other tokens are colours (or shadows) set for each theme */
const isSize = (token: string) => /-(text|leading|h|w|px|radius|icon|blur|stroke)$/.test(token);

type Rgba = [number, number, number, number];
/** A token's colour, following var() */
function color(t: Map<string, string>, token: string): Rgba {
  let v = t.get(token);
  for (let i = 0; v?.startsWith("var("); i++) v = t.get(/var\((--[\w-]+)\)/.exec(v)![1]);
  if (!v || !/^#([0-9a-f]{6}|[0-9a-f]{8})$/i.test(v)) throw new Error(`${token} isn't a hex colour: ${v}`);
  const n = [1, 3, 5, 7].map((i) => (i < v.length ? parseInt(v.slice(i, i + 2), 16) / 255 : 1));
  return n as Rgba;
}
const over = (top: Rgba, under: Rgba): Rgba => [0, 1, 2].map((i) => top[i] * top[3] + under[i] * (1 - top[3])).concat(1) as Rgba;
const luminance = ([r, g, b]: Rgba) => {
  const f = (c: number) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
};
/** The contrast of `fg` on the layers `bg` (the first one on top) */
function contrast(t: Map<string, string>, fg: string, ...bg: string[]) {
  const back = bg.map((b) => color(t, b)).reduceRight((under, top) => over(top, under));
  const [a, b] = [luminance(over(color(t, fg), back)), luminance(back)].sort((x, y) => y - x);
  return (a + 0.05) / (b + 0.05);
}

describe("the Mac style's tokens", () => {
  test("the shared stylesheet has the Windows style's tokens, which shared components use", () => {
    expect(windowsTokens).toEqual(expect.arrayContaining(["--tf-list-text", "--tf-sel-bg", "--tf-row-radius", "--tf-tool-h", "--tf-focus"]));
  });

  test("set every token the Windows style sets", () => {
    expect(windowsTokens.filter((k) => !macLight.has(k))).toEqual([]);
  });

  test("set every colour for the light and the dark theme, and sizes once", () => {
    const colours = [...macLight.keys()].filter((k) => !isSize(k));
    expect(colours.filter((k) => !macDark.has(k))).toEqual([]);
    expect([...macDark.keys()].filter((k) => !macLight.has(k) || isSize(k))).toEqual([]);
  });

  // Text: 4.5:1. Symbols, focus rings and the edges of controls: 3:1 (WCAG 2.2 AA, 1.4.3 and 1.4.11)
  const sidebar = (fg: string, min: number, ...on: string[]) => ["--mac-desktop-1", "--mac-desktop-2", "--mac-desktop-3"].map((d) => ({ fg, bg: [...on, "--mac-sidebar-bg", d], min }));
  const pairs = [
    // A selected row, a selected line item, a selected name in the icon views; a list that isn't the one in use
    { fg: "--tf-sel-fg", bg: ["--tf-sel-bg"], min: 4.5 },
    { fg: "--tf-sel-muted", bg: ["--tf-sel-bg"], min: 4.5 },
    { fg: "--tf-line-sel-fg", bg: ["--tf-line-sel-bg"], min: 4.5 },
    { fg: "--tf-name-sel-fg", bg: ["--tf-name-sel-bg"], min: 4.5 },
    { fg: "--tf-idle-fg", bg: ["--tf-idle-bg"], min: 4.5 },
    { fg: "--muted-foreground", bg: ["--tf-idle-bg"], min: 4.5 },
    // The edge of the picture selected in the Gallery view's strip
    { fg: "--tf-strip-sel-border", bg: ["--tf-strip-sel-bg", "--background"], min: 3 },
    // Rows hovered
    { fg: "--foreground", bg: ["--tf-row-hover", "--background"], min: 4.5 },
    { fg: "--muted-foreground", bg: ["--tf-row-hover", "--background"], min: 4.5 },
    // Focus rings, on the page and on a selection
    { fg: "--tf-focus", bg: ["--background"], min: 3 },
    { fg: "--tf-focus", bg: ["--mac-toolbar-bg"], min: 3 },
    { fg: "--tf-focus-on-sel", bg: ["--tf-sel-bg"], min: 3 },
    { fg: "--tf-focus-on-sel", bg: ["--tf-line-sel-bg"], min: 3 },
    // The toolbar: its symbols, hovered too; the title; the view switcher
    { fg: "--tf-tool-fg", bg: ["--mac-toolbar-bg"], min: 3 },
    { fg: "--tf-tool-fg", bg: ["--tf-tool-hover", "--mac-toolbar-bg"], min: 3 },
    { fg: "--foreground", bg: ["--mac-toolbar-bg"], min: 4.5 },
    { fg: "--tf-tool-fg", bg: ["--mac-seg-bg", "--mac-toolbar-bg"], min: 3 },
    { fg: "--foreground", bg: ["--mac-seg-on"], min: 4.5 },
    // The sidebar, over each colour of the backdrop it lets through: names, headings, symbols, hovered and selected
    ...sidebar("--mac-nav-fg", 4.5),
    ...sidebar("--mac-nav-heading", 4.5),
    ...sidebar("--mac-nav-symbol", 3),
    ...sidebar("--mac-nav-fg", 4.5, "--mac-nav-hover"),
    ...sidebar("--mac-nav-fg", 4.5, "--mac-nav-sel-bg"),
    ...sidebar("--mac-nav-symbol", 3, "--mac-nav-sel-bg"),
    ...sidebar("--tf-focus", 3),
  ];
  for (const name of ["light", "dark"] as const) {
    test(`have the contrast WCAG AA asks for, in the ${name} theme`, () => {
      const short = pairs
        .map(({ fg, bg, min }) => ({ pair: `${fg} on ${bg.join(" over ")}`, ratio: Math.round(contrast(theme[name], fg, ...bg) * 100) / 100, min }))
        .filter(({ ratio, min }) => ratio < min);
      expect(short).toEqual([]);
    });
  }
});

// ───────────── Icons ─────────────

describe("the Mac style's icons", () => {
  test("a folder is a folder; each general kind has a band or a picture of its own", () => {
    expect(macArtFor({ kind: "folder", type: null, ext: "" })).toEqual({ shape: "folder" });
    const kinds: (FileCategory | "table")[] = ["markdown", "pdf", "word", "sheet", "table", "slides", "image", "audio", "video", "archive", "code", "text", "other"];
    const looks = kinds.map((kind) => JSON.stringify(macArtFor({ kind, type: null, ext: "" })));
    expect(new Set(looks).size).toBe(kinds.length);
    for (const look of looks) expect(JSON.parse(look).shape).toBe("page");
  });

  test("documents show their extension on their kind's band; a file of no known kind, its extension in grey", () => {
    expect(macArtFor({ kind: "word", type: null, ext: "docx" })).toEqual({ shape: "page", band: { label: "DOCX", bg: "#2b6fd6" } });
    expect(macArtFor({ kind: "sheet", type: null, ext: "xlsx" })).toMatchObject({ band: { label: "XLSX" } });
    expect(macArtFor({ kind: "table", type: null, ext: "csv" })).toMatchObject({ band: { label: "CSV" } });
    expect(macArtFor({ kind: "pdf", type: null, ext: "" })).toMatchObject({ band: { label: "PDF" } });
    // Markdown says MD whatever its extension; an extension too long for the page isn't shown
    expect(macArtFor({ kind: "markdown", type: null, ext: "markdown" })).toMatchObject({ band: { label: "MD" } });
    expect(macArtFor({ kind: "other", type: null, ext: "xyz" })).toEqual({ shape: "page", picture: "blank", label: "XYZ" });
    expect(macArtFor({ kind: "other", type: null, ext: "customext" })).toEqual({ shape: "page", picture: "blank", label: undefined });
  });

  test("every format, language and tool lib/fileTypes.ts knows has its Mac icon: its label on a band, or its symbol in its colour", () => {
    const types = Object.values(FILE_TYPES);
    expect(types.map((type) => [type.id, macArtFor({ kind: "other", type, ext: "" })])).toEqual(
      types.map(({ id, mark: m }) => [id, "label" in m ? { shape: "page", band: { label: m.label, bg: m.bg } } : { shape: "page", glyph: m.icon, color: glyphColor(m.color) }]),
    );
    // Each symbol has a colour of its own, or the neutral grey
    expect(types.filter(({ mark: m }) => "icon" in m && !/^#[0-9a-f]{6}$/i.test(glyphColor(m.color))).map((type) => type.id)).toEqual([]);
  });

  test("a symbol's colour is its light theme's (the page stays light), or grey for the theme's own", () => {
    expect(glyphColor("text-[#e05d5d] dark:text-[#ed8585]")).toBe("#e05d5d");
    expect(glyphColor("text-muted-foreground")).toBe("#6b717c");
    expect(inkOn("#e8d44d")).toBe("#1a1a1a");
    expect(inkOn("#b7410e")).toBe("#ffffff");
  });

  test("the sidebar and toolbar have symbols of their own, named for the style", () => {
    const names = Object.keys(MAC_SYMBOLS);
    expect(names).toEqual(expect.arrayContaining(["favorites", "spaces", "recent", "trash", "back", "forward", "viewIcons", "viewList", "viewColumns", "share", "actions"]));
    for (const Symbol of Object.values(MAC_SYMBOLS)) expect(Symbol.displayName ?? "").toMatch(/^Mac/);
  });
});

// ───────────── Loading ─────────────

describe("the Mac style's assets", () => {
  let root: Root | null = null;
  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    document.body.replaceChildren();
  });

  function render(kit: typeof macKit, name: string) {
    const el = document.createElement("div");
    document.body.append(el);
    root = createRoot(el);
    act(() =>
      root!.render(
        <StyleKitContext.Provider value={kit}>
          <FileIcon node={{ kind: "file", mime: "", name }} className="size-4" />
          <FileIcon node={{ kind: "folder", mime: "", name: "Reports" }} className="size-4" />
        </StyleKitContext.Provider>,
      ),
    );
    return el;
  }

  test("the Windows style draws its own icons, without them", () => {
    const el = render(windowsKit, "main.rs");
    expect(el.querySelector('svg[data-type="rust"]')).not.toBeNull();
    expect(el.querySelector("[data-art]")).toBeNull();
  });

  test("load when an icon of the Mac style is drawn: until then, its place is kept; then the Mac icons", async () => {
    const el = render(macKit, "main.rs");
    // Drawing one started loading them, which takes a moment
    expect(macArt()).toBeUndefined();
    expect([...el.children].map((c) => c.tagName)).toEqual(["SPAN", "SPAN"]);
    loadMacArt();
    await expect.poll(() => macArt()).toBeTruthy();
    await act(async () => {});
    expect(el.querySelector('svg[data-art="mac"][data-type="rust"] text')?.textContent).toBe("RS");
    expect(el.querySelector('svg[data-art="mac"][data-kind="folder"]')).not.toBeNull();
    // The views and the toolbar take the style's symbols
    expect(macKit.views()[0].Icon).toBe(MAC_SYMBOLS.viewIcons);
    expect(macSymbols().back).toBe(MAC_SYMBOLS.back);
  });

  test("are imported by nothing but their loader, so they are a chunk of their own", () => {
    const art = join(src, "components", "style", "mac", "art");
    const files = (dir: string): string[] => readdirSync(dir).flatMap((f) => (statSync(join(dir, f)).isDirectory() ? files(join(dir, f)) : [join(dir, f)]));
    const importers = files(src)
      .filter((f) => /\.(ts|tsx)$/.test(f) && !f.startsWith(art))
      .filter((f) => /^import (?!type\b)[^;]*from "[^"]*\/art(\/[^"]*)?";/m.test(readFileSync(f, "utf8")) || /import\("[^"]*\/art"\)/.test(readFileSync(f, "utf8")))
      .map((f) => relative(src, f).replaceAll("\\", "/"));
    expect(importers).toEqual(["components/style/mac/loadArt.ts"]);
  });
});
