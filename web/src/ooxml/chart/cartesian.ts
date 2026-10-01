/**
 * Cartesian charts: column/bar, line, area, stock, scatter, bubble (combinable, including secondary axes).
 */

import { attr, kid, s, toggle } from "../core/package";
import { baseValue, drawAxes, drawGrid, layoutFrame, type AxisSpec, type Frame, type PlacedAxis, type Rect, type Side } from "./axes";
import { drawErrBars, drawTrendlines } from "./extras";
import { drawLabel, labelBlock, labelSpec, type LabelSpec } from "./labels";
import type { LegendEntry } from "./legend";
import { numOf, type AxisDef, type ChartModel, type Group, type Series } from "./model";
import { autoFor, linePaint, pointLine, seriesMarker, shapePaint, variesByPoint, type Ctx } from "./paint";
import { DISP_UNITS } from "./scale";
import { drawMarker, readFill, r2, strokeAttrs, strokeOr, type Stroke } from "./style";
import { bodyRotation, drawBlock, ellipsize, layoutBlock, lineHeight, measure, plainLines, textBlock, txPrStyle, type Block, type HAlign, type TextStyle, type VAlign } from "./text";
import { formatValue } from "../core/numfmt";
import { titleBlock } from "./title";

const isXY = (g: Group) => g.kind === "scatter" || g.kind === "bubble";
const stacked = (g: Group) => (g.grouping === "stacked" || g.grouping === "percentStacked") && (g.kind === "bar" || g.kind === "line" || g.kind === "area");
const percent = (g: Group) => g.grouping === "percentStacked" && (g.kind === "bar" || g.kind === "line" || g.kind === "area");

/** Number of categories in a group */
function catCount(g: Group): number {
  return Math.max(0, ...g.series.map((s) => Math.max(s.vals.length, s.cats.length)));
}

/** Range of values in a group (accounting for stacking and percent) */
function valueRange(g: Group): { lo: number; hi: number } {
  let lo = Infinity;
  let hi = -Infinity;
  const add = (v: number) => {
    if (v < lo) lo = v;
    if (v > hi) hi = v;
  };
  if (stacked(g)) {
    const n = catCount(g);
    for (let i = 0; i < n; i++) {
      let pos = 0;
      let neg = 0;
      let total = 0;
      for (const s of g.series) {
        const v = s.vals[i];
        if (v === null || v === undefined) continue;
        total += Math.abs(v);
        if (v >= 0) pos += v;
        else neg += v;
      }
      if (percent(g)) {
        if (total) {
          add(pos / total);
          add(neg / total);
        }
      } else {
        add(pos);
        add(neg);
      }
    }
  } else {
    for (const s of g.series) for (const v of s.vals) if (v !== null) add(v);
  }
  return { lo, hi };
}

function xRange(g: Group): { lo: number; hi: number } {
  let lo = Infinity;
  let hi = -Infinity;
  for (const s of g.series)
    s.xs?.forEach((x, i) => {
      if (x === null || s.vals[i] === null || s.vals[i] === undefined) return;
      lo = Math.min(lo, x);
      hi = Math.max(hi, x);
    });
  return { lo, hi };
}

const DEFAULT_AXIS_LINE: Stroke = { color: "#BFBFBF", width: 1 };
const DEFAULT_GRID: Stroke = { color: "#D9D9D9", width: 1 };

function tickMark(el: Element | null): AxisSpec["majorTick"] | null {
  const v = attr(el, "val");
  return v === "none" || v === "in" || v === "out" || v === "cross" ? v : null;
}

/** Build an AxisSpec from an AxisDef and the groups that use it */
function makeSpec(def: AxisDef | null, id: string, role: "cat" | "val", users: Group[], asX: boolean, model: ChartModel, ctx: Ctx, horizontal: boolean): AxisSpec {
  const el = def?.el ?? null;
  // Orientation is determined by chart type (a bar chart's category axis is vertical); axPos only decides the side
  const pos = def?.pos ?? null;
  const side: Side = pos && (pos === "b" || pos === "t") === horizontal ? pos : horizontal ? (pos === "r" ? "t" : "b") : pos === "t" ? "r" : "l";
  const text = txPrStyle(ctx.base, kid(el, "txPr"), ctx);
  const firstSeries = users.flatMap((g) => g.series)[0];
  let format = "General";
  let input: AxisSpec["input"] = null;
  let dispUnit = 1;
  let dispLabel: Block | null = null;
  if (role === "val") {
    let lo = Infinity;
    let hi = -Infinity;
    let pct = false;
    for (const g of users) {
      const r = asX ? xRange(g) : valueRange(g);
      lo = Math.min(lo, r.lo);
      hi = Math.max(hi, r.hi);
      if (!asX && percent(g)) pct = true;
    }
    if (!Number.isFinite(lo)) {
      lo = 0;
      hi = 1;
    }
    input = { lo, hi, min: def?.min ?? null, max: def?.max ?? null, major: def?.majorUnit ?? null, minor: def?.minorUnit ?? null, logBase: def?.logBase ?? null, percent: pct };
    const src = firstSeries ? (asX ? firstSeries.xFormat : firstSeries.format) : "General";
    format = def && !def.sourceLinked && def.numFmt ? def.numFmt : pct ? "0%" : src || "General";
    if (pct && (format === "General" || !format)) format = "0%";
    const du = kid(el, "dispUnits");
    if (du) {
      const built = kid(du, "builtInUnit")?.getAttribute("val");
      dispUnit = built ? (DISP_UNITS[built] ?? 1) : (numOf(du, "custUnit") ?? 1);
      const lbl = kid(du, "dispUnitsLbl");
      if (lbl && dispUnit !== 1) {
        const custom = titleBlock(lbl, text, ctx, null);
        // Without custom text, shown as "×1,000" (avoids locale-dependent text)
        dispLabel = custom ?? layoutBlock(plainLines(`×${dispUnit.toLocaleString("en-US")}`, text));
      }
    }
  }
  const count = role === "cat" ? Math.max(1, ...users.map(catCount)) : 0;
  const labelSrc = users.flatMap((g) => g.series).find((s) => s.cats.length);
  // When the category axis has a custom number format (not sourceLinked), numeric categories (e.g. dates) are reformatted with the axis format
  const axisFmt = def && !def.sourceLinked && def.numFmt && def.numFmt !== "General" ? def.numFmt : null;
  const catLabel = (i: number) => {
    const n = labelSrc?.catNums?.[i];
    if (axisFmt && n !== null && n !== undefined) return formatValue(n, axisFmt);
    return labelSrc?.cats[i] ?? String(i + 1);
  };
  const labels = role === "cat" ? Array.from({ length: count }, (_, i) => catLabel(i)) : [];
  const crossDef = def?.crossAx ? model.axes.get(def.crossAx) : undefined;
  const cb = crossDef?.crossBetween ?? null;
  const between = cb ? cb === "between" : !users.every((g) => g.kind === "area");
  const lineSp = kid(el, "spPr");
  const mg = kid(el, "majorGridlines");
  const ng = kid(el, "minorGridlines");
  const titleEl = kid(el, "title");
  const titleStyle: TextStyle = { ...text, size: Math.max(text.size, ctx.base.size), bold: true };
  const title = titleEl ? titleBlock(titleEl, titleStyle, ctx, null) : null;
  const tRot = titleEl ? (bodyRotation(kid(kid(titleEl, "tx"), "rich")) ?? bodyRotation(kid(titleEl, "txPr"))) : null;
  const lblPos = kid(el, "tickLblPos")?.getAttribute("val");
  const noMulti = !!toggle(kid(el, "noMultiLvlLbl"));
  return {
    id,
    role,
    horizontal,
    side,
    deleted: def ? def.deleted : false,
    reversed: def?.reversed ?? false,
    labels,
    count,
    between,
    lblSkip: numOf(el, "tickLblSkip"),
    markSkip: numOf(el, "tickMarkSkip"),
    levels: role === "cat" && !noMulti ? (labelSrc?.catLevels ?? []) : [],
    input,
    format,
    dispUnit,
    dispLabel,
    crossAx: def?.crossAx ?? null,
    crosses: def?.crosses ?? "autoZero",
    text,
    rot: bodyRotation(kid(el, "txPr")),
    line: strokeOr(lineSp, ctx.colors, DEFAULT_AXIS_LINE),
    majorGrid: mg ? strokeOr(kid(mg, "spPr"), ctx.colors, DEFAULT_GRID) : null,
    minorGrid: ng ? strokeOr(kid(ng, "spPr"), ctx.colors, { ...DEFAULT_GRID, color: "#F2F2F2" }) : null,
    majorTick: tickMark(kid(el, "majorTickMark")) ?? "out",
    minorTick: tickMark(kid(el, "minorTickMark")) ?? "none",
    lblPos: lblPos === "low" || lblPos === "high" || lblPos === "none" ? lblPos : "nextTo",
    title,
    titleRot: tRot ?? (horizontal ? 0 : -90),
  };
}

interface Binding {
  group: Group;
  /** Category axis (X axis for XY charts) */
  c: string;
  /** Value axis (Y axis for XY charts) */
  v: string;
}

/** Build AxisSpecs for all axes and record which axes each group uses */
function buildSpecs(model: ChartModel, groups: Group[], ctx: Ctx): { specs: AxisSpec[]; bindings: Binding[] } {
  const bindings: Binding[] = [];
  const horizontalBars = groups.some((g) => g.kind === "bar" && g.barDir === "bar");
  groups.forEach((g) => {
    let [c, v] = g.axIds;
    // Groups lacking axis definitions share the primary axes
    if (!c || !model.axes.has(c)) c = bindings[0]?.c ?? "auto-c";
    if (!v || !model.axes.has(v)) v = bindings[0]?.v ?? "auto-v";
    bindings.push({ group: g, c, v });
  });
  const specs: AxisSpec[] = [];
  const seen = new Set<string>();
  for (const b of bindings) {
    for (const [id, role] of [
      [b.c, isXY(b.group) ? "x" : "cat"],
      [b.v, "val"],
    ] as const) {
      if (seen.has(id)) continue;
      seen.add(id);
      const users = bindings.filter((x) => (role === "val" ? x.v === id : x.c === id)).map((x) => x.group);
      const def = model.axes.get(id) ?? null;
      const barH = users.some((g) => g.kind === "bar" && g.barDir === "bar") || (horizontalBars && !def);
      const horizontal = role === "x" ? true : role === "val" ? barH : !barH;
      // A category axis defined as valAx (e.g. XY charts) is treated as a value axis
      const r = role === "x" ? "val" : def?.type === "val" && role === "cat" ? "val" : role;
      const spec = makeSpec(def, id, r, users, role === "x" || (r === "val" && role === "cat"), model, ctx, horizontal);
      if (!def) spec.crossAx = role === "val" ? b.c : b.v;
      specs.push(spec);
    }
  }
  return { specs, bindings };
}

/** Legend entries for cartesian charts */
export function cartesianLegend(groups: Group[], ctx: Ctx): LegendEntry[] {
  const out: LegendEntry[] = [];
  for (const g of orderForDraw(groups)) {
    if (variesByPoint(g) && g.series.length === 1 && g.kind === "bar") {
      const s = g.series[0];
      const n = Math.max(s.vals.length, s.cats.length);
      for (let i = 0; i < n; i++) {
        const p = shapePaint(g, s, i, ctx);
        out.push({ text: s.cats[i] ?? String(i + 1), kind: "box", fill: p.fill, stroke: p.stroke, idx: i });
      }
      continue;
    }
    for (const s of g.series) out.push(seriesLegend(g, s, ctx));
  }
  return out;
}

export function seriesLegend(g: Group, s: Series, ctx: Ctx): LegendEntry {
  const lineLike = g.kind === "line" || g.kind === "scatter" || (g.kind === "radar" && g.radarStyle !== "filled") || g.kind === "stock";
  if (lineLike) {
    const stroke = g.kind === "stock" ? strokeOr(s.spPr, ctx.colors, null) : linePaint(g, s, ctx);
    return { text: s.name, kind: "line", stroke, marker: seriesMarker(g, s, ctx), idx: s.idx };
  }
  const p = shapePaint(g, s, null, ctx);
  return { text: s.name, kind: "box", fill: p.fill, stroke: p.stroke, idx: s.idx };
}

/** Drawing order: areas at the bottom, then bars, stock, lines, scatter and bubbles */
function orderForDraw(groups: Group[]): Group[] {
  const rank: Record<string, number> = { area: 0, surface: 0, bar: 1, stock: 2, line: 3, radar: 3, scatter: 4, bubble: 5 };
  return groups
    .map((g, i) => ({ g, i }))
    .sort((a, b) => (rank[a.g.kind] ?? 9) - (rank[b.g.kind] ?? 9) || a.i - b.i)
    .map((x) => x.g);
}

/** Cases where legend order is reversed: stacked column and stacked area charts (matching the stacking direction) */
export function legendReversed(groups: Group[]): boolean {
  return groups.length > 0 && groups.every((g) => (g.kind === "bar" && g.barDir === "col" && stacked(g)) || (g.kind === "area" && stacked(g)));
}

// ───────────── Drawing ─────────────

interface Layers {
  series: SVGElement;
  lines: SVGElement;
  labels: SVGElement;
}

export function renderCartesian(model: ChartModel, groups: Group[], area: Rect, inner: Rect | null, root: SVGElement, ctx: Ctx) {
  const { specs, bindings } = buildSpecs(model, groups, ctx);
  // Data table: replaces the horizontal category axis labels, placed below the plot area
  const dTable = kid(model.plotArea, "dTable");
  const catSpec = dTable ? (specs.find((sp) => sp.role === "cat" && sp.horizontal && !sp.deleted) ?? specs.find((sp) => sp.role === "cat" && sp.horizontal)) : undefined;
  const table = dTable && catSpec ? tableLayout(dTable, groups, catSpec, ctx) : null;
  if (table && catSpec) {
    // When the category axis is deleted the data table still shows, but without axis line and tick marks
    if (catSpec.deleted) Object.assign(catSpec, { deleted: false, line: null, majorTick: "none", minorTick: "none", title: null });
    catSpec.lblPos = "none";
    catSpec.extraThick = table.h;
  }
  const frame = layoutFrame(area, specs, inner, table ? { l: table.nameW + 4 } : {});
  const plotSp = kid(model.plotArea, "spPr");
  const pl = frame.plot;
  // Plot area background
  const bgFill = readFill(plotSp, ctx) ?? null;
  const bgStroke = strokeOr(plotSp, ctx.colors, null);
  if (bgFill || bgStroke) root.append(s("rect", { x: r2(pl.x), y: r2(pl.y), width: r2(pl.w), height: r2(pl.h), fill: bgFill ?? "none", ...strokeAttrs(bgStroke) }));
  const grid = s("g");
  drawGrid(frame, grid);
  root.append(grid);

  const clipStrict = ctx.uid();
  const clipLoose = ctx.uid();
  const defs = ctx.defs;
  defs.append(s("clipPath", { id: clipStrict }, s("rect", { x: r2(pl.x - 0.5), y: r2(pl.y - 0.5), width: r2(pl.w + 1), height: r2(pl.h + 1) })));
  defs.append(s("clipPath", { id: clipLoose }, s("rect", { x: r2(pl.x - 10), y: r2(pl.y - 10), width: r2(pl.w + 20), height: r2(pl.h + 20) })));
  const layers: Layers = {
    series: s("g", { "clip-path": `url(#${clipStrict})` }),
    lines: s("g", { "clip-path": `url(#${clipLoose})` }),
    labels: s("g"),
  };
  const axesLayer = s("g");
  for (const g of orderForDraw(groups)) {
    const b = bindings.find((x) => x.group === g)!;
    const c = frame.axes.get(b.c);
    const v = frame.axes.get(b.v);
    if (!c || !v || !v.scale) continue;
    switch (g.kind) {
      case "bar":
        drawBars(g, c, v, ctx, layers);
        break;
      case "area":
        drawAreas(g, c, v, ctx, layers);
        break;
      case "line":
      case "stock":
        drawLines(g, c, v, ctx, layers);
        break;
      case "scatter":
      case "bubble":
        if (c.scale) drawXY(g, c, v, frame, ctx, layers);
        break;
    }
  }
  root.append(layers.series);
  // Axes are drawn above bars and areas, below lines
  drawAxes(frame, axesLayer);
  root.append(axesLayer, layers.lines, layers.labels);
  if (table && catSpec) drawTable(table, frame, frame.axes.get(catSpec.id)!, root, ctx);
}

// ───────────── Data table ─────────────

interface Table {
  el: Element;
  rows: { entry: LegendEntry; vals: string[] }[];
  style: TextStyle;
  rowH: number;
  nameW: number;
  h: number;
}

function tableLayout(el: Element, groups: Group[], cat: AxisSpec, ctx: Ctx): Table {
  const style = txPrStyle(ctx.base, kid(el, "txPr"), ctx);
  const rows = orderForDraw(groups).flatMap((g) =>
    g.series.map((ser) => ({
      entry: seriesLegend(g, ser, ctx),
      vals: Array.from({ length: cat.count }, (_, i) => (ser.vals[i] === null || ser.vals[i] === undefined ? "" : formatValue(ser.vals[i], ser.format || "General"))),
    })),
  );
  const rowH = lineHeight(style) + 4;
  const keys = toggle(kid(el, "showKeys")) !== false;
  const nameW = Math.max(0, ...rows.map((r) => measure(r.entry.text, style))) + (keys ? 20 : 8);
  return { el, rows, style, rowH, nameW, h: rowH * (rows.length + 1) };
}

function drawTable(t: Table, frame: Frame, c: PlacedAxis, root: SVGElement, ctx: Ctx) {
  const pl = frame.plot;
  const g = s("g");
  const top = pl.y + pl.h;
  const left = pl.x - t.nameW;
  const right = pl.x + pl.w;
  const bottom = top + t.h;
  const border = strokeOr(kid(t.el, "spPr"), ctx.colors, { color: "#BFBFBF", width: 1 });
  const on = (name: string) => toggle(kid(t.el, name)) !== false;
  const keys = on("showKeys");
  const n = c.spec.count;
  let d = "";
  // Horizontal lines, vertical lines, outline
  if (on("showHorzBorder")) for (let r = 1; r <= t.rows.length; r++) d += `M${r2(r === 1 ? pl.x : left)},${r2(top + r * t.rowH)}H${r2(right)}`;
  if (on("showVertBorder")) for (let i = 0; i <= n; i++) d += `M${r2(c.pos(i))},${r2(top)}V${r2(bottom)}`;
  if (on("showOutline")) d += `M${r2(pl.x)},${r2(top)}H${r2(right)}V${r2(bottom)}H${r2(left)}V${r2(top + t.rowH)}H${r2(pl.x)}Z`;
  if (d && border) g.append(s("path", { d, fill: "none", ...strokeAttrs(border), "shape-rendering": "crispEdges" }));
  const catStyle = c.spec.text;
  for (let i = 0; i < n; i++) g.append(drawBlock(textBlock(ellipsize(c.spec.labels[i] ?? "", catStyle, Math.abs(c.band) - 4), catStyle), c.center(i), top + t.rowH / 2, "middle", "middle"));
  t.rows.forEach((row, r) => {
    const y = top + (r + 1.5) * t.rowH;
    let x = left + 4;
    if (keys) {
      const e = row.entry;
      if (e.kind === "line") {
        if (e.stroke) g.append(s("path", { d: `M${r2(x)},${r2(y)}h10`, fill: "none", ...strokeAttrs({ ...e.stroke, width: Math.min(e.stroke.width, 2) }) }));
      } else g.append(s("rect", { x: r2(x + 2), y: r2(y - 3), width: 6, height: 6, fill: e.fill ?? "none" }));
      x += 12;
    }
    g.append(drawBlock(textBlock(ellipsize(row.entry.text, t.style, t.nameW - (x - left)), t.style), x, y, "start", "middle"));
    row.vals.forEach((v, i) => {
      if (v) g.append(drawBlock(textBlock(ellipsize(v, t.style, Math.abs(c.band) - 4), t.style), c.center(i), y, "middle", "middle"));
    });
  });
  root.append(g);
}

/** Category i, value v → coordinates */
function pt(c: PlacedAxis, v: PlacedAxis, i: number, val: number): [number, number] {
  return c.spec.horizontal ? [c.center(i), v.pos(val)] : [v.pos(val), c.center(i)];
}

// ───────────── Bars ─────────────

function drawBars(g: Group, c: PlacedAxis, v: PlacedAxis, ctx: Ctx, L: Layers) {
  const n = c.spec.count;
  const series = g.series;
  const k = Math.max(series.length, 1);
  const isStacked = stacked(g);
  const pct = percent(g);
  const band = Math.abs(c.band);
  const dir = Math.sign(c.b - c.a) || 1;
  const gap = Math.max(0, g.gapWidth) / 100;
  const ov = Math.min(1, Math.max(-1, g.overlap / 100));
  const barW = isStacked ? band / (1 + gap) : band / (gap + k - (k - 1) * ov);
  const step = isStacked ? 0 : barW * (1 - ov);
  const groupW = barW + step * (k - 1);
  const base = baseValue(v, c);
  const pos = new Array<number>(n).fill(0);
  const neg = new Array<number>(n).fill(0);
  const totals = new Array<number>(n).fill(0);
  if (pct) for (const x of series) x.vals.forEach((q, i) => q !== null && i < n && (totals[i] += Math.abs(q)));
  series.forEach((ser, j) => {
    const off = isStacked ? -barW / 2 : -groupW / 2 + j * step;
    for (let i = 0; i < n; i++) {
      const raw = ser.vals[i];
      if (raw === null || raw === undefined) continue;
      let val = raw;
      if (pct) val = totals[i] ? raw / totals[i] : 0;
      let from = base;
      let to = val;
      if (isStacked) {
        from = val >= 0 ? pos[i] : neg[i];
        to = from + val;
        if (val >= 0) pos[i] = to;
        else neg[i] = to;
      }
      const centre = c.center(i);
      const p0 = centre + dir * off;
      const p1 = centre + dir * (off + barW);
      const v0 = v.pos(from);
      const v1 = v.pos(to);
      const paint = shapePaint(g, ser, i, ctx);
      let fill = paint.fill;
      let stroke = paint.stroke;
      if (raw < 0 && ser.invertIfNegative && !kid(ser.dPt.get(i), "spPr")) {
        stroke = stroke ?? (fill ? { color: fill, width: 1 } : null);
        fill = "#FFFFFF";
      }
      const rect = c.spec.horizontal
        ? { x: Math.min(p0, p1), y: Math.min(v0, v1), w: Math.abs(p1 - p0), h: Math.abs(v1 - v0) }
        : { x: Math.min(v0, v1), y: Math.min(p0, p1), w: Math.abs(v1 - v0), h: Math.abs(p1 - p0) };
      L.series.append(s("rect", { x: r2(rect.x), y: r2(rect.y), width: r2(Math.max(rect.w, 0)), height: r2(Math.max(rect.h, 0)), fill: fill ?? "none", ...strokeAttrs(stroke) }));
      const spec = labelSpec(g, ser, i);
      if (spec) {
        const blk = labelBlock(spec, ser, i, { value: raw, percent: pct ? val : undefined }, ctx);
        if (blk) placeBarLabel(spec, blk, c.spec.horizontal, centre + dir * (off + barW / 2), v0, v1, isStacked, ctx, L, fill);
      }
    }
    // Error bars drawn at the center of the bar end
    const mid = (x: number) => c.center(Math.round(x) - 1) + dir * (off + barW / 2);
    drawErrBars(
      ser,
      ser.vals.map((_, i) => i + 1),
      (x, y) => (c.spec.horizontal ? [mid(x), v.pos(y)] : [v.pos(y), mid(x)]),
      "y",
      ctx,
      L.labels,
    );
  });
}

function placeBarLabel(spec: LabelSpec, blk: Block, colChart: boolean, mid: number, v0: number, v1: number, isStacked: boolean, ctx: Ctx, L: Layers, fill: string | null) {
  const pos = spec.pos ?? (isStacked ? "ctr" : "outEnd");
  const sgn = Math.sign(v1 - v0) || (colChart ? -1 : 1);
  const gap = 3;
  let at: number;
  let align: "near" | "far" | "mid";
  switch (pos) {
    case "inEnd":
      at = v1 - sgn * gap;
      align = "far";
      break;
    case "inBase":
      at = v0 + sgn * gap;
      align = "near";
      break;
    case "ctr":
      at = (v0 + v1) / 2;
      align = "mid";
      break;
    default:
      at = v1 + sgn * gap;
      align = "near";
  }
  // near: label extends in the sgn direction; far: extends in the opposite direction
  if (colChart) {
    const v: VAlign = align === "mid" ? "middle" : (align === "near") === sgn > 0 ? "top" : "bottom";
    drawLabel(L.labels, spec, blk, mid, at, "middle", v, ctx, fill);
  } else {
    const h: HAlign = align === "mid" ? "middle" : (align === "near") === sgn > 0 ? "start" : "end";
    drawLabel(L.labels, spec, blk, at, mid, h, "middle", ctx, fill);
  }
}

// ───────────── Area ─────────────

function drawAreas(g: Group, c: PlacedAxis, v: PlacedAxis, ctx: Ctx, L: Layers) {
  const n = c.spec.count;
  const isStacked = stacked(g);
  const pct = percent(g);
  const base = baseValue(v, c);
  const acc = new Array<number>(n).fill(0);
  const totals = new Array<number>(n).fill(0);
  if (pct) for (const x of g.series) x.vals.forEach((q, i) => q !== null && i < n && (totals[i] += Math.abs(q)));
  for (const ser of g.series) {
    const top: [number, number][] = [];
    const bottom: [number, number][] = [];
    const vals: number[] = [];
    for (let i = 0; i < n; i++) {
      let val = ser.vals[i] ?? 0;
      if (pct) val = totals[i] ? val / totals[i] : 0;
      const from = isStacked ? acc[i] : base;
      const to = isStacked ? acc[i] + val : val;
      if (isStacked) acc[i] = to;
      top.push(pt(c, v, i, to));
      bottom.push(pt(c, v, i, from));
      vals.push(to);
    }
    if (!top.length) continue;
    const d = `M${top.map((p) => `${r2(p[0])},${r2(p[1])}`).join("L")}L${bottom
      .reverse()
      .map((p) => `${r2(p[0])},${r2(p[1])}`)
      .join("L")}Z`;
    const paint = shapePaint(g, ser, null, ctx);
    L.series.append(s("path", { d, fill: paint.fill ?? "none", ...strokeAttrs(paint.stroke), "stroke-linejoin": "round" }));
    bottom.reverse();
    for (let i = 0; i < n; i++) {
      const spec = labelSpec(g, ser, i);
      const raw = ser.vals[i];
      if (!spec || raw === null || raw === undefined) continue;
      const blk = labelBlock(spec, ser, i, { value: raw, percent: pct ? (totals[i] ? raw / totals[i] : 0) : undefined }, ctx);
      if (!blk) continue;
      const [x, y] = [(top[i][0] + bottom[i][0]) / 2, (top[i][1] + bottom[i][1]) / 2];
      drawLabel(L.labels, spec, blk, x, y, "middle", "middle", ctx, paint.fill);
    }
  }
}

// ───────────── Line (including stock charts) ─────────────

/** Smooth curve: Catmull-Rom converted to cubic Bézier curves */
export function smoothPath(p: [number, number][]): string {
  if (p.length < 3) return linePath(p);
  let d = `M${r2(p[0][0])},${r2(p[0][1])}`;
  for (let i = 0; i < p.length - 1; i++) {
    const p0 = p[i - 1] ?? p[i];
    const p1 = p[i];
    const p2 = p[i + 1];
    const p3 = p[i + 2] ?? p2;
    const t = 1 / 6;
    d += `C${r2(p1[0] + (p2[0] - p0[0]) * t)},${r2(p1[1] + (p2[1] - p0[1]) * t)} ${r2(p2[0] - (p3[0] - p1[0]) * t)},${r2(p2[1] - (p3[1] - p1[1]) * t)} ${r2(p2[0])},${r2(p2[1])}`;
  }
  return d;
}

export function linePath(p: [number, number][]): string {
  return p.map((q, i) => `${i ? "L" : "M"}${r2(q[0])},${r2(q[1])}`).join("");
}

/** Split points into continuous segments according to dispBlanksAs */
function segments(points: ([number, number] | null)[], blanks: Ctx["blanks"]): [number, number][][] {
  const out: [number, number][][] = [];
  let cur: [number, number][] = [];
  for (const p of points) {
    if (p) cur.push(p);
    else if (blanks !== "span" && cur.length) {
      out.push(cur);
      cur = [];
    }
  }
  if (cur.length) out.push(cur);
  return out;
}

function drawLines(g: Group, c: PlacedAxis, v: PlacedAxis, ctx: Ctx, L: Layers) {
  const n = c.spec.count;
  const isStacked = stacked(g);
  const pct = percent(g);
  const acc = new Array<number>(n).fill(0);
  const totals = new Array<number>(n).fill(0);
  if (pct) for (const s of g.series) s.vals.forEach((x, i) => x !== null && i < n && (totals[i] += Math.abs(x)));
  const stock = g.kind === "stock";
  const all: (number | null)[][] = [];
  const pointsBySeries: ([number, number] | null)[][] = [];
  for (const s of g.series) {
    const vals: (number | null)[] = [];
    for (let i = 0; i < n; i++) {
      let val = s.vals[i] ?? null;
      if (val === null && ctx.blanks === "zero") val = 0;
      if (val !== null && pct) val = totals[i] ? val / totals[i] : 0;
      if (val !== null && isStacked) {
        acc[i] += val;
        val = acc[i];
      }
      vals.push(val);
    }
    all.push(vals);
    pointsBySeries.push(vals.map((val, i) => (val === null ? null : pt(c, v, i, val))));
  }

  // High-low lines, drop lines, up/down bars
  if (g.hiLowLines || stock) drawHiLow(g, c, v, all, ctx, L);
  if (g.dropLines) {
    const st = strokeOr(kid(g.dropLines, "spPr"), ctx.colors, { color: "#000000", width: 1 });
    const base = baseValue(v, c);
    let d = "";
    for (const pts of pointsBySeries)
      pts.forEach((p, i) => {
        if (!p) return;
        const q = pt(c, v, i, base);
        d += `M${r2(p[0])},${r2(p[1])}L${r2(q[0])},${r2(q[1])}`;
      });
    if (d) L.lines.append(s("path", { d, fill: "none", ...strokeAttrs(st) }));
  }
  if (g.upDownBars && all.length >= 2) drawUpDown(g, c, v, all, ctx, L);

  g.series.forEach((ser, j) => {
    const pts = pointsBySeries[j];
    const stroke = stock ? strokeOr(ser.spPr, ctx.colors, null) : linePaint(g, ser, ctx);
    const group = s("g");
    if (stroke && stroke.color) {
      const hasPointLines = [...ser.dPt.values()].some((p) => kid(kid(p, "spPr"), "ln"));
      if (hasPointLines) {
        // Per-data-point lines: drawn segment by segment
        for (let i = 1; i < pts.length; i++) {
          const a = pts[i - 1];
          const b = pts[i];
          if (!a || !b) continue;
          const st = pointLine(ser, i, stroke, ctx);
          if (st && st.color) group.append(s("path", { d: linePath([a, b]), fill: "none", ...strokeAttrs(st), "stroke-linejoin": "round" }));
        }
      } else
        for (const seg of segments(pts, ctx.blanks)) {
          if (seg.length < 2) continue;
          group.append(s("path", { d: ser.smooth ? smoothPath(seg) : linePath(seg), fill: "none", ...strokeAttrs(stroke), "stroke-linejoin": "round" }));
        }
    }
    const xs = ser.vals.map((_, i) => i + 1);
    const toPx = (x: number, y: number) => pt(c, v, x - 1, y);
    drawTrendlines(ser, xs, toPx, stroke?.color || autoFor(g, ser, null, ctx), ctx, group);
    drawErrBars(ser, xs, toPx, "y", ctx, group);
    // Markers
    for (let i = 0; i < pts.length; i++) {
      const p = pts[i];
      if (!p) continue;
      const m = seriesMarker(g, ser, ctx, ser.dPt.has(i) ? i : null);
      if (m) group.append(drawMarker(m, p[0], p[1]));
    }
    // Stock charts (high-low-close) without up/down bars show the close as a short horizontal tick
    if (stock && !g.upDownBars && g.series.length === 3 && j === 2 && !kid(ser.marker, "symbol")) {
      const st = { color: "#404040", width: 1.5 };
      let d = "";
      for (const p of pts) if (p) d += c.spec.horizontal ? `M${r2(p[0])},${r2(p[1])}h${r2(Math.abs(c.band) * 0.15)}` : `M${r2(p[0])},${r2(p[1])}v${r2(Math.abs(c.band) * 0.15)}`;
      if (d) group.append(s("path", { d, fill: "none", ...strokeAttrs(st) }));
    }
    L.lines.append(group);
    for (let i = 0; i < pts.length; i++) {
      const p = pts[i];
      const raw = ser.vals[i];
      if (!p || raw === null || raw === undefined) continue;
      const spec = labelSpec(g, ser, i);
      if (!spec) continue;
      const blk = labelBlock(spec, ser, i, { value: raw, percent: pct ? (totals[i] ? raw / totals[i] : 0) : undefined }, ctx);
      if (blk) pointLabel(spec, blk, p, spec.pos ?? "r", ctx, L, stroke?.color ?? null);
    }
  });
}

/** Label position for point data (top, bottom, left, right, center) */
function pointLabel(spec: LabelSpec, blk: Block, p: [number, number], pos: string, ctx: Ctx, L: Layers, key: string | null) {
  const gap = 5;
  switch (pos) {
    case "t":
      drawLabel(L.labels, spec, blk, p[0], p[1] - gap, "middle", "bottom", ctx, key);
      break;
    case "b":
      drawLabel(L.labels, spec, blk, p[0], p[1] + gap, "middle", "top", ctx, key);
      break;
    case "l":
      drawLabel(L.labels, spec, blk, p[0] - gap, p[1], "end", "middle", ctx, key);
      break;
    case "ctr":
      drawLabel(L.labels, spec, blk, p[0], p[1], "middle", "middle", ctx, key);
      break;
    default:
      drawLabel(L.labels, spec, blk, p[0] + gap, p[1], "start", "middle", ctx, key);
  }
}

function drawHiLow(g: Group, c: PlacedAxis, v: PlacedAxis, all: (number | null)[][], ctx: Ctx, L: Layers) {
  const st = strokeOr(kid(g.hiLowLines, "spPr"), ctx.colors, { color: "#404040", width: 1 });
  if (!st) return;
  let d = "";
  for (let i = 0; i < c.spec.count; i++) {
    const vals = all.map((a) => a[i]).filter((x): x is number => x !== null && x !== undefined);
    if (vals.length < 2) continue;
    const a = pt(c, v, i, Math.max(...vals));
    const b = pt(c, v, i, Math.min(...vals));
    d += `M${r2(a[0])},${r2(a[1])}L${r2(b[0])},${r2(b[1])}`;
  }
  if (d) L.series.append(s("path", { d, fill: "none", ...strokeAttrs(st) }));
}

/** Up/down bars: from the first series (open) to the last series (close) */
function drawUpDown(g: Group, c: PlacedAxis, v: PlacedAxis, all: (number | null)[][], ctx: Ctx, L: Layers) {
  const el = g.upDownBars!;
  const gap = (numOf(el, "gapWidth") ?? 150) / 100;
  const w = Math.abs(c.band) / (1 + gap);
  const upSp = kid(kid(el, "upBars"), "spPr");
  const downSp = kid(kid(el, "downBars"), "spPr");
  const open = all[0];
  const close = all[all.length - 1];
  const paintOf = (sp: Element | null, def: string) => {
    const fill = readFill(sp, ctx);
    return { fill: fill === undefined ? def : fill, stroke: strokeOr(sp, ctx.colors, { color: "#404040", width: 1 }) };
  };
  const up = paintOf(upSp, "#FFFFFF");
  const down = paintOf(downSp, "#404040");
  for (let i = 0; i < c.spec.count; i++) {
    const o = open[i];
    const cl = close[i];
    if (o === null || cl === null || o === undefined || cl === undefined) continue;
    const p = cl >= o ? up : down;
    const a = v.pos(o);
    const b = v.pos(cl);
    const mid = c.center(i);
    const rect = c.spec.horizontal ? { x: mid - w / 2, y: Math.min(a, b), w, h: Math.max(Math.abs(a - b), 1) } : { x: Math.min(a, b), y: mid - w / 2, w: Math.max(Math.abs(a - b), 1), h: w };
    L.series.append(s("rect", { x: r2(rect.x), y: r2(rect.y), width: r2(rect.w), height: r2(rect.h), fill: p.fill ?? "none", ...strokeAttrs(p.stroke) }));
  }
}

// ───────────── Scatter and bubble ─────────────

function drawXY(g: Group, xAx: PlacedAxis, yAx: PlacedAxis, frame: Frame, ctx: Ctx, L: Layers) {
  const toPx = (x: number, y: number): [number, number] => (xAx.spec.horizontal ? [xAx.pos(x), yAx.pos(y)] : [yAx.pos(y), xAx.pos(x)]);
  if (g.kind === "bubble") {
    const pl = frame.plot;
    let maxSize = 0;
    for (const s of g.series) s.sizes?.forEach((z) => z !== null && (maxSize = Math.max(maxSize, g.showNegBubbles ? Math.abs(z) : z)));
    const rMax = ((Math.min(pl.w, pl.h) * 0.25) / 2) * (g.bubbleScale / 100);
    for (const ser of g.series) {
      ser.vals.forEach((y, i) => {
        const x = ser.xs?.[i];
        const z = ser.sizes?.[i] ?? null;
        if (y === null || x === null || x === undefined || z === null || !maxSize) return;
        if (z < 0 && !g.showNegBubbles) return;
        const ratio = Math.abs(z) / maxSize;
        const r = g.sizeRepresents === "w" ? rMax * ratio : rMax * Math.sqrt(ratio);
        const [px, py] = toPx(x, y);
        const paint = shapePaint(g, ser, i, ctx);
        const explicit = !!(kid(kid(ser.dPt.get(i), "spPr"), "solidFill") || kid(ser.spPr, "solidFill") || kid(ser.spPr, "gradFill"));
        L.series.append(
          s("circle", {
            cx: r2(px),
            cy: r2(py),
            r: r2(Math.max(r, 0.5)),
            fill: z < 0 ? "#FFFFFF" : (paint.fill ?? "none"),
            "fill-opacity": explicit || z < 0 ? undefined : 0.75,
            ...strokeAttrs(paint.stroke ?? (z < 0 ? { color: paint.fill ?? "#000", width: 1 } : null)),
          }),
        );
        const spec = labelSpec(g, ser, i);
        if (spec) {
          const blk = labelBlock(spec, ser, i, { value: y, size: z, cat: formatX(ser, x) }, ctx);
          if (blk) pointLabel(spec, blk, [px, py], spec.pos ?? "ctr", ctx, L, paint.fill);
        }
      });
    }
    return;
  }
  for (const ser of g.series) {
    const pts: ([number, number] | null)[] = ser.vals.map((y, i) => {
      const x = ser.xs?.[i];
      return y === null || x === null || x === undefined ? null : toPx(x, y);
    });
    const group = s("g");
    const stroke = linePaint(g, ser, ctx, 3);
    if (stroke && stroke.color)
      for (const seg of segments(pts, ctx.blanks)) {
        if (seg.length < 2) continue;
        group.append(s("path", { d: ser.smooth ? smoothPath(seg) : linePath(seg), fill: "none", ...strokeAttrs(stroke), "stroke-linejoin": "round" }));
      }
    const xs = ser.xs ?? ser.vals.map((_, i) => i + 1);
    drawTrendlines(ser, xs, toPx, stroke?.color || autoFor(g, ser, null, ctx), ctx, group);
    drawErrBars(ser, xs, toPx, null, ctx, group);
    pts.forEach((p, i) => {
      if (!p) return;
      const m = seriesMarker(g, ser, ctx, ser.dPt.has(i) ? i : null);
      if (m) group.append(drawMarker(m, p[0], p[1]));
    });
    L.lines.append(group);
    pts.forEach((p, i) => {
      const y = ser.vals[i];
      if (!p || y === null) return;
      const spec = labelSpec(g, ser, i);
      if (!spec) return;
      const x = ser.xs?.[i];
      const blk = labelBlock(spec, ser, i, { value: y, cat: x === null || x === undefined ? "" : formatX(ser, x) }, ctx);
      if (blk) pointLabel(spec, blk, p, spec.pos ?? "r", ctx, L, stroke?.color ?? null);
    });
  }
}

function formatX(ser: Series, x: number): string {
  return ser.cats.length ? (ser.cats[ser.xs?.indexOf(x) ?? -1] ?? String(x)) : String(x);
}
