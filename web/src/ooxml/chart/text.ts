/**
 * Chart text: font styles (txPr/defRPr/rPr), width measurement via canvas, layout and SVG output of multi-line/multi-paragraph text.
 */

import { attr, kid, kidPath, kids, numAttr, s } from "../core/package";
import { colorIn, fontStack, rgbaCss, themeFont, type ColorContext } from "../core/theme";

export interface TextStyle {
  /** Font size (pt) */
  size: number;
  bold: boolean;
  italic: boolean;
  underline: boolean;
  strike: boolean;
  color: string;
  /** CSS font-family */
  font: string;
}

export interface Run {
  text: string;
  style: TextStyle;
}

/** A line of text (may contain runs of different styles) */
export type Line = Run[];

/** Laid-out text block */
export interface Block {
  lines: Line[];
  widths: number[];
  heights: number[];
  w: number;
  h: number;
}

export const ptPx = (pt: number) => (pt * 96) / 72;

/** Source of text styles: theme fonts and colors */
export interface TextEnv {
  colors: ColorContext;
}

/** Apply attributes of a:defRPr/a:rPr */
export function applyRPr(base: TextStyle, rPr: Element | null | undefined, env: TextEnv): TextStyle {
  if (!rPr) return base;
  const st = { ...base };
  const sz = numAttr(rPr, "sz");
  if (sz) st.size = sz / 100;
  const b = attr(rPr, "b");
  if (b !== null) st.bold = b === "1" || b === "true";
  const i = attr(rPr, "i");
  if (i !== null) st.italic = i === "1" || i === "true";
  const u = attr(rPr, "u");
  if (u !== null) st.underline = u !== "none";
  const strike = attr(rPr, "strike");
  if (strike !== null) st.strike = strike !== "noStrike";
  const fill = kid(rPr, "solidFill");
  const c = fill ? rgbaCss(colorIn(fill, env.colors)) : undefined;
  if (c) st.color = c;
  else if (kid(rPr, "noFill")) st.color = "transparent";
  const theme = env.colors.theme;
  const latin = themeFont(attr(kid(rPr, "latin"), "typeface"), theme);
  const ea = themeFont(attr(kid(rPr, "ea"), "typeface"), theme);
  if (latin || ea) st.font = fontStack(latin, ea, themeFont("+mn-ea", theme));
  return st;
}

/** Default style of the first paragraph of txPr (or c:rich) */
export function txPrStyle(base: TextStyle, txPr: Element | null | undefined, env: TextEnv): TextStyle {
  if (!txPr) return base;
  let st = applyRPr(base, kidPath(txPr, "lstStyle", "lvl1pPr", "defRPr"), env);
  st = applyRPr(st, kidPath(txPr, "p", "pPr", "defRPr"), env);
  return st;
}

/**
 * Text rotation angle (degrees): a:bodyPr rot (1/60000 degree); vertical text (vert) is treated as ±90.
 * Returns null when unspecified or automatic (-60000000)
 */
export function bodyRotation(txPr: Element | null | undefined): number | null {
  const body = kid(txPr, "bodyPr");
  if (!body) return null;
  const vert = attr(body, "vert");
  if (vert === "vert" || vert === "eaVert") return 90;
  if (vert === "vert270") return -90;
  const rot = numAttr(body, "rot");
  if (rot === null || rot <= -60000000 || rot >= 60000000) return null;
  return rot / 60000;
}

/** c:rich/cx:rich → text lines (paragraphs and a:br line breaks) */
export function richLines(rich: Element | null | undefined, base: TextStyle, env: TextEnv): Line[] {
  const out: Line[] = [];
  if (!rich) return out;
  const pBase = txPrStyle(base, rich, env);
  for (const p of kids(rich, "p")) {
    const para = applyRPr(pBase, kidPath(p, "pPr", "defRPr"), env);
    let line: Line = [];
    for (const r of kids(p)) {
      if (r.localName === "r" || r.localName === "fld") {
        const text = kid(r, "t")?.textContent ?? "";
        if (text) line.push({ text, style: applyRPr(para, kid(r, "rPr"), env) });
      } else if (r.localName === "br") {
        out.push(line.length ? line : [{ text: "", style: para }]);
        line = [];
      }
    }
    out.push(line.length ? line : [{ text: "", style: applyRPr(para, kid(p, "endParaRPr"), env) }]);
  }
  // Remove trailing empty lines
  while (out.length > 1 && out[out.length - 1].every((r) => !r.text)) out.pop();
  return out;
}

/** Plain text (may contain newlines) → text lines */
export function plainLines(text: string, style: TextStyle): Line[] {
  return text.split(/\r?\n/).map((t) => [{ text: t, style }]);
}

// ───────────── Measurement ─────────────

type Ctx2D = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;

/** Full-width characters (CJK characters, full-width symbols) */
const WIDE_CLASS = "\u2e80-\u9fff\uac00-\ud7af\uff00-\uffef";
const WIDE = new RegExp(`[${WIDE_CLASS}]`);
/** Line-break units: English by word (including trailing spaces), full-width text per character */
const TOKEN = new RegExp(`[${WIDE_CLASS}]|[^\\s${WIDE_CLASS}]+\\s*|\\s+`, "g");
let measureCtx: Ctx2D | null | undefined;

function context(): Ctx2D | null {
  if (measureCtx !== undefined) return measureCtx;
  try {
    measureCtx = typeof OffscreenCanvas !== "undefined" ? new OffscreenCanvas(1, 1).getContext("2d") : document.createElement("canvas").getContext("2d");
  } catch {
    measureCtx = null;
  }
  return measureCtx;
}

export const cssFont = (st: TextStyle) => `${st.italic ? "italic " : ""}${st.bold ? "bold " : ""}${ptPx(st.size)}px ${st.font}`;

const widthCache = new Map<string, number>();

/** Text width (px); without canvas, estimated from character count (CJK 1em, others 0.55em) */
export function measure(text: string, st: TextStyle): number {
  if (!text) return 0;
  const font = cssFont(st);
  const key = `${font}\u0000${text}`;
  const hit = widthCache.get(key);
  if (hit !== undefined) return hit;
  const ctx = context();
  let w: number;
  if (ctx) {
    ctx.font = font;
    w = ctx.measureText(text).width;
  } else {
    let em = 0;
    for (const ch of text) em += WIDE.test(ch) ? 1 : 0.55;
    w = em * ptPx(st.size);
  }
  if (widthCache.size > 5000) widthCache.clear();
  widthCache.set(key, w);
  return w;
}

export const lineHeight = (st: TextStyle) => ptPx(st.size) * 1.2;

export function layoutBlock(lines: Line[]): Block {
  const widths = lines.map((l) => l.reduce((sum, r) => sum + measure(r.text, r.style), 0));
  const heights = lines.map((l) => Math.max(...l.map((r) => lineHeight(r.style)), 0));
  return { lines, widths, heights, w: Math.max(0, ...widths), h: heights.reduce((a, b) => a + b, 0) };
}

/** Simple text block with a single style */
export const textBlock = (text: string, style: TextStyle) => layoutBlock(plainLines(text, style));

function tokens(text: string): string[] {
  return text.match(TOKEN) ?? [];
}

/** Word-wrap to a maximum width */
export function wrapLines(lines: Line[], maxWidth: number): Line[] {
  const out: Line[] = [];
  for (const line of lines) {
    let cur: Line = [];
    let w = 0;
    const push = (text: string, style: TextStyle) => {
      const last = cur[cur.length - 1];
      if (last && last.style === style) last.text += text;
      else cur.push({ text, style });
    };
    for (const run of line) {
      for (const tok of tokens(run.text)) {
        const tw = measure(tok, run.style);
        const trimmed = measure(tok.trimEnd(), run.style);
        if (w > 0 && w + trimmed > maxWidth) {
          out.push(trimLine(cur));
          cur = [];
          w = 0;
          if (!tok.trim()) continue;
        }
        push(tok, run.style);
        w += tw;
      }
    }
    out.push(cur.length ? trimLine(cur) : line);
  }
  return out;
}

function trimLine(line: Line): Line {
  if (line.length) line[line.length - 1] = { ...line[line.length - 1], text: line[line.length - 1].text.trimEnd() };
  return line;
}

/** Truncate with an ellipsis when exceeding the width */
export function ellipsize(text: string, st: TextStyle, maxWidth: number): string {
  if (measure(text, st) <= maxWidth) return text;
  const chars = Array.from(text);
  let lo = 0;
  let hi = chars.length;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (measure(chars.slice(0, mid).join("") + "…", st) <= maxWidth) lo = mid;
    else hi = mid - 1;
  }
  return lo ? chars.slice(0, lo).join("") + "…" : "";
}

// ───────────── SVG output ─────────────

function runAttrs(st: TextStyle) {
  const deco = [st.underline && "underline", st.strike && "line-through"].filter(Boolean).join(" ");
  return {
    "font-size": Math.round(ptPx(st.size) * 100) / 100,
    "font-family": st.font,
    "font-weight": st.bold ? "bold" : undefined,
    "font-style": st.italic ? "italic" : undefined,
    "text-decoration": deco || undefined,
    fill: st.color,
  };
}

export type HAlign = "start" | "middle" | "end";
export type VAlign = "top" | "middle" | "bottom";

/**
 * Place a text block at (x, y): hAlign/vAlign pick where the anchor sits in the block; rot is the rotation around the anchor (degrees, clockwise).
 * Each line is aligned individually by hAlign
 */
export function drawBlock(block: Block, x: number, y: number, hAlign: HAlign = "middle", vAlign: VAlign = "top", rot = 0): SVGElement {
  const top = vAlign === "top" ? y : vAlign === "middle" ? y - block.h / 2 : y - block.h;
  // Lines align relative to the block: centered uses x as the midline, left-aligned uses x as the left edge
  const text = s("text", { "text-anchor": hAlign, "xml:space": "preserve" });
  let cy = top;
  block.lines.forEach((line, i) => {
    const lh = block.heights[i];
    const maxPx = lh / 1.2;
    const baseline = cy + lh / 2 + maxPx * 0.35;
    const tspan = s("tspan", { x: round2(x), y: round2(baseline) });
    for (const run of line) {
      const t = s("tspan", runAttrs(run.style));
      t.textContent = run.text;
      tspan.append(t);
    }
    if (!line.length) tspan.textContent = " ";
    text.append(tspan);
    cy += lh;
  });
  if (rot) text.setAttribute("transform", `rotate(${round2(rot)} ${round2(x)} ${round2(y)})`);
  return text;
}

const round2 = (n: number) => Math.round(n * 100) / 100;

/** Bounding box size of the rotated block */
export function rotatedSize(w: number, h: number, rot: number) {
  const r = (Math.abs(rot) * Math.PI) / 180;
  return { w: Math.abs(w * Math.cos(r)) + Math.abs(h * Math.sin(r)), h: Math.abs(w * Math.sin(r)) + Math.abs(h * Math.cos(r)) };
}
