/**
 * DrawingML objects (w:drawing): pictures (crop, rotation, outline), shapes (fill, line, preset geometry), text boxes,
 * groups (including nested groups), canvases, charts, SmartArt (pre-rendered drawings), and inline and floating (text-wrapped) layout.
 */

import { attr, css, emuToPx, h, kid, kidPath, kids, numAttr, s, safeHref } from "../core/package";
import { colorIn, rgbaCss, type ColorContext, type Rgba } from "../core/theme";
import { renderChart } from "../chart";
import { fillBlocks } from "./blocks";
import { childFlow, type Flow } from "./context";
import { adjustValues, customPath, presetPath } from "./geometry";
import { contentHeight } from "./section";
import { flatKids } from "./xml";

export type PlaceMode = "inline" | "float" | "block" | "para" | "page";

export interface ObjOut {
  el: HTMLElement;
  mode: PlaceMode;
}

const r2 = (n: number) => Math.round(n * 100) / 100;
let uid = 0;

// ───────────── Image sources ─────────────

/** Gray placeholder box (for formats that cannot be displayed, such as EMF/WMF) */
export function placeholder(w?: number, hgt?: number) {
  return h("span", { class: "tf-docx-ph", style: css({ width: w ? `${r2(w)}px` : "100%", height: hgt ? `${r2(hgt)}px` : "100%" }) });
}

/** Load an image by relationship id; fall back to a placeholder box on failure or unsupported formats */
export function loadImage(img: HTMLImageElement, relId: string | null | undefined, f: Flow, part?: string) {
  const rels = part ? f.doc.rels.get(part) ?? [] : f.rels;
  const rel = relId ? rels.find((r) => r.id === relId) : undefined;
  const swap = () => img.replaceWith(placeholder());
  if (!rel || rel.external) {
    swap();
    return;
  }
  f.doc.pending.push(
    f.doc.pkg.mediaUrl(rel.target).then((url) => {
      if (url) img.src = url;
      else swap();
    }),
  );
}

// ───────────── Fill and line ─────────────

type Fill = { kind: "none" } | { kind: "solid"; color: Rgba } | { kind: "grad"; stops: { pos: number; color: Rgba }[]; angle: number; path: boolean } | { kind: "blip"; blip: Element };

function styleRef(style: Element | null, name: string) {
  const ref = kid(style, name);
  return { idx: numAttr(ref, "idx") ?? 0, el: ref };
}

function readFill(el: Element, ctx: ColorContext): Fill | null {
  switch (el.localName) {
    case "noFill":
      return { kind: "none" };
    case "solidFill": {
      const c = colorIn(el, ctx);
      return c ? { kind: "solid", color: c } : { kind: "none" };
    }
    case "gradFill": {
      const stops = kids(kid(el, "gsLst"), "gs")
        .map((g) => ({ pos: (numAttr(g, "pos") ?? 0) / 1000, color: colorIn(g, ctx) }))
        .filter((x): x is { pos: number; color: Rgba } => !!x.color)
        .sort((a, b) => a.pos - b.pos);
      if (!stops.length) return null;
      return { kind: "grad", stops, angle: (numAttr(kid(el, "lin"), "ang") ?? 0) / 60000, path: !!kid(el, "path") };
    }
    case "pattFill": {
      const c = colorIn(kid(el, "fgClr"), ctx);
      return c ? { kind: "solid", color: c } : null;
    }
    case "blipFill":
      return kid(el, "blip") ? { kind: "blip", blip: kid(el, "blip")! } : null;
  }
  return null;
}

function fillOf(spPr: Element | null, style: Element | null, f: Flow, inherited?: Fill | null): Fill {
  const theme = f.doc.theme;
  for (const c of kids(spPr)) {
    if (c.localName === "grpFill") return inherited ?? { kind: "none" };
    const v = readFill(c, { theme });
    if (v) return v;
  }
  const ref = styleRef(style, "fillRef");
  if (ref.idx > 0 && theme) {
    const ph = colorIn(ref.el, { theme });
    const tmpl = ref.idx >= 1000 ? theme.bgFillStyles[ref.idx - 1001] : theme.fillStyles[ref.idx - 1];
    const v = tmpl ? readFill(tmpl, { theme, phClr: ph }) : null;
    if (v) return v;
    if (ph) return { kind: "solid", color: ph };
  }
  return { kind: "none" };
}

interface Line {
  color: Rgba;
  width: number;
  dash: string | null;
  head: string | null;
  tail: string | null;
}

const DASH: Record<string, number[]> = {
  dash: [4, 3], sysDash: [3, 1], dot: [1, 1], sysDot: [1, 1], dashDot: [4, 3, 1, 3], lgDash: [8, 3],
  lgDashDot: [8, 3, 1, 3], lgDashDotDot: [8, 3, 1, 3, 1, 3], sysDashDot: [3, 1, 1, 1], sysDashDotDot: [3, 1, 1, 1, 1, 1],
};

function lineOf(spPr: Element | null, style: Element | null, f: Flow): Line | null {
  const theme = f.doc.theme;
  const ln = kid(spPr, "ln");
  if (kid(ln, "noFill")) return null;
  const ref = styleRef(style, "lnRef");
  let color = colorIn(kid(ln, "solidFill"), { theme });
  const tmpl = ref.idx > 0 ? theme?.lineStyles[ref.idx - 1] : undefined;
  if (!color && ref.idx > 0) {
    const ph = colorIn(ref.el, { theme });
    color = colorIn(kid(tmpl, "solidFill"), { theme, phClr: ph }) ?? ph;
  }
  const grad = kid(ln, "gradFill");
  if (!color && grad) color = colorIn(kids(kid(grad, "gsLst"), "gs")[0], { theme });
  if (!color) return null;
  const wEmu = numAttr(ln, "w") ?? numAttr(tmpl, "w") ?? 9525;
  const width = Math.max(0.5, emuToPx(wEmu));
  const dashName = attr(kid(ln, "prstDash"), "val") ?? attr(kid(tmpl, "prstDash"), "val");
  const dash = dashName && DASH[dashName] ? DASH[dashName].map((d) => r2(d * width)).join(",") : null;
  const end = (n: string) => {
    const t = attr(kid(ln, n), "type");
    return t && t !== "none" ? t : null;
  };
  return { color, width, dash, head: end("headEnd"), tail: end("tailEnd") };
}

// ───────────── Shape SVG ─────────────

function shapeSvg(spPr: Element | null, style: Element | null, w: number, hgt: number, f: Flow, inherited?: Fill | null): SVGElement | null {
  const prstGeom = kid(spPr, "prstGeom");
  const custGeom = kid(spPr, "custGeom");
  const prst = attr(prstGeom, "prst") ?? (custGeom ? "" : "rect");
  const path = custGeom ? customPath(custGeom, w, hgt) : presetPath(prst, w, hgt, adjustValues(prstGeom));
  const fill = path.open ? ({ kind: "none" } as Fill) : fillOf(spPr, style, f, inherited);
  const line = lineOf(spPr, style, f);
  if (fill.kind === "none" && !line) return null;
  const svg = s("svg", { class: "tf-docx-svg", width: r2(Math.max(w, 1)), height: r2(Math.max(hgt, 1)), viewBox: `0 0 ${r2(Math.max(w, 1))} ${r2(Math.max(hgt, 1))}` });
  const defs = s("defs");
  let fillAttr = "none";
  let fillOpacity: number | undefined;
  if (fill.kind === "solid") {
    fillAttr = rgbaCss({ ...fill.color, a: 1 }) ?? "none";
    if (fill.color.a < 1) fillOpacity = fill.color.a;
  } else if (fill.kind === "grad") {
    const id = `tf-docx-g${++uid}`;
    const g = fill.path
      ? s("radialGradient", { id, cx: "50%", cy: "50%", r: "70%" })
      : s("linearGradient", { id, x1: "0", y1: "0", x2: "1", y2: "0", gradientTransform: `rotate(${r2(fill.angle)} 0.5 0.5)` });
    for (const st of fill.stops) g.append(s("stop", { offset: `${r2(st.pos)}%`, "stop-color": rgbaCss({ ...st.color, a: 1 }), "stop-opacity": st.color.a < 1 ? r2(st.color.a) : undefined }));
    defs.append(g);
    fillAttr = `url(#${id})`;
  } else if (fill.kind === "blip") {
    fillAttr = "#d9d9d9";
  }
  const attrs: Record<string, string | number | undefined> = { d: path.d, fill: fillAttr, "fill-opacity": fillOpacity, "fill-rule": "evenodd" };
  if (line) {
    attrs.stroke = rgbaCss({ ...line.color, a: 1 });
    if (line.color.a < 1) attrs["stroke-opacity"] = r2(line.color.a);
    attrs["stroke-width"] = r2(line.width);
    if (line.dash) attrs["stroke-dasharray"] = line.dash;
    attrs["stroke-linejoin"] = "round";
    for (const [end, key] of [
      [line.head, "marker-start"],
      [line.tail, "marker-end"],
    ] as const) {
      if (!end) continue;
      const id = `tf-docx-m${++uid}`;
      const shape = end === "oval" ? s("circle", { cx: 5, cy: 5, r: 4 }) : end === "diamond" ? s("path", { d: "M5,0 L10,5 L5,10 L0,5 Z" }) : s("path", { d: key === "marker-end" ? "M0,0 L10,5 L0,10 Z" : "M10,0 L0,5 L10,10 Z" });
      shape.setAttribute("fill", attrs.stroke as string);
      defs.append(s("marker", { id, viewBox: "0 0 10 10", refX: 5, refY: 5, markerWidth: 4, markerHeight: 4, orient: "auto" }, shape));
      attrs[key] = `url(#${id})`;
    }
  } else attrs.stroke = "none";
  if (defs.childNodes.length) svg.append(defs);
  svg.append(s("path", attrs));
  // Outer shadow
  const shdw = kidPath(spPr, "effectLst", "outerShdw");
  if (shdw) {
    const c = colorIn(shdw, { theme: f.doc.theme });
    const dist = emuToPx(numAttr(shdw, "dist") ?? 0);
    const dir = (((numAttr(shdw, "dir") ?? 0) / 60000) * Math.PI) / 180;
    const blur = emuToPx(numAttr(shdw, "blurRad") ?? 0);
    if (c) svg.style.filter = `drop-shadow(${r2(dist * Math.cos(dir))}px ${r2(dist * Math.sin(dir))}px ${r2(blur / 2)}px ${rgbaCss(c)})`;
  }
  return svg;
}

// ───────────── Text boxes ─────────────

function textBox(txbx: Element, bodyPr: Element | null, style: Element | null, w: number, hgt: number, f: Flow): HTMLElement | null {
  const content = kid(txbx, "txbxContent");
  if (!content || f.depth > 6) return null;
  const emu = (n: string, d: number) => emuToPx(numAttr(bodyPr, n) ?? d);
  const l = emu("lIns", 91440);
  const r = emu("rIns", 91440);
  const t = emu("tIns", 45720);
  const b = emu("bIns", 45720);
  const vert = attr(bodyPr, "vert") ?? "horz";
  const vertical = vert !== "horz";
  const anchor = attr(bodyPr, "anchor") ?? "t";
  const color = colorIn(kid(style, "fontRef"), { theme: f.doc.theme });
  const flow = childFlow(f, { story: "textbox", width: Math.max(10, (vertical ? hgt : w) - l - r), depth: f.depth + 1, color: color ? rgbaCss(color) : f.color, floats: [] });
  const box = h("div", {
    class: "tf-docx-txbx",
    style: css({
      padding: `${r2(t)}px ${r2(r)}px ${r2(b)}px ${r2(l)}px`,
      "justify-content": anchor === "ctr" ? "center" : anchor === "b" ? "flex-end" : "flex-start",
      color: flow.color,
      "writing-mode": vertical ? (vert === "vert270" || vert === "mongolianVert" ? "vertical-lr" : "vertical-rl") : undefined,
      "text-orientation": /^(eaVert|wordArtVert|wordArtVertRtl)$/.test(vert) ? "upright" : undefined,
      transform: vert === "vert270" ? "rotate(180deg)" : undefined,
      "white-space": attr(bodyPr, "wrap") === "none" ? "nowrap" : undefined,
    }),
  });
  const inner = h("div", { class: "tf-docx-txbx-in" });
  fillBlocks(inner, flatKids(content), flow);
  box.append(inner);
  return box;
}

// ───────────── Pictures ─────────────

function picture(pic: Element, w: number, hgt: number, f: Flow, alt: string): HTMLElement {
  const blipFill = kid(pic, "blipFill");
  const blip = kid(blipFill, "blip");
  const svgBlip = blip?.getElementsByTagNameNS("*", "svgBlip")[0];
  const relId = attr(svgBlip, "embed") ?? attr(blip, "embed") ?? attr(blip, "link");
  const box = h("span", { class: "tf-docx-pic", style: css({ width: `${r2(w)}px`, height: `${r2(hgt)}px` }) });
  const img = h("img", { alt, draggable: "false" });
  const src = kid(blipFill, "srcRect");
  const crop = (n: string) => Math.max(-100000, Math.min(90000, numAttr(src, n) ?? 0)) / 100000;
  const cl = Math.max(0, crop("l"));
  const ct = Math.max(0, crop("t"));
  const cr = Math.max(0, crop("r"));
  const cb = Math.max(0, crop("b"));
  if (cl || ct || cr || cb) {
    const sw = 1 - cl - cr;
    const sh = 1 - ct - cb;
    img.setAttribute("style", css({ position: "absolute", width: `${r2(100 / sw)}%`, height: `${r2(100 / sh)}%`, left: `${r2((-cl / sw) * 100)}%`, top: `${r2((-ct / sh) * 100)}%`, "max-width": "none" }));
  } else img.setAttribute("style", "width:100%;height:100%");
  // Transparency (alphaModFix)
  const alpha = numAttr(kid(blip, "alphaModFix"), "amt");
  if (alpha !== null) img.style.opacity = String(r2(alpha / 100000));
  box.append(img);
  loadImage(img, relId, f);
  // Outline and clipping shapes such as ellipses
  const spPr = kid(pic, "spPr");
  const line = lineOf(spPr, null, f);
  if (line) {
    box.style.outline = `${r2(line.width)}px ${line.dash ? "dashed" : "solid"} ${rgbaCss(line.color)}`;
    box.style.outlineOffset = `${r2(-line.width / 2)}px`;
  }
  const prst = attr(kid(spPr, "prstGeom"), "prst");
  if (prst === "ellipse") box.style.borderRadius = "50%";
  else if (prst === "roundRect") box.style.borderRadius = `${r2(Math.min(w, hgt) * 0.1667)}px`;
  return box;
}

// ───────────── Shapes ─────────────

function transformOf(xfrm: Element | null) {
  const rot = (numAttr(xfrm, "rot") ?? 0) / 60000;
  const fh = attr(xfrm, "flipH") === "1" || attr(xfrm, "flipH") === "true";
  const fv = attr(xfrm, "flipV") === "1" || attr(xfrm, "flipV") === "true";
  const parts: string[] = [];
  if (rot) parts.push(`rotate(${r2(rot)}deg)`);
  if (fh) parts.push("scaleX(-1)");
  if (fv) parts.push("scaleY(-1)");
  return parts.join(" ");
}

function shape(wsp: Element, w: number, hgt: number, f: Flow, inherited?: Fill | null): HTMLElement {
  const spPr = kid(wsp, "spPr");
  const style = kid(wsp, "style");
  const bodyPr = kid(wsp, "bodyPr");
  const box = h("span", { class: "tf-docx-shape", style: css({ width: `${r2(w)}px`, height: `${r2(hgt)}px` }) });
  const svg = shapeSvg(spPr, style, w, hgt, f, inherited);
  if (svg) box.append(svg);
  const txbx = kid(wsp, "txbx");
  if (txbx) {
    const tb = textBox(txbx, bodyPr, style, w, hgt, f);
    if (tb) {
      box.append(tb);
      // Auto-fit: the shape grows taller when there is more text
      if (kid(bodyPr, "spAutoFit")) {
        box.style.height = "auto";
        box.style.minHeight = `${r2(hgt)}px`;
        tb.style.position = "relative";
      }
    }
  }
  return box;
}

interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

type MapFn = (off: [number, number], ext: [number, number]) => Box;

function xfrmOf(el: Element | null, name = "spPr") {
  const xfrm = kidPath(el, name, "xfrm");
  const off = kid(xfrm, "off");
  const ext = kid(xfrm, "ext");
  return {
    xfrm,
    off: [numAttr(off, "x") ?? 0, numAttr(off, "y") ?? 0] as [number, number],
    ext: [numAttr(ext, "cx") ?? 0, numAttr(ext, "cy") ?? 0] as [number, number],
    chOff: [numAttr(kid(xfrm, "chOff"), "x") ?? 0, numAttr(kid(xfrm, "chOff"), "y") ?? 0] as [number, number],
    chExt: [numAttr(kid(xfrm, "chExt"), "cx") ?? 0, numAttr(kid(xfrm, "chExt"), "cy") ?? 0] as [number, number],
  };
}

/** Group: child coordinates are in the group's child coordinate space (chOff/chExt); nested groups are converted level by level */
function groupChildren(g: Element, host: HTMLElement, map: MapFn, f: Flow, depth: number, fill: Fill | null) {
  if (depth > 8) return;
  for (const c of flatKids(g)) {
    const name = c.localName;
    if (name === "grpSpPr" || name === "nvGrpSpPr" || name === "cNvGrpSpPr" || name === "bg" || name === "whole") continue;
    if (name === "grpSp") {
      const x = xfrmOf(c, "grpSpPr");
      const b = map(x.off, x.ext);
      const sx = x.chExt[0] ? b.w / x.chExt[0] : emuToPx(1);
      const sy = x.chExt[1] ? b.h / x.chExt[1] : emuToPx(1);
      const inner: MapFn = (o, e) => ({ x: b.x + (o[0] - x.chOff[0]) * sx, y: b.y + (o[1] - x.chOff[1]) * sy, w: e[0] * sx, h: e[1] * sy });
      const gf = fillOf(kid(c, "grpSpPr"), null, f, fill);
      groupChildren(c, host, inner, f, depth + 1, gf);
      continue;
    }
    let el: HTMLElement | null = null;
    let b: Box | null = null;
    let tf = "";
    if (name === "wsp" || name === "sp" || name === "cxnSp") {
      const x = xfrmOf(c);
      b = map(x.off, x.ext);
      tf = transformOf(x.xfrm);
      el = name === "wsp" ? shape(c, b.w, b.h, f, fill) : shape(c, b.w, b.h, f, fill);
    } else if (name === "pic") {
      const x = xfrmOf(c);
      b = map(x.off, x.ext);
      tf = transformOf(x.xfrm);
      el = picture(c, b.w, b.h, f, attr(kidPath(c, "nvPicPr", "cNvPr"), "descr") ?? "");
    } else if (name === "graphicFrame") {
      const x = xfrmOf(c, "xfrm");
      const xf = kid(c, "xfrm");
      const off = kid(xf, "off");
      const ext = kid(xf, "ext");
      b = map([numAttr(off, "x") ?? x.off[0], numAttr(off, "y") ?? x.off[1]], [numAttr(ext, "cx") ?? 0, numAttr(ext, "cy") ?? 0]);
      el = graphic(kidPath(c, "graphic", "graphicData"), b.w, b.h, f, "");
    }
    if (!el || !b) continue;
    el.classList.add("tf-docx-gchild");
    el.style.left = `${r2(b.x)}px`;
    el.style.top = `${r2(b.y)}px`;
    if (tf) el.style.transform = tf;
    host.append(el);
  }
}

function group(g: Element, w: number, hgt: number, f: Flow): HTMLElement {
  const box = h("span", { class: "tf-docx-group", style: css({ width: `${r2(w)}px`, height: `${r2(hgt)}px` }) });
  const x = xfrmOf(g, "grpSpPr");
  const cw = x.chExt[0] || x.ext[0];
  const ch = x.chExt[1] || x.ext[1];
  const sx = cw ? w / cw : emuToPx(1);
  const sy = ch ? hgt / ch : emuToPx(1);
  const map: MapFn = (o, e) => ({ x: (o[0] - x.chOff[0]) * sx, y: (o[1] - x.chOff[1]) * sy, w: e[0] * sx, h: e[1] * sy });
  groupChildren(g, box, map, f, 0, fillOf(kid(g, "grpSpPr"), null, f, null));
  return box;
}

/** Canvas (wpc): children are placed at absolute EMU coordinates */
function canvas(c: Element, w: number, hgt: number, f: Flow): HTMLElement {
  const box = h("span", { class: "tf-docx-group", style: css({ width: `${r2(w)}px`, height: `${r2(hgt)}px` }) });
  const bg = kid(c, "bg");
  const fill = bg ? fillOf(bg, null, f) : null;
  if (fill?.kind === "solid") box.style.background = rgbaCss(fill.color) ?? "";
  groupChildren(c, box, (o, e) => ({ x: emuToPx(o[0]), y: emuToPx(o[1]), w: emuToPx(e[0]), h: emuToPx(e[1]) }), f, 0, null);
  return box;
}

// ───────────── SmartArt (uses the drawing part pre-rendered by Word) ─────────────

function smartArt(gd: Element, w: number, hgt: number, f: Flow): HTMLElement {
  const box = h("span", { class: "tf-docx-group", style: css({ width: `${r2(w)}px`, height: `${r2(hgt)}px` }) });
  const relIds = kid(gd, "relIds");
  const drawing = f.doc.diagrams.get(`${f.part}|${attr(relIds, "dm") ?? ""}`);
  const tree = kid(drawing?.documentElement, "spTree");
  if (!tree) {
    box.append(placeholder(w, hgt));
    return box;
  }
  const sx = emuToPx(1);
  for (const sp of kids(tree, "sp")) {
    const x = xfrmOf(sp);
    const b = { x: x.off[0] * sx, y: x.off[1] * sx, w: x.ext[0] * sx, h: x.ext[1] * sx };
    const el = h("span", { class: "tf-docx-shape tf-docx-gchild", style: css({ left: `${r2(b.x)}px`, top: `${r2(b.y)}px`, width: `${r2(b.w)}px`, height: `${r2(b.h)}px`, transform: transformOf(x.xfrm) || undefined }) });
    const svg = shapeSvg(kid(sp, "spPr"), kid(sp, "style"), b.w, b.h, f);
    if (svg) el.append(svg);
    const txBody = kid(sp, "txBody");
    if (txBody) el.append(drawingText(txBody, kid(sp, "style"), f));
    box.append(el);
  }
  return box;
}

/** DrawingML text (a:p/a:r), used for SmartArt shapes */
function drawingText(txBody: Element, style: Element | null, f: Flow): HTMLElement {
  const theme = f.doc.theme;
  const bodyPr = kid(txBody, "bodyPr");
  const anchor = attr(bodyPr, "anchor") ?? "ctr";
  const fontColor = colorIn(kid(style, "fontRef"), { theme });
  const box = h("div", {
    class: "tf-docx-txbx",
    style: css({ "justify-content": anchor === "t" ? "flex-start" : anchor === "b" ? "flex-end" : "center", padding: "2px 4px", color: fontColor ? rgbaCss(fontColor) : undefined }),
  });
  const inner = h("div", { class: "tf-docx-txbx-in" });
  for (const p of kids(txBody, "p")) {
    const algn = attr(kid(p, "pPr"), "algn") ?? "ctr";
    const para = h("div", { style: css({ "text-align": algn === "ctr" ? "center" : algn === "r" ? "right" : "left", "line-height": "1.2" }) });
    for (const r of kids(p)) {
      if (r.localName !== "r" && r.localName !== "fld") continue;
      const rPr = kid(r, "rPr");
      const sz = numAttr(rPr, "sz");
      const col = colorIn(kid(rPr, "solidFill"), { theme });
      para.append(h("span", { style: css({ "font-size": sz ? `${sz / 100}pt` : undefined, "font-weight": attr(rPr, "b") === "1" ? "bold" : undefined, "font-style": attr(rPr, "i") === "1" ? "italic" : undefined, color: col ? rgbaCss(col) : undefined }) }, kid(r, "t")?.textContent ?? ""));
    }
    if (!para.childNodes.length) para.append(h("br"));
    inner.append(para);
  }
  box.append(inner);
  return box;
}

// ───────────── Dispatch ─────────────

function graphic(gd: Element | null, w: number, hgt: number, f: Flow, alt: string): HTMLElement | null {
  if (!gd) return null;
  for (const c of flatKids(gd)) {
    switch (c.localName) {
      case "pic":
        return picture(c, w, hgt, f, alt);
      case "wsp":
        return shape(c, w, hgt, f);
      case "wgp":
        return group(c, w, hgt, f);
      case "wpc":
        return canvas(c, w, hgt, f);
      case "chart": {
        const rel = f.rels.find((r) => r.id === attr(c, "id"));
        const box = h("span", { class: "tf-docx-chart", style: css({ width: `${r2(w)}px`, height: `${r2(hgt)}px` }) });
        if (rel)
          f.doc.pending.push(
            renderChart(f.doc.pkg, rel.target, { width: w, height: hgt, colors: { theme: f.doc.theme } })
              .then((el) => box.replaceChildren(el))
              .catch(() => box.replaceChildren(placeholder(w, hgt))),
          );
        return box;
      }
      case "relIds":
        return smartArt(gd, w, hgt, f);
    }
  }
  return placeholder(w, hgt);
}

/** Compute the position as page coordinates (px) */
function axis(pos: Element | null, horizontal: boolean, size: number, f: Flow): { v: number; rel: string } {
  const sec = f.section;
  const rel = attr(pos, "relativeFrom") ?? (horizontal ? "column" : "paragraph");
  let base = 0;
  let len = 0;
  if (horizontal) {
    switch (rel) {
      case "page":
        len = sec.width;
        break;
      case "leftMargin":
      case "insideMargin":
        len = sec.left;
        break;
      case "rightMargin":
      case "outsideMargin":
        base = sec.width - sec.right;
        len = sec.right;
        break;
      default:
        base = sec.left;
        len = f.width;
    }
  } else {
    switch (rel) {
      case "page":
        len = sec.height;
        break;
      case "margin":
        base = sec.top;
        len = contentHeight(sec);
        break;
      case "topMargin":
      case "insideMargin":
        len = sec.top;
        break;
      case "bottomMargin":
      case "outsideMargin":
        base = sec.height - sec.bottom;
        len = sec.bottom;
        break;
      default:
        len = 0;
    }
  }
  const align = kid(pos, "align")?.textContent?.trim();
  const off = kid(pos, "posOffset")?.textContent;
  const pct = (pos?.getElementsByTagNameNS("*", horizontal ? "pctPosHOffset" : "pctPosVOffset")[0]?.textContent ?? "").trim();
  let v = base;
  if (align) {
    if (align === "center") v = base + (len - size) / 2;
    else if (align === "right" || align === "bottom" || align === "outside") v = base + len - size;
  } else if (pct) v = base + (len * Number(pct)) / 100000;
  else if (off) v = base + emuToPx(Number(off) || 0);
  return { v, rel };
}

export function renderDrawing(d: Element, f: Flow, _p: HTMLElement, pLeft: number): ObjOut | null {
  const node = flatKids(d).find((c) => c.localName === "inline" || c.localName === "anchor");
  if (!node) return null;
  const docPr = kid(node, "docPr");
  if (/^(1|true)$/i.test(attr(docPr, "hidden") ?? "")) return null;
  const ext = kid(node, "extent");
  const w = emuToPx(numAttr(ext, "cx") ?? 0);
  const hgt = emuToPx(numAttr(ext, "cy") ?? 0);
  const alt = attr(docPr, "descr") ?? attr(docPr, "title") ?? "";
  const gd = kidPath(node, "graphic", "graphicData");
  const content = graphic(gd, w, hgt, f, alt);
  if (!content) return null;
  // Rotation and flip of the picture itself
  const xfrm = gd?.getElementsByTagNameNS("*", "xfrm")[0] ?? null;
  const tf = xfrm && kid(gd, "pic") ? transformOf(xfrm) : kid(gd, "wsp") ? transformOf(kidPath(kid(gd, "wsp"), "spPr", "xfrm")) : "";
  if (tf) content.style.transform = tf;

  // Picture hyperlink (docPr/hlinkClick)
  let outer: HTMLElement = content;
  const click = kid(docPr, "hlinkClick");
  if (click) {
    const rel = f.rels.find((r) => r.id === attr(click, "id"));
    const href = safeHref(rel?.target);
    if (href) {
      const a = h("a", { href, target: "_blank", rel: "noopener noreferrer", class: "tf-docx-link" });
      a.append(content);
      outer = a;
    }
  }

  const eff = kid(node, "effectExtent");
  const e = (n: string) => emuToPx(Math.max(0, numAttr(eff, n) ?? 0));
  if (node.localName === "inline") {
    const wrap = h("span", { class: "tf-docx-inl", style: css({ margin: `${r2(e("t"))}px ${r2(e("r"))}px ${r2(e("b"))}px ${r2(e("l"))}px` }) }, outer);
    return { el: wrap, mode: "inline" };
  }

  // Floating objects
  const sec = f.section;
  const dist = (n: string) => emuToPx(numAttr(node, n) ?? 0);
  const behind = /^(1|true)$/i.test(attr(node, "behindDoc") ?? "");
  const wrapEl = kids(node).find((c) => c.localName.startsWith("wrap"));
  const wrap = wrapEl?.localName ?? "wrapNone";
  const hx = axis(kid(node, "positionH"), true, w, f);
  const vy = axis(kid(node, "positionV"), false, hgt, f);
  const z = numAttr(node, "relativeHeight") ?? 0;
  const paraV = vy.rel === "paragraph" || vy.rel === "line" || vy.rel === "char";
  const colLeft = sec.left;
  const fullPage = w >= sec.width * 0.9 && hgt >= sec.height * 0.9;
  const box = h("span", { class: "tf-docx-obj" }, outer);

  if (fullPage || wrap === "wrapNone" || f.story === "header" || f.story === "footer") {
    box.classList.add("tf-docx-abs");
    if (behind || fullPage) box.classList.add("tf-docx-behind");
    else box.style.zIndex = String(Math.min(50, 3 + Math.floor(z / 1e7)));
    if (paraV && f.story !== "header" && f.story !== "footer") {
      box.style.left = `${r2(hx.v - colLeft - pLeft)}px`;
      box.style.top = `${r2(vy.v)}px`;
      return { el: box, mode: "para" };
    }
    // Paragraph-relative vertical position inside a header/footer: estimated from the top of the header (footer) area
    const top = paraV ? (f.story === "footer" ? sec.height - sec.footer - hgt + vy.v : sec.header + vy.v) : vy.v;
    box.style.left = `${r2(hx.v)}px`;
    box.style.top = `${r2(top)}px`;
    return { el: box, mode: "page" };
  }
  if (wrap === "wrapTopAndBottom") {
    box.classList.add("tf-docx-block");
    box.style.marginLeft = `${r2(Math.max(0, hx.v - colLeft - pLeft))}px`;
    box.style.marginTop = `${r2(dist("distT") + (paraV && vy.v > 0 ? vy.v : 0))}px`;
    box.style.marginBottom = `${r2(dist("distB"))}px`;
    return { el: box, mode: "block" };
  }
  // Text wrapping (square, tight, through): float left or right
  box.classList.add("tf-docx-float");
  const centre = hx.v + w / 2;
  const right = kid(kid(node, "positionH"), "align")?.textContent === "right" || hx.rel === "rightMargin" || centre > colLeft + f.width / 2 + 1;
  box.style.float = right ? "right" : "left";
  if (right) box.style.marginRight = `${r2(Math.max(0, colLeft + f.width - (hx.v + w)))}px`;
  else box.style.marginLeft = `${r2(Math.max(0, hx.v - colLeft - pLeft))}px`;
  box.style.paddingLeft = right ? `${r2(dist("distL"))}px` : "";
  box.style.paddingRight = right ? "" : `${r2(dist("distR"))}px`;
  box.style.paddingBottom = `${r2(dist("distB"))}px`;
  box.style.marginTop = `${r2(Math.max(0, paraV ? vy.v : 0) + dist("distT"))}px`;
  return { el: box, mode: "float" };
}

/** Picture bullet (numPicBullet): the image relationship lives in numbering.xml */
export function renderPictureBullet(pb: Element, f: Flow, sizePt: number): Node | null {
  const part = f.doc.numberingPart;
  const blip = pb.getElementsByTagNameNS("*", "blip")[0];
  const imagedata = pb.getElementsByTagNameNS("*", "imagedata")[0];
  const relId = attr(blip, "embed") ?? attr(imagedata, "id");
  if (!relId || !part) return null;
  const px = r2((sizePt * 96) / 72);
  const img = h("img", { alt: "", style: css({ width: `${px}px`, height: `${px}px`, "vertical-align": "-0.1em" }) });
  loadImage(img, relId, f, part);
  return img;
}
