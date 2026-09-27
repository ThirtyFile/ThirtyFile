/**
 * Pie and doughnut charts: first slice angle, explosion, doughnut hole size, concentric rings for multiple series, and data labels on slices.
 */

import { kid, s } from "@/lib/office/ooxml";
import type { Rect } from "./axes";
import { drawLabel, labelBlock, labelSpec, type LabelSpec } from "./labels";
import type { LegendEntry } from "./legend";
import type { Group } from "./model";
import { shapePaint, type Ctx } from "./paint";
import { r2, strokeAttrs, type Stroke } from "./style";
import type { Block, HAlign, VAlign } from "./text";

const SLICE_BORDER: Stroke = { color: "#FFFFFF", width: 1 };

/** Pie chart legend: one entry per category */
export function pieLegend(g: Group, ctx: Ctx): LegendEntry[] {
  const s0 = g.series[0];
  if (!s0) return [];
  const n = Math.max(s0.vals.length, s0.cats.length);
  const out: LegendEntry[] = [];
  for (let i = 0; i < n; i++) {
    const p = shapePaint(g, s0, i, ctx, SLICE_BORDER);
    out.push({ text: s0.cats[i] ?? String(i + 1), kind: "box", fill: p.fill, stroke: null, idx: i });
  }
  return out;
}

interface PieLabel {
  spec: LabelSpec;
  block: Block;
  mid: number;
  r0: number;
  r1: number;
  explode: number;
  fill: string | null;
  span: number;
}

export function renderPie(g: Group, area: Rect, root: SVGElement, ctx: Ctx) {
  const doughnut = g.kind === "doughnut";
  const rings = doughnut ? g.series : g.series.slice(0, 1);
  if (!rings.length) return;
  const cx = area.x + area.w / 2;
  const cy = area.y + area.h / 2;
  let R = Math.max(Math.min(area.w, area.h) / 2 - 4, 4);

  // Build labels first; outside labels need reserved space
  const pending: { ring: number; i: number; spec: LabelSpec; block: Block }[] = [];
  rings.forEach((ser, k) => {
    const total = ser.vals.reduce<number>((a, v) => a + Math.abs(v ?? 0), 0);
    ser.vals.forEach((v, i) => {
      if (v === null) return;
      const spec = labelSpec(g, ser, i);
      if (!spec) return;
      const block = labelBlock(spec, ser, i, { value: v, percent: total ? Math.abs(v) / total : 0 }, ctx);
      if (block) pending.push({ ring: k, i, spec, block });
    });
  });
  const outside = !doughnut && pending.some((p) => p.spec.pos === "outEnd" || p.spec.pos === "bestFit" || !p.spec.pos);
  if (outside) {
    const lw = Math.max(0, ...pending.map((p) => p.block.w));
    const lh = Math.max(0, ...pending.map((p) => p.block.h));
    R = Math.max(Math.min(area.w / 2 - lw - 8, area.h / 2 - lh - 8), R * 0.55);
  }
  let maxExpl = 0;
  for (const ser of rings) {
    maxExpl = Math.max(maxExpl, ser.explosion);
    for (const p of ser.dPt.values()) maxExpl = Math.max(maxExpl, Number(kid(p, "explosion")?.getAttribute("val") ?? 0));
  }
  if (!doughnut && maxExpl > 0) R /= 1 + Math.min(maxExpl, 400) / 100;
  const hole = doughnut ? (Math.min(Math.max(g.holeSize, 10), 90) / 100) * R : 0;
  const ringW = (R - hole) / rings.length;
  const start = (g.firstSliceAng * Math.PI) / 180;
  const layer = s("g");
  const labels: PieLabel[] = [];

  rings.forEach((ser, k) => {
    const total = ser.vals.reduce<number>((a, v) => a + Math.abs(v ?? 0), 0);
    if (!total) return;
    // The first series of a doughnut chart is the innermost ring
    const r0 = hole + ringW * k;
    const r1 = r0 + ringW;
    let ang = start;
    ser.vals.forEach((v, i) => {
      if (v === null || v === 0) return;
      const span = (Math.abs(v) / total) * Math.PI * 2;
      const a0 = ang;
      const a1 = ang + span;
      ang = a1;
      const mid = (a0 + a1) / 2;
      const pt = ser.dPt.get(i);
      const expl = Number(kid(pt, "explosion")?.getAttribute("val") ?? ser.explosion) || 0;
      const off = doughnut ? (k === rings.length - 1 ? (expl / 100) * ringW : 0) : (expl / 100) * R;
      const dx = Math.sin(mid) * off;
      const dy = -Math.cos(mid) * off;
      const paint = shapePaint(g, ser, i, ctx, SLICE_BORDER);
      layer.append(s("path", { d: slicePath(cx + dx, cy + dy, r0, r1, a0, a1), fill: paint.fill ?? "none", ...strokeAttrs(paint.stroke), "stroke-linejoin": "round" }));
      const lbl = pending.find((p) => p.ring === k && p.i === i);
      if (lbl) labels.push({ spec: lbl.spec, block: lbl.block, mid, r0, r1, explode: off, fill: paint.fill, span });
    });
  });
  root.append(layer);

  const lg = s("g");
  for (const l of labels) {
    const pos = l.spec.pos ?? "bestFit";
    const ringMid = (l.r0 + l.r1) / 2;
    let r: number;
    let inside = true;
    if (doughnut) r = ringMid;
    else if (pos === "ctr") r = l.r1 * 0.5;
    else if (pos === "inEnd") r = l.r1 * 0.78;
    else if (pos === "inBase") r = l.r1 * 0.3;
    else if (pos === "outEnd") {
      r = l.r1 + 6;
      inside = false;
    } else {
      // bestFit: inside the slice if it fits, otherwise outside
      r = l.r1 * 0.62;
      const arc = l.span * r;
      if (l.block.w > arc * 1.1 && l.span < Math.PI || l.block.h > l.r1 * 0.5) {
        r = l.r1 + 6;
        inside = false;
      }
    }
    r += l.explode;
    const sin = Math.sin(l.mid);
    const cos = -Math.cos(l.mid);
    const x = cx + sin * r;
    const y = cy + cos * r;
    let h: HAlign = "middle";
    let v: VAlign = "middle";
    if (!inside) {
      h = sin > 0.15 ? "start" : sin < -0.15 ? "end" : "middle";
      v = cos > 0.5 ? "top" : cos < -0.5 ? "bottom" : "middle";
    }
    drawLabel(lg, l.spec, l.block, x, y, h, v, ctx, l.fill);
  }
  root.append(lg);
}

/** Slice (a ring segment when r0 > 0); angles clockwise from 12 o'clock */
function slicePath(cx: number, cy: number, r0: number, r1: number, a0: number, a1: number): string {
  const pt = (r: number, a: number) => `${r2(cx + Math.sin(a) * r)},${r2(cy - Math.cos(a) * r)}`;
  if (a1 - a0 >= Math.PI * 2 - 1e-6) {
    // Full circle: composed of two semicircles
    const outer = `M${pt(r1, 0)}A${r2(r1)},${r2(r1)} 0 1 1 ${pt(r1, Math.PI)}A${r2(r1)},${r2(r1)} 0 1 1 ${pt(r1, 0)}Z`;
    if (r0 <= 0) return outer;
    return `${outer}M${pt(r0, 0)}A${r2(r0)},${r2(r0)} 0 1 0 ${pt(r0, Math.PI)}A${r2(r0)},${r2(r0)} 0 1 0 ${pt(r0, 0)}Z`;
  }
  const large = a1 - a0 > Math.PI ? 1 : 0;
  if (r0 <= 0) return `M${r2(cx)},${r2(cy)}L${pt(r1, a0)}A${r2(r1)},${r2(r1)} 0 ${large} 1 ${pt(r1, a1)}Z`;
  return `M${pt(r1, a0)}A${r2(r1)},${r2(r1)} 0 ${large} 1 ${pt(r1, a1)}L${pt(r0, a1)}A${r2(r0)},${r2(r0)} 0 ${large} 0 ${pt(r0, a0)}Z`;
}
