/**
 * Fills and lines: DrawingML solidFill/gradFill/blipFill/pattFill/a:ln converted to SVG or CSS.
 */

import { attr, css, kid, kids, numAttr, s, type OoxmlPackage } from "../core/package";
import { colorIn, rgbaCss, type ColorContext, type Rgba, type Theme } from "../core/theme";

export interface GradStop {
  pos: number; // 0–1
  c: Rgba;
}

export type Fill =
  | { t: "none" }
  | { t: "solid"; c: Rgba }
  | {
      t: "grad";
      stops: GradStop[];
      /** Linear gradient angle (degrees, clockwise from east); null means radial */
      ang: number | null;
      /** Focus rectangle of a radial gradient (fractions) */
      focus: { l: number; t: number; r: number; b: number };
      path: string;
    }
  | { t: "blip"; el: Element; part: string }
  | { t: "patt"; prst: string; fg: Rgba; bg: Rgba }
  | { t: "group" };

const FILL_TAGS = new Set(["noFill", "solidFill", "gradFill", "blipFill", "pattFill", "grpFill"]);

/** First fill element under a parent element */
export function fillElIn(parent: Element | null | undefined): Element | null {
  for (const c of kids(parent)) if (FILL_TAGS.has(c.localName)) return c;
  return null;
}

/** Fill element → Fill */
export function readFill(el: Element | null | undefined, cc: ColorContext, part: string): Fill | null {
  if (!el) return null;
  switch (el.localName) {
    case "noFill":
      return { t: "none" };
    case "solidFill": {
      const c = colorIn(el, cc);
      return c ? { t: "solid", c } : { t: "none" };
    }
    case "gradFill": {
      const stops: GradStop[] = [];
      for (const gs of kids(kid(el, "gsLst"), "gs")) {
        const c = colorIn(gs, cc);
        if (c) stops.push({ pos: Math.min(1, Math.max(0, (numAttr(gs, "pos") ?? 0) / 100000)), c });
      }
      stops.sort((a, b) => a.pos - b.pos);
      if (!stops.length) return { t: "none" };
      if (stops.length === 1) return { t: "solid", c: stops[0].c };
      const lin = kid(el, "lin");
      const path = kid(el, "path");
      const ftr = kid(path, "fillToRect");
      const f = (k: string) => (numAttr(ftr, k) ?? 0) / 100000;
      return {
        t: "grad",
        stops,
        ang: path && !lin ? null : (numAttr(lin, "ang") ?? 0) / 60000,
        focus: { l: f("l"), t: f("t"), r: f("r"), b: f("b") },
        path: attr(path, "path") ?? "circle",
      };
    }
    case "blipFill":
      return { t: "blip", el, part };
    case "pattFill":
      return {
        t: "patt",
        prst: attr(el, "prst") ?? "pct50",
        fg: colorIn(kid(el, "fgClr"), cc) ?? { r: 0, g: 0, b: 0, a: 1 },
        bg: colorIn(kid(el, "bgClr"), cc) ?? { r: 255, g: 255, b: 255, a: 1 },
      };
    case "grpFill":
      return { t: "group" };
  }
  return null;
}

/** Style matrix reference (fillRef/lnRef/effectRef/bgRef): idx and color */
export function styleRef(ref: Element | null | undefined, cc: ColorContext): { idx: number; color: Rgba | null } | null {
  if (!ref) return null;
  return { idx: numAttr(ref, "idx") ?? 0, color: colorIn(ref, cc) };
}

/** fillRef/bgRef → theme fill style (idx 1–999 is fillStyleLst, 1001+ is bgFillStyleLst) */
export function themeFill(ref: Element | null | undefined, cc: ColorContext, theme: Theme | null, part: string): Fill | null {
  const r = styleRef(ref, cc);
  if (!r || !theme || r.idx === 0) return r && r.idx === 0 ? { t: "none" } : null;
  const list = r.idx >= 1001 ? theme.bgFillStyles : theme.fillStyles;
  const el = list[(r.idx >= 1001 ? r.idx - 1001 : r.idx - 1)];
  return readFill(el, { ...cc, phClr: r.color }, part);
}

/** Average color (where gradients can't be used, e.g. text color) */
export function fillColor(f: Fill | null): Rgba | null {
  if (!f) return null;
  if (f.t === "solid") return f.c;
  if (f.t === "grad") return f.stops[0].c;
  if (f.t === "patt") return f.fg;
  return null;
}

// ───────────── Picture fill ─────────────

export interface BlipInfo {
  /** Image path within the package (null: external link or not found) */
  path: string | null;
  /** Crop (fractions; negative = extend outward) */
  crop: { l: number; t: number; r: number; b: number };
  /** Stretch rectangle (fillRect) */
  fillRect: { l: number; t: number; r: number; b: number };
  tile: Element | null;
  opacity: number;
  grayscale: boolean;
  /** Brightness/contrast (-1–1) */
  bright: number;
  contrast: number;
  duotone: boolean;
}

/** blipFill → image info; prefers the SVG version (asvg:svgBlip) */
export async function blipInfo(el: Element, pkg: OoxmlPackage, part: string): Promise<BlipInfo> {
  const blip = kid(el, "blip");
  let id = attr(blip, "embed");
  const svg = blip ? Array.from(blip.getElementsByTagNameNS("*", "svgBlip"))[0] : undefined;
  if (svg && attr(svg, "embed")) id = attr(svg, "embed");
  let path: string | null = null;
  if (id) {
    const rel = await pkg.rel(part, id);
    if (rel && !rel.external) path = rel.target;
  }
  const src = kid(el, "srcRect");
  const fr = kid(kid(el, "stretch"), "fillRect");
  const p = (e: Element | null, k: string) => (numAttr(e, k) ?? 0) / 100000;
  const alpha = kid(blip, "alphaModFix");
  const lum = kid(blip, "lum");
  return {
    path,
    crop: { l: p(src, "l"), t: p(src, "t"), r: p(src, "r"), b: p(src, "b") },
    fillRect: { l: p(fr, "l"), t: p(fr, "t"), r: p(fr, "r"), b: p(fr, "b") },
    tile: kid(el, "tile"),
    opacity: alpha ? (numAttr(alpha, "amt") ?? 100000) / 100000 : 1,
    grayscale: !!kid(blip, "grayscl"),
    bright: p(lum, "bright"),
    contrast: p(lum, "contrast"),
    duotone: !!kid(blip, "duotone"),
  };
}

/** Image effects → CSS filter */
export function blipFilter(b: BlipInfo): string | undefined {
  const f: string[] = [];
  if (b.grayscale || b.duotone) f.push("grayscale(1)");
  // PowerPoint brightness adds a fixed amount while CSS multiplies; use a milder factor
  if (b.bright) f.push(`brightness(${Math.round((1 + b.bright * 0.6) * 100)}%)`);
  if (b.contrast) f.push(`contrast(${Math.round((1 + b.contrast) * 100)}%)`);
  return f.length ? f.join(" ") : undefined;
}

/** Get the image's natural size (needed for tiling) */
function imageSize(url: string): Promise<{ w: number; h: number } | null> {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve({ w: img.naturalWidth, h: img.naturalHeight });
    img.onerror = () => resolve(null);
    img.src = url;
  });
}

// ───────────── SVG ─────────────

/** Keep SVG ids unique within the same document */
let uid = 0;
export const nextId = (p = "g") => `tfp${p}${(++uid).toString(36)}`;

/** A shape's <defs>: created only when needed */
export class Defs {
  el: SVGElement | null = null;
  constructor(private svg: SVGElement) {}
  add(node: SVGElement): string {
    if (!this.el) {
      this.el = s("defs");
      this.svg.prepend(this.el);
    }
    const id = nextId();
    node.setAttribute("id", id);
    this.el.append(node);
    return id;
  }
}

const stopEls = (stops: GradStop[]) =>
  stops.map((st) => s("stop", { offset: `${Math.round(st.pos * 1000) / 10}%`, "stop-color": rgbaCss({ ...st.c, a: 1 }), "stop-opacity": st.c.a < 1 ? Math.round(st.c.a * 1000) / 1000 : undefined }));

/** Linear gradient endpoints (userSpace): same as CSS linear-gradient, with corners landing exactly at 0% and 100% */
function linearEnds(ang: number, w: number, h: number) {
  const r = (ang * Math.PI) / 180;
  const dx = Math.cos(r);
  const dy = Math.sin(r);
  const half = (Math.abs(w * dx) + Math.abs(h * dy)) / 2;
  return { x1: w / 2 - dx * half, y1: h / 2 - dy * half, x2: w / 2 + dx * half, y2: h / 2 + dy * half };
}

/** Pattern (pattFill): foreground pattern on an 8×8 unit grid */
function patternShapes(prst: string, fg: string): SVGElement[] {
  const rect = (x: number, y: number, w = 1, h = 1) => s("rect", { x, y, width: w, height: h, fill: fg });
  const line = (d: string, sw = 1) => s("path", { d, stroke: fg, "stroke-width": sw, fill: "none", "shape-rendering": "crispEdges" });
  const dots = (pts: [number, number][]) => pts.map(([x, y]) => rect(x, y));
  const pct = /^pct(\d+)$/.exec(prst);
  if (pct) {
    // Approximate percentages by dot density
    const p = Number(pct[1]);
    const n = Math.max(1, Math.round((p / 100) * 64));
    const out: [number, number][] = [];
    // Fill in order of an 8×8 ordered dither matrix
    const bayer = [0, 32, 8, 40, 2, 34, 10, 42, 48, 16, 56, 24, 50, 18, 58, 26, 12, 44, 4, 36, 14, 46, 6, 38, 60, 28, 52, 20, 62, 30, 54, 22, 3, 35, 11, 43, 1, 33, 9, 41, 51, 19, 59, 27, 49, 17, 57, 25, 15, 47, 7, 39, 13, 45, 5, 37, 63, 31, 55, 23, 61, 29, 53, 21];
    bayer.forEach((v, i) => v < n && out.push([i % 8, Math.floor(i / 8)]));
    return dots(out);
  }
  switch (prst) {
    case "horz":
    case "ltHorz":
    case "narHorz":
      return [line(prst === "narHorz" ? "M0 0.5H8M0 4.5H8" : "M0 0.5H8")];
    case "dkHorz":
      return [line("M0 1H8", 2)];
    case "vert":
    case "ltVert":
    case "narVert":
      return [line(prst === "narVert" ? "M0.5 0V8M4.5 0V8" : "M0.5 0V8")];
    case "dkVert":
      return [line("M1 0V8", 2)];
    case "dashHorz":
      return [line("M0 0.5H4")];
    case "dashVert":
      return [line("M0.5 0V4")];
    case "cross":
    case "smGrid":
      return [line(prst === "smGrid" ? "M0 0.5H8M0 4.5H8M0.5 0V8M4.5 0V8" : "M0 0.5H8M0.5 0V8")];
    case "lgGrid":
      return [line("M0 0.5H8M0.5 0V8")];
    case "dnDiag":
    case "ltDnDiag":
    case "wdDnDiag":
    case "dkDnDiag":
    case "dashDnDiag":
      return [s("path", { d: "M0 0L8 8M-4 4L4 12M4 -4L12 4", stroke: fg, "stroke-width": prst === "wdDnDiag" || prst === "dkDnDiag" ? 2 : 1, fill: "none" })];
    case "upDiag":
    case "ltUpDiag":
    case "wdUpDiag":
    case "dkUpDiag":
    case "dashUpDiag":
      return [s("path", { d: "M0 8L8 0M-4 4L4 -4M4 12L12 4", stroke: fg, "stroke-width": prst === "wdUpDiag" || prst === "dkUpDiag" ? 2 : 1, fill: "none" })];
    case "diagCross":
    case "openDmnd":
    case "smCheck":
    case "lgCheck":
      return [s("path", { d: "M0 0L8 8M0 8L8 0", stroke: fg, "stroke-width": 1, fill: "none" })];
    case "dotGrid":
      return dots([[0, 0], [4, 0], [0, 4], [4, 4]]);
    case "dotDmnd":
    case "smConfetti":
    case "lgConfetti":
      return dots([[0, 0], [4, 4], [2, 6], [6, 2]]);
    case "solidDmnd":
      return [s("path", { d: "M4 1L7 4L4 7L1 4Z", fill: fg })];
    case "horzBrick":
      return [line("M0 0.5H8M0 4.5H8M0.5 0V4M4.5 4V8")];
    case "diagBrick":
      return [s("path", { d: "M0 8L8 0M-4 4L4 -4M4 12L12 4M2 2L6 6", stroke: fg, "stroke-width": 1, fill: "none" })];
    case "shingle":
    case "zigZag":
    case "wave":
      return [s("path", { d: "M0 6L4 2L8 6", stroke: fg, "stroke-width": 1, fill: "none" })];
    case "trellis":
    case "weave":
    case "plaid":
    case "divot":
    case "sphere":
      return dots([[0, 0], [2, 2], [4, 4], [6, 6], [4, 0], [0, 4], [6, 2], [2, 6]]);
    default:
      return [s("path", { d: "M0 0L8 8M0 8L8 0", stroke: fg, "stroke-width": 1, fill: "none" })];
  }
}

export interface PaintCtx {
  pkg: OoxmlPackage;
  defs: Defs;
  /** Drawing area (px) */
  w: number;
  h: number;
}

/**
 * Fill → SVG fill attribute value (possibly url(#id)); picture fills add the image asynchronously.
 */
export function svgPaint(fill: Fill | null, pc: PaintCtx): { paint: string; opacity?: number } {
  if (!fill || fill.t === "none" || fill.t === "group") return { paint: "none" };
  if (fill.t === "solid") return { paint: rgbaCss({ ...fill.c, a: 1 })!, opacity: fill.c.a < 1 ? fill.c.a : undefined };
  if (fill.t === "grad") {
    if (fill.ang !== null) {
      const e = linearEnds(fill.ang, pc.w, pc.h);
      const id = pc.defs.add(s("linearGradient", { gradientUnits: "userSpaceOnUse", ...e }, ...stopEls(fill.stops)));
      return { paint: `url(#${id})` };
    }
    // Radial: centered on the focus rectangle's center, radius reaching the farthest corner
    const f = fill.focus;
    const cx = ((f.l + (1 - f.r)) / 2) * pc.w;
    const cy = ((f.t + (1 - f.b)) / 2) * pc.h;
    const far = Math.max(...[[0, 0], [pc.w, 0], [0, pc.h], [pc.w, pc.h]].map(([x, y]) => Math.hypot(x - cx, y - cy)));
    const id = pc.defs.add(s("radialGradient", { gradientUnits: "userSpaceOnUse", cx, cy, r: Math.max(far, 1) }, ...stopEls(fill.stops)));
    return { paint: `url(#${id})` };
  }
  if (fill.t === "patt") {
    const bg = rgbaCss(fill.bg)!;
    const pat = s("pattern", { patternUnits: "userSpaceOnUse", width: 8, height: 8 }, s("rect", { width: 8, height: 8, fill: bg }), ...patternShapes(fill.prst, rgbaCss(fill.fg)!));
    return { paint: `url(#${pc.defs.add(pat)})` };
  }
  // Picture: wrap the image in a pattern
  const pat = s("pattern", { patternUnits: "userSpaceOnUse", width: pc.w, height: pc.h });
  const id = pc.defs.add(pat);
  void (async () => {
    const info = await blipInfo(fill.el, pc.pkg, fill.part);
    const url = info.path ? await pc.pkg.mediaUrl(info.path) : null;
    if (!url) {
      pat.append(s("rect", { width: pc.w, height: pc.h, fill: "#e8e8e8" }));
      return;
    }
    const filter = blipFilter(info);
    if (info.tile) {
      const size = await imageSize(url);
      if (!size) return;
      const sx = (numAttr(info.tile, "sx") ?? 100000) / 100000;
      const sy = (numAttr(info.tile, "sy") ?? 100000) / 100000;
      const tw = Math.max(1, size.w * sx);
      const th = Math.max(1, size.h * sy);
      pat.setAttribute("width", String(tw));
      pat.setAttribute("height", String(th));
      pat.setAttribute("x", String((numAttr(info.tile, "tx") ?? 0) / 9525));
      pat.setAttribute("y", String((numAttr(info.tile, "ty") ?? 0) / 9525));
      pat.append(s("image", { href: url, width: tw, height: th, preserveAspectRatio: "none", opacity: info.opacity < 1 ? info.opacity : undefined, style: filter ? `filter:${filter}` : undefined }));
      return;
    }
    const r = imageRect(info, pc.w, pc.h);
    pat.append(s("image", { href: url, x: r.x, y: r.y, width: r.w, height: r.h, preserveAspectRatio: "none", opacity: info.opacity < 1 ? info.opacity : undefined, style: filter ? `filter:${filter}` : undefined }));
  })();
  return { paint: `url(#${id})` };
}

/** Position of the image itself within the w×h area after stretch + crop */
export function imageRect(info: BlipInfo, w: number, h: number) {
  const fr = info.fillRect;
  const bx = fr.l * w;
  const by = fr.t * h;
  const bw = w * (1 - fr.l - fr.r);
  const bh = h * (1 - fr.t - fr.b);
  const c = info.crop;
  const kw = 1 - c.l - c.r;
  const kh = 1 - c.t - c.b;
  const iw = kw > 0.001 ? bw / kw : bw;
  const ih = kh > 0.001 ? bh / kh : bh;
  return { x: bx - c.l * iw, y: by - c.t * ih, w: iw, h: ih };
}

// ───────────── CSS (slide background) ─────────────

export function cssGradient(f: Extract<Fill, { t: "grad" }>): string {
  const stops = f.stops.map((st) => `${rgbaCss(st.c)} ${Math.round(st.pos * 1000) / 10}%`).join(",");
  if (f.ang !== null) return `linear-gradient(${Math.round((f.ang + 90) * 100) / 100}deg,${stops})`;
  const cx = Math.round(((f.focus.l + (1 - f.focus.r)) / 2) * 100);
  const cy = Math.round(((f.focus.t + (1 - f.focus.b)) / 2) * 100);
  return `radial-gradient(${f.path === "circle" ? "circle" : "ellipse"} farthest-corner at ${cx}% ${cy}%,${stops})`;
}

/** Fill → element background style (images applied asynchronously) */
export function applyCssFill(el: HTMLElement, fill: Fill | null, pkg: OoxmlPackage) {
  if (!fill || fill.t === "none" || fill.t === "group") return;
  if (fill.t === "solid") el.style.backgroundColor = rgbaCss(fill.c)!;
  else if (fill.t === "grad") el.style.backgroundImage = cssGradient(fill);
  else if (fill.t === "patt") {
    el.style.backgroundColor = rgbaCss(fill.bg)!;
    el.style.backgroundImage = `repeating-linear-gradient(45deg,${rgbaCss(fill.fg)} 0 1px,transparent 1px 4px)`;
  } else if (fill.t === "blip") {
    void (async () => {
      const info = await blipInfo(fill.el, pkg, fill.part);
      const url = info.path ? await pkg.mediaUrl(info.path) : null;
      if (!url) return;
      if (info.tile) {
        const size = await imageSize(url);
        const sx = (numAttr(info.tile, "sx") ?? 100000) / 100000;
        const sy = (numAttr(info.tile, "sy") ?? 100000) / 100000;
        el.style.cssText += `;${css({
          "background-image": `url("${url}")`,
          "background-repeat": "repeat",
          "background-size": size ? `${size.w * sx}px ${size.h * sy}px` : "auto",
        })}`;
        return;
      }
      // Shown as a child element so crop and transparency can be applied
      const w = el.offsetWidth || parseFloat(el.style.width) || 0;
      const h = el.offsetHeight || parseFloat(el.style.height) || 0;
      const r = imageRect(info, w, h);
      const img = document.createElement("img");
      img.src = url;
      img.alt = "";
      img.decoding = "async";
      img.style.cssText = css({
        position: "absolute",
        left: `${r.x}px`,
        top: `${r.y}px`,
        width: `${r.w}px`,
        height: `${r.h}px`,
        opacity: info.opacity < 1 ? info.opacity : undefined,
        filter: blipFilter(info),
        "pointer-events": "none",
      });
      el.prepend(img);
    })();
  }
}

// ───────────── Lines ─────────────

export interface Arrow {
  type: string;
  w: number;
  len: number;
}

export interface Line {
  fill: Fill;
  /** px */
  width: number;
  dash: number[] | null;
  cap: "butt" | "round" | "square";
  join: "round" | "bevel" | "miter";
  head: Arrow | null;
  tail: Arrow | null;
  compound: string;
}

const DASH: Record<string, number[]> = {
  dot: [1, 3],
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

const ARROW_SIZE: Record<string, number> = { sm: 2, med: 3, lg: 5 };

function readArrow(el: Element | null): Arrow | null {
  const type = attr(el, "type");
  if (!el || !type || type === "none") return null;
  return { type, w: ARROW_SIZE[attr(el, "w") ?? "med"] ?? 3, len: ARROW_SIZE[attr(el, "len") ?? "med"] ?? 3 };
}

/**
 * Line: lns are ordered by priority (the shape's own a:ln, the layout's a:ln…),
 * ref is the lnRef of p:style (theme line style + color). Each property is resolved down the chain independently.
 */
export function resolveLine(lns: (Element | null | undefined)[], ref: Element | null | undefined, cc: ColorContext, theme: Theme | null, part: string): Line | null {
  const chain = lns.filter((e): e is Element => !!e);
  const r = styleRef(ref, cc);
  let refCc = cc;
  if (r && theme && r.idx > 0) {
    const styleLn = theme.lineStyles[r.idx - 1];
    if (styleLn) {
      chain.push(styleLn);
      refCc = { ...cc, phClr: r.color };
    }
  }
  if (!chain.length) return null;
  let fill: Fill | null = null;
  for (const ln of chain) {
    const f = fillElIn(ln);
    if (f) {
      // Theme line styles take the lnRef color
      fill = readFill(f, ln.parentElement?.localName === "lnStyleLst" ? refCc : cc, part);
      break;
    }
  }
  if (!fill || fill.t === "none") return null;
  const a = (name: string) => {
    for (const ln of chain) {
      const v = attr(ln, name);
      if (v !== null) return v;
    }
    return null;
  };
  const k = (name: string) => {
    for (const ln of chain) {
      const v = kid(ln, name);
      if (v) return v;
    }
    return null;
  };
  const wEmu = Number(a("w") ?? 9525);
  const width = Math.max(wEmu / 9525, 0.5);
  const prst = attr(k("prstDash"), "val");
  let dash: number[] | null = prst && prst !== "solid" ? (DASH[prst] ?? null) : null;
  const custDash = k("custDash");
  if (custDash) {
    const ds = kids(custDash, "ds").flatMap((d) => [(numAttr(d, "d") ?? 100000) / 100000, (numAttr(d, "sp") ?? 100000) / 100000]);
    if (ds.length) dash = ds;
  }
  const capAttr = a("cap");
  const cap = capAttr === "rnd" ? "round" : capAttr === "sq" ? "square" : "butt";
  const joinEl = chain.map((ln) => kids(ln).find((c) => /^(round|bevel|miter)$/.test(c.localName))).find(Boolean);
  const join = (joinEl?.localName as Line["join"]) ?? "round";
  return { fill, width, dash, cap, join, head: readArrow(k("headEnd")), tail: readArrow(k("tailEnd")), compound: a("cmpd") ?? "sng" };
}

/** Arrowhead markers (SVG marker), sized in units of line width */
function marker(a: Arrow, color: string, width: number, start: boolean, defs: Defs): string {
  // Arrowheads on thin lines keep a minimum size (the size at roughly 0.7mm line width)
  const base = Math.max(width, 2.65);
  const L = a.len * base;
  const W = a.w * base;
  const m = s("marker", {
    markerUnits: "userSpaceOnUse",
    markerWidth: L + width,
    markerHeight: W + width,
    viewBox: `${-width / 2} ${-W / 2 - width / 2} ${L + width} ${W + width}`,
    refX: L,
    refY: 0,
    orient: start ? "auto-start-reverse" : "auto",
    overflow: "visible",
  });
  const half = W / 2;
  let shape: SVGElement;
  switch (a.type) {
    case "stealth":
      shape = s("path", { d: `M0 ${-half}L${L} 0L0 ${half}L${L * 0.35} 0Z`, fill: color });
      break;
    case "diamond":
      shape = s("path", { d: `M0 0L${L / 2} ${-half}L${L} 0L${L / 2} ${half}Z`, fill: color });
      m.setAttribute("refX", String(L / 2));
      break;
    case "oval":
      shape = s("ellipse", { cx: L / 2, cy: 0, rx: L / 2, ry: half, fill: color });
      m.setAttribute("refX", String(L / 2));
      break;
    case "arrow":
      shape = s("path", { d: `M0 ${-half}L${L} 0L0 ${half}`, fill: "none", stroke: color, "stroke-width": width, "stroke-linejoin": "miter" });
      break;
    default:
      shape = s("path", { d: `M0 ${-half}L${L} 0L0 ${half}Z`, fill: color });
  }
  m.append(shape);
  return defs.add(m);
}

/** Line → SVG attributes */
export function strokeAttrs(line: Line | null, pc: PaintCtx, withMarkers: boolean): Record<string, string | number | undefined> {
  if (!line) return { stroke: "none" };
  const { paint, opacity } = svgPaint(line.fill, pc);
  const out: Record<string, string | number | undefined> = {
    stroke: paint,
    "stroke-opacity": opacity,
    "stroke-width": Math.round(line.width * 100) / 100,
    "stroke-linecap": line.cap,
    "stroke-linejoin": line.join,
    "stroke-miterlimit": line.join === "miter" ? 8 : undefined,
    "stroke-dasharray": line.dash ? line.dash.map((d) => Math.round(d * line.width * 100) / 100).join(" ") : undefined,
  };
  if (withMarkers && (line.head || line.tail)) {
    const color = line.fill.t === "solid" ? rgbaCss(line.fill.c)! : paint.startsWith("url") ? "#000" : paint;
    if (line.head) out["marker-start"] = `url(#${marker(line.head, color, line.width, true, pc.defs)})`;
    if (line.tail) out["marker-end"] = `url(#${marker(line.tail, color, line.width, false, pc.defs)})`;
  }
  return out;
}

// ───────────── Effects ─────────────

/** Outer shadow in effectLst → CSS drop-shadow */
export function shadowFilter(effectLst: Element | null | undefined, cc: ColorContext): string | undefined {
  const sh = kid(effectLst, "outerShdw");
  const glow = kid(effectLst, "glow");
  const parts: string[] = [];
  if (glow) {
    const c = colorIn(glow, cc);
    const rad = (numAttr(glow, "rad") ?? 0) / 9525;
    // Glow: CSS shadows can't spread, so stack several blurred shadows for a more solid edge
    if (c && rad > 0) {
      const one = `drop-shadow(0 0 ${Math.round(rad * 0.3 * 10) / 10}px ${rgbaCss(c)})`;
      parts.push(one, one, one);
    }
  }
  if (sh) {
    const c = colorIn(sh, cc) ?? { r: 0, g: 0, b: 0, a: 0.4 };
    const dist = (numAttr(sh, "dist") ?? 0) / 9525;
    const dir = ((numAttr(sh, "dir") ?? 0) / 60000) * (Math.PI / 180);
    const blur = (numAttr(sh, "blurRad") ?? 0) / 9525;
    const r2 = (n: number) => Math.round(n * 10) / 10;
    parts.push(`drop-shadow(${r2(dist * Math.cos(dir))}px ${r2(dist * Math.sin(dir))}px ${r2(blur)}px ${rgbaCss(c)})`);
  }
  return parts.length ? parts.join(" ") : undefined;
}

/** Reflection (a:reflection, downward reflections only) → -webkit-box-reflect */
export function reflectionCss(effectLst: Element | null | undefined): string | undefined {
  const r = kid(effectLst, "reflection");
  if (!r) return undefined;
  const dir = (numAttr(r, "dir") ?? 5400000) / 60000;
  if (Math.abs(dir - 90) > 10) return undefined;
  const stA = (numAttr(r, "stA") ?? 100000) / 100000;
  const endA = (numAttr(r, "endA") ?? 0) / 100000;
  const endPos = (numAttr(r, "endPos") ?? 100000) / 1000;
  const dist = (numAttr(r, "dist") ?? 0) / 9525;
  // The lower edge of the mask sits close to the shape itself
  return `below ${Math.round(dist * 10) / 10}px linear-gradient(to bottom,rgba(0,0,0,0) 0%,rgba(0,0,0,${endA}) ${Math.round(100 - endPos)}%,rgba(0,0,0,${stA}) 100%)`;
}
