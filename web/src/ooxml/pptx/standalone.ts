/**
 * DrawingML shapes outside slides (e.g. xdr:sp/xdr:cxnSp/xdr:grpSp/xdr:pic in Excel drawings xl/drawings/drawingN.xml).
 * Reuses the presentation geometry, fill and text engines, but without placeholder and master inheritance;
 * default text is the theme body font at 11pt, with color from fontRef or tx1.
 */

import { attr, kid, parseXml, type OoxmlPackage } from "../core/package";
import type { ColorContext, Theme } from "../core/theme";
import type { Pres, SlideInfo } from "./model";
import { pickAlternate, relsMap, renderNode, type Ctx, type Mapper, type SlideRender } from "./shapes";
import { C, CSS } from "./styles";

export interface DrawingShapeOptions {
  pkg: OoxmlPackage;
  /** Part used to resolve r:id (e.g. "xl/drawings/drawing1.xml") */
  part: string;
  theme: Theme | null;
  colors: ColorContext;
  /** Size computed by the host from the cell anchors (px) */
  width: number;
  height: number;
}

const A_NS = "http://schemas.openxmlformats.org/drawingml/2006/main";
const STYLE_ID = "tf-pptx-drawing-styles";

/** Add the tf-pptx- styles to the document once (prefixed classes only, not affecting other page elements) */
export function ensureDrawingStyles(doc: Document = document) {
  if (doc.getElementById(STYLE_ID)) return;
  const style = doc.createElement("style");
  style.id = STYLE_ID;
  style.textContent = CSS;
  doc.head.append(style);
}

/** Default text for Excel shapes: 11pt, body font */
let defaultText: Element | null | undefined;
function excelDefaultText(): Element | null {
  if (defaultText === undefined) {
    const rpr = `<a:defRPr sz="1100"><a:latin typeface="+mn-lt"/><a:ea typeface="+mn-ea"/></a:defRPr>`;
    defaultText = parseXml(`<a:lstStyle xmlns:a="${A_NS}"><a:defPPr>${rpr}</a:defPPr><a:lvl1pPr>${rpr}</a:lvl1pPr></a:lstStyle>`)?.documentElement ?? null;
  }
  return defaultText;
}

/** xfrm of a shape (in grpSpPr for grpSp, in spPr otherwise) */
function xfrmOf(el: Element): Element | null {
  return kid(kid(el, el.localName === "grpSp" ? "grpSpPr" : "spPr"), "xfrm");
}

/**
 * Single shape → element (sized width×height, positioned by the caller).
 * Returns null when it cannot be displayed.
 */
export async function renderDrawingShape(el: Element, opts: DrawingShapeOptions): Promise<HTMLElement | null> {
  const { pkg, part, theme, colors, width, height } = opts;
  // mc:AlternateContent: take the first shape in the supported branch
  if (el.localName === "AlternateContent") {
    const inner = pickAlternate(el)?.firstElementChild;
    return inner ? renderDrawingShape(inner, opts) : null;
  }
  const root = el.ownerDocument.documentElement;
  const pres: Pres = {
    pkg,
    cx: Math.max(1, width * 9525),
    cy: Math.max(1, height * 9525),
    slides: [],
    defaultText: excelDefaultText(),
    tableStyles: new Map(),
    layouts: new Map(),
    masters: new Map(),
  };
  const info: SlideInfo = { path: part, root, tree: null, phs: [], pres, layout: null, master: null, theme, cc: { ...colors, theme: colors.theme ?? theme }, num: 1 };
  const sr: SlideRender = { info, pkg, after: [], rels: new Map(), alive: () => true };
  const ctx: Ctx = { sr, part, rels: await relsMap(sr, part), layer: "drawing", groupFill: null, depth: 0 };

  // Anchor size wins: map the shape's own xfrm to 0,0–width,height; when there is no xfrm or its size is 0, fill in on a clone (the original document is not modified)
  let target = el;
  let xfrm = xfrmOf(el);
  const ext0 = kid(xfrm, "ext");
  // Excel often writes ext cx=0 cy=0 (size determined by the anchor)
  if (!xfrm || !ext0 || (!Number(attr(ext0, "cx")) && !Number(attr(ext0, "cy")))) {
    target = el.cloneNode(true) as Element;
    const holder = kid(target, target.localName === "grpSp" ? "grpSpPr" : "spPr");
    if (!holder) return null;
    const x = kid(holder, "xfrm") ?? target.ownerDocument.createElementNS(A_NS, "a:xfrm");
    if (!x.parentNode) holder.prepend(x);
    const off = kid(x, "off") ?? x.appendChild(target.ownerDocument.createElementNS(A_NS, "a:off"));
    const ext = kid(x, "ext") ?? x.appendChild(target.ownerDocument.createElementNS(A_NS, "a:ext"));
    if (!attr(off, "x")) off.setAttribute("x", "0");
    if (!attr(off, "y")) off.setAttribute("y", "0");
    ext.setAttribute("cx", String(Math.round(width * 9525)));
    ext.setAttribute("cy", String(Math.round(height * 9525)));
    xfrm = x;
  }
  const off = kid(xfrm, "off");
  const ext = kid(xfrm, "ext");
  const cx = Number(attr(ext, "cx") ?? 0);
  const cy = Number(attr(ext, "cy") ?? 0);
  const m: Mapper = {
    ox: Number(attr(off, "x") ?? 0),
    oy: Number(attr(off, "y") ?? 0),
    sx: cx > 0 ? width / cx : 1 / 9525,
    sy: cy > 0 ? height / cy : 1 / 9525,
  };
  const node = await renderNode(target, ctx, m);
  if (!(node instanceof HTMLElement)) return null;
  node.classList.add(C.drawing);
  node.style.left = "";
  node.style.top = "";
  node.style.width = `${Math.round(width * 100) / 100}px`;
  node.style.height = `${Math.round(height * 100) / 100}px`;
  // Text autofit needs layout: measure after the caller has added the element to the document
  if (sr.after.length) {
    setTimeout(() => {
      for (const f of sr.after) {
        try {
          f();
        } catch {
          // Leave as is when measuring fails
        }
      }
    }, 0);
  }
  return node;
}
