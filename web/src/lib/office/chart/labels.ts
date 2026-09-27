/**
 * Data labels (c:dLbls/c:dLbl): each setting resolved in data point → series → chart group order, then text assembled and drawn.
 */

import { attr, kid, kids, numAttr, s, toggle } from "@/lib/office/ooxml";
import { formatValue } from "@/lib/sheet/format";
import type { Group, Series } from "./model";
import type { Ctx } from "./paint";
import { readFill, r2, strokeAttrs, strokeOr } from "./style";
import { drawBlock, layoutBlock, plainLines, richLines, txPrStyle, type Block, type HAlign, type TextStyle, type VAlign } from "./text";

export interface LabelSpec {
  showVal: boolean;
  showCat: boolean;
  showSer: boolean;
  showPct: boolean;
  showSize: boolean;
  showKey: boolean;
  sep: string | null;
  numFmt: string | null;
  pos: string | null;
  txPr: Element | null;
  spPr: Element | null;
  rich: Element | null;
}

const FLAGS: [keyof LabelSpec, string][] = [
  ["showVal", "showVal"],
  ["showCat", "showCatName"],
  ["showSer", "showSerName"],
  ["showPct", "showPercent"],
  ["showSize", "showBubbleSize"],
  ["showKey", "showLegendKey"],
];

/** Label settings for data point i; returns null when not shown */
export function labelSpec(g: Group, ser: Series, i: number): LabelSpec | null {
  const sd = ser.dLbls;
  const one = kids(sd, "dLbl").find((d) => numAttr(kid(d, "idx"), "val") === i) ?? null;
  const chain = [one, sd, g.dLbls].filter((x): x is Element => !!x);
  if (!chain.length) return null;
  for (const el of chain) {
    const del = kid(el, "delete");
    if (del) {
      if (toggle(del)) return null;
      break;
    }
  }
  const pick = (name: string) => {
    for (const el of chain) {
      const c = kid(el, name);
      if (c) return c;
    }
    return null;
  };
  const spec: LabelSpec = {
    showVal: false,
    showCat: false,
    showSer: false,
    showPct: false,
    showSize: false,
    showKey: false,
    sep: pick("separator")?.textContent ?? null,
    numFmt: attr(pick("numFmt"), "formatCode"),
    pos: attr(pick("dLblPos"), "val"),
    txPr: pick("txPr"),
    spPr: pick("spPr"),
    rich: kid(kid(one, "tx"), "rich"),
  };
  for (const [key, tag] of FLAGS) (spec[key] as boolean) = !!toggle(pick(tag));
  if (!spec.showVal && !spec.showCat && !spec.showSer && !spec.showPct && !spec.showSize && !spec.rich) return null;
  return spec;
}

/** Default label text style: 9pt, dark gray */
export function labelStyle(spec: LabelSpec, ctx: Ctx): TextStyle {
  return txPrStyle({ ...ctx.base, color: ctx.base.color }, spec.txPr, ctx);
}

/** Assemble label text: series name, category name, value, percentage, bubble size */
export function labelBlock(spec: LabelSpec, ser: Series, i: number, opts: { value: number | null; percent?: number; size?: number | null; cat?: string }, ctx: Ctx): Block | null {
  const st = labelStyle(spec, ctx);
  if (spec.rich) return layoutBlock(richLines(spec.rich, st, ctx));
  const parts: string[] = [];
  if (spec.showSer) parts.push(ser.name);
  if (spec.showCat) parts.push(opts.cat ?? ser.cats[i] ?? String(i + 1));
  const fmt = spec.numFmt && spec.numFmt !== "General" ? spec.numFmt : ser.format;
  if (spec.showVal && opts.value !== null) parts.push(formatValue(opts.value, fmt || "General"));
  if (spec.showPct && opts.percent !== undefined) parts.push(formatValue(opts.percent, spec.numFmt && /%/.test(spec.numFmt) ? spec.numFmt : "0%"));
  if (spec.showSize && opts.size !== undefined && opts.size !== null) parts.push(formatValue(opts.size, "General"));
  if (!parts.length) return null;
  const sep = spec.sep ?? ", ";
  return layoutBlock(plainLines(parts.join(sep), st));
}

/**
 * Draw a label: (x, y) is the anchor. Background and outline come from the label's spPr; with showKey a legend key is added on the left
 */
export function drawLabel(g: SVGElement, spec: LabelSpec, block: Block, x: number, y: number, h: HAlign, v: VAlign, ctx: Ctx, keyFill?: string | null) {
  const fill = readFill(spec.spPr, ctx);
  const stroke = strokeOr(spec.spPr, ctx.colors, null);
  const pad = fill || stroke ? 2 : 0;
  const key = spec.showKey && keyFill ? block.h / (block.lines.length || 1) * 0.6 : 0;
  const w = block.w + (key ? key + 3 : 0);
  const left = h === "start" ? x : h === "middle" ? x - w / 2 : x - w;
  const top = v === "top" ? y : v === "middle" ? y - block.h / 2 : y - block.h;
  if (fill || stroke) g.append(s("rect", { x: r2(left - pad), y: r2(top - pad), width: r2(w + pad * 2), height: r2(block.h + pad * 2), fill: fill ?? "none", ...strokeAttrs(stroke) }));
  if (key) g.append(s("rect", { x: r2(left), y: r2(top + (block.h - key) / 2), width: r2(key), height: r2(key), fill: keyFill! }));
  g.append(drawBlock(block, key ? left + key + 3 + (h === "start" ? 0 : h === "middle" ? block.w / 2 : block.w) : x, y, h, v));
}
