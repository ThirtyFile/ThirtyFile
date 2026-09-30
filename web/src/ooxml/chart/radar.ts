/**
 * Radar charts: radial category axes, concentric polygon gridlines, value ticks, and line/marker/filled styles.
 */

import { attr, kid, s } from "../core/package";
import type { Rect } from "./axes";
import { drawLabel, labelBlock, labelSpec } from "./labels";
import type { ChartModel, Group } from "./model";
import { linePaint, seriesMarker, shapePaint, type Ctx } from "./paint";
import { fraction, valueScale } from "./scale";
import { drawMarker, r2, strokeAttrs, strokeOr, type Stroke } from "./style";
import { drawBlock, ellipsize, lineHeight, textBlock, txPrStyle, type HAlign, type VAlign } from "./text";
import { formatValue } from "../core/numfmt";

export function renderRadar(model: ChartModel, groups: Group[], area: Rect, root: SVGElement, ctx: Ctx) {
  const g0 = groups[0];
  const catDef = model.axes.get(g0.axIds[0]);
  const valDef = model.axes.get(g0.axIds[1]);
  const series = groups.flatMap((g) => g.series.map((ser) => ({ g, ser })));
  const n = Math.max(1, ...series.map((x) => Math.max(x.ser.vals.length, x.ser.cats.length)));
  const labelSrc = series.find((x) => x.ser.cats.length)?.ser;
  const cats = Array.from({ length: n }, (_, i) => labelSrc?.cats[i] ?? String(i + 1));
  const catStyle = txPrStyle(ctx.base, kid(catDef?.el, "txPr"), ctx);
  const valStyle = txPrStyle(ctx.base, kid(valDef?.el, "txPr"), ctx);
  const showCats = !catDef?.deleted && attr(kid(catDef?.el, "tickLblPos"), "val") !== "none";
  const showVals = !!valDef && !valDef.deleted && attr(kid(valDef.el, "tickLblPos"), "val") !== "none";

  const maxLbl = area.w * 0.25;
  const blocks = showCats ? cats.map((c) => textBlock(ellipsize(c, catStyle, maxLbl), catStyle)) : [];
  const lw = Math.max(0, ...blocks.map((b) => b.w));
  const lh = showCats ? lineHeight(catStyle) : 0;
  const cx = area.x + area.w / 2;
  const cy = area.y + area.h / 2;
  const R = Math.max(10, Math.min(area.w / 2 - lw - 8, area.h / 2 - lh - 8));

  let lo = Infinity;
  let hi = -Infinity;
  for (const { ser } of series)
    for (const v of ser.vals)
      if (v !== null) {
        lo = Math.min(lo, v);
        hi = Math.max(hi, v);
      }
  if (!Number.isFinite(lo)) {
    lo = 0;
    hi = 1;
  }
  const sc = valueScale(
    { lo, hi, min: valDef?.min ?? null, max: valDef?.max ?? null, major: valDef?.majorUnit ?? null, minor: valDef?.minorUnit ?? null, logBase: null },
    Math.max(2, Math.floor(R / Math.max(lineHeight(valStyle) * 1.6, 14))),
  );
  const rev = !!catDef?.reversed;
  const angle = (i: number) => ((rev ? -1 : 1) * i * Math.PI * 2) / n;
  const at = (i: number, v: number): [number, number] => {
    const r = Math.max(0, fraction(sc, v)) * R;
    return [cx + Math.sin(angle(i)) * r, cy - Math.cos(angle(i)) * r];
  };

  // Gridlines: value-axis major gridlines are concentric polygons; category axis gridlines are radial lines
  const grid = s("g");
  const valGrid = kid(valDef?.el, "majorGridlines");
  const gridSt = valGrid ? strokeOr(kid(valGrid, "spPr"), ctx.colors, { color: "#D9D9D9", width: 1 }) : null;
  if (gridSt)
    for (const t of sc.ticks) {
      if (fraction(sc, t) <= 0) continue;
      grid.append(s("polygon", { points: Array.from({ length: n }, (_, i) => at(i, t).map(r2).join(",")).join(" "), fill: "none", ...strokeAttrs(gridSt) }));
    }
  const catLine: Stroke | null = catDef && !catDef.deleted ? strokeOr(kid(catDef.el, "spPr"), ctx.colors, { color: "#D9D9D9", width: 1 }) : null;
  const catGrid = kid(catDef?.el, "majorGridlines");
  const spokeSt = catGrid ? strokeOr(kid(catGrid, "spPr"), ctx.colors, { color: "#D9D9D9", width: 1 }) : catLine;
  if (spokeSt) {
    let d = "";
    for (let i = 0; i < n; i++) {
      const [x, y] = at(i, sc.max);
      d += `M${r2(cx)},${r2(cy)}L${r2(x)},${r2(y)}`;
    }
    grid.append(s("path", { d, fill: "none", ...strokeAttrs(spokeSt) }));
  }
  root.append(grid);

  // Series
  const layer = s("g");
  const labels = s("g");
  for (const { g, ser } of series) {
    const pts = Array.from({ length: n }, (_, i) => {
      const v = ser.vals[i];
      return v === null || v === undefined ? null : at(i, v);
    });
    const valid = pts.filter((p): p is [number, number] => !!p);
    if (!valid.length) continue;
    const poly = valid.map((p) => p.map(r2).join(",")).join(" ");
    if (g.radarStyle === "filled") {
      const p = shapePaint(g, ser, null, ctx);
      layer.append(s("polygon", { points: poly, fill: p.fill ?? "none", ...strokeAttrs(p.stroke), "stroke-linejoin": "round" }));
    } else {
      const st = linePaint(g, ser, ctx, 2.5);
      if (st && st.color) layer.append(s("polygon", { points: poly, fill: "none", ...strokeAttrs(st), "stroke-linejoin": "round" }));
    }
    pts.forEach((p, i) => {
      if (!p) return;
      const m = seriesMarker(g, ser, ctx, ser.dPt.has(i) ? i : null);
      if (m && g.radarStyle !== "filled") layer.append(drawMarker(m, p[0], p[1]));
      const spec = labelSpec(g, ser, i);
      const v = ser.vals[i];
      if (!spec || v === null || v === undefined) return;
      const blk = labelBlock(spec, ser, i, { value: v }, ctx);
      if (blk) drawLabel(labels, spec, blk, p[0], p[1] - 4, "middle", "bottom", ctx, null);
    });
  }
  root.append(layer);

  // Value ticks: along the first radial line (12 o'clock direction)
  if (showVals && valDef) {
    const axis = s("g");
    const vLine = strokeOr(kid(valDef.el, "spPr"), ctx.colors, { color: "#BFBFBF", width: 1 });
    if (vLine) axis.append(s("path", { d: `M${r2(cx)},${r2(cy)}V${r2(cy - R)}`, fill: "none", ...strokeAttrs(vLine) }));
    const fmt = valDef.numFmt && !valDef.sourceLinked ? valDef.numFmt : (series[0]?.ser.format ?? "General");
    for (const t of sc.ticks) {
      const y = cy - fraction(sc, t) * R;
      axis.append(drawBlock(textBlock(formatValue(t, fmt || "General"), valStyle), cx - 4, y, "end", "middle"));
    }
    root.append(axis);
  }
  // Category labels
  if (showCats) {
    const lg = s("g");
    blocks.forEach((b, i) => {
      const a = angle(i);
      const sin = Math.sin(a);
      const cos = -Math.cos(a);
      const x = cx + sin * (R + 6);
      const y = cy + cos * (R + 6);
      const h: HAlign = sin > 0.1 ? "start" : sin < -0.1 ? "end" : "middle";
      const v: VAlign = cos > 0.1 ? "top" : cos < -0.1 ? "bottom" : "middle";
      lg.append(drawBlock(b, x, y, h, v));
    });
    root.append(lg);
  }
  root.append(labels);
}
