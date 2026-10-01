/**
 * Properties → CSS: borders, shading, colors, text formatting.
 */

import { attr, numAttr } from "../core/package";
import { applyHslTint, hexToRgba, rgbaHex, wordColor, type Rgba, type Theme } from "../core/theme";
import { familyOf, hasCjk, lineRatio, resolveFonts, type ResolvedFonts } from "./fonts";
import { DEFAULT_SZ, type RunProps } from "./styles";
import { isHex } from "./xml";

// ───────────── Colors ─────────────

/** Generic Word color: val/fill and themeColor/themeFill (with tint, shade) */
export function wColor(el: Element | null | undefined, theme: Theme | null, valAttr: string, themeAttr: string, tintAttr: string, shadeAttr: string): Rgba | null {
  if (!el) return null;
  let c: Rgba | null = null;
  const th = attr(el, themeAttr);
  if (th && theme) {
    const map: Record<string, string> = { text1: "dk1", dark1: "dk1", background1: "lt1", light1: "lt1", text2: "dk2", dark2: "dk2", background2: "lt2", light2: "lt2", hyperlink: "hlink", followedHyperlink: "folHlink" };
    c = hexToRgba(theme.colors[map[th] ?? th]);
  }
  if (!c) {
    const v = attr(el, valAttr);
    if (!isHex(v)) return null;
    c = hexToRgba(v);
  }
  if (!c) return null;
  const tint = attr(el, tintAttr);
  const shade = attr(el, shadeAttr);
  if (tint && /^[0-9a-f]{2}$/i.test(tint)) c = applyHslTint(c, 1 - parseInt(tint, 16) / 255);
  if (shade && /^[0-9a-f]{2}$/i.test(shade)) c = applyHslTint(c, parseInt(shade, 16) / 255 - 1);
  return c;
}

export const textColor = (el: Element | null | undefined, theme: Theme | null) => {
  const c = wordColor(el, theme);
  return c ? rgbaHex(c) : null;
};

/** Shading (w:shd): solid uses the foreground color, pctNN blends foreground and background by ratio, other patterns use the background only */
export function shadeColor(el: Element | null | undefined, theme: Theme | null): string | null {
  if (!el) return null;
  const v = attr(el, "val") ?? "clear";
  if (v === "nil") return null;
  const fill = wColor(el, theme, "fill", "themeFill", "themeFillTint", "themeFillShade");
  const fg = wColor(el, theme, "color", "themeColor", "themeTint", "themeShade");
  if (v === "solid") return rgbaHex(fg ?? { r: 0, g: 0, b: 0, a: 1 });
  const pct = /^pct(\d+)$/.exec(v);
  if (pct && (fg || fill)) {
    const p = Number(pct[1]) / 100;
    const a = fg ?? { r: 0, g: 0, b: 0, a: 1 };
    const b = fill ?? { r: 255, g: 255, b: 255, a: 1 };
    return rgbaHex({ r: a.r * p + b.r * (1 - p), g: a.g * p + b.g * (1 - p), b: a.b * p + b.b * (1 - p), a: 1 });
  }
  return fill ? rgbaHex(fill) : null;
}

/** Dark background (auto text color must become white) */
export function isDark(hex: string | null | undefined) {
  const c = hexToRgba(hex);
  if (!c) return false;
  return 0.299 * c.r + 0.587 * c.g + 0.114 * c.b < 110;
}

// ───────────── Borders ─────────────

// oxfmt-ignore
const BORDER_STYLE: Record<string, string> = {
  single: "solid", thick: "solid", hairline: "solid", wave: "solid", doubleWave: "double",
  double: "double", triple: "double",
  thinThickSmallGap: "double", thickThinSmallGap: "double", thinThickThinSmallGap: "double",
  thinThickMediumGap: "double", thickThinMediumGap: "double", thinThickThinMediumGap: "double",
  thinThickLargeGap: "double", thickThinLargeGap: "double", thinThickThinLargeGap: "double",
  dotted: "dotted", dashed: "dashed", dashSmallGap: "dashed", dotDash: "dashed", dotDotDash: "dashed", dashDotStroked: "dashed",
  threeDEmboss: "ridge", threeDEngrave: "groove", outset: "outset", inset: "inset",
};

/** Border element → CSS border value; nil/none returns "none", a missing element returns null */
export function borderCss(el: Element | null | undefined, theme: Theme | null): string | null {
  if (!el) return null;
  const v = attr(el, "val") ?? "single";
  if (v === "nil" || v === "none") return "none";
  const style = BORDER_STYLE[v] ?? "solid";
  let w = (numAttr(el, "sz") ?? 4) / 8;
  if (v === "hairline") w = 0.25;
  if (style === "double") w = Math.max(w * 3, 2.25);
  w = Math.max(0.5, Math.min(w, 12));
  const c = attr(el, "color");
  const col = wColor(el, theme, "color", "themeColor", "themeTint", "themeShade");
  const color = col ? rgbaHex(col) : c === "auto" || !c ? "#000" : "#000";
  return `${Math.round(w * 100) / 100}pt ${style} ${color}`;
}

/** Distance between border and text (pt) */
export const borderSpace = (el: Element | null | undefined) => Math.min(31, numAttr(el, "space") ?? 0);

/** Border width (pt), used to compute positions */
export function borderWidthPt(css: string | null) {
  if (!css || css === "none") return 0;
  return parseFloat(css) || 0;
}

/** Border signature for comparison (adjacent paragraphs with identical borders are merged) */
export function borderSig(el: Element | null | undefined) {
  if (!el) return "";
  return ["val", "sz", "space", "color", "themeColor"].map((a) => attr(el, a) ?? "").join("/");
}

// ───────────── Text ─────────────

// oxfmt-ignore
const HIGHLIGHT: Record<string, string> = {
  yellow: "#FFFF00", green: "#00FF00", cyan: "#00FFFF", magenta: "#FF00FF", blue: "#0000FF", red: "#FF0000",
  darkBlue: "#000080", darkCyan: "#008080", darkGreen: "#008000", darkMagenta: "#800080", darkRed: "#800000",
  darkYellow: "#808000", darkGray: "#808080", lightGray: "#C0C0C0", black: "#000000", white: "#FFFFFF",
};

// oxfmt-ignore
const UNDERLINE: Record<string, string> = {
  single: "solid", words: "solid", double: "double", thick: "solid", dotted: "dotted", dottedHeavy: "dotted",
  dash: "dashed", dashedHeavy: "dashed", dashLong: "dashed", dashLongHeavy: "dashed", dotDash: "dashed",
  dashDotHeavy: "dashed", dotDotDash: "dotted", dashDotDotHeavy: "dotted", wave: "wavy", wavyHeavy: "wavy", wavyDouble: "wavy",
};

const EMPHASIS: Record<string, string> = { dot: "filled dot", comma: "filled sesame", circle: "open circle", underDot: "filled dot" };

export interface RunStyle {
  css: Record<string, string>;
  hidden: boolean;
  /** Actual font size (pt, before sub/superscript shrinking) */
  size: number;
  fonts: ResolvedFonts;
  family: string;
  /** Background color (determines the auto text color) */
  bg: string | null;
  /** Horizontal scale (w:w, 1 = 100%) */
  scale?: number;
}

export function runStyle(p: RunProps, theme: Theme | null, script: string): RunStyle {
  const fonts = resolveFonts(p.fonts, theme, script);
  const rtl = !!p.rtl;
  const family = familyOf(fonts, rtl);
  const size = ((rtl || p.cs ? p.szCs ?? p.sz : p.sz) ?? DEFAULT_SZ) / 2;
  const css: Record<string, string> = {};
  css["font-family"] = family;
  let fs = size;
  const bold = rtl || p.cs ? p.bCs ?? p.b : p.b;
  const italic = rtl || p.cs ? p.iCs ?? p.i : p.i;
  if (bold) css["font-weight"] = "bold";
  if (italic) css["font-style"] = "italic";
  if (p.caps) css["text-transform"] = "uppercase";
  else if (p.smallCaps) css["font-variant"] = "small-caps";

  // Underline and strikethrough
  const lines: string[] = [];
  const u = attr(p.u, "val");
  let uStyle = "";
  if (p.u && u !== "none") {
    lines.push("underline");
    uStyle = UNDERLINE[u ?? "single"] ?? "solid";
    const uc = attr(p.u, "color");
    const ucol = wColor(p.u, theme, "color", "themeColor", "themeTint", "themeShade");
    if (ucol && uc !== "auto") css["text-decoration-color"] = rgbaHex(ucol);
    if (/thick|Heavy/.test(u ?? "")) css["text-decoration-thickness"] = "2px";
  }
  if (p.strike || p.dstrike) lines.push("line-through");
  if (lines.length) {
    css["text-decoration-line"] = lines.join(" ");
    const st = p.dstrike ? "double" : uStyle;
    if (st && st !== "solid") css["text-decoration-style"] = st;
    css["text-underline-offset"] = "0.15em";
  }

  // Color
  let bg: string | null = null;
  if (p.highlight && p.highlight !== "none" && HIGHLIGHT[p.highlight]) bg = HIGHLIGHT[p.highlight];
  else if (p.shd) bg = shadeColor(p.shd, theme);
  if (bg) css["background-color"] = bg;
  let color = textColor(p.color, theme);
  if (!color && p.textFill) {
    const hex = p.textFill.getElementsByTagNameNS("*", "srgbClr")[0];
    const v = attr(hex, "val");
    if (isHex(v)) color = `#${v}`;
  }
  if (color) css.color = color;
  else if (bg && isDark(bg)) css.color = "#fff";

  // Sub/superscript, position
  let top = 0;
  if (p.vertAlign === "superscript" || p.vertAlign === "subscript") {
    fs = size * 0.65;
    top = p.vertAlign === "superscript" ? -size * 0.33 : size * 0.14;
  }
  if (p.position) top -= p.position / 2;
  if (top) {
    css.position = "relative";
    css.top = `${Math.round(top * 100) / 100}pt`;
  }
  css["font-size"] = `${Math.round(fs * 100) / 100}pt`;
  if (p.spacing) css["letter-spacing"] = `${Math.round((p.spacing / 20) * 100) / 100}pt`;
  if (p.em && p.em !== "none" && EMPHASIS[p.em]) {
    css["text-emphasis"] = EMPHASIS[p.em];
    css["text-emphasis-position"] = p.em === "underDot" ? "under right" : "over right";
  }
  if (p.bdr) {
    const b = borderCss(p.bdr, theme);
    if (b && b !== "none") css.border = b;
  }
  if (p.shadow) css["text-shadow"] = "0.06em 0.06em 0 rgba(0,0,0,.35)";
  if (p.emboss) css["text-shadow"] = "-0.04em -0.04em 0 rgba(255,255,255,.8),0.04em 0.04em 0 rgba(0,0,0,.4)";
  if (p.imprint) css["text-shadow"] = "0.04em 0.04em 0 rgba(255,255,255,.8),-0.04em -0.04em 0 rgba(0,0,0,.4)";
  if (p.outline) {
    css["-webkit-text-stroke"] = "0.03em currentColor";
    css["-webkit-text-fill-color"] = "transparent";
  }
  if (rtl) {
    css.direction = "rtl";
    css["unicode-bidi"] = "embed";
  }
  let scale: number | undefined;
  if (p.w && p.w !== 100) {
    scale = Math.max(0.1, Math.min(6, p.w / 100));
    css.display = "inline-block";
    css.transform = `scaleX(${scale})`;
    css["transform-origin"] = "0 0";
    css["text-indent"] = "0";
  }
  return { css, hidden: !!p.vanish && !p.specVanish, size, fonts, family, bg, scale };
}

/** Line-height ratio for text: with CJK characters, take the larger of the Latin and East Asian fonts */
export function ratioOf(fonts: ResolvedFonts, text: string) {
  const latin = lineRatio(fonts.latin ?? fonts.hAnsi);
  if (!hasCjk(text) && !fonts.eaHint) return latin;
  return Math.max(latin, lineRatio(fonts.ea ?? fonts.latin));
}
