/**
 * Extended charts (cx:chartSpace, Office 2016+): histogram and Pareto, waterfall, funnel, box & whisker, treemap, sunburst.
 * Data comes only from cached values in cx:chartData; maps (regionMap) are not supported, only the title is shown.
 */

import { attr, kid, kids, numAttr, s } from "../core/package";
import type { ColorContext } from "../core/theme";
import { formatGeneral, formatValue } from "../core/numfmt";
import { drawAxes, drawGrid, layoutFrame, type AxisSpec, type PlacedAxis, type Rect, type Side } from "./axes";
import { buildLegend, type LegendEntry } from "./legend";
import type { Ctx } from "./paint";
import { createCanvas, drawBackground } from "./render";
import { autoColor, luminance, readFill, r2, shiftLum, strokeAttrs, strokeOr, type Stroke } from "./style";
import { drawBlock, ellipsize, layoutBlock, lineHeight, plainLines, textBlock, txPrStyle, wrapLines, type TextStyle } from "./text";
import { titleBlock } from "./title";
import { MAX_POINTS } from "./model";

interface Dim {
  /** Levels (the first level is innermost); numeric dimensions have only one level */
  levels: (string | null)[][];
  format: string;
}

interface ExSeries {
  el: Element;
  layout: string;
  name: string;
  idx: number;
  spPr: Element | null;
  dataPt: Map<number, Element>;
  labels: Element | null;
  layoutPr: Element | null;
  axisIds: string[];
  hidden: boolean;
  cat: Dim | null;
  val: Dim | null;
  size: Dim | null;
}

function readDim(el: Element): Dim {
  const levels = kids(el, "lvl").map((lvl) => {
    const n = Math.max(0, Math.min(numAttr(lvl, "ptCount") ?? 0, MAX_POINTS));
    const arr: (string | null)[] = new Array(n).fill(null);
    for (const pt of kids(lvl, "pt")) {
      const i = numAttr(pt, "idx") ?? 0;
      if (Number.isInteger(i) && i >= 0 && i < MAX_POINTS) arr[i] = pt.textContent ?? "";
    }
    return arr;
  });
  return { levels, format: attr(kid(el, "lvl"), "formatCode") ?? "General" };
}

const nums = (d: Dim | null) => (d?.levels[0] ?? []).map((v) => (v === null || v.trim() === "" || !Number.isFinite(Number(v)) ? null : Number(v)));

function readSeries(el: Element, data: Map<string, Element>, i: number): ExSeries {
  const d = data.get(attr(kid(el, "dataId"), "val") ?? "");
  const dims = kids(d).filter((x) => x.localName === "strDim" || x.localName === "numDim");
  const find = (types: string[]) => {
    const e = dims.find((x) => types.includes(attr(x, "type") ?? ""));
    return e ? readDim(e) : null;
  };
  const dataPt = new Map<number, Element>();
  for (const p of kids(el, "dataPt")) dataPt.set(numAttr(p, "idx") ?? 0, p);
  const tx = kid(el, "tx");
  return {
    el,
    layout: attr(el, "layoutId") ?? "",
    name: kid(kid(tx, "txData"), "v")?.textContent ?? kid(tx, "v")?.textContent ?? "",
    idx: numAttr(el, "formatIdx") ?? i,
    spPr: kid(el, "spPr"),
    dataPt,
    labels: kid(el, "dataLabels"),
    layoutPr: kid(el, "layoutPr"),
    axisIds: kids(el, "axisId").map((a) => attr(a, "val") ?? ""),
    hidden: attr(el, "hidden") === "1",
    cat: find(["cat", "colorStr"]),
    val: find(["val", "y"]),
    size: find(["size"]),
  };
}

/** Fill of a series (or data point) */
function fillOf(ser: ExSeries, i: number | null, auto: string, ctx: Ctx): string {
  const pt = i !== null ? ser.dataPt.get(i) : undefined;
  const f = readFill(kid(pt, "spPr"), ctx) ?? readFill(ser.spPr, ctx);
  return f === undefined || f === null ? (f === null ? "none" : auto) : f;
}

function strokeOf(ser: ExSeries, i: number | null, ctx: Ctx, def: Stroke | null): Stroke | null {
  const pt = i !== null ? ser.dataPt.get(i) : undefined;
  return strokeOr(kid(pt, "spPr"), ctx.colors, strokeOr(ser.spPr, ctx.colors, def));
}

interface LabelVis {
  value: boolean;
  cat: boolean;
  ser: boolean;
  fmt: string | null;
  style: TextStyle;
  pos: string | null;
}

function labelVis(ser: ExSeries, ctx: Ctx): LabelVis | null {
  const el = ser.labels;
  if (!el) return null;
  const vis = kid(el, "visibility");
  const v: LabelVis = {
    value: vis ? attr(vis, "value") === "1" : true,
    cat: attr(vis, "categoryName") === "1",
    ser: attr(vis, "seriesName") === "1",
    fmt: attr(kid(el, "numFmt"), "formatCode"),
    style: txPrStyle(ctx.base, kid(el, "txPr"), ctx),
    pos: attr(el, "pos"),
  };
  return v.value || v.cat || v.ser ? v : null;
}

function labelText(v: LabelVis, ser: ExSeries, cat: string, value: number | null, fmt: string): string {
  const parts: string[] = [];
  if (v.ser) parts.push(ser.name);
  if (v.cat) parts.push(cat);
  if (v.value && value !== null) parts.push(formatValue(value, v.fmt && v.fmt !== "General" ? v.fmt : fmt || "General"));
  return parts.join(", ");
}

/** cx:axis → AxisSpec */
function axisSpec(el: Element | null, id: string, role: "cat" | "val", horizontal: boolean, side: Side, ctx: Ctx, extra: Partial<AxisSpec>): AxisSpec {
  const text = txPrStyle(ctx.base, kid(el, "txPr"), ctx);
  const titleEl = kid(el, "title");
  const sc = kid(el, "valScaling");
  const num = (name: string) => {
    const v = attr(sc, name);
    return v === null || v === "auto" || !Number.isFinite(Number(v)) ? null : Number(v);
  };
  const grid = kid(el, "majorGridlines");
  const minor = kid(el, "minorGridlines");
  const tick = attr(kid(el, "majorTickMarks"), "type");
  return {
    id,
    role,
    horizontal,
    side,
    deleted: !el || attr(el, "hidden") === "1",
    reversed: false,
    labels: [],
    count: 0,
    between: true,
    lblSkip: null,
    markSkip: null,
    levels: [],
    input: null,
    format: attr(kid(el, "numFmt"), "formatCode") ?? "General",
    dispUnit: 1,
    dispLabel: null,
    crossAx: null,
    crosses: "autoZero",
    text,
    rot: null,
    line: strokeOr(kid(el, "spPr"), ctx.colors, role === "cat" ? { color: "#D9D9D9", width: 1 } : null),
    majorGrid: grid ? strokeOr(kid(grid, "spPr"), ctx.colors, { color: "#D9D9D9", width: 1 }) : null,
    minorGrid: minor ? strokeOr(kid(minor, "spPr"), ctx.colors, { color: "#F2F2F2", width: 1 }) : null,
    majorTick: tick === "in" || tick === "out" || tick === "cross" ? tick : "none",
    minorTick: "none",
    lblPos: kid(el, "tickLabels") ? "nextTo" : "none",
    title: titleEl ? titleBlock(titleEl, { ...text, size: Math.max(text.size, ctx.base.size), bold: true }, ctx, null) : null,
    titleRot: horizontal ? 0 : -90,
    ...extra,
    ...(sc ? { input: extra.input ? { ...extra.input, min: num("min") ?? extra.input.min, max: num("max") ?? extra.input.max, major: num("majorUnit"), minor: num("minorUnit") } : null } : {}),
  };
}

export function renderChartEx(doc: Document, W: number, H: number, colors: ColorContext, transparent: boolean): SVGElement {
  const space = doc.documentElement;
  const { svg, ctx } = createCanvas(W, H, colors, kid(space, "txPr"), transparent);
  drawBackground(svg, kid(space, "spPr"), W, H, ctx, false);
  const chart = kid(space, "chart");
  const data = new Map<string, Element>();
  for (const d of kids(kid(space, "chartData"), "data")) data.set(attr(d, "id") ?? "", d);
  const region = kid(kid(chart, "plotArea"), "plotAreaRegion");
  const series = kids(region, "series").map((el, i) => readSeries(el, data, i));
  const axes = kids(kid(chart, "plotArea"), "axis");

  const pad = Math.max(4, Math.min(10, Math.min(W, H) * 0.03));
  let area: Rect = { x: pad, y: pad, w: W - pad * 2, h: H - pad * 2 };
  const titleEl = kid(chart, "title");
  const title = titleEl ? titleBlock(titleEl, { ...ctx.base, size: Math.round(ctx.base.size * 1.4 * 10) / 10 }, ctx, null, W * 0.85) : null;
  if (title) area = { ...area, y: area.y + title.h + 4, h: area.h - title.h - 4 };

  const main = series.find((x) => !x.hidden && x.layout !== "paretoLine") ?? series[0];
  const layout = main?.layout ?? "";
  // Legend entries
  let entries: LegendEntry[] = [];
  if (layout === "treemap" || layout === "sunburst") {
    const top = main ? topLevels(main) : [];
    entries = top.map((name, i) => ({ text: name, kind: "box", fill: autoColor(i, ctx.colors), idx: i }));
  } else if (layout !== "waterfall")
    entries = series
      .filter((x) => !x.hidden)
      .map((x, i) => (x.layout === "paretoLine" ? { text: x.name, kind: "line" as const, stroke: strokeOf(x, null, ctx, { color: autoColor(1, ctx.colors), width: 2 }), idx: i } : { text: x.name, kind: "box" as const, fill: fillOf(x, null, autoColor(i, ctx.colors), ctx), idx: i }));
  const legend = buildLegend(kid(chart, "legend"), entries, area, W, H, ctx, false);
  if (legend?.take) {
    const { side, size } = legend.take;
    if (side === "b") area = { ...area, h: area.h - size };
    else if (side === "t") area = { ...area, y: area.y + size, h: area.h - size };
    else if (side === "l") area = { ...area, x: area.x + size, w: area.w - size };
    else area = { ...area, w: area.w - size };
  }

  const plot = s("g");
  if (main) {
    switch (layout) {
      case "clusteredColumn":
        renderColumns(series, axes, area, plot, ctx);
        break;
      case "waterfall":
        renderWaterfall(main, axes, area, plot, ctx);
        break;
      case "funnel":
        renderFunnel(main, axes, area, plot, ctx);
        break;
      case "boxWhisker":
        renderBoxes(series.filter((x) => x.layout === "boxWhisker" && !x.hidden), axes, area, plot, ctx);
        break;
      case "treemap":
        renderTreemap(main, area, plot, ctx);
        break;
      case "sunburst":
        renderSunburst(main, area, plot, ctx);
        break;
    }
  }
  svg.append(plot);
  if (legend) {
    const lg = s("g");
    legend.draw(lg, area);
    svg.append(lg);
  }
  if (title) svg.append(drawBlock(title, W / 2, pad, "middle", "top"));
  return svg;
}

const axisOf = (axes: Element[], kind: "catScaling" | "valScaling", nth = 0) => axes.filter((a) => kid(a, kind))[nth] ?? null;

/** Cartesian frame: category axis (horizontal) + value axis (vertical), optionally a second value axis (right) */
function frameFor(axes: Element[], labels: string[], lo: number, hi: number, fmt: string, area: Rect, ctx: Ctx, secondary: boolean) {
  const catEl = axisOf(axes, "catScaling");
  const valEl = axisOf(axes, "valScaling");
  const specs: AxisSpec[] = [
    axisSpec(catEl, "c", "cat", true, "b", ctx, { labels, count: labels.length, crossAx: "v" }),
    axisSpec(valEl, "v", "val", false, "l", ctx, { input: { lo, hi, min: null, max: null, major: null, minor: null, logBase: null }, crossAx: "c", format: attr(kid(valEl, "numFmt"), "formatCode") ?? fmt }),
  ];
  if (secondary) {
    const el2 = axisOf(axes, "valScaling", 1);
    specs.push(axisSpec(el2, "p", "val", false, "r", ctx, { input: { lo: 0, hi: 1, min: 0, max: 1, major: null, minor: null, logBase: null, percent: true }, format: "0%", crossAx: "c", crosses: "max", deleted: el2 ? attr(el2, "hidden") === "1" : false, majorGrid: null }));
  }
  const frame = layoutFrame(area, specs, null);
  return { frame, c: frame.axes.get("c")!, v: frame.axes.get("v")!, p: frame.axes.get("p") ?? null };
}

function gapOf(axes: Element[], def: number): number {
  const g = attr(kid(axisOf(axes, "catScaling"), "catScaling"), "gapWidth");
  return g === null || g === "auto" || !Number.isFinite(Number(g)) ? def : Number(g);
}

function barRect(c: PlacedAxis, v: PlacedAxis, i: number, from: number, to: number, gap: number) {
  const band = Math.abs(c.band);
  const w = band / (1 + gap);
  const x = c.center(i) - w / 2;
  const y0 = v.pos(from);
  const y1 = v.pos(to);
  return { x, y: Math.min(y0, y1), w, h: Math.abs(y1 - y0) };
}

// ───────────── Histogram / Pareto ─────────────

const binLabel = (v: number) => formatGeneral(Number(v.toPrecision(4)));

/** Histogram binning: by binSize/binCount, or automatically via Scott's rule */
function histogram(values: number[], binning: Element | null): { labels: string[]; counts: number[] } {
  const vals = values.slice().sort((a, b) => a - b);
  if (!vals.length) return { labels: [], counts: [] };
  const closedLeft = attr(binning, "intervalClosed") === "l";
  const under = attr(binning, "underflow");
  const over = attr(binning, "overflow");
  const underV = under !== null && under !== "auto" && Number.isFinite(Number(under)) ? Number(under) : null;
  const overV = over !== null && over !== "auto" && Number.isFinite(Number(over)) ? Number(over) : null;
  const lo = underV ?? vals[0];
  const hi = overV ?? vals[vals.length - 1];
  const size = numAttr(kid(binning, "binSize"), "val");
  const count = numAttr(kid(binning, "binCount"), "val");
  let width: number;
  if (size && size > 0) width = size;
  else if (count && count > 0) width = (hi - lo) / count || 1;
  else {
    const n = vals.length;
    const mean = vals.reduce((a, b) => a + b, 0) / n;
    const sd = Math.sqrt(vals.reduce((a, b) => a + (b - mean) ** 2, 0) / Math.max(n - 1, 1));
    width = (3.5 * sd) / Math.cbrt(n) || 1;
  }
  const nBins = Math.max(1, Math.min(200, Math.ceil((hi - lo) / width - 1e-9)));
  const labels: string[] = [];
  const counts: number[] = [];
  if (underV !== null) {
    labels.push(`≤${binLabel(underV)}`);
    counts.push(vals.filter((v) => (closedLeft ? v < underV : v <= underV)).length);
  }
  for (let b = 0; b < nBins; b++) {
    const a = lo + b * width;
    const z = a + width;
    const first = b === 0 && underV === null;
    const last = b === nBins - 1 && overV === null;
    const inBin = (v: number) => {
      if (closedLeft) return (v >= a || (first && v >= a)) && (v < z || (last && v <= z));
      return (v > a || (first && v >= a)) && (v <= z || (last && v <= z));
    };
    counts.push(vals.filter((v) => (underV === null || (closedLeft ? v >= underV : v > underV)) && (overV === null || v <= overV) && inBin(v)).length);
    const open = closedLeft ? "[" : first ? "[" : "(";
    const close = closedLeft ? (last ? "]" : ")") : "]";
    labels.push(`${open}${binLabel(a)}, ${binLabel(z)}${close}`);
  }
  if (overV !== null) {
    labels.push(`>${binLabel(overV)}`);
    counts.push(vals.filter((v) => v > overV).length);
  }
  return { labels, counts };
}

function renderColumns(series: ExSeries[], axes: Element[], area: Rect, g: SVGElement, ctx: Ctx) {
  const bars = series.find((x) => x.layout === "clusteredColumn")!;
  const pareto = series.find((x) => x.layout === "paretoLine" && !x.hidden);
  const binning = kid(bars.layoutPr, "binning");
  let labels: string[];
  let values: number[];
  const catLevel = bars.cat?.levels[0];
  if (catLevel && !binning) {
    // Categorical data: merge identical categories, then sort descending (Pareto)
    const sums = new Map<string, number>();
    nums(bars.val).forEach((v, i) => {
      const k = catLevel[i] ?? "";
      sums.set(k, (sums.get(k) ?? 0) + (v ?? 0));
    });
    let list = [...sums.entries()];
    if (pareto) list = list.sort((a, b) => b[1] - a[1]);
    labels = list.map((x) => x[0]);
    values = list.map((x) => x[1]);
  } else {
    const h = histogram(nums(bars.val).filter((v): v is number => v !== null), binning);
    labels = h.labels;
    values = h.counts;
  }
  const hi = Math.max(0, ...values);
  const { frame, c, v, p } = frameFor(axes, labels, 0, hi, bars.val?.format ?? "General", area, ctx, !!pareto);
  const gl = s("g");
  drawGrid({ plot: frame.plot, axes: new Map([...frame.axes].filter(([k]) => k !== "p")) }, gl);
  g.append(gl);
  const gap = gapOf(axes, binning ? 0 : 0.1);
  const vis = labelVis(bars, ctx);
  const auto = autoColor(0, ctx.colors);
  values.forEach((val, i) => {
    const r = barRect(c, v, i, 0, val, gap);
    g.append(s("rect", { x: r2(r.x), y: r2(r.y), width: r2(r.w), height: r2(r.h), fill: fillOf(bars, i, auto, ctx), ...strokeAttrs(strokeOf(bars, i, ctx, gap === 0 ? { color: "#FFFFFF", width: 0.75 } : null)) }));
    if (vis) {
      const t = labelText(vis, bars, labels[i], val, "General");
      if (t) g.append(drawBlock(textBlock(t, vis.style), c.center(i), r.y - 3, "middle", "bottom"));
    }
  });
  const ax = s("g");
  drawAxes(frame, ax);
  g.append(ax);
  if (pareto && p) {
    const total = values.reduce((a, b) => a + b, 0) || 1;
    let acc = 0;
    const pts = values.map((val, i) => {
      acc += val;
      return `${r2(c.center(i))},${r2(p.pos(acc / total))}`;
    });
    const st = strokeOf(pareto, null, ctx, { color: autoColor(1, ctx.colors), width: 2 });
    g.append(s("polyline", { points: pts.join(" "), fill: "none", ...strokeAttrs(st), "stroke-linejoin": "round" }));
  }
}

// ───────────── Waterfall ─────────────

function renderWaterfall(ser: ExSeries, axes: Element[], area: Rect, g: SVGElement, ctx: Ctx) {
  const vals = nums(ser.val);
  const labels = (ser.cat?.levels[0] ?? vals.map((_, i) => String(i + 1))).map((x) => x ?? "");
  const totals = new Set(kids(kid(ser.layoutPr, "subtotals"), "idx").map((x) => numAttr(x, "val") ?? -1));
  const spans: { from: number; to: number; kind: 0 | 1 | 2 }[] = [];
  let run = 0;
  vals.forEach((v, i) => {
    const val = v ?? 0;
    if (totals.has(i)) {
      spans.push({ from: 0, to: val, kind: 2 });
      run = val;
    } else {
      spans.push({ from: run, to: run + val, kind: val >= 0 ? 0 : 1 });
      run += val;
    }
  });
  const lo = Math.min(0, ...spans.flatMap((x) => [x.from, x.to]));
  const hi = Math.max(0, ...spans.flatMap((x) => [x.from, x.to]));
  const { frame, c, v } = frameFor(axes, labels, lo, hi, ser.val?.format ?? "General", area, ctx, false);
  const gl = s("g");
  drawGrid(frame, gl);
  g.append(gl);
  const gap = gapOf(axes, 0.5);
  const vis = labelVis(ser, ctx);
  // Default colors: increase = accent 1, decrease = accent 2, total = accent 3
  const autos = [autoColor(0, ctx.colors), autoColor(1, ctx.colors), autoColor(2, ctx.colors)];
  const connectors = attr(kid(ser.layoutPr, "visibility"), "connectorLines") !== "0";
  spans.forEach((sp, i) => {
    const r = barRect(c, v, i, sp.from, sp.to, gap);
    g.append(s("rect", { x: r2(r.x), y: r2(r.y), width: r2(r.w), height: r2(Math.max(r.h, 0.5)), fill: fillOf(ser, i, autos[sp.kind], ctx), ...strokeAttrs(strokeOf(ser, i, ctx, null)) }));
    if (connectors && i < spans.length - 1) {
      const y = v.pos(sp.to);
      const next = barRect(c, v, i + 1, 0, 0, gap);
      g.append(s("path", { d: `M${r2(r.x + r.w)},${r2(y)}H${r2(next.x)}`, fill: "none", stroke: "#A6A6A6", "stroke-width": 0.75 }));
    }
    if (vis) {
      const t = labelText(vis, ser, labels[i], vals[i], ser.val?.format ?? "General");
      if (t) g.append(drawBlock(textBlock(t, vis.style), c.center(i), Math.min(v.pos(sp.from), v.pos(sp.to)) - 3, "middle", "bottom"));
    }
  });
  const ax = s("g");
  drawAxes(frame, ax);
  g.append(ax);
}

// ───────────── Funnel ─────────────

function renderFunnel(ser: ExSeries, axes: Element[], area: Rect, g: SVGElement, ctx: Ctx) {
  const vals = nums(ser.val);
  const labels = (ser.cat?.levels[0] ?? vals.map((_, i) => String(i + 1))).map((x) => x ?? "");
  const n = vals.length;
  if (!n) return;
  const catEl = axisOf(axes, "catScaling");
  const st = txPrStyle(ctx.base, kid(catEl, "txPr"), ctx);
  const showCats = !!catEl && attr(catEl, "hidden") !== "1" && !!kid(catEl, "tickLabels");
  const blocks = showCats ? labels.map((t) => textBlock(ellipsize(t, st, area.w * 0.3), st)) : [];
  const lw = showCats ? Math.max(0, ...blocks.map((b) => b.w)) + 8 : 0;
  const plot: Rect = { x: area.x + lw, y: area.y + 4, w: area.w - lw - 4, h: area.h - 8 };
  const band = plot.h / n;
  const gap = gapOf(axes, 0.06);
  const bh = band / (1 + gap);
  const max = Math.max(...vals.map((v) => Math.abs(v ?? 0)), 1);
  const vis = labelVis(ser, ctx);
  const auto = autoColor(0, ctx.colors);
  vals.forEach((v, i) => {
    const w = (Math.abs(v ?? 0) / max) * plot.w;
    const cx = plot.x + plot.w / 2;
    const cy = plot.y + band * (i + 0.5);
    const fill = fillOf(ser, i, auto, ctx);
    g.append(s("rect", { x: r2(cx - w / 2), y: r2(cy - bh / 2), width: r2(w), height: r2(bh), fill, ...strokeAttrs(strokeOf(ser, i, ctx, null)) }));
    if (showCats) g.append(drawBlock(blocks[i], plot.x - 6, cy, "end", "middle"));
    if (vis) {
      const t = labelText(vis, ser, labels[i], v, ser.val?.format ?? "General");
      if (t) {
        const style = { ...vis.style, color: luminance(fill) < 0.55 ? "#FFFFFF" : vis.style.color };
        g.append(drawBlock(textBlock(t, style), cx, cy, "middle", "middle"));
      }
    }
  });
}

// ───────────── Box & whisker ─────────────

function quantile(sorted: number[], p: number, exclusive: boolean): number {
  const n = sorted.length;
  if (!n) return NaN;
  let pos = exclusive ? p * (n + 1) : p * (n - 1) + 1;
  pos = Math.min(Math.max(pos, 1), n);
  const lo = Math.floor(pos);
  const frac = pos - lo;
  return sorted[lo - 1] + (lo < n ? (sorted[lo] - sorted[lo - 1]) * frac : 0);
}

function renderBoxes(list: ExSeries[], axes: Element[], area: Rect, g: SVGElement, ctx: Ctx) {
  if (!list.length) return;
  // Categories: distinct values of each series' cat dimension (in order of appearance); without categories, one group per series
  const cats: string[] = [];
  for (const ser of list) for (const c of ser.cat?.levels[0] ?? []) if (c !== null && !cats.includes(c)) cats.push(c);
  if (!cats.length) cats.push("");
  const all = list.flatMap((x) => nums(x.val).filter((v): v is number => v !== null));
  const lo = Math.min(...all, 0);
  const hi = Math.max(...all, 1);
  const { frame, c, v } = frameFor(axes, cats, lo, hi, list[0].val?.format ?? "General", area, ctx, false);
  const gl = s("g");
  drawGrid(frame, gl);
  g.append(gl);
  const k = list.length;
  const band = Math.abs(c.band);
  const gap = gapOf(axes, 1);
  const groupW = band / (1 + gap);
  const w = groupW / k;
  list.forEach((ser, j) => {
    const vals = nums(ser.val);
    const vis = kid(ser.layoutPr, "visibility");
    const exclusive = attr(kid(ser.layoutPr, "statistics"), "quartileMethod") !== "inclusive";
    const fill = fillOf(ser, null, autoColor(j, ctx.colors), ctx);
    const line = strokeOf(ser, null, ctx, { color: shiftLum(fill.startsWith("url") ? "#404040" : fill, -0.25), width: 1 }) ?? { color: "#404040", width: 1 };
    const means: [number, number][] = [];
    cats.forEach((cat, ci) => {
      const group = vals.filter((x, i): x is number => x !== null && (cats.length === 1 && !cat ? true : (ser.cat?.levels[0]?.[i] ?? "") === cat)).sort((a, b) => a - b);
      if (!group.length) return;
      const q1 = quantile(group, 0.25, exclusive);
      const q2 = quantile(group, 0.5, exclusive);
      const q3 = quantile(group, 0.75, exclusive);
      const iqr = q3 - q1;
      const inside = group.filter((x) => x >= q1 - 1.5 * iqr && x <= q3 + 1.5 * iqr);
      const wLo = Math.min(...inside);
      const wHi = Math.max(...inside);
      const x0 = c.center(ci) - groupW / 2 + j * w + w * 0.1;
      const bw = w * 0.8;
      const cx = x0 + bw / 2;
      const y = (val: number) => v.pos(val);
      g.append(s("path", { d: `M${r2(cx)},${r2(y(wHi))}V${r2(y(q3))}M${r2(cx)},${r2(y(q1))}V${r2(y(wLo))}M${r2(cx - bw / 4)},${r2(y(wHi))}h${r2(bw / 2)}M${r2(cx - bw / 4)},${r2(y(wLo))}h${r2(bw / 2)}`, fill: "none", ...strokeAttrs(line) }));
      g.append(s("rect", { x: r2(x0), y: r2(y(q3)), width: r2(bw), height: r2(Math.abs(y(q1) - y(q3))), fill, ...strokeAttrs(line) }));
      g.append(s("path", { d: `M${r2(x0)},${r2(y(q2))}h${r2(bw)}`, fill: "none", ...strokeAttrs(line) }));
      const mean = group.reduce((a, b) => a + b, 0) / group.length;
      means.push([cx, y(mean)]);
      if (attr(vis, "meanMarker") !== "0") {
        const m = Math.max(3, Math.min(bw * 0.15, 5));
        g.append(s("path", { d: `M${r2(cx - m)},${r2(y(mean) - m)}l${r2(m * 2)},${r2(m * 2)}M${r2(cx + m)},${r2(y(mean) - m)}l${r2(-m * 2)},${r2(m * 2)}`, fill: "none", ...strokeAttrs({ ...line, width: 1.25 }) }));
      }
      if (attr(vis, "outliers") !== "0")
        for (const o of group.filter((x) => x < q1 - 1.5 * iqr || x > q3 + 1.5 * iqr)) g.append(s("circle", { cx: r2(cx), cy: r2(y(o)), r: 2.5, fill: "none", ...strokeAttrs(line) }));
      if (attr(vis, "nonoutliers") === "1") for (const o of inside) g.append(s("circle", { cx: r2(cx), cy: r2(y(o)), r: 1.8, fill: fill, stroke: "none" }));
    });
    if (attr(vis, "meanLine") === "1" && means.length > 1) g.append(s("polyline", { points: means.map((p) => p.map(r2).join(",")).join(" "), fill: "none", ...strokeAttrs(line) }));
  });
  const ax = s("g");
  drawAxes(frame, ax);
  g.append(ax);
}

// ───────────── Treemap and sunburst ─────────────

interface Node {
  name: string;
  size: number;
  idx: number;
  children: Node[];
  top: number;
}

/** Hierarchy: the first level is innermost, so build from the last level (root) backward */
function hierarchy(ser: ExSeries): Node {
  const levels = ser.cat?.levels ?? [];
  const sizes = nums(ser.size ?? ser.val);
  const root: Node = { name: "", size: 0, idx: -1, children: [], top: 0 };
  const n = Math.max(sizes.length, levels[0]?.length ?? 0);
  // Blank cells carry over the previous value (Excel hierarchical data often omits repeated parent names)
  const filled = levels.map((lvl) => {
    let last = "";
    return Array.from({ length: n }, (_, i) => {
      const v = lvl[i];
      if (v !== null && v !== undefined && v !== "") last = v;
      return v === null || v === undefined || v === "" ? last : v;
    });
  });
  for (let i = 0; i < n; i++) {
    const size = Math.max(0, sizes[i] ?? 0);
    if (!size) continue;
    const path = filled.map((l) => l[i]).reverse();
    let node = root;
    path.forEach((name, d) => {
      let child = node.children.find((c) => c.name === name && d < path.length - 1);
      if (!child || d === path.length - 1) {
        child = { name, size: 0, idx: d === path.length - 1 ? i : -1, children: [], top: 0 };
        node.children.push(child);
      }
      child.size += size;
      node = child;
    });
    root.size += size;
  }
  root.children.forEach((c, i) => {
    const mark = (x: Node) => {
      x.top = i;
      x.children.forEach(mark);
    };
    mark(c);
  });
  return root;
}

function topLevels(ser: ExSeries): string[] {
  return hierarchy(ser).children.map((c) => c.name);
}

/** Squarified treemap layout */
function squarify(items: Node[], rect: Rect): { node: Node; r: Rect }[] {
  const out: { node: Node; r: Rect }[] = [];
  const list = items.filter((x) => x.size > 0).sort((a, b) => b.size - a.size);
  const total = list.reduce((a, b) => a + b.size, 0);
  if (!total || rect.w <= 0 || rect.h <= 0) return out;
  const scale = (rect.w * rect.h) / total;
  let r = { ...rect };
  let row: Node[] = [];
  const worst = (row: Node[], side: number) => {
    const areas = row.map((x) => x.size * scale);
    const sum = areas.reduce((a, b) => a + b, 0);
    const mx = Math.max(...areas);
    const mn = Math.min(...areas);
    return Math.max((side * side * mx) / (sum * sum), (sum * sum) / (side * side * mn));
  };
  const layoutRow = (row: Node[]) => {
    const sum = row.reduce((a, b) => a + b.size * scale, 0);
    if (r.w >= r.h) {
      const w = sum / r.h;
      let y = r.y;
      for (const n of row) {
        const h = (n.size * scale) / w;
        out.push({ node: n, r: { x: r.x, y, w, h } });
        y += h;
      }
      r = { x: r.x + w, y: r.y, w: r.w - w, h: r.h };
    } else {
      const h = sum / r.w;
      let x = r.x;
      for (const n of row) {
        const w = (n.size * scale) / h;
        out.push({ node: n, r: { x, y: r.y, w, h } });
        x += w;
      }
      r = { x: r.x, y: r.y + h, w: r.w, h: r.h - h };
    }
  };
  for (const item of list) {
    const side = Math.min(r.w, r.h);
    if (row.length && worst([...row, item], side) > worst(row, side)) {
      layoutRow(row);
      row = [];
    }
    row.push(item);
  }
  if (row.length) layoutRow(row);
  return out;
}

function nodeFill(ser: ExSeries, node: Node, flat: boolean, ctx: Ctx): string {
  const auto = autoColor(flat ? Math.max(node.idx, 0) : node.top, ctx.colors);
  return fillOf(ser, node.idx >= 0 ? node.idx : null, auto, ctx);
}

function renderTreemap(ser: ExSeries, area: Rect, g: SVGElement, ctx: Ctx) {
  const root = hierarchy(ser);
  const flat = root.children.every((c) => !c.children.length);
  const vis = labelVis(ser, ctx);
  const st = vis?.style ?? ctx.base;
  const parentMode = attr(kid(ser.layoutPr, "parentLabelLayout"), "val") ?? "overlapping";
  const border: Stroke = { color: "#FFFFFF", width: 1.5 };
  const rect: Rect = { x: area.x + 2, y: area.y + 2, w: area.w - 4, h: area.h - 4 };
  const leafLabel = (node: Node, r: Rect, fill: string) => {
    if (!vis) return;
    const t = labelText({ ...vis, cat: true }, ser, node.name, node.size, ser.size?.format ?? ser.val?.format ?? "General");
    if (!t || r.w < 16 || r.h < lineHeight(st)) return;
    const style = { ...st, color: luminance(fill) < 0.6 ? "#FFFFFF" : "#404040" };
    const lines = wrapLines(plainLines(t.replace(", ", "\n"), style), r.w - 6).slice(0, Math.max(1, Math.floor((r.h - 4) / lineHeight(style))));
    g.append(drawBlock(layoutBlock(lines), r.x + 4, r.y + 3, "start", "top"));
  };
  const drawLeaves = (nodes: Node[], r: Rect) => {
    for (const { node, r: nr } of squarify(nodes, r)) {
      if (node.children.length) {
        drawLeaves(node.children, nr);
        continue;
      }
      const fill = nodeFill(ser, node, flat, ctx);
      g.append(s("rect", { x: r2(nr.x), y: r2(nr.y), width: r2(Math.max(nr.w, 0)), height: r2(Math.max(nr.h, 0)), fill, ...strokeAttrs(strokeOf(ser, node.idx, ctx, border)) }));
      leafLabel(node, nr, fill);
    }
  };
  if (flat) {
    drawLeaves(root.children, rect);
    return;
  }
  const lh = lineHeight(st);
  for (const { node, r } of squarify(root.children, rect)) {
    const banner = parentMode === "banner" && r.h > lh * 2;
    const inner = banner ? { x: r.x, y: r.y + lh + 2, w: r.w, h: r.h - lh - 2 } : r;
    const color = autoColor(node.top, ctx.colors);
    if (banner) g.append(s("rect", { x: r2(r.x), y: r2(r.y), width: r2(r.w), height: r2(lh + 2), fill: shiftLum(color, -0.1), ...strokeAttrs(border) }));
    drawLeaves(node.children, inner);
    if (parentMode !== "none" && vis) {
      const style = { ...st, bold: true, color: "#FFFFFF" };
      g.append(drawBlock(textBlock(ellipsize(node.name, style, r.w - 6), style), r.x + 4, r.y + (banner ? 1 : 3), "start", "top"));
    }
  }
}

function renderSunburst(ser: ExSeries, area: Rect, g: SVGElement, ctx: Ctx) {
  const root = hierarchy(ser);
  const depth = (n: Node): number => (n.children.length ? 1 + Math.max(...n.children.map(depth)) : 0);
  const D = Math.max(1, depth(root));
  const cx = area.x + area.w / 2;
  const cy = area.y + area.h / 2;
  const R = Math.max(10, Math.min(area.w, area.h) / 2 - 4);
  const hole = R * 0.12;
  const ring = (R - hole) / D;
  const flat = root.children.every((c) => !c.children.length);
  const vis = labelVis(ser, ctx);
  const st = vis?.style ?? ctx.base;
  const labels = s("g");
  const walk = (nodes: Node[], a0: number, span: number, d: number) => {
    const total = nodes.reduce((a, b) => a + b.size, 0) || 1;
    let a = a0;
    for (const node of nodes) {
      const sweep = (node.size / total) * span;
      const r0 = hole + ring * d;
      const r1 = r0 + ring;
      const fill = d === 0 || flat ? nodeFill(ser, node, flat, ctx) : shiftLum(autoColor(node.top, ctx.colors), Math.min(0.3, d * 0.1));
      g.append(s("path", { d: arc(cx, cy, r0, r1, a, a + sweep), fill, stroke: "#FFFFFF", "stroke-width": 1.5 }));
      if (vis && sweep * ((r0 + r1) / 2) > 18) {
        const mid = a + sweep / 2;
        const rr = (r0 + r1) / 2;
        const style = { ...st, color: luminance(fill) < 0.6 ? "#FFFFFF" : "#404040" };
        const text = ellipsize(node.name, style, Math.min(ring - 4, sweep * rr));
        if (text) labels.append(drawBlock(textBlock(text, style), cx + Math.sin(mid) * rr, cy - Math.cos(mid) * rr, "middle", "middle"));
      }
      if (node.children.length) walk(node.children, a, sweep, d + 1);
      a += sweep;
    }
  };
  walk(root.children, 0, Math.PI * 2, 0);
  g.append(labels);
}

function arc(cx: number, cy: number, r0: number, r1: number, a0: number, a1: number): string {
  const pt = (r: number, a: number) => `${r2(cx + Math.sin(a) * r)},${r2(cy - Math.cos(a) * r)}`;
  if (a1 - a0 >= Math.PI * 2 - 1e-6) {
    return `M${pt(r1, 0)}A${r2(r1)},${r2(r1)} 0 1 1 ${pt(r1, Math.PI)}A${r2(r1)},${r2(r1)} 0 1 1 ${pt(r1, 0)}ZM${pt(r0, 0)}A${r2(r0)},${r2(r0)} 0 1 0 ${pt(r0, Math.PI)}A${r2(r0)},${r2(r0)} 0 1 0 ${pt(r0, 0)}Z`;
  }
  const large = a1 - a0 > Math.PI ? 1 : 0;
  return `M${pt(r1, a0)}A${r2(r1)},${r2(r1)} 0 ${large} 1 ${pt(r1, a1)}L${pt(r0, a1)}A${r2(r0)},${r2(r0)} 0 ${large} 0 ${pt(r0, a0)}Z`;
}
