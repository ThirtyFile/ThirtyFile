/**
 * Slide shape tree (p:spTree) → DOM: shapes, pictures, groups, connectors, graphic frames (table/chart/SmartArt/OLE).
 * Shape outlines are drawn with SVG (preset and custom geometry); text is overlaid on top as HTML.
 */

import { attr, css, h, kid, kidPath, kids, numAttr, s, type OoxmlPackage, type Rel } from "../core/package";
import { colorIn, type ColorContext, type Theme } from "../core/theme";
import { renderChart } from "../chart";
import { evaluate, parseCustGeom, readAdjust, type GeomDef, type GeomOut } from "./geometry";
import { presetGeom, RECT_GEOM } from "./presets";
import { matchLayoutPh, matchMasterPh, phOf, phStyleKind, type SlideInfo } from "./model";
import { blipFilter, blipInfo, Defs, fillElIn, imageRect, readFill, reflectionCss, resolveLine, shadowFilter, strokeAttrs, svgPaint, themeFill, type Fill, type Line } from "./paint";
import { hasVisibleText, readBodyPr, textBox, type BodyProps, type TextCtx } from "./text";
import { renderTable } from "./table";
import { C } from "./styles";

// ───────────── Context ─────────────

export interface SlideRender {
  info: SlideInfo;
  pkg: OoxmlPackage;
  /** Work that can only run after layout (text autofit) */
  after: (() => void)[];
  /** Relationship cache per part */
  rels: Map<string, Promise<Map<string, Rel>>>;
  /** Stop continuing once disposed */
  alive: () => boolean;
}

export interface Ctx {
  sr: SlideRender;
  /** Part path used to resolve r:id */
  part: string;
  rels: Map<string, Rel>;
  layer: "slide" | "layout" | "master" | "drawing";
  groupFill: Fill | null;
  depth: number;
}

/**
 * Child coordinates (EMU) → px in the parent container.
 * fw/fh: when a group is flipped, children are mirrored within the group (width/height px), so text direction is not flipped
 */
export interface Mapper {
  ox: number;
  oy: number;
  sx: number;
  sy: number;
  fw?: number | null;
  fh?: number | null;
}

export const ROOT_MAP: Mapper = { ox: 0, oy: 0, sx: 1 / 9525, sy: 1 / 9525 };

export function relsMap(sr: SlideRender, part: string): Promise<Map<string, Rel>> {
  let p = sr.rels.get(part);
  if (!p) {
    p = sr.pkg.rels(part).then((list) => new Map(list.map((r) => [r.id, r])));
    sr.rels.set(part, p);
  }
  return p;
}

interface Xfrm {
  x: number;
  y: number;
  cx: number;
  cy: number;
  rot: number;
  flipH: boolean;
  flipV: boolean;
}

function readXfrm(el: Element | null): Xfrm | null {
  if (!el) return null;
  const off = kid(el, "off");
  const ext = kid(el, "ext");
  if (!off && !ext) return null;
  return {
    x: numAttr(off, "x") ?? 0,
    y: numAttr(off, "y") ?? 0,
    cx: Math.max(0, numAttr(ext, "cx") ?? 0),
    cy: Math.max(0, numAttr(ext, "cy") ?? 0),
    rot: (numAttr(el, "rot") ?? 0) / 60000,
    flipH: attr(el, "flipH") === "1" || attr(el, "flipH") === "true",
    flipV: attr(el, "flipV") === "1" || attr(el, "flipV") === "true",
  };
}

const r2 = (n: number) => Math.round(n * 100) / 100;

/** Outer div of a shape: position, size, rotation; returns the effective flip and rotation after applying group mirroring */
function shapeDiv(x: Xfrm, m: Mapper) {
  const w = x.cx * m.sx;
  const hh = x.cy * m.sy;
  let left = (x.x - m.ox) * m.sx;
  let top = (x.y - m.oy) * m.sy;
  const mx = m.fw != null;
  const my = m.fh != null;
  if (mx) left = m.fw! - left - w;
  if (my) top = m.fh! - top - hh;
  // A single mirroring reverses the rotation direction
  const rot = mx !== my ? -x.rot : x.rot;
  const eff: Xfrm = { ...x, rot, flipH: x.flipH !== mx, flipV: x.flipV !== my };
  return {
    el: h("div", {
      class: C.sp,
      style: css({
        left: `${r2(left)}px`,
        top: `${r2(top)}px`,
        width: `${r2(w)}px`,
        height: `${r2(hh)}px`,
        transform: rot ? `rotate(${r2(rot)}deg)` : undefined,
      }),
    }),
    w,
    h: hh,
    x: eff,
  };
}

const flipCss = (x: Xfrm) => (x.flipH || x.flipV ? `scale(${x.flipH ? -1 : 1},${x.flipV ? -1 : 1})` : undefined);

// ───────────── Placeholder inheritance ─────────────

interface PhSources {
  type: string | null;
  layout: Element | null;
  master: Element | null;
}

function phSources(el: Element, ctx: Ctx): PhSources {
  if (ctx.layer !== "slide") return { type: null, layout: null, master: null };
  const ph = phOf(el);
  if (!ph) return { type: null, layout: null, master: null };
  const info = ctx.sr.info;
  const lp = info.layout ? matchLayoutPh(info.layout.phs, ph.type, ph.idx) : undefined;
  const mp = info.master ? matchMasterPh(info.master.phs, lp?.type ?? ph.type) : undefined;
  return { type: ph.type, layout: lp?.el ?? null, master: mp?.el ?? null };
}

const spPrOf = (el: Element | null) => (el ? kid(el, "spPr") : null);

/** Default position (EMU) when there is no xfrm at all */
function fallbackXfrm(type: string, info: SlideInfo): Xfrm {
  const W = info.pres.cx;
  const H = info.pres.cy;
  const mg = W / 16;
  const box = (y: number, hh: number) => ({ x: mg, y, cx: W - 2 * mg, cy: hh, rot: 0, flipH: false, flipV: false });
  if (type === "title" || type === "ctrTitle") return box(H / 8, H / 4);
  if (type === "subTitle") return box((3 * H) / 8, H / 4);
  if (type === "body" || type === "obj") return box((3 * H) / 8, H / 2);
  return box(H / 4, H / 2);
}

/** Text context: list style chain, fontRef */
function textCtx(ctx: Ctx, src: PhSources, style: Element | null, cc: ColorContext): TextCtx {
  const info = ctx.sr.info;
  const master = info.master;
  const lists: (Element | null)[] = [];
  if (src.type !== null) {
    lists.push(kidPath(src.layout, "txBody", "lstStyle"), kidPath(src.master, "txBody", "lstStyle"));
    const kind = phStyleKind(src.type);
    lists.push(kind === "title" ? (master?.titleStyle ?? null) : kind === "body" ? (master?.bodyStyle ?? null) : (master?.otherStyle ?? null));
    lists.push(info.pres.defaultText);
  } else {
    lists.push(info.pres.defaultText, master?.otherStyle ?? null);
  }
  const fontRef = kid(style, "fontRef");
  const fIdx = attr(fontRef, "idx");
  return {
    cc,
    theme: info.theme,
    lists: lists.filter(Boolean),
    own: 0,
    fontRefColor: fontRef ? colorIn(fontRef, cc) : null,
    fontRefFace: fIdx === "major" ? "+mj-lt" : fIdx === "minor" ? "+mn-lt" : null,
    slideNum: info.num,
    rels: ctx.rels,
    media: async (rid) => {
      const rel = ctx.rels.get(rid);
      return rel && !rel.external ? ctx.sr.pkg.mediaUrl(rel.target) : null;
    },
  };
}

/** Background: the first of slide → layout → master that has p:bg */
export function slideBackground(info: SlideInfo): { fill: Fill | null; part: string } {
  const parts: [Element | null, string][] = [
    [info.root, info.path],
    [info.layout?.root ?? null, info.layout?.path ?? ""],
    [info.master?.root ?? null, info.master?.path ?? ""],
  ];
  for (const [root, part] of parts) {
    const bg = kidPath(root, "cSld", "bg");
    if (!bg) continue;
    const bgPr = kid(bg, "bgPr");
    if (bgPr) return { fill: readFill(fillElIn(bgPr), info.cc, part), part };
    const ref = kid(bg, "bgRef");
    if (ref) return { fill: themeFill(ref, info.cc, info.theme, part), part };
  }
  return { fill: null, part: info.path };
}

// ───────────── Geometry and outline ─────────────

function geometryOf(spPr: Element | null): { def: GeomDef; adj: Map<string, number> | null } {
  const cust = kid(spPr, "custGeom");
  if (cust) return { def: parseCustGeom(cust), adj: null };
  const prst = kid(spPr, "prstGeom");
  const def = presetGeom(attr(prst, "prst") ?? "rect") ?? RECT_GEOM;
  return { def, adj: readAdjust(kid(prst, "avLst")) };
}

const SHADE: Record<string, [string, number]> = {
  darken: ["#000", 0.4],
  darkenLess: ["#000", 0.2],
  lighten: ["#fff", 0.4],
  lightenLess: ["#fff", 0.2],
};

/** Geometry → SVG (fill + outline + arrowheads) */
function drawGeometry(geo: GeomOut, w: number, hh: number, fill: Fill | null, line: Line | null, pkg: OoxmlPackage, x: Xfrm | null, filter?: string): SVGElement | null {
  const svg = s("svg", {
    class: C.svg,
    width: r2(Math.max(w, 1)),
    height: r2(Math.max(hh, 1)),
    style: css({ transform: x ? flipCss(x) : undefined, filter }),
  });
  const pc = { pkg, defs: new Defs(svg), w, h: hh };
  let drawn = false;
  for (const p of geo.paths) {
    const hasFill = p.fill !== "none" && fill && fill.t !== "none";
    const hasStroke = p.stroke && !!line;
    if (!hasFill && !hasStroke) continue;
    const open = !/Z\s*$/i.test(p.d) || p.fill === "none";
    if (hasFill) {
      const paint = svgPaint(fill, pc);
      svg.append(s("path", { d: p.d, fill: paint.paint, "fill-opacity": paint.opacity, "fill-rule": "evenodd", stroke: "none" }));
      const shade = SHADE[p.fill];
      if (shade) svg.append(s("path", { d: p.d, fill: shade[0], "fill-opacity": shade[1], stroke: "none" }));
    }
    if (hasStroke && line) {
      const attrs = strokeAttrs(line, pc, open);
      // Compound lines (double, triple): hollow out the middle with a mask
      let mask: string | undefined;
      if (line.compound !== "sng") {
        const lw = line.width;
        const pad = lw * 2;
        const m = s("mask", { maskUnits: "userSpaceOnUse", x: -pad, y: -pad, width: w + pad * 2, height: hh + pad * 2 });
        const band = (color: string, width: number) => s("path", { d: p.d, fill: "none", stroke: color, "stroke-width": width, "stroke-linejoin": line.join });
        m.append(band("#fff", lw));
        if (line.compound === "tri") m.append(band("#000", lw * 0.6), band("#fff", lw * 0.2));
        else m.append(band("#000", lw / 3));
        mask = `url(#${pc.defs.add(m)})`;
      }
      svg.append(s("path", { d: p.d, fill: "none", ...attrs, mask }));
    }
    drawn = true;
  }
  return drawn ? svg : null;
}

function effectOf(spPr: Element | null, style: Element | null, cc: ColorContext, theme: Theme | null): string | undefined {
  const own = kid(spPr, "effectLst");
  if (own) return shadowFilter(own, cc);
  if (kid(spPr, "effectDag")) return undefined;
  const ref = kid(style, "effectRef");
  const idx = numAttr(ref, "idx") ?? 0;
  if (!ref || !theme || idx < 1) return undefined;
  const st = theme.effectStyles[idx - 1];
  return shadowFilter(kid(st, "effectLst"), { ...cc, phClr: colorIn(ref, cc) });
}

// ───────────── Shapes (p:sp, p:cxnSp) ─────────────

function renderSp(el: Element, ctx: Ctx, m: Mapper): HTMLElement | null {
  const info = ctx.sr.info;
  const cc = info.cc;
  const src = phSources(el, ctx);
  const spPr = kid(el, "spPr");
  const lSpPr = spPrOf(src.layout);
  const mSpPr = spPrOf(src.master);
  const x = readXfrm(kid(spPr, "xfrm")) ?? readXfrm(kid(lSpPr, "xfrm")) ?? readXfrm(kid(mSpPr, "xfrm")) ?? (src.type ? fallbackXfrm(src.type, info) : null);
  if (!x) return null;
  const style = kid(el, "style");

  // Fill: shape → placeholder → p:style
  let fill: Fill | null = null;
  const own = fillElIn(spPr);
  if (own) fill = readFill(own, cc, ctx.part);
  else if (fillElIn(lSpPr)) fill = readFill(fillElIn(lSpPr), cc, info.layout?.path ?? ctx.part);
  else if (fillElIn(mSpPr)) fill = readFill(fillElIn(mSpPr), cc, info.master?.path ?? ctx.part);
  else fill = themeFill(kid(style, "fillRef"), cc, info.theme, ctx.part);
  if (fill?.t === "group") fill = ctx.groupFill;
  // useBgFill: fill with the slide background
  if (attr(el, "useBgFill") === "1" && !own) fill = slideBackground(info).fill;

  const line = resolveLine([kid(spPr, "ln"), kid(lSpPr, "ln"), kid(mSpPr, "ln")], kid(style, "lnRef"), cc, info.theme, ctx.part);
  const { def, adj } = geometryOf(spPr ?? lSpPr);
  const box = shapeDiv(x, m);
  // Evaluate at the actual display size (after group scaling), so the output is in px
  const geo = evaluate(def, box.w * 9525, box.h * 9525, adj);
  const filter = effectOf(spPr, style, cc, info.theme);
  const svg = drawGeometry(geo, box.w, box.h, fill, line, ctx.sr.pkg, box.x, filter);
  if (svg) box.el.append(svg);
  const reflect = reflectionCss(kid(spPr, "effectLst"));
  if (reflect) box.el.style.setProperty("-webkit-box-reflect", reflect);

  // Text
  const txBody = kid(el, "txBody");
  if (txBody && hasVisibleText(txBody)) {
    const tc = textCtx(ctx, src, style, cc);
    const bp = readBodyPr([kid(txBody, "bodyPr"), kidPath(src.layout, "txBody", "bodyPr"), kidPath(src.master, "txBody", "bodyPr")]);
    // Text area follows the geometry's text rectangle; SmartArt drawings have their own dsp:txXfrm
    let tb = geo.text;
    const tx = readXfrm(kid(el, "txXfrm"));
    if (tx && tx.cx > 0 && tx.cy > 0) tb = { x: (tx.x - x.x) * m.sx, y: (tx.y - x.y) * m.sy, w: tx.cx * m.sx, h: tx.cy * m.sy };
    // Flipped shapes: the text area is mirrored along, but the text itself isn't (text is rotated 180 degrees on vertical flip)
    if (box.x.flipH) tb = { ...tb, x: box.w - tb.x - tb.w };
    if (box.x.flipV) tb = { ...tb, y: box.h - tb.y - tb.h };
    const node = textBox(txBody, tc, bp, tb);
    if (box.x.flipV) node.style.transform = `${node.style.transform || ""} rotate(180deg)`.trim();
    box.el.append(node);
    if (bp.autofit === "norm" && bp.fontScale === 1 && bp.lnReduce === 0) ctx.sr.after.push(() => shrinkToFit(node, (sc) => textBox(txBody, tc, bp, tb, sc)));
  }
  if (!box.el.firstChild) return null;
  return box.el;
}

/** "Shrink text on overflow" (normAutofit without a recorded scale in the file): shrink font size after measuring */
function shrinkToFit(node: HTMLElement, build: (scale: number) => HTMLElement) {
  const fits = (n: HTMLElement, slack = 1) => {
    const inner = n.firstElementChild as HTMLElement | null;
    if (!inner) return true;
    const cs = getComputedStyle(n);
    const avail = n.clientHeight - parseFloat(cs.paddingTop) - parseFloat(cs.paddingBottom);
    return inner.scrollHeight <= avail * slack + 1;
  };
  // No recorded scale means PowerPoint judged it to fit at the time; small overflows from font metric differences are ignored
  if (!node.isConnected || fits(node, 1.15)) return;
  let cur = node;
  let lo = 0.25;
  let hi = 1;
  let best: HTMLElement | null = null;
  for (let i = 0; i < 6; i++) {
    const mid = (lo + hi) / 2;
    const next = build(mid);
    next.style.transform = node.style.transform;
    cur.replaceWith(next);
    cur = next;
    if (fits(next)) {
      best = next;
      lo = mid;
    } else hi = mid;
  }
  if (best && best !== cur) cur.replaceWith(best);
  else if (!best) {
    const last = build(lo);
    last.style.transform = node.style.transform;
    cur.replaceWith(last);
  }
}

// ───────────── Pictures (p:pic) ─────────────

async function renderPic(el: Element, ctx: Ctx, m: Mapper): Promise<HTMLElement | null> {
  const info = ctx.sr.info;
  const cc = info.cc;
  const src = phSources(el, ctx);
  const spPr = kid(el, "spPr");
  const x = readXfrm(kid(spPr, "xfrm")) ?? readXfrm(kid(spPrOf(src.layout), "xfrm")) ?? readXfrm(kid(spPrOf(src.master), "xfrm"));
  if (!x) return null;
  const style = kid(el, "style");
  const blipFill = kid(el, "blipFill");
  // Picture placeholder without a picture (not shown during slideshow)
  if (!blipFill || !kid(blipFill, "blip")) return null;
  const box = shapeDiv(x, m);
  const { def, adj } = geometryOf(spPr);
  const geo = evaluate(def, box.w * 9525, box.h * 9525, adj);
  const line = resolveLine([kid(spPr, "ln")], kid(style, "lnRef"), cc, info.theme, ctx.part);
  const filter = effectOf(spPr, style, cc, info.theme);
  const isRect = !kid(spPr, "custGeom") && ["rect", null].includes(attr(kid(spPr, "prstGeom"), "prst"));
  const bi = await blipInfo(blipFill, ctx.sr.pkg, ctx.part);
  const url = bi?.path ? await ctx.sr.pkg.mediaUrl(bi.path) : null;

  if (!url || bi.tile || !isRect) {
    if (!url) {
      // Formats that can't be displayed (EMF, WMF, external links): a neutral gray box
      box.el.append(h("div", { class: C.na }));
    } else {
      const svg = drawGeometry(geo, box.w, box.h, { t: "blip", el: blipFill, part: ctx.part }, line, ctx.sr.pkg, box.x, filter);
      if (svg) box.el.append(svg);
      return box.el;
    }
  } else {
    const r = imageRect(bi, box.w, box.h);
    const img = h("img", {
      src: url,
      alt: "",
      decoding: "async",
      loading: "lazy",
      draggable: "false",
      style: css({
        left: `${r2(r.x)}px`,
        top: `${r2(r.y)}px`,
        width: `${r2(r.w)}px`,
        height: `${r2(r.h)}px`,
        opacity: bi.opacity < 1 ? bi.opacity : undefined,
        filter: blipFilter(bi),
      }),
    });
    box.el.append(h("div", { class: C.pic, style: css({ inset: "0", transform: flipCss(box.x), filter }) }, img));
  }
  if (line) {
    const svg = drawGeometry(geo, box.w, box.h, null, line, ctx.sr.pkg, box.x);
    if (svg) box.el.append(svg);
  }
  return box.el;
}

// ───────────── Groups ─────────────

async function renderGroup(el: Element, ctx: Ctx, m: Mapper): Promise<HTMLElement | null> {
  if (ctx.depth > 20) return null;
  const gp = kid(el, "grpSpPr");
  const xf = kid(gp, "xfrm");
  const x = readXfrm(xf);
  const fillEl = fillElIn(gp);
  const groupFill = fillEl ? readFill(fillEl, ctx.sr.info.cc, ctx.part) : ctx.groupFill;
  const sub: Ctx = { ...ctx, groupFill: groupFill?.t === "group" ? ctx.groupFill : groupFill, depth: ctx.depth + 1 };
  let inner: Mapper = m;
  let host: HTMLElement;
  if (x) {
    const box = shapeDiv(x, m);
    host = box.el;
    const chOff = kid(xf, "chOff");
    const chExt = kid(xf, "chExt");
    const cx = numAttr(chExt, "cx") || x.cx;
    const cy = numAttr(chExt, "cy") || x.cy;
    // Child coordinates: chOff/chExt map to the group size; when the group is flipped, children are mirrored within it
    inner = {
      ox: numAttr(chOff, "x") ?? x.x,
      oy: numAttr(chOff, "y") ?? x.y,
      sx: cx ? box.w / cx : m.sx,
      sy: cy ? box.h / cy : m.sy,
      fw: box.x.flipH ? box.w : null,
      fh: box.x.flipV ? box.h : null,
    };
  } else host = h("div", { class: C.sp, style: "left:0;top:0;width:0;height:0" });
  const filter = effectOf(gp, null, ctx.sr.info.cc, ctx.sr.info.theme);
  if (filter) host.style.filter = filter;
  await renderTree(el, sub, inner, host);
  return host.firstChild ? host : null;
}

// ───────────── Graphic frames ─────────────

async function renderFrame(el: Element, ctx: Ctx, m: Mapper): Promise<HTMLElement | null> {
  const src = phSources(el, ctx);
  const x = readXfrm(kid(el, "xfrm")) ?? readXfrm(kid(spPrOf(src.layout), "xfrm"));
  if (!x) return null;
  const data = kidPath(el, "graphic", "graphicData");
  const uri = attr(data, "uri") ?? "";
  const box = shapeDiv(x, m);
  if (uri.endsWith("/table")) {
    const tbl = kid(data, "tbl");
    if (tbl) box.el.append(renderTable(tbl, { ...ctx, textFor: (st) => textCtx(ctx, { type: null, layout: null, master: null }, st, ctx.sr.info.cc) }, box.w, box.h, m));
    return box.el;
  }
  if (uri.endsWith("/chart")) {
    const rel = ctx.rels.get(attr(kid(data, "chart"), "id") ?? "");
    if (rel && !rel.external) {
      const chart = await renderChart(ctx.sr.pkg, rel.target, { width: box.w, height: box.h, colors: ctx.sr.info.cc });
      box.el.append(chart);
      return box.el;
    }
  }
  if (uri.endsWith("/diagram")) {
    const node = await renderDiagram(data!, ctx, box.w, box.h);
    if (node) {
      box.el.append(node);
      return box.el;
    }
  }
  // OLE objects and others: use the preview image
  const pic = Array.from(el.getElementsByTagNameNS("*", "pic"))[0];
  if (pic) {
    const node = await renderPic(
      pic,
      { ...ctx, layer: "drawing" },
      { ox: numAttr(kidPath(pic, "spPr", "xfrm", "off"), "x") ?? 0, oy: numAttr(kidPath(pic, "spPr", "xfrm", "off"), "y") ?? 0, sx: m.sx, sy: m.sy },
    );
    if (node) {
      node.style.left = "0";
      node.style.top = "0";
      node.style.width = "100%";
      node.style.height = "100%";
      box.el.append(node);
      return box.el;
    }
  }
  const blip = Array.from(el.getElementsByTagNameNS("*", "blip"))[0];
  const rel = blip ? ctx.rels.get(attr(blip, "embed") ?? "") : undefined;
  const url = rel && !rel.external ? await ctx.sr.pkg.mediaUrl(rel.target) : null;
  if (url) box.el.append(h("div", { class: C.pic, style: "inset:0" }, h("img", { src: url, alt: "", decoding: "async", style: "left:0;top:0;width:100%;height:100%" })));
  else box.el.append(h("div", { class: C.na }));
  return box.el;
}

/** SmartArt: uses the drawing part saved by PowerPoint (dsp:drawing) */
async function renderDiagram(data: Element, ctx: Ctx, w: number, hh: number): Promise<HTMLElement | null> {
  const relIds = kid(data, "relIds");
  const pkg = ctx.sr.pkg;
  let drawingPath: string | null = null;
  const dmRel = ctx.rels.get(attr(relIds, "dm") ?? "");
  if (dmRel && !dmRel.external) {
    const dm = await pkg.xml(dmRel.target);
    const ext = dm ? Array.from(dm.getElementsByTagNameNS("*", "dataModelExt"))[0] : undefined;
    const rid = attr(ext, "relId");
    const r = rid ? ctx.rels.get(rid) : undefined;
    if (r && !r.external) drawingPath = r.target;
  }
  if (!drawingPath) {
    // When no match is found, use the drawing part from the slide relationships (case of a single SmartArt)
    const drawings = [...ctx.rels.values()].filter((r) => r.type.endsWith("/diagramDrawing"));
    if (drawings.length === 1) drawingPath = drawings[0].target;
  }
  if (!drawingPath) return null;
  const doc = await pkg.xml(drawingPath);
  const tree = doc ? Array.from(doc.getElementsByTagNameNS("*", "spTree"))[0] : undefined;
  if (!tree) return null;
  const host = h("div", { class: C.sp, style: css({ left: "0", top: "0", width: `${r2(w)}px`, height: `${r2(hh)}px` }) });
  const sub: Ctx = { ...ctx, part: drawingPath, rels: await relsMap(ctx.sr, drawingPath), layer: "drawing", depth: ctx.depth + 1 };
  await renderTree(tree, sub, ROOT_MAP, host);
  return host;
}

// ───────────── Tree traversal ─────────────

/** mc:AlternateContent: use Choice for supported namespaces, otherwise Fallback */
const SUPPORTED_NS = new Set(["a14", "p14", "p15", "a15", "a16", "asvg", "v", "p", "a", "r", "dsp", "c14"]);

export function pickAlternate(el: Element): Element | null {
  for (const c of kids(el)) {
    if (c.localName === "Choice") {
      const req = (attr(c, "Requires") ?? "").split(/\s+/).filter(Boolean);
      if (req.every((p) => SUPPORTED_NS.has(p))) return c;
    } else if (c.localName === "Fallback") return c;
  }
  return null;
}

export async function renderNode(c: Element, ctx: Ctx, m: Mapper): Promise<Node | null> {
  // Placeholders on layouts and masters are not displayed directly
  if (ctx.layer === "layout" || ctx.layer === "master") {
    if (c.localName !== "AlternateContent" && c.localName !== "grpSp" && phOf(c)) return null;
  }
  switch (c.localName) {
    case "sp":
    case "cxnSp":
      return renderSp(c, ctx, m);
    case "pic":
      return renderPic(c, ctx, m);
    case "grpSp":
      return renderGroup(c, ctx, m);
    case "graphicFrame":
      return renderFrame(c, ctx, m);
    case "AlternateContent": {
      const branch = pickAlternate(c);
      if (!branch) return null;
      const frag = document.createDocumentFragment();
      for (const k of kids(branch)) {
        const n = await renderNode(k, ctx, m);
        if (n) frag.append(n);
      }
      return frag;
    }
  }
  return null;
}

export async function renderTree(tree: Element, ctx: Ctx, m: Mapper, parent: HTMLElement) {
  for (const c of kids(tree)) {
    if (!ctx.sr.alive()) return;
    try {
      const n = await renderNode(c, ctx, m);
      if (n) parent.append(n);
    } catch (e) {
      // A failure in a single shape doesn't affect other content
      console.warn("pptx shape", e);
    }
  }
}

export type { BodyProps };
