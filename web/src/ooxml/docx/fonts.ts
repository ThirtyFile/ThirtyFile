/**
 * Fonts: resolves the rFonts slots (ascii/hAnsi/eastAsia/cs) into a CSS font-family,
 * line-height ratios (Word's "single" line height depends on the font's own ascent/descent), and symbol font (Wingdings, Symbol) mappings.
 */

import { fontStack, themeFont, type Theme } from "../core/theme";

/** Merged rFonts result (theme fonts take precedence over explicit font names) */
export interface FontSlots {
  ascii?: string;
  hAnsi?: string;
  eastAsia?: string;
  cs?: string;
  asciiTheme?: string;
  hAnsiTheme?: string;
  eastAsiaTheme?: string;
  cstheme?: string;
  hint?: string;
}

/** Strip characters that could break CSS before putting a font name into a style attribute */
export function cleanFace(face: string | null | undefined): string | undefined {
  if (!face) return undefined;
  // oxlint-disable-next-line no-control-regex -- control characters are removed on purpose
  const v = face.replace(/[;{}<>\\"'`\u0000-\u001f]/g, "").trim().slice(0, 64);
  return v || undefined;
}

export interface ResolvedFonts {
  latin?: string;
  hAnsi?: string;
  ea?: string;
  cs?: string;
  /** hint="eastAsia": ambiguous characters (quotes, punctuation) use the East Asian font */
  eaHint: boolean;
}

export function resolveFonts(f: FontSlots, theme: Theme | null, script: string): ResolvedFonts {
  const slot = (lit?: string, th?: string) => cleanFace((th && themeFont(th, theme, script)) || lit);
  // With no font specified at all, use Word's defaults (Latin: Times New Roman; East Asian: the theme font for the language)
  const latin = slot(f.ascii, f.asciiTheme) ?? slot(f.hAnsi, f.hAnsiTheme) ?? "Times New Roman";
  return {
    latin,
    hAnsi: slot(f.hAnsi, f.hAnsiTheme),
    ea: slot(f.eastAsia, f.eastAsiaTheme) ?? (theme ? cleanFace(themeFont("minorEastAsia", theme, script)) : undefined) ?? DEFAULT_EA[script],
    cs: slot(f.cs, f.cstheme),
    eaHint: f.hint === "eastAsia",
  };
}

const DEFAULT_EA: Record<string, string> = { Hant: "PMingLiU", Hans: "SimSun", Jpan: "MS Mincho", Hang: "Batang" };

const stackCache = new Map<string, string>();

/** CSS font-family: Latin font first (CJK characters fall through to the East Asian font after it) */
export function familyOf(r: ResolvedFonts, rtl = false): string {
  const faces = rtl ? [r.cs, r.latin, r.ea] : r.eaHint ? [r.ea, r.latin, r.hAnsi] : [r.latin, r.hAnsi, r.ea];
  const key = faces.join("|");
  let v = stackCache.get(key);
  if (!v) {
    // Generic family names must not be quoted (quoted, they are treated as ordinary font names)
    v = fontStack(...faces).replace(/"(serif|sans-serif|monospace)"/g, "$1");
    stackCache.set(key, v);
  }
  return v;
}

/**
 * A font's "single" line height (as a multiple of the font size): winAscent + winDescent of the Windows font.
 * Word's single line spacing is exactly this value; the browser's line-height: normal varies across systems, so we use a fixed table.
 */
const LINE_RATIO: [RegExp, number][] = [
  [/^calibri( light)?$/i, 1.22],
  [/^carlito$/i, 1.22],
  [/^cambria/i, 1.17],
  [/^(arial|helvetica|liberation sans)/i, 1.15],
  [/^(times new roman|liberation serif)/i, 1.15],
  [/^courier/i, 1.13],
  [/^georgia/i, 1.14],
  [/^verdana/i, 1.22],
  [/^tahoma/i, 1.21],
  [/^segoe ui/i, 1.33],
  [/^aptos/i, 1.2],
  [/^century gothic/i, 1.23],
  [/^garamond/i, 1.13],
  [/^(consolas)/i, 1.17],
  [/^(microsoft jhenghei|微軟正黑體|microsoft yahei|微软雅黑)/i, 1.32], // i18n-ignore: CJK font names
  [/^(dengxian|等线)/i, 1.3], // i18n-ignore: CJK font names
  [/^(pmingliu|mingliu|新細明體|細明體|mingliu_hkscs)/i, 1.3], // i18n-ignore: CJK font names
  [/^(dfkai-sb|標楷體|kaiti|楷体|biaukai)/i, 1.3], // i18n-ignore: CJK font names
  [/^(simsun|nsimsun|宋体|新宋体)/i, 1.3], // i18n-ignore: CJK font names
  [/^(simhei|黑体)/i, 1.3], // i18n-ignore: CJK font names
  [/^(ms mincho|ms gothic|ms pmincho|ms pgothic|ｍｓ)/i, 1.3], // i18n-ignore: CJK font names
  [/^(yu gothic|yu mincho|游)/i, 1.33], // i18n-ignore: CJK font names
  [/^meiryo/i, 1.5],
  [/^(malgun gothic|batang|gulim|dotum)/i, 1.33],
  [/^(noto sans|noto serif|source han)/i, 1.45],
];

const ratioCache = new Map<string, number>();

export function lineRatio(face: string | undefined): number {
  if (!face) return 1.2;
  let v = ratioCache.get(face);
  if (v === undefined) {
    v = LINE_RATIO.find(([re]) => re.test(face))?.[1] ?? 1.2;
    ratioCache.set(face, v);
  }
  return v;
}

/** Whether the text contains CJK characters */
export const hasCjk = (text: string) => /[\u2e80-\u9fff\uac00-\ud7af\uf900-\ufaff\uff00-\uffef]/.test(text);

// ───────────── Symbol fonts ─────────────

/** Symbol font (F0xx or 00xx) → Unicode; mostly Greek letters and math symbols */
const SYMBOL: Record<number, string> = {
  0x22: "∀", 0x24: "∃", 0x27: "∋", 0x2a: "∗", 0x2d: "−", 0x40: "≅",
  0x41: "Α", 0x42: "Β", 0x43: "Χ", 0x44: "Δ", 0x45: "Ε", 0x46: "Φ", 0x47: "Γ", 0x48: "Η", 0x49: "Ι", 0x4a: "ϑ",
  0x4b: "Κ", 0x4c: "Λ", 0x4d: "Μ", 0x4e: "Ν", 0x4f: "Ο", 0x50: "Π", 0x51: "Θ", 0x52: "Ρ", 0x53: "Σ", 0x54: "Τ",
  0x55: "Υ", 0x56: "ς", 0x57: "Ω", 0x58: "Ξ", 0x59: "Ψ", 0x5a: "Ζ", 0x5c: "∴", 0x5e: "⊥",
  0x61: "α", 0x62: "β", 0x63: "χ", 0x64: "δ", 0x65: "ε", 0x66: "φ", 0x67: "γ", 0x68: "η", 0x69: "ι", 0x6a: "ϕ",
  0x6b: "κ", 0x6c: "λ", 0x6d: "μ", 0x6e: "ν", 0x6f: "ο", 0x70: "π", 0x71: "θ", 0x72: "ρ", 0x73: "σ", 0x74: "τ",
  0x75: "υ", 0x76: "ϖ", 0x77: "ω", 0x78: "ξ", 0x79: "ψ", 0x7a: "ζ", 0x7e: "∼",
  0xa1: "ϒ", 0xa2: "′", 0xa3: "≤", 0xa4: "⁄", 0xa5: "∞", 0xa6: "ƒ", 0xa7: "♣", 0xa8: "♦", 0xa9: "♥", 0xaa: "♠",
  0xab: "↔", 0xac: "←", 0xad: "↑", 0xae: "→", 0xaf: "↓", 0xb0: "°", 0xb1: "±", 0xb2: "″", 0xb3: "≥", 0xb4: "×",
  0xb5: "∝", 0xb6: "∂", 0xb7: "•", 0xb8: "÷", 0xb9: "≠", 0xba: "≡", 0xbb: "≈", 0xbc: "…", 0xbf: "↵",
  0xc0: "ℵ", 0xc4: "⊗", 0xc5: "⊕", 0xc6: "∅", 0xc7: "∩", 0xc8: "∪", 0xc9: "⊃", 0xca: "⊇", 0xcb: "⊄", 0xcc: "⊂",
  0xcd: "⊆", 0xce: "∈", 0xcf: "∉", 0xd0: "∠", 0xd1: "∇", 0xd2: "®", 0xd3: "©", 0xd4: "™", 0xd5: "∏", 0xd6: "√",
  0xd7: "⋅", 0xd8: "¬", 0xd9: "∧", 0xda: "∨", 0xdb: "⇔", 0xdc: "⇐", 0xdd: "⇑", 0xde: "⇒", 0xdf: "⇓",
  0xe0: "◊", 0xe1: "〈", 0xe5: "∑", 0xf1: "〉", 0xf2: "∫", // i18n-ignore: Symbol font glyph mapping
};

/** Wingdings → Unicode (common bullets, check boxes, arrows) */
const WINGDINGS: Record<number, string> = {
  0x21: "✏", 0x22: "✂", 0x28: "☎", 0x2a: "✉", 0x36: "⌛", 0x3e: "✇", 0x3f: "✍",
  0x41: "✌", 0x42: "👌", 0x43: "👍", 0x44: "👎", 0x4a: "☺", 0x4b: "😐", 0x4c: "☹", 0x4e: "☠", 0x52: "☼",
  0x54: "❄", 0x55: "✞", 0x58: "✠", 0x59: "✡", 0x5b: "☯", 0x5e: "♈",
  0x6c: "●", 0x6d: "❍", 0x6e: "■", 0x6f: "□", 0x70: "◻", 0x71: "❑", 0x72: "❒", 0x73: "⬧", 0x74: "⧫",
  0x75: "◆", 0x76: "❖", 0x77: "⬥", 0x78: "⌧", 0x7a: "⌘", 0x7b: "❀", 0x7c: "✿", 0x7d: "❝", 0x7e: "❞",
  0x80: "⓪", 0x81: "①", 0x82: "②", 0x83: "③", 0x84: "④", 0x85: "⑤", 0x86: "⑥", 0x87: "⑦", 0x88: "⑧", 0x89: "⑨", 0x8a: "⑩",
  0x8c: "➀", 0x9e: "·", 0x9f: "•", 0xa0: "▪", 0xa1: "○", 0xa2: "⭕", 0xa4: "◉", 0xa5: "◎", 0xa7: "▪", 0xa8: "◻",
  0xaa: "✦", 0xab: "★", 0xac: "✶", 0xad: "✴", 0xae: "✹", 0xaf: "✵", 0xb1: "⌖", 0xb2: "⟡", 0xb4: "⍰",
  0xd5: "⌫", 0xd6: "⌦", 0xd8: "➢", 0xdf: "⇦", 0xe0: "⇨", 0xe1: "⇧", 0xe2: "⇩", 0xe8: "➔",
  0xef: "⇦", 0xf0: "⇨", 0xf1: "⇧", 0xf2: "⇩", 0xfb: "✗", 0xfc: "✓", 0xfd: "☒", 0xfe: "☑",
};

/** Common Wingdings 2/3 characters */
const WINGDINGS2: Record<number, string> = { 0x50: "✓", 0x51: "✗", 0x52: "☑", 0x53: "☒", 0x54: "☒", 0x98: "●", 0xa3: "□", 0xa4: "■", 0xf0: "✦" };
const WINGDINGS3: Record<number, string> = { 0x7d: "▲", 0x7e: "▼", 0x71: "►", 0x70: "◄", 0x75: "▶", 0x76: "◀" };

/**
 * Convert a symbol-font character (w:sym or bullet text) to regular Unicode;
 * returns null for non-symbol fonts or unmapped characters, so the caller keeps the original character and font.
 */
export function mapSymbol(font: string | undefined | null, code: number): string | null {
  const f = (font ?? "").toLowerCase();
  const c = code >= 0xf000 && code <= 0xf0ff ? code - 0xf000 : code;
  if (f.startsWith("wingdings 2")) return WINGDINGS2[c] ?? null;
  if (f.startsWith("wingdings 3")) return WINGDINGS3[c] ?? null;
  if (f.startsWith("wingdings") || f.startsWith("webdings")) return WINGDINGS[c] ?? null;
  if (f === "symbol") return SYMBOL[c] ?? (c >= 0x20 && c < 0x7f ? String.fromCharCode(c) : null);
  return null;
}

export const isSymbolFont = (font: string | undefined | null) => !!font && /^(symbol|wingdings|webdings)/i.test(font);

/** Bullet text: Private Use Area characters are mapped by font; Courier New's "o" is a hollow circle */
export function bulletText(text: string, font: string | undefined): { text: string; keepFont: boolean } {
  if (!text) return { text, keepFont: true };
  const chars = Array.from(text);
  if (chars.length === 1) {
    const code = chars[0].codePointAt(0) ?? 0;
    const mapped = mapSymbol(font, code);
    if (mapped) return { text: mapped, keepFont: false };
    if (code >= 0xf000 && code <= 0xf0ff) {
      // Private Use Area characters with an unknown font: common mappings
      const m = PUA_FALLBACK[code];
      if (m) return { text: m, keepFont: false };
    }
    if (text === "o" && /courier/i.test(font ?? "")) return { text: "◦", keepFont: false };
    if (text === "§" && /wingdings/i.test(font ?? "")) return { text: "▪", keepFont: false };
  }
  return { text, keepFont: !isSymbolFont(font) };
}

const PUA_FALLBACK: Record<number, string> = {
  0xf0b7: "•", 0xf0a7: "▪", 0xf076: "❖", 0xf0d8: "➢", 0xf0fc: "✓", 0xf0a8: "☐", 0xf0fe: "☑",
  0xf02d: "–", 0xf06e: "■", 0xf071: "❑", 0xf075: "◆", 0xf06c: "●", 0xf06f: "□", 0xf0e8: "➔",
};
