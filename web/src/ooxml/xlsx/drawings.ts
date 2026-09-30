/**
 * Drawing objects on an Excel worksheet (xl/drawings/drawingN.xml): pictures, charts, shapes, groups.
 * Positions are recorded as cell anchors (twoCellAnchor/oneCellAnchor/absoluteAnchor);
 * the caller converts them to pixels using actual column widths and row heights and places them over the cells.
 */

import { attr, css, h, kid, kidPath, kids, numAttr, px, type OoxmlPackage } from "../core/package";
import type { ColorContext, Theme } from "../core/theme";
import type { Box, DrawingItem } from "./anchors";

export { anchorBox, readDrawings, type Anchor, type Box, type DrawingItem } from "./anchors";
import { ensureDrawingStyles, renderDrawingShape } from "../pptx/standalone";
import { renderChart } from "../chart";

// ───────────── Rendering ─────────────

export interface RenderContext {
  pkg: OoxmlPackage;
  theme: Theme | null;
  colors: ColorContext;
}

/** One drawing object → an absolutely positioned element */
export async function renderItem(item: DrawingItem, box: Box, ctx: RenderContext): Promise<HTMLElement | null> {
  const el = await renderContent(item.el, item.part, box, ctx, 0);
  if (!el) return null;
  el.style.position = "absolute";
  el.style.left = px(box.x);
  el.style.top = px(box.y);
  return el;
}

async function renderContent(el: Element, part: string, box: Box, ctx: RenderContext, depth: number): Promise<HTMLElement | null> {
  if (depth > 20 || box.w <= 0 || box.h <= 0) return null;
  switch (el.localName) {
    case "pic":
      return renderPicture(el, part, box, ctx);
    case "graphicFrame":
      return renderFrame(el, part, box, ctx);
    case "sp":
    case "cxnSp":
    case "grpSp": {
      // Shapes, connectors, groups: same geometry, fill and text layout as slides
      ensureDrawingStyles(document);
      const node = await renderDrawingShape(el, { pkg: ctx.pkg, part, theme: ctx.theme, colors: ctx.colors, width: box.w, height: box.h });
      if (!node) return null;
      const wrap = h("div", { style: css({ width: px(box.w), height: px(box.h) }) });
      node.style.left = "0";
      node.style.top = "0";
      wrap.append(node);
      return wrap;
    }
    default:
      return null;
  }
}

function transform(xfrm: Element | null): string | undefined {
  const rot = (numAttr(xfrm, "rot") ?? 0) / 60000;
  const flipH = attr(xfrm, "flipH") === "1";
  const flipV = attr(xfrm, "flipV") === "1";
  const parts = [rot ? `rotate(${rot}deg)` : "", flipH ? "scaleX(-1)" : "", flipV ? "scaleY(-1)" : ""].filter(Boolean);
  return parts.length ? parts.join(" ") : undefined;
}

async function renderPicture(el: Element, part: string, box: Box, ctx: RenderContext) {
  const blip = kidPath(el, "blipFill", "blip");
  const rel = await ctx.pkg.rel(part, attr(blip, "embed"));
  const url = rel && !rel.external ? await ctx.pkg.mediaUrl(rel.target) : null;
  const wrap = h("div", { style: css({ width: px(box.w), height: px(box.h), overflow: "hidden", transform: transform(kidPath(el, "spPr", "xfrm")) }) });
  if (!url) {
    wrap.style.cssText += ";background:#f1f3f4;border:1px solid #dadce0;box-sizing:border-box";
    return wrap;
  }
  const img = h("img", { src: url, alt: attr(kidPath(el, "nvPicPr", "cNvPr"), "descr") ?? "", draggable: "false" });
  // Cropping (srcRect in thousandths of a percent): enlarge the image, then offset it
  const crop = kidPath(el, "blipFill", "srcRect");
  const l = (numAttr(crop, "l") ?? 0) / 100000;
  const t = (numAttr(crop, "t") ?? 0) / 100000;
  const r = (numAttr(crop, "r") ?? 0) / 100000;
  const b = (numAttr(crop, "b") ?? 0) / 100000;
  const kw = 1 - l - r;
  const kh = 1 - t - b;
  if (kw > 0 && kh > 0 && (l || t || r || b)) {
    img.style.cssText = css({ position: "absolute", width: px(box.w / kw), height: px(box.h / kh), left: px((-box.w * l) / kw), top: px((-box.h * t) / kh), "max-width": "none" });
    wrap.style.position = "relative";
  } else img.style.cssText = "width:100%;height:100%;display:block";
  const alpha = numAttr(kid(blip, "alphaModFix"), "amt");
  if (alpha !== null) img.style.opacity = String(alpha / 100000);
  wrap.append(img);
  return wrap;
}

async function renderFrame(el: Element, part: string, box: Box, ctx: RenderContext) {
  const data = kidPath(el, "graphic", "graphicData");
  const chart = kids(data).find((c) => c.localName === "chart");
  if (!chart) return null;
  const rel = await ctx.pkg.rel(part, attr(chart, "id"));
  if (!rel || rel.external) return null;
  const node = await renderChart(ctx.pkg, rel.target, { width: Math.round(box.w), height: Math.round(box.h), colors: ctx.colors });
  const wrap = h("div", { style: css({ width: px(box.w), height: px(box.h) }) });
  wrap.append(node);
  return wrap;
}
