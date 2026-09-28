/**
 * Theme (theme1.xml): color scheme, fonts, format scheme (fills/lines), and DrawingML color computation.
 */

import { attr, kid, kids, numAttr } from "@/lib/office/ooxml";

// ───────────── Colors ─────────────

/** r/g/b are 0–255, a is 0–1 */
export interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

const clamp = (v: number, lo = 0, hi = 1) => Math.min(hi, Math.max(lo, v));

export function hexToRgba(hex: string | null | undefined, a = 1): Rgba | null {
  if (!hex) return null;
  const m = /^#?([0-9a-f]{6})([0-9a-f]{2})?$/i.exec(hex.trim());
  if (!m) return null;
  const n = parseInt(m[1], 16);
  // With 8 digits the first two are alpha (SpreadsheetML ARGB is handled separately; cases other than RRGGBBAA don't reach here)
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255, a };
}

export function rgbaHex(c: Rgba) {
  return `#${[c.r, c.g, c.b].map((v) => Math.round(clamp(v, 0, 255)).toString(16).padStart(2, "0")).join("")}`;
}

export function rgbaCss(c: Rgba | null | undefined): string | undefined {
  if (!c) return undefined;
  if (c.a >= 0.999) return rgbaHex(c);
  return `rgba(${Math.round(c.r)},${Math.round(c.g)},${Math.round(c.b)},${Math.round(c.a * 1000) / 1000})`;
}

// sRGB ↔ linear (tint/shade are computed in linear space, which is closer to Office's results)
const toLinear = (v: number) => {
  const c = v / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};
const fromLinear = (c: number) => 255 * (c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055);

interface Hsl {
  h: number; // 0–360
  s: number; // 0–1
  l: number; // 0–1
}

export function rgbToHsl({ r, g, b }: Rgba): Hsl {
  const R = r / 255;
  const G = g / 255;
  const B = b / 255;
  const max = Math.max(R, G, B);
  const min = Math.min(R, G, B);
  const l = (max + min) / 2;
  if (max === min) return { h: 0, s: 0, l };
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h = max === R ? (G - B) / d + (G < B ? 6 : 0) : max === G ? (B - R) / d + 2 : (R - G) / d + 4;
  h *= 60;
  return { h, s, l };
}

export function hslToRgb({ h, s, l }: Hsl, a = 1): Rgba {
  if (s === 0) return { r: l * 255, g: l * 255, b: l * 255, a };
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  const hue = (t: number) => {
    let x = t;
    if (x < 0) x += 1;
    if (x > 1) x -= 1;
    if (x < 1 / 6) return p + (q - p) * 6 * x;
    if (x < 1 / 2) return q;
    if (x < 2 / 3) return p + (q - p) * (2 / 3 - x) * 6;
    return p;
  };
  const H = (((h % 360) + 360) % 360) / 360;
  return { r: hue(H + 1 / 3) * 255, g: hue(H) * 255, b: hue(H - 1 / 3) * 255, a };
}

/** SpreadsheetML/Word tint (-1–1): adjusts HSL lightness; negative darkens, positive lightens */
export function applyHslTint(c: Rgba, tint: number): Rgba {
  if (!tint) return c;
  const hsl = rgbToHsl(c);
  hsl.l = tint < 0 ? hsl.l * (1 + tint) : hsl.l * (1 - tint) + tint;
  return hslToRgb({ ...hsl, l: clamp(hsl.l) }, c.a);
}

/** DrawingML color modifiers (applied in document order) */
function applyModifiers(base: Rgba, el: Element): Rgba {
  let c = { ...base };
  for (const m of kids(el)) {
    const v = (numAttr(m, "val") ?? 0) / 100000;
    switch (m.localName) {
      case "tint": {
        // Toward white: 1 - (1 - c) × tint in linear space
        const f = (x: number) => fromLinear(1 - (1 - toLinear(x)) * v);
        c = { r: f(c.r), g: f(c.g), b: f(c.b), a: c.a };
        break;
      }
      case "shade": {
        const f = (x: number) => fromLinear(toLinear(x) * v);
        c = { r: f(c.r), g: f(c.g), b: f(c.b), a: c.a };
        break;
      }
      case "alpha":
        c.a = clamp(v);
        break;
      case "alphaMod":
        c.a = clamp(c.a * v);
        break;
      case "alphaOff":
        c.a = clamp(c.a + v);
        break;
      case "lumMod":
      case "lumOff":
      case "lum":
      case "satMod":
      case "satOff":
      case "sat":
      case "hueMod":
      case "hueOff":
      case "hue": {
        const hsl = rgbToHsl(c);
        const n = m.localName;
        if (n === "lumMod") hsl.l *= v;
        else if (n === "lumOff") hsl.l += v;
        else if (n === "lum") hsl.l = v;
        else if (n === "satMod") hsl.s *= v;
        else if (n === "satOff") hsl.s += v;
        else if (n === "sat") hsl.s = v;
        // Hue is in units of 1/60000 degree
        else if (n === "hueMod") hsl.h *= v;
        else if (n === "hueOff") hsl.h += (numAttr(m, "val") ?? 0) / 60000;
        else hsl.h = (numAttr(m, "val") ?? 0) / 60000;
        c = hslToRgb({ h: hsl.h, s: clamp(hsl.s), l: clamp(hsl.l) }, c.a);
        break;
      }
      case "comp": {
        const hsl = rgbToHsl(c);
        c = hslToRgb({ ...hsl, h: hsl.h + 180 }, c.a);
        break;
      }
      case "inv":
        c = { r: 255 - c.r, g: 255 - c.g, b: 255 - c.b, a: c.a };
        break;
      case "gray": {
        const y = 0.3 * c.r + 0.59 * c.g + 0.11 * c.b;
        c = { r: y, g: y, b: y, a: c.a };
        break;
      }
      case "red":
      case "green":
      case "blue":
      case "redMod":
      case "greenMod":
      case "blueMod":
      case "redOff":
      case "greenOff":
      case "blueOff": {
        const ch = m.localName.startsWith("red") ? "r" : m.localName.startsWith("green") ? "g" : "b";
        // Adjust a single channel in linear space
        const lin = toLinear(c[ch]);
        const next = m.localName.endsWith("Mod") ? lin * v : m.localName.endsWith("Off") ? lin + v : v;
        c[ch] = fromLinear(clamp(next));
        break;
      }
      case "gamma":
        c = { r: fromLinear(c.r / 255), g: fromLinear(c.g / 255), b: fromLinear(c.b / 255), a: c.a };
        break;
      case "invGamma":
        c = { r: toLinear(c.r) * 255, g: toLinear(c.g) * 255, b: toLinear(c.b) * 255, a: c.a };
        break;
    }
  }
  return c;
}

/** Preset color names (prstClr); only common ones are listed, others are treated as black */
const PRESET: Record<string, string> = {
  black: "000000", white: "FFFFFF", red: "FF0000", green: "008000", blue: "0000FF", yellow: "FFFF00",
  cyan: "00FFFF", magenta: "FF00FF", gray: "808080", grey: "808080", darkGray: "A9A9A9", lightGray: "D3D3D3",
  orange: "FFA500", purple: "800080", brown: "A52A2A", pink: "FFC0CB", navy: "000080", teal: "008080",
  olive: "808000", maroon: "800000", silver: "C0C0C0", lime: "00FF00", gold: "FFD700", darkBlue: "00008B",
  darkRed: "8B0000", darkGreen: "006400", lightBlue: "ADD8E6", skyBlue: "87CEEB", violet: "EE82EE",
};

/** System colors (sysClr): prefer lastClr, otherwise a common default */
const SYSTEM: Record<string, string> = { windowText: "000000", window: "FFFFFF", btnFace: "F0F0F0", highlight: "0078D7", highlightText: "FFFFFF", grayText: "6D6D6D" };

export interface ColorContext {
  theme?: Theme | null;
  /** Slide master color mapping (bg1 → lt1, tx1 → dk1…) */
  clrMap?: Record<string, string> | null;
  /** Color to substitute for phClr (placeholder color) in the format scheme */
  phClr?: Rgba | null;
}

const COLOR_TAGS = new Set(["srgbClr", "schemeClr", "sysClr", "prstClr", "hslClr", "scrgbClr"]);

/** Default color mapping (Word, Excel, and slides without clrMap) */
const DEFAULT_MAP: Record<string, string> = { bg1: "lt1", tx1: "dk1", bg2: "lt2", tx2: "dk2" };

export function schemeColor(name: string, ctx: ColorContext): Rgba | null {
  if (name === "phClr") return ctx.phClr ?? null;
  const mapped = (ctx.clrMap ?? DEFAULT_MAP)[name] ?? DEFAULT_MAP[name] ?? name;
  return hexToRgba(ctx.theme?.colors[mapped] ?? ctx.theme?.colors[name]);
}

/** Color element (srgbClr, schemeClr…) → color */
export function readColor(el: Element | null | undefined, ctx: ColorContext): Rgba | null {
  if (!el) return null;
  let base: Rgba | null = null;
  switch (el.localName) {
    case "srgbClr":
      base = hexToRgba(attr(el, "val"));
      break;
    case "schemeClr":
      base = schemeColor(attr(el, "val") ?? "", ctx);
      break;
    case "sysClr":
      base = hexToRgba(attr(el, "lastClr") ?? SYSTEM[attr(el, "val") ?? ""] ?? "000000");
      break;
    case "prstClr":
      base = hexToRgba(PRESET[attr(el, "val") ?? ""] ?? "000000");
      break;
    case "hslClr":
      base = hslToRgb({ h: (numAttr(el, "hue") ?? 0) / 60000, s: (numAttr(el, "sat") ?? 0) / 100000, l: (numAttr(el, "lum") ?? 0) / 100000 });
      break;
    case "scrgbClr":
      base = {
        r: fromLinear((numAttr(el, "r") ?? 0) / 100000),
        g: fromLinear((numAttr(el, "g") ?? 0) / 100000),
        b: fromLinear((numAttr(el, "b") ?? 0) / 100000),
        a: 1,
      };
      break;
    default:
      return null;
  }
  return base ? applyModifiers(base, el) : null;
}

/** Find the first color child under an element and compute it (e.g. <a:solidFill><a:schemeClr …/></a:solidFill>) */
export function colorIn(parent: Element | null | undefined, ctx: ColorContext): Rgba | null {
  for (const c of kids(parent)) if (COLOR_TAGS.has(c.localName)) return readColor(c, ctx);
  return null;
}

// ───────────── SpreadsheetML/WordprocessingML colors ─────────────

/** Excel indexed colors (indexed="n"); 64 and 65 are the system foreground and background colors */
export const INDEXED_COLORS = [
  "000000", "FFFFFF", "FF0000", "00FF00", "0000FF", "FFFF00", "FF00FF", "00FFFF",
  "000000", "FFFFFF", "FF0000", "00FF00", "0000FF", "FFFF00", "FF00FF", "00FFFF",
  "800000", "008000", "000080", "808000", "800080", "008080", "C0C0C0", "808080",
  "9999FF", "993366", "FFFFCC", "CCFFFF", "660066", "FF8080", "0066CC", "CCCCFF",
  "000080", "FF00FF", "FFFF00", "00FFFF", "800080", "800000", "008080", "0000FF",
  "00CCFF", "CCFFFF", "CCFFCC", "FFFF99", "99CCFF", "FF99CC", "CC99FF", "FFCC99",
  "3366FF", "33CCCC", "99CC00", "FFCC00", "FF9900", "FF6600", "666699", "969696",
  "003366", "339966", "003300", "333300", "993300", "993366", "333399", "333333",
  "000000", "FFFFFF",
];

/** Excel theme="n": 0/1 and 2/3 are swapped (0 = light 1, 1 = dark 1) */
const SHEET_THEME_ORDER = ["lt1", "dk1", "lt2", "dk2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink", "folHlink"];

/**
 * SpreadsheetML <color rgb|theme|indexed tint auto/>.
 * rgb is ARGB (the first two digits are alpha, which Excel actually ignores)
 */
export function sheetColor(el: Element | null | undefined, theme: Theme | null | undefined, palette: string[] = INDEXED_COLORS): Rgba | null {
  if (!el || attr(el, "auto") === "1") return null;
  let c: Rgba | null = null;
  const rgb = attr(el, "rgb");
  const themeIdx = numAttr(el, "theme");
  const indexed = numAttr(el, "indexed");
  if (rgb && /^[0-9a-f]{8}$/i.test(rgb)) c = hexToRgba(rgb.slice(2));
  else if (rgb && /^[0-9a-f]{6}$/i.test(rgb)) c = hexToRgba(rgb);
  else if (themeIdx !== null) c = hexToRgba(theme?.colors[SHEET_THEME_ORDER[themeIdx] ?? ""]);
  else if (indexed !== null) c = hexToRgba(palette[indexed] ?? INDEXED_COLORS[indexed]);
  if (!c) return null;
  const tint = numAttr(el, "tint");
  return tint ? applyHslTint(c, tint) : c;
}

/** Maps Word themeColor names (text1, background1, accent1…) to theme color scheme names */
const WORD_THEME: Record<string, string> = {
  text1: "dk1", dark1: "dk1", background1: "lt1", light1: "lt1", text2: "dk2", dark2: "dk2", background2: "lt2", light2: "lt2",
  hyperlink: "hlink", followedHyperlink: "folHlink",
};

/**
 * Word color: w:color val="FF0000" themeColor="accent1" themeTint="99" themeShade="BF".
 * themeTint/themeShade are 00–FF: tint moves toward white, shade toward black (computed on HSL lightness)
 */
export function wordColor(el: Element | null | undefined, theme: Theme | null | undefined, valAttr = "val"): Rgba | null {
  if (!el) return null;
  const themeName = attr(el, "themeColor");
  let c: Rgba | null = null;
  if (themeName && theme) c = hexToRgba(theme.colors[WORD_THEME[themeName] ?? themeName]);
  if (!c) {
    const v = attr(el, valAttr);
    if (!v || v === "auto") return null;
    c = hexToRgba(v);
  }
  if (!c) return null;
  const tint = attr(el, "themeTint");
  const shade = attr(el, "themeShade");
  if (tint) c = applyHslTint(c, 1 - parseInt(tint, 16) / 255);
  if (shade) c = applyHslTint(c, parseInt(shade, 16) / 255 - 1);
  return c;
}

// ───────────── Theme ─────────────

export interface FontScheme {
  latin: string;
  ea: string;
  cs: string;
  /** Fonts specified per script (e.g. Hant → PMingLiU) */
  scripts: Record<string, string>;
}

export interface Theme {
  colors: Record<string, string>;
  major: FontScheme;
  minor: FontScheme;
  fillStyles: Element[];
  lineStyles: Element[];
  effectStyles: Element[];
  bgFillStyles: Element[];
}

function readFontScheme(el: Element | null): FontScheme {
  const scripts: Record<string, string> = {};
  for (const f of kids(el, "font")) {
    const sc = attr(f, "script");
    const face = attr(f, "typeface");
    if (sc && face) scripts[sc] = face;
  }
  return {
    latin: attr(kid(el, "latin"), "typeface") ?? "",
    ea: attr(kid(el, "ea"), "typeface") ?? "",
    cs: attr(kid(el, "cs"), "typeface") ?? "",
    scripts,
  };
}

export function parseTheme(doc: Document | null): Theme | null {
  const root = doc?.documentElement;
  if (!root) return null;
  const elements = kid(root, "themeElements");
  const colors: Record<string, string> = {};
  for (const c of kids(kid(elements, "clrScheme"))) {
    const v = colorIn(c, {});
    if (v) colors[c.localName] = rgbaHex(v).slice(1);
  }
  const fonts = kid(elements, "fontScheme");
  const fmt = kid(elements, "fmtScheme");
  return {
    colors,
    major: readFontScheme(kid(fonts, "majorFont")),
    minor: readFontScheme(kid(fonts, "minorFont")),
    fillStyles: kids(kid(fmt, "fillStyleLst")),
    lineStyles: kids(kid(fmt, "lnStyleLst")),
    effectStyles: kids(kid(fmt, "effectStyleLst")),
    bgFillStyles: kids(kid(fmt, "bgFillStyleLst")),
  };
}

// ───────────── Fonts ─────────────

/** CJK font fallbacks: find fonts likely installed on the system by style (Ming/serif, Kai/script, Hei/sans) */
const SERIF_CJK = ["PMingLiU", "新細明體", "MingLiU", "細明體", "SimSun", "宋体", "NSimSun", "MS Mincho", "Yu Mincho", "Songti TC", "Noto Serif TC", "Noto Serif CJK TC"]; // i18n-ignore: CJK font names
const KAI_CJK = ["DFKai-SB", "標楷體", "BiauKai", "KaiTi", "楷体", "Kaiti TC"]; // i18n-ignore: CJK font names
const SANS_CJK = ["Microsoft JhengHei", "微軟正黑體", "Microsoft YaHei", "微软雅黑", "DengXian", "等线", "Meiryo", "Yu Gothic", "PingFang TC", "Noto Sans TC", "Noto Sans CJK TC"]; // i18n-ignore: CJK font names

const SERIF_LATIN = /times|cambria|georgia|garamond|book antiqua|palatino|century|serif|mincho|ming|明|宋|song/i; // i18n-ignore: regex matching CJK font names
const KAI = /kai|楷/i; // i18n-ignore: regex matching CJK font names
const MONO = /courier|consolas|mono|lucida console/i;

/** Metric-compatible substitutes for common Office fonts on other systems */
const METRIC: Record<string, string> = {
  calibri: "Carlito",
  cambria: "Caladea",
  arial: "Liberation Sans",
  "times new roman": "Liberation Serif",
  "courier new": "Liberation Mono",
};

/** Generic families (serif, sans-serif…) must not be quoted, or they'd be treated as a font literally named "sans-serif"; all other font names are quoted */
// oxlint-disable-next-line no-control-regex -- control characters are removed on purpose
const quote = (f: string) => (/^(serif|sans-serif|monospace|cursive|fantasy|system-ui)$/.test(f) ? f : `"${f.replace(/["\\\x00-\x1f;{}]/g, "")}"`);

/**
 * CSS font-family: original font → metric-compatible font → CJK font of the same style → generic family.
 * This lets machines without the font installed still render a close appearance.
 */
export function fontStack(...faces: (string | null | undefined)[]): string {
  const list: string[] = [];
  const add = (f: string) => {
    if (f && !list.includes(f)) list.push(f);
  };
  for (const f of faces) {
    if (!f) continue;
    add(f);
    const alt = METRIC[f.toLowerCase()];
    if (alt) add(alt);
  }
  const primary = faces.find(Boolean) ?? "";
  if (MONO.test(primary)) return [...list, "monospace"].map(quote).join(",");
  const cjk = KAI.test(faces.join(" ")) ? KAI_CJK : faces.some((f) => f && SERIF_LATIN.test(f)) ? SERIF_CJK : SANS_CJK;
  for (const f of cjk) add(f);
  const generic = faces.some((f) => f && SERIF_LATIN.test(f)) || KAI.test(faces.join(" ")) ? "serif" : "sans-serif";
  return [...list, generic].map(quote).join(",");
}

/**
 * Replace theme font tokens (+mj-lt, +mn-ea, majorEastAsia…) with actual font names.
 * When the East Asian font is unspecified, use the font from scripts for the language (Traditional Chinese: Hant)
 */
export function themeFont(face: string | null | undefined, theme: Theme | null | undefined, script = "Hant"): string | null {
  if (!face) return null;
  if (!theme) return face.startsWith("+") ? null : face;
  const m = /^\+(mj|mn)-(lt|ea|cs)$/.exec(face);
  const word = /^(major|minor)(Ascii|HAnsi|EastAsia|Bidi)$/.exec(face);
  if (!m && !word) return face;
  const scheme = (m ? m[1] === "mj" : word![1] === "major") ? theme.major : theme.minor;
  const slot = m ? m[2] : word![2] === "EastAsia" ? "ea" : word![2] === "Bidi" ? "cs" : "lt";
  if (slot === "ea") return scheme.ea || scheme.scripts[script] || scheme.scripts.Hans || scheme.latin || null;
  if (slot === "cs") return scheme.cs || scheme.latin || null;
  return scheme.latin || null;
}
