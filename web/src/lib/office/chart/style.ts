/**
 * Chart fills, lines, markers and automatic colors.
 */

import { attr, emuToPx, kid, kids, numAttr, s } from "@/lib/office/ooxml";
import { colorIn, hexToRgba, hslToRgb, rgbaCss, rgbToHsl, schemeColor, type ColorContext, type Rgba } from "@/lib/office/theme";

/** Drawing context: colors, container for gradient definitions and unique ids */
export interface Env {
  colors: ColorContext;
  defs: SVGElement;
  uid: () => string;
}

export interface Stroke {
  color: string;
  /** px */
  width: number;
  dash?: string;
  cap?: "round" | "square" | "butt";
}

/** Office 2013 default accent colors, used when the theme has no color scheme */
const DEFAULT_ACCENTS = ["4472C4", "ED7D31", "A5A5A5", "FFC000", "5B9BD5", "70AD47"];

/** Cycle modifiers for automatic colors (round k: luminance multiplier, luminance offset) */
const CYCLE: [number, number][] = [
  [1, 0],
  [0.6, 0],
  [0.8, 0.2],
  [0.8, 0],
  [0.6, 0.4],
  [0.5, 0],
  [0.7, 0.3],
  [0.7, 0],
  [0.5, 0.5],
];

export function accent(n: number, colors: ColorContext): Rgba {
  return schemeColor(`accent${n}`, colors) ?? hexToRgba(DEFAULT_ACCENTS[n - 1])!;
}

/** HSL luminance multiplier and offset (DrawingML lumMod/lumOff) */
export function lumModOff(c: Rgba, mod: number, off: number): string {
  const hsl = rgbToHsl(c);
  return rgbaCss(hslToRgb({ ...hsl, l: Math.min(1, Math.max(0, hsl.l * mod + off)) }, c.a))!;
}

/** Automatic color of the i-th series (or data point): cycles accents 1–6, applying a different luminance modifier each round */
export function autoColor(i: number, colors: ColorContext): string {
  const idx = Math.max(0, i);
  const base = accent((idx % 6) + 1, colors);
  const [mod, off] = CYCLE[Math.floor(idx / 6) % CYCLE.length];
  return mod === 1 && off === 0 ? rgbaCss(base)! : lumModOff(base, mod, off);
}

/** Lighten/darken a color (HSL luminance offset) */
export function shiftLum(css: string, delta: number): string {
  const c = cssToRgba(css);
  if (!c) return css;
  const hsl = rgbToHsl(c);
  return rgbaCss(hslToRgb({ ...hsl, l: Math.min(1, Math.max(0, hsl.l + delta)) }, c.a))!;
}

export function cssToRgba(css: string): Rgba | null {
  const hex = hexToRgba(css);
  if (hex) return hex;
  const m = /^rgba?\(([\d.]+),([\d.]+),([\d.]+)(?:,([\d.]+))?\)$/.exec(css.replace(/\s/g, ""));
  return m ? { r: +m[1], g: +m[2], b: +m[3], a: m[4] ? +m[4] : 1 } : null;
}

/** Relative luminance of a color (0–1), used to decide dark or light text on top of it */
export function luminance(css: string): number {
  const c = cssToRgba(css);
  if (!c) return 1;
  return (0.299 * c.r + 0.587 * c.g + 0.114 * c.b) / 255;
}

// ───────────── Fill ─────────────

/**
 * Fill of spPr (or other elements containing a fill):
 * returns a CSS color or url(#gradient); null = no fill; undefined = unspecified (use automatic value)
 */
export function readFill(sp: Element | null | undefined, env: Env): string | null | undefined {
  if (!sp) return undefined;
  for (const c of kids(sp)) {
    switch (c.localName) {
      case "noFill":
        return null;
      case "solidFill":
        return rgbaCss(colorIn(c, env.colors)) ?? undefined;
      case "gradFill":
        return gradient(c, env);
      case "pattFill": {
        // Patterns are approximated by a blend of foreground and background colors
        const fg = colorIn(kid(c, "fgClr"), env.colors);
        const bg = colorIn(kid(c, "bgClr"), env.colors);
        if (fg && bg) return rgbaCss({ r: (fg.r * 2 + bg.r) / 3, g: (fg.g * 2 + bg.g) / 3, b: (fg.b * 2 + bg.b) / 3, a: fg.a });
        return rgbaCss(fg ?? bg) ?? undefined;
      }
    }
  }
  return undefined;
}

/** Gradient fill → SVG linearGradient/radialGradient, returns url() */
function gradient(el: Element, env: Env): string | undefined {
  const stops = kids(kid(el, "gsLst"), "gs")
    .map((gs) => ({ pos: (numAttr(gs, "pos") ?? 0) / 100000, color: colorIn(gs, env.colors) }))
    .filter((x): x is { pos: number; color: Rgba } => !!x.color)
    .sort((a, b) => a.pos - b.pos);
  if (!stops.length) return undefined;
  if (stops.length === 1) return rgbaCss(stops[0].color);
  const id = env.uid();
  const path = attr(kid(el, "path"), "path");
  let g: SVGElement;
  if (path) {
    g = s("radialGradient", { id, cx: "50%", cy: "50%", r: "70%" });
  } else {
    // lin ang: 1/60000 degree, clockwise, 0 degrees is left to right
    const ang = ((numAttr(kid(el, "lin"), "ang") ?? 5400000) / 60000) * (Math.PI / 180);
    const dx = Math.cos(ang) / 2;
    const dy = Math.sin(ang) / 2;
    g = s("linearGradient", { id, x1: pct(0.5 - dx), y1: pct(0.5 - dy), x2: pct(0.5 + dx), y2: pct(0.5 + dy) });
  }
  for (const st of stops) {
    const c = st.color;
    g.append(s("stop", { offset: pct(st.pos), "stop-color": rgbaCss({ ...c, a: 1 }), "stop-opacity": c.a < 1 ? Math.round(c.a * 1000) / 1000 : undefined }));
  }
  env.defs.append(g);
  return `url(#${id})`;
}

const pct = (v: number) => `${Math.round(v * 10000) / 100}%`;

/** Representative color of a fill (first gradient stop for gradients), for label contrast or lines */
export function fillColor(sp: Element | null | undefined, env: ColorContext): string | null {
  for (const c of kids(sp)) {
    if (c.localName === "solidFill") return rgbaCss(colorIn(c, env)) ?? null;
    if (c.localName === "gradFill") return rgbaCss(colorIn(kid(kid(c, "gsLst"), "gs"), env)) ?? null;
    if (c.localName === "pattFill") return rgbaCss(colorIn(kid(c, "fgClr"), env)) ?? null;
  }
  return null;
}

// ───────────── Lines ─────────────

/** Preset dash styles (in units of line width) */
const DASH: Record<string, number[]> = {
  dot: [1, 1],
  sysDot: [1, 1],
  dash: [4, 3],
  sysDash: [3, 1],
  lgDash: [8, 3],
  dashDot: [4, 3, 1, 3],
  sysDashDot: [3, 1, 1, 1],
  lgDashDot: [8, 3, 1, 3],
  lgDashDotDot: [8, 3, 1, 3, 1, 3],
  sysDashDotDot: [3, 1, 1, 1, 1, 1],
};

function dashArray(ln: Element, width: number): string | undefined {
  const w = Math.max(width, 1);
  const prst = attr(kid(ln, "prstDash"), "val");
  if (prst && DASH[prst]) return DASH[prst].map((v) => Math.round(v * w * 100) / 100).join(" ");
  const cust = kid(ln, "custDash");
  if (cust) {
    // d/sp are in thousandths of a percent of the line width
    const parts = kids(cust, "ds").flatMap((ds) => [(numAttr(ds, "d") ?? 100000) / 100000, (numAttr(ds, "sp") ?? 100000) / 100000]);
    if (parts.length) return parts.map((v) => Math.round(v * w * 100) / 100).join(" ");
  }
  return undefined;
}

/**
 * a:ln of spPr: null = no line; undefined = unspecified.
 * Uses defWidth (px) when width is unspecified
 */
export function readStroke(sp: Element | null | undefined, env: ColorContext, defWidth = 1): Stroke | null | undefined {
  const ln = kid(sp, "ln");
  if (!ln) return undefined;
  if (kid(ln, "noFill")) return null;
  const w = numAttr(ln, "w");
  const width = w !== null ? Math.max(emuToPx(w), 0.5) : defWidth;
  const solid = kid(ln, "solidFill");
  let color: string | null | undefined = solid ? rgbaCss(colorIn(solid, env)) : undefined;
  if (!color) {
    const grad = kid(ln, "gradFill");
    if (grad) color = rgbaCss(colorIn(kid(kid(grad, "gsLst"), "gs"), env));
  }
  const cap = attr(ln, "cap");
  return {
    color: color ?? "",
    width,
    dash: dashArray(ln, width),
    cap: cap === "rnd" ? "round" : cap === "sq" ? "square" : undefined,
  };
}

/**
 * Parse a line and fill in defaults:
 * unspecified → fallback; specified without color → color taken from fallback
 */
export function strokeOr(sp: Element | null | undefined, env: ColorContext, fallback: Stroke | null): Stroke | null {
  const st = readStroke(sp, env, fallback?.width ?? 1);
  if (st === undefined) return fallback;
  if (st === null) return null;
  if (!st.color) {
    if (!fallback) return null;
    st.color = fallback.color;
  }
  return st;
}

export function strokeAttrs(st: Stroke | null | undefined): Record<string, string | number | undefined> {
  if (!st || !st.color) return { stroke: "none" };
  return {
    stroke: st.color,
    "stroke-width": Math.round(st.width * 100) / 100,
    "stroke-dasharray": st.dash,
    "stroke-linecap": st.cap,
  };
}

// ───────────── Markers ─────────────

export interface Marker {
  symbol: string;
  /** Diameter (px) */
  size: number;
  fill: string | null;
  stroke: Stroke | null;
}

/** Automatic marker shape cycle (default order of older Excel versions) */
const AUTO_SYMBOLS = ["diamond", "square", "triangle", "x", "star", "circle", "plus", "dot", "dash"];
export const autoSymbol = (i: number) => AUTO_SYMBOLS[Math.max(0, i) % AUTO_SYMBOLS.length];

/**
 * c:marker → marker; returns null for symbol none.
 * Uses defSymbol when symbol is unspecified (may also be null, meaning hidden by default)
 */
export function readMarker(el: Element | null | undefined, defSymbol: string | null, color: string, env: Env, defSizePt = 5): Marker | null {
  const sym = attr(kid(el, "symbol"), "val") ?? defSymbol;
  if (!sym || sym === "none") return null;
  const symbol = sym === "auto" ? (defSymbol && defSymbol !== "auto" ? defSymbol : "circle") : sym;
  const size = ((numAttr(kid(el, "size"), "val") ?? defSizePt) * 96) / 72;
  const sp = kid(el, "spPr");
  const fill = readFill(sp, env);
  const stroke = strokeOr(sp, env.colors, { color, width: 0.75 * (96 / 72) });
  return { symbol, size, fill: fill === undefined ? color : fill, stroke };
}

/** Draw a marker at (x, y) */
export function drawMarker(m: Marker, x: number, y: number): SVGElement {
  const r = m.size / 2;
  const fillAttrs = { fill: m.fill ?? "none", ...strokeAttrs(m.stroke) };
  const pts = (list: [number, number][]) => list.map(([a, b]) => `${r2(x + a * r)},${r2(y + b * r)}`).join(" ");
  switch (m.symbol) {
    case "square":
      return s("rect", { x: r2(x - r), y: r2(y - r), width: r2(m.size), height: r2(m.size), ...fillAttrs });
    case "diamond":
      return s("polygon", { points: pts([[0, -1], [1, 0], [0, 1], [-1, 0]]), ...fillAttrs });
    case "triangle":
      return s("polygon", { points: pts([[0, -1], [1, 0.8], [-1, 0.8]]), ...fillAttrs });
    case "star": {
      const list: [number, number][] = [];
      for (let i = 0; i < 10; i++) {
        const a = -Math.PI / 2 + (i * Math.PI) / 5;
        const rr = i % 2 ? 0.45 : 1;
        list.push([Math.cos(a) * rr, Math.sin(a) * rr]);
      }
      return s("polygon", { points: pts(list), ...fillAttrs });
    }
    case "x":
    case "plus": {
      const d = m.symbol === "x" ? `M${r2(x - r)},${r2(y - r)}L${r2(x + r)},${r2(y + r)}M${r2(x + r)},${r2(y - r)}L${r2(x - r)},${r2(y + r)}` : `M${r2(x - r)},${r2(y)}L${r2(x + r)},${r2(y)}M${r2(x)},${r2(y - r)}L${r2(x)},${r2(y + r)}`;
      const st = m.stroke ?? (m.fill ? { color: m.fill, width: 1 } : null);
      return s("path", { d, fill: "none", ...strokeAttrs(st) });
    }
    case "dash":
      return s("rect", { x: r2(x - r), y: r2(y - r / 4), width: r2(m.size), height: r2(r / 2), ...fillAttrs });
    case "dot":
      return s("circle", { cx: r2(x), cy: r2(y), r: r2(r / 2), ...fillAttrs });
    case "picture":
    case "circle":
    default:
      return s("circle", { cx: r2(x), cy: r2(y), r: r2(r), ...fillAttrs });
  }
}

export const r2 = (n: number) => Math.round(n * 100) / 100;
