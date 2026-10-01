/**
 * Axes for cartesian charts: tick label layout (rotation, wrapping, skipping), plot area sizing from label sizes, and drawing of gridlines, axis lines, tick marks and titles.
 * Input is the format-agnostic AxisSpec, shared by regular charts and extended charts (chartEx).
 */

import { s } from "../core/package";
import { formatValue } from "../core/numfmt";
import { fraction, valueScale, type ScaleInput, type ValueScale } from "./scale";
import { strokeAttrs, r2, type Stroke } from "./style";
import { drawBlock, ellipsize, layoutBlock, lineHeight, measure, plainLines, ptPx, rotatedSize, textBlock, wrapLines, type Block, type HAlign, type TextStyle, type VAlign } from "./text";

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export type Side = "b" | "t" | "l" | "r";
export type TickMark = "none" | "in" | "out" | "cross";

export interface AxisSpec {
  id: string;
  role: "cat" | "val";
  horizontal: boolean;
  side: Side;
  deleted: boolean;
  reversed: boolean;
  /** Category axis: labels, count, whether positioned between categories (between), outer category levels */
  labels: string[];
  count: number;
  between: boolean;
  lblSkip: number | null;
  markSkip: number | null;
  levels: string[][];
  /** Value axis: tick inputs, number format and display units */
  input: ScaleInput | null;
  format: string;
  dispUnit: number;
  dispLabel: Block | null;
  crossAx: string | null;
  crosses: "autoZero" | "min" | "max" | number;
  text: TextStyle;
  /** Explicit label rotation angle (degrees); null means automatic */
  rot: number | null;
  line: Stroke | null;
  majorGrid: Stroke | null;
  minorGrid: Stroke | null;
  majorTick: TickMark;
  minorTick: TickMark;
  lblPos: "nextTo" | "low" | "high" | "none";
  title: Block | null;
  titleRot: number;
  /** Extra thickness reserved for the label area (e.g. data table) */
  extraThick?: number;
}

export interface PlacedAxis {
  spec: AxisSpec;
  /** Side the labels actually sit on (outward when the axis line is at the plot area edge; by axPos when in the middle) */
  side: Side;
  scale: ValueScale | null;
  /** Pixel positions of the min and max ends (x for horizontal axes, y for vertical axes) */
  a: number;
  b: number;
  /** Number of units on a category axis (n when between, otherwise n - 1) */
  span: number;
  /** Value (value axis) or category coordinate (category axis, 0 – span) → pixel */
  pos: (v: number) => number;
  /** Center of the i-th category */
  center: (i: number) => number;
  /** Pixel width occupied by each category */
  band: number;
  /** Tick labels */
  tickLabels: { at: number; block: Block }[];
  lblRot: number;
  /** Thickness taken by the labels (perpendicular to the axis, including tick marks and gaps) */
  labelThick: number;
  /** Thickness of the first-level tick labels themselves (outer levels of multi-level categories are drawn outside it) */
  coreThick: number;
  titleThick: number;
}

export interface Frame {
  plot: Rect;
  axes: Map<string, PlacedAxis>;
}

const TICK = 4;
const GAP = 3;

const visible = (a: AxisSpec) => !a.deleted;
const hasLabels = (a: AxisSpec) => !a.deleted && a.lblPos !== "none";

/** Text of value-axis tick labels */
export function valueLabel(spec: AxisSpec, v: number): string {
  const n = spec.dispUnit !== 1 ? v / spec.dispUnit : v;
  return formatValue(Number(n.toPrecision(12)), spec.format || "General");
}

function placeAxis(spec: AxisSpec, plot: Rect, area: Rect, side: Side): PlacedAxis {
  const len = spec.horizontal ? plot.w : plot.h;
  // Horizontal axes increase left to right; vertical axes increase bottom to top; swapped when reversed
  let a = spec.horizontal ? plot.x : plot.y + plot.h;
  let b = spec.horizontal ? plot.x + plot.w : plot.y;
  if (spec.reversed) [a, b] = [b, a];
  const st = spec.text;
  const lh = lineHeight(st);
  const placed: PlacedAxis = {
    spec,
    side,
    scale: null,
    a,
    b,
    span: 1,
    pos: (v) => a + (b - a) * v,
    center: () => (a + b) / 2,
    band: len,
    tickLabels: [],
    lblRot: 0,
    labelThick: 0,
    coreThick: 0,
    titleThick: 0,
  };
  const showLabels = hasLabels(spec);

  if (spec.role === "val" && spec.input) {
    // Major unit sets tick density from the available length; horizontal axes further reduce ticks by label width
    let sc = valueScale(spec.input, spec.horizontal ? 10 : Math.max(2, Math.floor(len / Math.max(lh * 1.75, 16))));
    if (spec.horizontal && !spec.input.major) {
      for (let k = 0; k < 6; k++) {
        const widest = Math.max(...sc.ticks.map((t) => measure(valueLabel(spec, t), st)), 1);
        const fit = Math.max(1, Math.floor(len / Math.max(widest + ptPx(st.size) * 2, ptPx(st.size) * 3.3)));
        const intervals = sc.ticks.length - 1;
        if (intervals <= fit) break;
        sc = valueScale(spec.input, Math.min(fit, intervals - 1));
      }
    }
    placed.scale = sc;
    placed.pos = (v) => a + (b - a) * fraction(sc, v);
    placed.center = placed.pos;
    if (showLabels) placed.tickLabels = sc.ticks.map((t) => ({ at: t, block: textBlock(valueLabel(spec, t), st) }));
    const maxW = Math.max(0, ...placed.tickLabels.map((l) => l.block.w));
    const maxH = Math.max(0, ...placed.tickLabels.map((l) => l.block.h));
    const rot = spec.rot ?? 0;
    if (rot) {
      placed.lblRot = rot;
      const r = Math.max(...placed.tickLabels.map((l) => rotatedSize(l.block.w, l.block.h, rot)[spec.horizontal ? "h" : "w"]), 0);
      placed.labelThick = showLabels ? r : 0;
    } else placed.labelThick = showLabels ? (spec.horizontal ? maxH : maxW) : 0;
  } else {
    const n = Math.max(spec.count, 1);
    const span = spec.between ? n : Math.max(n - 1, 1);
    placed.span = span;
    placed.pos = (u) => a + ((b - a) * u) / span;
    placed.center = (i) => placed.pos(spec.between ? i + 0.5 : i);
    placed.band = len / span;
    if (showLabels) {
      arrangeCategoryLabels(placed, area);
      placed.coreThick = placed.labelThick;
      if (spec.levels.length && side === "b") placed.labelThick += levelsThickness(spec);
    }
  }

  if (showLabels && placed.labelThick > 0) placed.labelThick += GAP + (spec.majorTick === "out" || spec.majorTick === "cross" ? TICK : 0);
  if (!spec.deleted && spec.extraThick) placed.labelThick += spec.extraThick;
  if (!spec.deleted && spec.title) {
    const sz = rotatedSize(spec.title.w, spec.title.h, spec.titleRot);
    placed.titleThick = (spec.horizontal ? sz.h : sz.w) + GAP * 2;
  }
  if (!spec.deleted && spec.dispLabel) placed.titleThick += spec.dispLabel.h + GAP;
  return placed;
}

/** Category labels: horizontal if they fit; wrapped if breakable (up to 3 lines); otherwise rotated 45 degrees, skipping some labels as needed */
function arrangeCategoryLabels(p: PlacedAxis, area: Rect) {
  const spec = p.spec;
  const st = spec.text;
  const lh = lineHeight(st);
  const n = spec.count;
  const slot = Math.abs(p.band);
  const texts = spec.labels.slice(0, n);
  while (texts.length < n) texts.push("");
  let skip = spec.lblSkip && spec.lblSkip > 0 ? spec.lblSkip : 0;
  const make = (rot: number, blocks: Block[]) => {
    p.lblRot = rot;
    p.tickLabels = blocks.map((block, i) => ({ at: i, block })).filter((_, i) => i % (skip || 1) === 0);
  };
  if (!spec.horizontal) {
    // Vertical category axis (bar chart): labels laid out horizontally, truncated when too long
    const maxW = area.w * 0.4;
    if (!skip) skip = Math.max(1, Math.ceil((lh * 1.0) / Math.max(slot, 1)));
    make(
      0,
      texts.map((t) => layoutBlock(plainLines(ellipsize(t, st, maxW), st))),
    );
    p.labelThick = Math.max(0, ...p.tickLabels.map((l) => l.block.w));
    return;
  }
  const blocks = texts.map((t) => textBlock(t, st));
  const rot = spec.rot;
  const maxThick = area.h * 0.4;
  if (rot === null || rot === 0) {
    const widest = Math.max(0, ...blocks.map((b) => b.w));
    const room = slot - 4;
    if (rot === 0 || widest <= room) {
      if (!skip && widest > room) skip = Math.ceil((widest + 4) / Math.max(slot, 1));
      make(0, blocks);
      p.labelThick = Math.max(0, ...p.tickLabels.map((l) => l.block.h));
      return;
    }
    // Try wrapping
    if (!skip && room > ptPx(st.size) * 2) {
      const wrapped = texts.map((t) => layoutBlock(wrapLines(plainLines(t, st), room)));
      if (wrapped.every((b) => b.lines.length <= 3 && b.w <= room + 0.5)) {
        make(0, wrapped);
        p.labelThick = Math.max(0, ...wrapped.map((b) => b.h));
        return;
      }
    }
  }
  const angle = rot ?? -45;
  const rad = (Math.abs(angle) * Math.PI) / 180;
  // Distance needed between adjacent labels along the axis after rotation
  const need = Math.abs(angle) >= 89 ? lh : lh / Math.sin(rad || 1e-6);
  if (!skip) skip = Math.max(1, Math.ceil(need / Math.max(slot, 1) - 0.05));
  const maxLen = maxThick / Math.max(Math.sin(rad), 0.2);
  make(
    angle,
    texts.map((t) => layoutBlock(plainLines(ellipsize(t, st, maxLen), st))),
  );
  p.labelThick = Math.max(0, ...p.tickLabels.map((l) => rotatedSize(l.block.w, l.block.h, angle).h));
}

/**
 * Compute the plot area: subtract the space needed for each axis's labels and title.
 * When inner is given (manualLayout's inner) it is used directly
 */
export function layoutFrame(area: Rect, specs: AxisSpec[], inner: Rect | null, minMargin: Partial<Record<Side, number>> = {}): Frame {
  const pad = 4;
  let plot: Rect = inner ?? { x: area.x + pad, y: area.y + pad, w: Math.max(area.w - pad * 2, 10), h: Math.max(area.h - pad * 2, 10) };
  let placed = new Map<string, PlacedAxis>();
  const sides = new Map<string, Side>(specs.map((sp) => [sp.id, sp.side]));
  const place = () => {
    placed = new Map(specs.map((sp) => [sp.id, placeAxis(sp, plot, area, sides.get(sp.id)!)]));
    const frame = { plot, axes: placed };
    let changed = false;
    for (const p of placed.values()) {
      const side = labelSide(frame, p);
      if (side !== sides.get(p.spec.id)) {
        sides.set(p.spec.id, side);
        changed = true;
      }
    }
    return changed;
  };
  for (let iter = 0; iter < 5; iter++) {
    const changed = place();
    if (inner) {
      if (changed) place();
      break;
    }
    const m = { l: pad, r: pad, t: pad, b: pad };
    for (const p of placed.values()) {
      if (!visible(p.spec)) continue;
      m[p.side] += p.labelThick + p.titleThick;
    }
    for (const k of ["l", "r", "t", "b"] as const) m[k] = Math.max(m[k], minMargin[k] ?? 0);
    // Parts of horizontal-axis labels overflowing left/right and vertical-axis labels overflowing top/bottom
    for (const p of placed.values()) {
      if (!hasLabels(p.spec) || !p.tickLabels.length) continue;
      const first = p.tickLabels[0];
      const last = p.tickLabels[p.tickLabels.length - 1];
      if (p.spec.horizontal) {
        const ends = [first, last].map((l) => ({ x: p.center(l.at), w: p.lblRot ? 0 : l.block.w / 2 }));
        for (const e of ends) {
          const left = plot.x - (e.x - e.w);
          const right = e.x + e.w - (plot.x + plot.w);
          m.l = Math.max(m.l, pad + left);
          m.r = Math.max(m.r, pad + right);
        }
        // Rotated labels extend toward the lower left
        if (p.lblRot < 0) {
          const ext = rotatedSize(first.block.w, first.block.h, p.lblRot).w - (p.center(first.at) - plot.x);
          m.l = Math.max(m.l, pad + ext);
        } else if (p.lblRot > 0) {
          const ext = rotatedSize(last.block.w, last.block.h, p.lblRot).w - (plot.x + plot.w - p.center(last.at));
          m.r = Math.max(m.r, pad + ext);
        }
      } else {
        const half = Math.max(first.block.h, last.block.h) / 2;
        m.t = Math.max(m.t, pad + half - 0);
        m.b = Math.max(m.b, pad + half);
      }
    }
    const next: Rect = { x: area.x + m.l, y: area.y + m.t, w: Math.max(area.w - m.l - m.r, 10), h: Math.max(area.h - m.t - m.b, 10) };
    const same = Math.abs(next.x - plot.x) < 0.5 && Math.abs(next.y - plot.y) < 0.5 && Math.abs(next.w - plot.w) < 0.5 && Math.abs(next.h - plot.h) < 0.5;
    plot = next;
    if (same && !changed) break;
  }
  if (!inner) place();
  return { plot, axes: placed };
}

/** Side for the labels: outward when the axis line (or the low/high label line) lies on the plot area edge, otherwise by axPos */
function labelSide(frame: Frame, p: PlacedAxis): Side {
  const spec = p.spec;
  const other = spec.crossAx ? frame.axes.get(spec.crossAx) : undefined;
  if (!other) return spec.side;
  const line = labelLine(frame, p);
  const pl = frame.plot;
  if (spec.horizontal) {
    if (Math.abs(line - pl.y) < 1) return "t";
    if (Math.abs(line - (pl.y + pl.h)) < 1) return "b";
  } else {
    if (Math.abs(line - pl.x) < 1) return "l";
    if (Math.abs(line - (pl.x + pl.w)) < 1) return "r";
  }
  return spec.side;
}

/** Line the tick labels follow: nextTo is beside the axis line; low/high are the min/max end of the crossing axis */
function labelLine(frame: Frame, p: PlacedAxis): number {
  const spec = p.spec;
  const line = crossPos(frame, p);
  const other = spec.crossAx ? frame.axes.get(spec.crossAx) : undefined;
  if (other && (spec.lblPos === "low" || spec.lblPos === "high")) {
    const low = spec.lblPos === "low";
    return other.scale ? other.pos(low ? other.scale.min : other.scale.max) : other.pos(low ? 0 : other.span);
  }
  return line;
}

/** Position where the axis crosses the other axis (pixel along the crossing axis direction) */
export function crossPos(frame: Frame, p: PlacedAxis): number {
  const other = p.spec.crossAx ? frame.axes.get(p.spec.crossAx) : undefined;
  if (!other) {
    const pl = frame.plot;
    const side = p.spec.side;
    return side === "b" ? pl.y + pl.h : side === "t" ? pl.y : side === "l" ? pl.x : pl.x + pl.w;
  }
  return crossValuePos(other, p.spec.crosses);
}

/** Pixel position on the crossing axis other that the crosses setting maps to */
export function crossValuePos(other: PlacedAxis, crosses: AxisSpec["crosses"]): number {
  if (other.scale) {
    const sc = other.scale;
    let v: number;
    if (crosses === "min") v = sc.min;
    else if (crosses === "max") v = sc.max;
    else if (crosses === "autoZero") v = sc.log ? sc.min : Math.min(Math.max(0, sc.min), sc.max);
    else v = Math.min(Math.max(crosses, sc.min), sc.max);
    return other.pos(v);
  }
  if (crosses === "max") return other.pos(other.span);
  if (typeof crosses === "number") return other.pos(Math.min(Math.max(crosses - 1, 0), other.span));
  return other.pos(0);
}

/** Baseline value for bars on the value axis (where the category axis crosses) */
export function baseValue(valAxis: PlacedAxis, catAxis: PlacedAxis | undefined): number {
  const sc = valAxis.scale!;
  const crosses = catAxis?.spec.crosses ?? "autoZero";
  if (crosses === "min") return sc.min;
  if (crosses === "max") return sc.max;
  if (typeof crosses === "number") return Math.min(Math.max(crosses, sc.min), sc.max);
  return sc.log ? sc.min : Math.min(Math.max(0, sc.min), sc.max);
}

// ───────────── Drawing ─────────────

/** Gridlines (placed below the data series) */
export function drawGrid(frame: Frame, g: SVGElement) {
  const pl = frame.plot;
  for (const p of frame.axes.values()) {
    const spec = p.spec;
    const lines: [number[], Stroke | null][] = [];
    if (p.scale) {
      lines.push([p.scale.minorTicks, spec.minorGrid]);
      lines.push([p.scale.ticks, spec.majorGrid]);
    } else {
      const n = spec.count;
      const edges = spec.between ? Array.from({ length: n + 1 }, (_, i) => i) : Array.from({ length: n }, (_, i) => i);
      lines.push([edges, spec.minorGrid]);
      lines.push([edges, spec.majorGrid]);
    }
    for (const [vals, st] of lines) {
      if (!st || !st.color) continue;
      let d = "";
      for (const v of vals) {
        const at = r2(p.pos(v));
        d += spec.horizontal ? `M${at},${r2(pl.y)}V${r2(pl.y + pl.h)}` : `M${r2(pl.x)},${at}H${r2(pl.x + pl.w)}`;
      }
      if (d) g.append(s("path", { d, fill: "none", ...strokeAttrs(st), "shape-rendering": "crispEdges" }));
    }
  }
}

/** Axis lines, tick marks, tick labels and axis titles */
export function drawAxes(frame: Frame, g: SVGElement) {
  const pl = frame.plot;
  // With multiple axes on the same side, titles stack outward
  const offsets: Record<Side, number> = { b: 0, t: 0, l: 0, r: 0 };
  for (const p of frame.axes.values()) {
    const spec = p.spec;
    if (spec.deleted) continue;
    const side = p.side;
    const sign = side === "b" || side === "r" ? 1 : -1;
    const line = crossPos(frame, p);
    const lblLine = labelLine(frame, p);
    const ticksAt: number[] = p.scale ? p.scale.ticks : Array.from({ length: spec.between ? spec.count + 1 : spec.count }, (_, i) => i);
    const minorAt: number[] = p.scale ? p.scale.minorTicks : [];
    const skipMarks = spec.markSkip && spec.markSkip > 1 ? spec.markSkip : 1;

    // Axis line
    if (spec.line && spec.line.color) {
      const d = spec.horizontal ? `M${r2(Math.min(p.a, p.b))},${r2(line)}H${r2(Math.max(p.a, p.b))}` : `M${r2(line)},${r2(Math.min(p.a, p.b))}V${r2(Math.max(p.a, p.b))}`;
      g.append(s("path", { d, fill: "none", ...strokeAttrs(spec.line), "shape-rendering": "crispEdges" }));
    }
    // Tick marks
    const tickStroke = spec.line && spec.line.color ? spec.line : null;
    const tickPath = (vals: number[], mark: TickMark, len: number) => {
      if (mark === "none" || !tickStroke) return;
      const out = mark === "out" || mark === "cross" ? len : 0;
      const inn = mark === "in" || mark === "cross" ? len : 0;
      let d = "";
      vals.forEach((v, i) => {
        if (i % skipMarks) return;
        const at = r2(p.pos(v));
        if (spec.horizontal) d += `M${at},${r2(line - inn * sign)}V${r2(line + out * sign)}`;
        else d += `M${r2(line - inn * sign)},${at}H${r2(line + out * sign)}`;
      });
      if (d) g.append(s("path", { d, fill: "none", ...strokeAttrs(tickStroke), "shape-rendering": "crispEdges" }));
    };
    tickPath(ticksAt, spec.majorTick, TICK);
    tickPath(minorAt, spec.minorTick, TICK / 2);

    // Tick labels
    const tickOut = spec.majorTick === "out" || spec.majorTick === "cross" ? TICK : 0;
    const off = tickOut + GAP;
    for (const lbl of p.tickLabels) {
      const at = p.center(lbl.at);
      const rot = p.lblRot;
      let x: number;
      let y: number;
      let h: HAlign;
      let v: VAlign;
      if (spec.horizontal) {
        x = at;
        y = lblLine + off * sign;
        if (!rot) {
          h = "middle";
          v = sign > 0 ? "top" : "bottom";
        } else {
          // Rotated labels align the end (or start) of the text with the tick position
          h = rot < 0 === sign > 0 ? "end" : "start";
          v = "middle";
        }
      } else {
        x = lblLine + off * sign;
        y = at;
        h = sign > 0 ? "start" : "end";
        v = "middle";
      }
      g.append(drawBlock(lbl.block, x, y, h, v, rot));
    }
    // Outer labels of multi-level categories
    if (spec.role === "cat" && spec.levels.length && side === "b" && hasLabels(spec)) drawLevels(p, lblLine + off + p.coreThick, g);

    // Axis title: placed outside the label area on that side
    const base = side === "b" ? pl.y + pl.h : side === "t" ? pl.y : side === "l" ? pl.x : pl.x + pl.w;
    const outer = base + sign * (offsets[side] + p.labelThick + GAP);
    if (spec.title) {
      const sz = rotatedSize(spec.title.w, spec.title.h, spec.titleRot);
      const thick = spec.horizontal ? sz.h : sz.w;
      const mid = (p.a + p.b) / 2;
      const c = outer + (sign * thick) / 2;
      g.append(spec.horizontal ? drawBlock(spec.title, mid, c, "middle", "middle", spec.titleRot) : drawBlock(spec.title, c, mid, "middle", "middle", spec.titleRot));
    }
    if (spec.dispLabel) {
      // Display unit label placed at the max end of the axis
      const blk = spec.dispLabel;
      if (spec.horizontal)
        g.append(drawBlock(blk, Math.max(p.a, p.b), outer + sign * (spec.title ? rotatedSize(spec.title.w, spec.title.h, spec.titleRot).h + GAP : 0), "end", sign > 0 ? "top" : "bottom"));
      else
        g.append(
          drawBlock(blk, outer + (sign * blk.h) / 2 + sign * (spec.title ? rotatedSize(spec.title.w, spec.title.h, spec.titleRot).w + GAP : 0), Math.min(p.a, p.b), "end", "middle", -90),
        );
    }
    offsets[side] += p.labelThick + p.titleThick;
  }
}

/** Multi-level categories: outer labels centered below the categories they span, levels separated by divider lines */
function drawLevels(p: PlacedAxis, top: number, g: SVGElement) {
  const spec = p.spec;
  const st = spec.text;
  const lh = lineHeight(st);
  let y = top + GAP;
  for (const level of spec.levels) {
    const n = spec.count;
    let start = 0;
    for (let i = 1; i <= n; i++) {
      if (i === n || level[i]) {
        const text = level[start] ?? "";
        if (text) {
          const x0 = p.pos(start);
          const x1 = p.pos(i);
          const w = Math.abs(x1 - x0);
          g.append(drawBlock(textBlock(ellipsize(text, st, w - 2), st), (x0 + x1) / 2, y, "middle", "top"));
        }
        start = i;
      }
    }
    y += lh + GAP;
  }
}

/** Thickness needed by the outer levels of multi-level categories */
export function levelsThickness(spec: AxisSpec) {
  return spec.levels.length * (lineHeight(spec.text) + GAP);
}
