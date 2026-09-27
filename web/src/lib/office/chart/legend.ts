/**
 * Legend: entries arranged by position (right, left, top, bottom, top-right); legend keys are color swatches or line + marker.
 */

import { attr, kid, kids, numAttr, s, toggle } from "@/lib/office/ooxml";
import type { Rect } from "./axes";
import type { Ctx } from "./paint";
import { drawMarker, readFill, r2, strokeAttrs, strokeOr, type Marker, type Stroke } from "./style";
import { drawBlock, ellipsize, layoutBlock, lineHeight, plainLines, ptPx, txPrStyle, type Block, type TextStyle } from "./text";

export interface LegendEntry {
  text: string;
  /** Color swatch or line */
  kind: "box" | "line";
  fill?: string | null;
  stroke?: Stroke | null;
  marker?: Marker | null;
  /** idx of the matching c:legendEntry */
  idx: number;
}

export interface Legend {
  /** Space the legend takes on the legendPos side (0 when overlay) */
  take: { side: "l" | "r" | "t" | "b"; size: number } | null;
  draw: (g: SVGElement, plotArea: Rect) => void;
}

interface Item {
  entry: LegendEntry;
  block: Block;
  w: number;
}

/** Manual layout (c:layout/c:manualLayout) → rectangle relative to the whole chart; only handles positions in edge and factor mode */
export function manualRect(layout: Element | null, W: number, H: number, auto: Rect | null): Rect | null {
  const m = kid(layout, "manualLayout");
  if (!m) return null;
  const x = numAttr(kid(m, "x"), "val");
  const y = numAttr(kid(m, "y"), "val");
  const w = numAttr(kid(m, "w"), "val");
  const h = numAttr(kid(m, "h"), "val");
  if (x === null && y === null && w === null && h === null) return null;
  const xFactor = attr(kid(m, "xMode"), "val") === "factor";
  const yFactor = attr(kid(m, "yMode"), "val") === "factor";
  const base = auto ?? { x: 0, y: 0, w: W, h: H };
  const rx = x === null ? base.x : xFactor ? base.x + x * W : x * W;
  const ry = y === null ? base.y : yFactor ? base.y + y * H : y * H;
  const rw = w === null ? base.w : attr(kid(m, "wMode"), "val") === "edge" ? w * W - rx : w * W;
  const rh = h === null ? base.h : attr(kid(m, "hMode"), "val") === "edge" ? h * H - ry : h * H;
  return { x: rx, y: ry, w: Math.max(rw, 1), h: Math.max(rh, 1) };
}

export function buildLegend(el: Element | null, entries: LegendEntry[], area: Rect, W: number, H: number, ctx: Ctx, reverse: boolean): Legend | null {
  if (!el) return null;
  const hidden = new Set(kids(el, "legendEntry").filter((e) => toggle(kid(e, "delete"))).map((e) => numAttr(kid(e, "idx"), "val")));
  let list = entries.filter((e) => !hidden.has(e.idx));
  if (!list.length) return null;
  // c:legend specifies position via the legendPos child; cx:legend via the pos attribute
  const pos = (attr(kid(el, "legendPos"), "val") ?? attr(el, "pos") ?? "r") as "r" | "l" | "t" | "b" | "tr";
  const vertical = pos === "r" || pos === "l" || pos === "tr";
  if (reverse && vertical) list = list.slice().reverse();
  const st: TextStyle = txPrStyle(ctx.base, kid(el, "txPr"), ctx);
  const px = ptPx(st.size);
  const lh = lineHeight(st);
  const hasLine = list.some((e) => e.kind === "line");
  const keyW = hasLine ? Math.max(px * 1.9, 18) : px * 0.65;
  const keyGap = 4;
  const itemGap = px * 0.9;
  const pad = 4;
  const maxText = vertical ? Math.max(area.w * 0.33, 40) - keyW - keyGap - pad * 2 : area.w - pad * 2 - keyW - keyGap;
  const items: Item[] = list.map((entry) => {
    const block = layoutBlock(plainLines(ellipsize(entry.text, st, maxText), st));
    return { entry, block, w: keyW + keyGap + block.w };
  });

  // Layout: a single column when vertical; wraps into rows automatically when horizontal
  const rows: Item[][] = [];
  if (vertical) for (const it of items) rows.push([it]);
  else {
    let row: Item[] = [];
    let w = 0;
    for (const it of items) {
      if (row.length && w + itemGap + it.w > area.w - pad * 2) {
        rows.push(row);
        row = [];
        w = 0;
      }
      w += (row.length ? itemGap : 0) + it.w;
      row.push(it);
    }
    if (row.length) rows.push(row);
  }
  const rowH = (r: Item[]) => Math.max(lh, ...r.map((it) => it.block.h));
  const rowW = (r: Item[]) => r.reduce((sum, it, i) => sum + it.w + (i ? itemGap : 0), 0);
  let boxW = Math.max(...rows.map(rowW)) + pad * 2;
  let boxH = rows.reduce((sum, r) => sum + rowH(r), 0) + pad * 2;
  // Truncate vertical legends that are too tall
  const maxH = area.h;
  let shown = rows;
  if (boxH > maxH && vertical) {
    shown = [];
    let hh = pad * 2;
    for (const r of rows) {
      if (hh + rowH(r) > maxH) break;
      hh += rowH(r);
      shown.push(r);
    }
    boxH = hh;
  }
  boxW = Math.min(boxW, area.w);

  const manual = manualRect(kid(el, "layout"), W, H, null);
  const overlay = !!toggle(kid(el, "overlay")) || attr(el, "overlay") === "1";
  let box: Rect;
  const margin = 6;
  if (manual) box = { x: manual.x, y: manual.y, w: Math.max(boxW, manual.w), h: Math.max(boxH, manual.h) };
  else if (pos === "r") box = { x: area.x + area.w - boxW, y: area.y + (area.h - boxH) / 2, w: boxW, h: boxH };
  else if (pos === "l") box = { x: area.x, y: area.y + (area.h - boxH) / 2, w: boxW, h: boxH };
  else if (pos === "t") box = { x: area.x + (area.w - boxW) / 2, y: area.y, w: boxW, h: boxH };
  else if (pos === "tr") box = { x: area.x + area.w - boxW, y: area.y, w: boxW, h: boxH };
  else box = { x: area.x + (area.w - boxW) / 2, y: area.y + area.h - boxH, w: boxW, h: boxH };

  const take = manual || overlay ? null : pos === "b" ? { side: "b" as const, size: boxH + margin } : pos === "t" ? { side: "t" as const, size: boxH + margin } : pos === "l" ? { side: "l" as const, size: boxW + margin } : { side: "r" as const, size: boxW + margin };

  const spPr = kid(el, "spPr");
  return {
    take,
    draw(g) {
      const fill = readFill(spPr, ctx);
      const stroke = strokeOr(spPr, ctx.colors, null);
      if (fill || stroke) g.append(s("rect", { x: r2(box.x), y: r2(box.y), width: r2(box.w), height: r2(box.h), fill: fill ?? "none", ...strokeAttrs(stroke) }));
      let y = box.y + pad;
      for (const row of shown) {
        const h = rowH(row);
        const rw = rowW(row);
        // Horizontal legends center each row; vertical legends align left
        let x = vertical ? box.x + pad : box.x + (box.w - rw) / 2;
        for (const it of row) {
          drawKey(g, it.entry, x, y + h / 2, keyW, px);
          g.append(drawBlock(it.block, x + keyW + keyGap, y + h / 2, "start", "middle"));
          x += it.w + itemGap;
        }
        y += h;
      }
    },
  };
}

function drawKey(g: SVGElement, e: LegendEntry, x: number, cy: number, keyW: number, px: number) {
  if (e.kind === "line") {
    if (e.stroke && e.stroke.color) g.append(s("path", { d: `M${r2(x)},${r2(cy)}H${r2(x + keyW)}`, fill: "none", ...strokeAttrs({ ...e.stroke, width: Math.min(e.stroke.width, 4) }) }));
    if (e.marker) g.append(drawMarker({ ...e.marker, size: Math.min(e.marker.size, px * 0.9) }, x + keyW / 2, cy));
    return;
  }
  const size = px * 0.65;
  const kx = x + (keyW - size) / 2;
  g.append(s("rect", { x: r2(kx), y: r2(cy - size / 2), width: r2(size), height: r2(size), fill: e.fill ?? "none", ...strokeAttrs(e.stroke ? { ...e.stroke, width: Math.min(e.stroke.width, 1.5) } : null) }));
}
