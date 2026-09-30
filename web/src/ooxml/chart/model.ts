/**
 * Chart part (c:chartSpace) → data model: chart groups, series, axes.
 * Reads only cached values (numCache/strCache/numLit/strLit); formulas are not evaluated.
 */

import { attr, kid, kids, numAttr, toggle } from "../core/package";
import { formatValue } from "../core/numfmt";

export type Kind = "bar" | "line" | "area" | "pie" | "doughnut" | "scatter" | "bubble" | "radar" | "stock" | "surface";
export type Grouping = "clustered" | "stacked" | "percentStacked" | "standard";

export interface Series {
  el: Element;
  idx: number;
  order: number;
  name: string;
  /** Category labels (number format already applied) */
  cats: string[];
  /** Raw values of numeric categories (date axes etc.), for custom axis formats */
  catNums: (number | null)[] | null;
  /** Outer levels of multi-level categories (inner to outer, each indexed by data point; empty string means continuation of the previous one) */
  catLevels: string[][];
  vals: (number | null)[];
  /** Number format of the values (numCache formatCode) */
  format: string;
  /** X values for scatter/bubble charts (1, 2, 3… when non-numeric) */
  xs: (number | null)[] | null;
  xFormat: string;
  sizes: (number | null)[] | null;
  spPr: Element | null;
  marker: Element | null;
  dLbls: Element | null;
  dPt: Map<number, Element>;
  smooth: boolean;
  explosion: number;
  invertIfNegative: boolean;
}

export interface Group {
  kind: Kind;
  el: Element;
  is3D: boolean;
  barDir: "col" | "bar";
  grouping: Grouping;
  varyColors: boolean;
  gapWidth: number;
  overlap: number;
  series: Series[];
  axIds: string[];
  dLbls: Element | null;
  /** c:marker of a line chart group (whether markers are shown); undefined when unspecified */
  showMarker: boolean | undefined;
  firstSliceAng: number;
  holeSize: number;
  scatterStyle: string;
  radarStyle: string;
  bubbleScale: number;
  showNegBubbles: boolean;
  sizeRepresents: "area" | "w";
  hiLowLines: Element | null;
  upDownBars: Element | null;
  dropLines: Element | null;
}

export type AxisType = "cat" | "val" | "date" | "ser";

export interface AxisDef {
  el: Element;
  id: string;
  type: AxisType;
  pos: "b" | "t" | "l" | "r" | null;
  deleted: boolean;
  reversed: boolean;
  min: number | null;
  max: number | null;
  logBase: number | null;
  majorUnit: number | null;
  minorUnit: number | null;
  numFmt: string | null;
  sourceLinked: boolean;
  crossAx: string | null;
  crosses: "autoZero" | "min" | "max" | number;
  crossBetween: "between" | "midCat" | null;
}

export interface ChartModel {
  space: Element;
  chart: Element;
  plotArea: Element;
  groups: Group[];
  axes: Map<string, AxisDef>;
  dispBlanksAs: "gap" | "zero" | "span";
}

const CHART_TAGS: Record<string, [Kind, boolean]> = {
  barChart: ["bar", false],
  bar3DChart: ["bar", true],
  lineChart: ["line", false],
  line3DChart: ["line", true],
  areaChart: ["area", false],
  area3DChart: ["area", true],
  pieChart: ["pie", false],
  pie3DChart: ["pie", true],
  ofPieChart: ["pie", false],
  doughnutChart: ["doughnut", false],
  scatterChart: ["scatter", false],
  bubbleChart: ["bubble", false],
  radarChart: ["radar", false],
  stockChart: ["stock", false],
  surfaceChart: ["surface", false],
  surface3DChart: ["surface", true],
};

const valOf = (el: Element | null, name: string) => attr(kid(el, name), "val");
const numOf = (el: Element | null, name: string) => numAttr(kid(el, name), "val");

// ───────────── Data cache ─────────────

interface Cache {
  pts: Map<number, string>;
  count: number;
  format: string;
}

/** Maximum number of data points per series */
export const MAX_POINTS = 20000;

/** Cached values of numRef/strRef/numLit/strLit */
function readCache(ref: Element | null): Cache | null {
  if (!ref) return null;
  const src = kid(ref, "numCache") ?? kid(ref, "strCache") ?? (ref.localName.endsWith("Lit") ? ref : null);
  if (!src) return null;
  const pts = new Map<number, string>();
  let maxIdx = -1;
  for (const pt of kids(src, "pt")) {
    const i = numAttr(pt, "idx") ?? 0;
    // Skip indices beyond the limit or unreasonable ones (avoid allocating huge arrays based on numbers in the file)
    if (!Number.isInteger(i) || i < 0 || i >= MAX_POINTS) continue;
    pts.set(i, kid(pt, "v")?.textContent ?? "");
    const code = attr(pt, "formatCode");
    if (code) pts.set(-1 - i, code);
    maxIdx = Math.max(maxIdx, i);
  }
  const count = Math.min(numOf(src, "ptCount") ?? maxIdx + 1, MAX_POINTS);
  return { pts, count: Math.max(Number.isFinite(count) ? count : 0, maxIdx + 1), format: kid(src, "formatCode")?.textContent ?? "General" };
}

/** Reference beneath a data source element (c:val, c:cat…) */
function source(el: Element | null): Element | null {
  return kid(el, "numRef") ?? kid(el, "strRef") ?? kid(el, "numLit") ?? kid(el, "strLit") ?? kid(el, "multiLvlStrRef");
}

function numbers(el: Element | null): { vals: (number | null)[]; format: string } | null {
  const c = readCache(source(el));
  if (!c) return null;
  const vals: (number | null)[] = [];
  for (let i = 0; i < c.count; i++) {
    const v = c.pts.get(i);
    const n = v === undefined || v.trim() === "" ? NaN : Number(v);
    vals.push(Number.isFinite(n) ? n : null);
  }
  return { vals, format: c.format };
}

interface Categories {
  labels: string[];
  nums: (number | null)[] | null;
  levels: string[][];
  format: string;
}

/** Categories (or X values): text used as is; numbers get the cached number format */
function categories(el: Element | null, numFmt?: string | null): Categories | null {
  const ref = source(el);
  if (!ref) return null;
  if (ref.localName === "multiLvlStrRef") {
    const levels = kids(kid(ref, "multiLvlStrCache"), "lvl").map((lvl) => {
      const m = new Map<number, string>();
      for (const pt of kids(lvl, "pt")) m.set(numAttr(pt, "idx") ?? 0, kid(pt, "v")?.textContent ?? "");
      return m;
    });
    const count = numOf(kid(ref, "multiLvlStrCache"), "ptCount") ?? 0;
    const arr = levels.map((m) => Array.from({ length: count }, (_, i) => m.get(i) ?? ""));
    return { labels: arr[0] ?? [], nums: null, levels: arr.slice(1), format: "General" };
  }
  const c = readCache(ref);
  if (!c) return null;
  const numeric = ref.localName.startsWith("num");
  const labels: string[] = [];
  const nums: (number | null)[] = [];
  for (let i = 0; i < c.count; i++) {
    const v = c.pts.get(i);
    const n = v === undefined || v.trim() === "" ? NaN : Number(v);
    nums.push(Number.isFinite(n) ? n : null);
    if (numeric && Number.isFinite(n)) labels.push(formatValue(n, numFmt || c.pts.get(-1 - i) || c.format));
    else labels.push(v ?? "");
  }
  return { labels, nums: numeric ? nums : null, levels: [], format: c.format };
}

/** Series name: strRef cache of c:tx or c:v */
function seriesName(ser: Element, fallbackIdx: number): string {
  const tx = kid(ser, "tx");
  const ref = kid(tx, "strRef");
  if (ref) {
    const c = readCache(ref);
    const v = c?.pts.get(0);
    if (v !== undefined) return v;
  }
  const v = kid(tx, "v")?.textContent;
  if (v !== null && v !== undefined) return v;
  // When a series name has no text, fall back to Excel's English default (not UI text)
  return `Series${fallbackIdx + 1}`;
}

function readSeries(ser: Element, pos: number, kind: Kind): Series {
  const idx = numOf(ser, "idx") ?? pos;
  const isXY = kind === "scatter" || kind === "bubble";
  const valEl = kid(ser, isXY ? "yVal" : "val") ?? kid(ser, "val");
  const values = numbers(valEl);
  const catEl = kid(ser, isXY ? "xVal" : "cat") ?? kid(ser, "cat");
  const cats = categories(catEl);
  let xs: (number | null)[] | null = null;
  if (isXY) {
    const n = values?.vals.length ?? 0;
    xs = cats?.nums ?? Array.from({ length: Math.max(n, cats?.labels.length ?? 0) }, (_, i) => i + 1);
  }
  const dPt = new Map<number, Element>();
  for (const p of kids(ser, "dPt")) dPt.set(numOf(p, "idx") ?? 0, p);
  const smoothEl = kid(ser, "smooth");
  return {
    el: ser,
    idx,
    order: numOf(ser, "order") ?? idx,
    name: seriesName(ser, idx),
    cats: cats?.labels ?? [],
    catNums: cats?.nums ?? null,
    catLevels: cats?.levels ?? [],
    vals: values?.vals ?? [],
    format: values?.format ?? "General",
    xs,
    xFormat: cats?.nums ? cats.format : "General",
    sizes: kind === "bubble" ? (numbers(kid(ser, "bubbleSize"))?.vals ?? null) : null,
    spPr: kid(ser, "spPr"),
    marker: kid(ser, "marker"),
    dLbls: kid(ser, "dLbls"),
    dPt,
    smooth: !!toggle(smoothEl),
    explosion: numOf(ser, "explosion") ?? 0,
    invertIfNegative: !!toggle(kid(ser, "invertIfNegative")),
  };
}

function readGroup(el: Element, kind: Kind, is3D: boolean): Group {
  const grouping = (valOf(el, "grouping") ?? (kind === "bar" ? "clustered" : "standard")) as Grouping;
  const series = kids(el, "ser").map((s, i) => readSeries(s, i, kind));
  series.sort((a, b) => a.order - b.order);
  const varyEl = kid(el, "varyColors");
  const holeSize = numOf(el, "holeSize");
  return {
    kind,
    el,
    is3D,
    barDir: valOf(el, "barDir") === "bar" ? "bar" : "col",
    grouping,
    // Pie charts vary colors by data point by default
    varyColors: varyEl ? !!toggle(varyEl) : kind === "pie" || kind === "doughnut",
    gapWidth: numOf(el, "gapWidth") ?? 150,
    overlap: numOf(el, "overlap") ?? (grouping === "stacked" || grouping === "percentStacked" ? 100 : 0),
    series,
    axIds: kids(el, "axId").map((a) => attr(a, "val") ?? ""),
    dLbls: kid(el, "dLbls"),
    showMarker: kid(el, "marker") ? !!toggle(kid(el, "marker")) : undefined,
    firstSliceAng: numOf(el, "firstSliceAng") ?? 0,
    holeSize: holeSize ?? 50,
    scatterStyle: valOf(el, "scatterStyle") ?? "lineMarker",
    radarStyle: valOf(el, "radarStyle") ?? "standard",
    bubbleScale: numOf(el, "bubbleScale") ?? 100,
    showNegBubbles: !!toggle(kid(el, "showNegBubbles")),
    sizeRepresents: valOf(el, "sizeRepresents") === "w" ? "w" : "area",
    hiLowLines: kid(el, "hiLowLines"),
    upDownBars: kid(el, "upDownBars"),
    dropLines: kid(el, "dropLines"),
  };
}

function readAxis(el: Element): AxisDef {
  const type = el.localName === "valAx" ? "val" : el.localName === "dateAx" ? "date" : el.localName === "serAx" ? "ser" : "cat";
  const scaling = kid(el, "scaling");
  const pos = valOf(el, "axPos");
  const crossesAt = numOf(el, "crossesAt");
  const crosses = valOf(el, "crosses");
  const numFmt = kid(el, "numFmt");
  const cb = valOf(el, "crossBetween");
  return {
    el,
    id: valOf(el, "axId") ?? "",
    type,
    pos: pos === "b" || pos === "t" || pos === "l" || pos === "r" ? pos : null,
    deleted: !!toggle(kid(el, "delete")),
    reversed: valOf(scaling, "orientation") === "maxMin",
    min: numOf(scaling, "min"),
    max: numOf(scaling, "max"),
    logBase: numOf(scaling, "logBase"),
    majorUnit: numOf(el, "majorUnit"),
    minorUnit: numOf(el, "minorUnit"),
    numFmt: attr(numFmt, "formatCode"),
    sourceLinked: numFmt ? attr(numFmt, "sourceLinked") === "1" : true,
    crossAx: valOf(el, "crossAx"),
    crosses: crossesAt !== null ? crossesAt : crosses === "min" || crosses === "max" ? crosses : "autoZero",
    crossBetween: cb === "between" || cb === "midCat" ? cb : null,
  };
}

export function parseChart(doc: Document): ChartModel | null {
  const space = doc.documentElement;
  const chart = kid(space, "chart");
  const plotArea = kid(chart, "plotArea");
  if (!chart || !plotArea) return null;
  const groups: Group[] = [];
  const axes = new Map<string, AxisDef>();
  for (const el of kids(plotArea)) {
    const tag = CHART_TAGS[el.localName];
    if (tag) groups.push(readGroup(el, tag[0], tag[1]));
    else if (/^(cat|val|date|ser)Ax$/.test(el.localName)) {
      const ax = readAxis(el);
      axes.set(ax.id, ax);
    }
  }
  const blanks = valOf(chart, "dispBlanksAs");
  return {
    space,
    chart,
    plotArea,
    groups,
    axes,
    // Excel treats unspecified as gap (files usually specify it explicitly)
    dispBlanksAs: blanks === "zero" || blanks === "span" ? blanks : "gap",
  };
}

/** Text when c:tx is a strRef (titles, axis titles) */
export function strRefText(tx: Element | null): string | null {
  const ref = kid(tx, "strRef");
  if (!ref) return null;
  const c = readCache(ref);
  if (!c) return null;
  return Array.from({ length: c.count }, (_, i) => c.pts.get(i) ?? "").join(" ");
}

export { numOf };
