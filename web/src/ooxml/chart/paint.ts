/**
 * Appearance of series and data points: fill, outline, line, marker (explicit settings → automatic colors).
 */

import { kid } from "../core/package";
import type { Group, Series } from "./model";
import { autoColor, autoSymbol, readFill, readMarker, strokeOr, type Env, type Marker, type Stroke } from "./style";
import type { TextStyle } from "./text";

/** Shared context for drawing one chart */
export interface Ctx extends Env {
  /** Default chart text style (chartSpace txPr) */
  base: TextStyle;
  blanks: "gap" | "zero" | "span";
  /** Background when the chart area has no fill (transparent in PowerPoint, white otherwise) */
  autoFill: string | null;
}

export interface ShapePaint {
  fill: string | null;
  stroke: Stroke | null;
}

/** Whether colors vary by data point (pie charts; bar charts with a single series, etc.) */
export function variesByPoint(g: Group): boolean {
  if (!g.varyColors) return false;
  if (g.kind === "pie" || g.kind === "doughnut") return true;
  const visible = g.series.length;
  return visible === 1 && (g.kind === "bar" || g.kind === "bubble" || g.kind === "scatter" || g.kind === "line" || g.kind === "radar" || g.kind === "area");
}

/** Automatic color of a series (or data point) */
export function autoFor(g: Group, s: Series, i: number | null, ctx: Ctx): string {
  if (i !== null && variesByPoint(g)) return autoColor(i, ctx.colors);
  return autoColor(s.idx, ctx.colors);
}

/** Fill and outline of area-like elements (bars, areas, slices, bubbles); i is the data point (null for the whole series) */
export function shapePaint(g: Group, s: Series, i: number | null, ctx: Ctx, defStroke: Stroke | null = null): ShapePaint {
  const pt = i !== null ? s.dPt.get(i) : undefined;
  const ptSp = kid(pt, "spPr");
  const auto = autoFor(g, s, i, ctx);
  // Data point → series → automatic (null means an explicit no fill)
  let fill = readFill(ptSp, ctx);
  if (fill === undefined) fill = readFill(s.spPr, ctx);
  if (fill === undefined) fill = auto;
  let stroke = strokeOr(s.spPr, ctx.colors, defStroke);
  if (ptSp) stroke = strokeOr(ptSp, ctx.colors, stroke);
  if (stroke && !stroke.color) stroke = { ...stroke, color: auto };
  return { fill, stroke };
}

/** Line of line-type series (line, scatter, radar); default 2.25pt */
export function linePaint(g: Group, s: Series, ctx: Ctx, defWidth = 3): Stroke | null {
  const auto = autoFor(g, s, null, ctx);
  return strokeOr(s.spPr, ctx.colors, { color: auto, width: defWidth, cap: "round" });
}

/** Line of a data point (dPt can override) */
export function pointLine(s: Series, i: number, base: Stroke | null, ctx: Ctx): Stroke | null {
  const pt = s.dPt.get(i);
  const sp = kid(pt, "spPr");
  return sp ? strokeOr(sp, ctx.colors, base) : base;
}

/**
 * Series marker: the symbol of c:marker takes precedence;
 * when unspecified, the chart type decides whether automatic markers are shown
 */
export function seriesMarker(g: Group, s: Series, ctx: Ctx, i: number | null = null): Marker | null {
  const pt = i !== null ? s.dPt.get(i) : undefined;
  const el = kid(pt, "marker") ?? s.marker;
  let def: string | null = null;
  if (g.kind === "scatter") def = autoSymbol(s.idx);
  else if (g.kind === "line") def = g.showMarker === false ? null : autoSymbol(s.idx);
  else if (g.kind === "radar") def = g.radarStyle === "marker" ? autoSymbol(s.idx) : null;
  if (g.kind === "line" && g.showMarker === false && !kid(el, "symbol")) return null;
  const color = autoFor(g, s, i, ctx);
  const line = linePaint(g, s, ctx);
  const col = line?.color || color;
  return readMarker(el, def, col, ctx);
}
