/**
 * Conditional formatting (for preview): decides which format applies to each cell based on the cached values in the file.
 *
 * Supported: cell value comparisons (cellIs), formulas (expression), top/bottom N, above/below average, duplicate/unique values,
 * text contains/begins with/ends with, blanks, errors, color scales, data bars, icon sets. Applied from highest to lowest priority;
 * for each property the first matching rule wins (rules after stopIfTrue no longer apply to that cell).
 */

import { sheetColor, rgbaHex, type Theme } from "../core/theme";
import { WORK_LIMIT, evaluateAt, isErr, type Budget, type Value } from "./formula";
import { MAX_COLS, key, parseCellName, type Range, type Scalar, type Workbook } from "./model";

/** Differential format (dxf in styles.xml) */
export interface Dxf {
  bg?: string;
  color?: string;
  bold?: boolean;
  italic?: boolean;
  underline?: boolean;
  strike?: boolean;
}

export type IconKind = "up" | "right" | "down" | "circle" | "flag" | "check" | "cross" | "bang";

export interface CellDecoration extends Dxf {
  /** Data bar: start/end fractions (0–1) of the cell width, and color */
  bar?: { from: number; to: number; color: string };
  icon?: { kind: IconKind; color: string };
  /** Data bar / icon set configured to show only the graphic */
  hideValue?: boolean;
}

const child = (el: Element | null | undefined, name: string) => (el ? (Array.from(el.children).find((c) => c.localName === name) ?? null) : null);
const children = (el: Element | null | undefined, name: string) => (el ? Array.from(el.children).filter((c) => c.localName === name) : []);
const on = (el: Element | null) => !!el && el.getAttribute("val") !== "0" && el.getAttribute("val") !== "false";

/** dxfs from styles.xml */
export function readDxfs(styles: Document | null, theme: Theme | null, palette?: string[]): Dxf[] {
  const list = styles ? Array.from(styles.getElementsByTagNameNS("*", "dxfs")[0]?.children ?? []) : [];
  const color = (el: Element | null) => {
    const c = sheetColor(el, theme, palette);
    return c ? rgbaHex(c) : undefined;
  };
  return list.map((dxf) => {
    const d: Dxf = {};
    const font = child(dxf, "font");
    if (font) {
      if (child(font, "b")) d.bold = on(child(font, "b"));
      if (child(font, "i")) d.italic = on(child(font, "i"));
      if (child(font, "strike")) d.strike = on(child(font, "strike"));
      const u = child(font, "u");
      if (u) d.underline = u.getAttribute("val") !== "none";
      const fc = color(child(font, "color"));
      if (fc) d.color = fc;
    }
    // Conditional-format fill color lives in bgColor (with a solid pattern it may also be in fgColor)
    const pattern = child(child(dxf, "fill"), "patternFill");
    const bg = color(child(pattern, "bgColor")) ?? color(child(pattern, "fgColor"));
    if (bg && pattern?.getAttribute("patternType") !== "none") d.bg = bg;
    return d;
  });
}

// ───────────── Ranges and values ─────────────

function parseSqref(sqref: string, maxRow: number, maxCol: number): Range[] {
  const out: Range[] = [];
  for (const part of sqref.split(/\s+/).filter(Boolean)) {
    const [a, b = a] = part.replace(/\$/g, "").split(":");
    // Whole columns (A:A) and whole rows (1:1) are clamped to the data range
    const whole = (s: string, end: boolean): [number, number] | null => {
      if (/^[A-Z]+$/i.test(s)) {
        const p = parseCellName(`${s}1`);
        return p ? [end ? maxRow : 0, p[1]] : null;
      }
      if (/^\d+$/.test(s)) return [Number(s) - 1, end ? maxCol : 0];
      return parseCellName(s);
    };
    const p1 = whole(a, false);
    const p2 = whole(b, true);
    if (!p1 || !p2) continue;
    out.push({ r1: Math.min(p1[0], p2[0]), c1: Math.min(p1[1], p2[1]), r2: Math.min(Math.max(p1[0], p2[0]), maxRow), c2: Math.min(Math.max(p1[1], p2[1]), maxCol) });
  }
  return out;
}

/** Per-rule cap on cell-by-cell checks; for larger ranges only non-empty cells are checked (blank cells get no format) */
const MAX_RULE_CELLS = 200_000;
/** Time budget (ms) for evaluating a whole sheet's conditional formats: files may contain huge ranges or very slow formulas */
const TIME_BUDGET = 1500;
/**
 * Cell positions all of a sheet's rule formulas may visit together: a single slow formula call can't be interrupted
 * by the time budget, so the formulas stop themselves (#CALC!) once this is spent
 */
const WORK_BUDGET = 2 * WORK_LIMIT;

function area(ranges: Range[]) {
  return ranges.reduce((n, g) => n + (g.r2 - g.r1 + 1) * (g.c2 - g.c1 + 1), 0);
}

/** Cells within a rule's range that need checking */
function cellsIn(ranges: Range[], cells: Map<number, unknown>): [number, number][] {
  const out: [number, number][] = [];
  if (area(ranges) <= MAX_RULE_CELLS) {
    for (const g of ranges) for (let r = g.r1; r <= g.r2; r++) for (let c = g.c1; c <= g.c2; c++) out.push([r, c]);
    return out;
  }
  for (const k of cells.keys()) {
    const r = Math.floor(k / MAX_COLS);
    const c = k % MAX_COLS;
    if (ranges.some((g) => r >= g.r1 && r <= g.r2 && c >= g.c1 && c <= g.c2)) out.push([r, c]);
    if (out.length >= MAX_RULE_CELLS) break;
  }
  return out;
}

const isError = (v: Scalar) => typeof v === "string" && /^#(NULL!|DIV\/0!|VALUE!|REF!|NAME\?|NUM!|N\/A|SPILL!|CALC!)/.test(v);
const isBlank = (v: Scalar) => v === null || (typeof v === "string" && v.trim() === "");
const textOf = (v: Scalar) => (v === null ? "" : typeof v === "boolean" ? (v ? "TRUE" : "FALSE") : String(v)).toLowerCase();

/** Compare two values: number < text < boolean (Excel's order); text is case-insensitive */
function compare(a: Scalar | Value, b: Scalar | Value): number {
  if (isErr(a as Value) || isErr(b as Value)) return NaN;
  const rank = (v: Scalar | Value) => (typeof v === "number" || v === null ? 0 : typeof v === "string" ? 1 : 2);
  const x = a === null ? 0 : a;
  const y = b === null ? 0 : b;
  if (rank(x) !== rank(y)) return rank(x) - rank(y);
  if (typeof x === "number" && typeof y === "number") return x - y;
  if (typeof x === "boolean" && typeof y === "boolean") return Number(x) - Number(y);
  return textOf(x as Scalar).localeCompare(textOf(y as Scalar));
}

function truthy(v: Value) {
  if (isErr(v)) return false;
  if (typeof v === "boolean") return v;
  if (typeof v === "number") return v !== 0;
  return false;
}

function mix(a: string, b: string, t: number) {
  const ch = (h: string, i: number) => parseInt(h.slice(i, i + 2), 16);
  return `#${[1, 3, 5]
    .map((i) =>
      Math.round(ch(a, i) + (ch(b, i) - ch(a, i)) * t)
        .toString(16)
        .padStart(2, "0"),
    )
    .join("")}`;
}

/** Percentile (linear interpolation, same as Excel PERCENTILE) */
function percentile(sorted: number[], p: number) {
  if (!sorted.length) return 0;
  const i = (sorted.length - 1) * p;
  const lo = Math.floor(i);
  return sorted[lo] + (sorted[Math.min(lo + 1, sorted.length - 1)] - sorted[lo]) * (i - lo);
}

// ───────────── Evaluation ─────────────

export interface ConditionalInput {
  book: Workbook;
  sheetIndex: number;
  sheetDoc: Document;
  dxfs: Dxf[];
  theme: Theme | null;
  palette?: string[];
  /** Cell value (cached value) */
  value(r: number, c: number): Scalar;
}

const ICON_SETS: Record<string, { kind: IconKind; color: string }[]> = {
  "3Arrows": [
    { kind: "down", color: "#c0504d" },
    { kind: "right", color: "#e8a33d" },
    { kind: "up", color: "#3f9b4f" },
  ],
  "3ArrowsGray": [
    { kind: "down", color: "#808080" },
    { kind: "right", color: "#808080" },
    { kind: "up", color: "#808080" },
  ],
  "3TrafficLights1": [
    { kind: "circle", color: "#d6453d" },
    { kind: "circle", color: "#f0c33c" },
    { kind: "circle", color: "#3f9b4f" },
  ],
  "3Symbols": [
    { kind: "cross", color: "#d6453d" },
    { kind: "bang", color: "#f0c33c" },
    { kind: "check", color: "#3f9b4f" },
  ],
  "3Flags": [
    { kind: "flag", color: "#d6453d" },
    { kind: "flag", color: "#f0c33c" },
    { kind: "flag", color: "#3f9b4f" },
  ],
};

function iconsFor(set: string, n: number) {
  if (ICON_SETS[set]) return ICON_SETS[set];
  const arrows = /Arrows/.test(set);
  const base = ["#c0504d", "#e8763d", "#f0c33c", "#9bbb59", "#3f9b4f"];
  const kinds: IconKind[] = n === 3 ? ["down", "right", "up"] : n === 4 ? ["down", "down", "up", "up"] : ["down", "down", "right", "up", "up"];
  const colors = n === 3 ? [base[0], base[2], base[4]] : n === 4 ? [base[0], base[1], base[3], base[4]] : base;
  return colors.slice(0, n).map((color, i) => ({ kind: arrows ? kinds[i] : ("circle" as IconKind), color }));
}

export function computeConditional(input: ConditionalInput): Map<number, CellDecoration> {
  const { book, sheetIndex, sheetDoc, dxfs, theme, value } = input;
  const sheet = book.sheets[sheetIndex];
  const out = new Map<number, CellDecoration>();
  const stopped = new Set<number>();
  const deadline = performance.now() + TIME_BUDGET;
  const budget: Budget = { left: WORK_BUDGET };
  const outOfTime = () => performance.now() > deadline || budget.left <= 0;
  const color = (el: Element | null) => {
    const c = sheetColor(el, theme, input.palette);
    return c ? rgbaHex(c) : null;
  };
  const get = (si: number, r: number, c: number): Value => book.sheets[si]?.cells.get(key(r, c))?.v ?? null;
  // Whole-column ranges in formulas (AVERAGE($A:$A) etc.) need sorted cell keys: sort only once per sheet
  const keyCache = new Map<number, number[]>();
  const sortedKeys = (si: number) => {
    let k = keyCache.get(si);
    if (!k) keyCache.set(si, (k = Array.from(book.sheets[si]?.cells.keys() ?? []).sort((a, b) => a - b)));
    return k;
  };

  const rules: { rule: Element; ranges: Range[] }[] = [];
  for (const cf of Array.from(sheetDoc.getElementsByTagNameNS("*", "conditionalFormatting"))) {
    // x14 extension conditional formats (inside extLst) use xm:sqref; only the regular ones are handled here
    const ranges = parseSqref(cf.getAttribute("sqref") ?? "", sheet.maxRow, sheet.maxCol);
    if (!ranges.length) continue;
    for (const rule of children(cf, "cfRule")) rules.push({ rule, ranges });
  }
  rules.sort((a, b) => Number(a.rule.getAttribute("priority") ?? 1e9) - Number(b.rule.getAttribute("priority") ?? 1e9));

  const deco = (k: number) => {
    let d = out.get(k);
    if (!d) out.set(k, (d = {}));
    return d;
  };
  const applyDxf = (k: number, dxf: Dxf | undefined) => {
    if (!dxf) return;
    const d = deco(k);
    for (const p of ["bg", "color", "bold", "italic", "underline", "strike"] as const) if (d[p] === undefined && dxf[p] !== undefined) (d as Record<string, unknown>)[p] = dxf[p];
  };

  for (const { rule, ranges } of rules) {
    const type = rule.getAttribute("type");
    const dxf = rule.getAttribute("dxfId") !== null ? dxfs[Number(rule.getAttribute("dxfId"))] : undefined;
    const stop = rule.getAttribute("stopIfTrue") === "1";
    const formulas = children(rule, "formula").map((f) => f.textContent ?? "");
    const base: [number, number] = [ranges[0].r1, ranges[0].c1];
    // Time budget exceeded: skip the remaining rules (the preview still renders normally)
    if (outOfTime()) break;
    const range = cellsIn(ranges, sheet.cells);
    const all = range.filter(([r, c]) => !stopped.has(key(r, c)));
    const nums = () =>
      range
        .map(([r, c]) => value(r, c))
        .filter((v): v is number => typeof v === "number")
        .sort((a, b) => a - b);
    const matched = (test: (v: Scalar, r: number, c: number) => boolean) => {
      const hits: number[] = [];
      for (let i = 0; i < all.length; i++) {
        // Formulas may be slow: check the time every 256 cells; if over budget the whole rule is not applied
        if (((i & 255) === 0 && performance.now() > deadline) || budget.left <= 0) return;
        const [r, c] = all[i];
        if (test(value(r, c), r, c)) hits.push(key(r, c));
      }
      // The formulas ran out of work: some cells weren't really checked
      if (budget.left <= 0) return;
      for (const k of hits) {
        applyDxf(k, dxf);
        if (stop) stopped.add(k);
      }
    };
    const evalF = (f: string, r: number, c: number) => evaluateAt(book, f, sheetIndex, base, [r, c], get, sortedKeys, budget);

    switch (type) {
      case "cellIs": {
        const op = rule.getAttribute("operator") ?? "equal";
        matched((v, r, c) => {
          const a = evalF(formulas[0] ?? "", r, c);
          const b = formulas[1] !== undefined ? evalF(formulas[1], r, c) : null;
          const x = compare(v, a);
          switch (op) {
            case "between":
              return x >= 0 && compare(v, b) <= 0;
            case "notBetween":
              return x < 0 || compare(v, b) > 0;
            case "equal":
              return x === 0;
            case "notEqual":
              return x !== 0;
            case "greaterThan":
              return x > 0;
            case "lessThan":
              return x < 0;
            case "greaterThanOrEqual":
              return x >= 0;
            case "lessThanOrEqual":
              return x <= 0;
            default:
              return false;
          }
        });
        break;
      }
      case "expression":
        matched((_v, r, c) => truthy(evalF(formulas[0] ?? "", r, c)));
        break;
      case "top10": {
        const list = nums();
        const rank = Number(rule.getAttribute("rank") ?? 10);
        const n = rule.getAttribute("percent") === "1" ? Math.max(1, Math.floor((list.length * rank) / 100)) : rank;
        const bottom = rule.getAttribute("bottom") === "1";
        const limit = bottom ? list[Math.min(n, list.length) - 1] : list[Math.max(0, list.length - n)];
        matched((v) => typeof v === "number" && (bottom ? v <= limit : v >= limit));
        break;
      }
      case "aboveAverage": {
        const list = nums();
        const avg = list.reduce((a, b) => a + b, 0) / (list.length || 1);
        const sd = Math.sqrt(list.reduce((a, b) => a + (b - avg) ** 2, 0) / (list.length || 1));
        const above = rule.getAttribute("aboveAverage") !== "0";
        const equal = rule.getAttribute("equalAverage") === "1";
        const dev = Number(rule.getAttribute("stdDev") ?? 0) * sd;
        matched((v) => {
          if (typeof v !== "number") return false;
          const t = above ? avg + dev : avg - dev;
          return above ? v > t || (equal && v === t) : v < t || (equal && v === t);
        });
        break;
      }
      case "duplicateValues":
      case "uniqueValues": {
        const count = new Map<string, number>();
        for (const [r, c] of all) {
          const v = value(r, c);
          if (!isBlank(v)) count.set(textOf(v), (count.get(textOf(v)) ?? 0) + 1);
        }
        const dup = type === "duplicateValues";
        matched((v) => !isBlank(v) && (count.get(textOf(v)) ?? 0) > 1 === dup);
        break;
      }
      case "containsText":
      case "notContainsText":
      case "beginsWith":
      case "endsWith": {
        const text = (rule.getAttribute("text") ?? "").toLowerCase();
        matched((v) => {
          const s = textOf(v);
          if (type === "containsText") return s.includes(text);
          if (type === "notContainsText") return !s.includes(text);
          if (type === "beginsWith") return s.startsWith(text);
          return s.endsWith(text);
        });
        break;
      }
      case "containsBlanks":
        matched((v) => isBlank(v));
        break;
      case "notContainsBlanks":
        matched((v) => !isBlank(v));
        break;
      case "containsErrors":
        matched((v) => isError(v));
        break;
      case "notContainsErrors":
        matched((v) => !isError(v));
        break;
      case "colorScale": {
        const scale = child(rule, "colorScale");
        const cfvos = children(scale, "cfvo");
        const colors = children(scale, "color").map((c) => color(c) ?? "#ffffff");
        const list = nums();
        if (!list.length || cfvos.length < 2) break;
        const stops = cfvos.map((cf) => threshold(cf, list, (f) => evalF(f, base[0], base[1])));
        for (const [r, c] of all) {
          const v = value(r, c);
          if (typeof v !== "number") continue;
          const d = deco(key(r, c));
          if (d.bg !== undefined) continue;
          let i = 0;
          while (i < stops.length - 2 && v > stops[i + 1]) i++;
          const lo = stops[i];
          const hi = stops[i + 1];
          const t = hi === lo ? 0 : Math.min(1, Math.max(0, (v - lo) / (hi - lo)));
          d.bg = mix(colors[i], colors[i + 1] ?? colors[i], t);
        }
        break;
      }
      case "dataBar": {
        const bar = child(rule, "dataBar");
        const cfvos = children(bar, "cfvo");
        const barColor = color(child(bar, "color")) ?? "#638ec6";
        const list = nums();
        if (!list.length) break;
        const lo = cfvos[0] ? threshold(cfvos[0], list, (f) => evalF(f, base[0], base[1])) : list[0];
        const hi = cfvos[1] ? threshold(cfvos[1], list, (f) => evalF(f, base[0], base[1])) : list[list.length - 1];
        const hide = bar?.getAttribute("showValue") === "0";
        const span = hi - lo || 1;
        // With negative values the axis is at 0: positive values extend right, negative left (red), as in Excel 2010+
        const axis = lo < 0 && hi > 0 ? -lo / span : null;
        for (const [r, c] of all) {
          const v = value(r, c);
          if (typeof v !== "number") continue;
          const d = deco(key(r, c));
          if (d.bar) continue;
          if (axis !== null) {
            const end = axis + v / span;
            d.bar = v >= 0 ? { from: axis, to: Math.min(1, end), color: barColor } : { from: Math.max(0, end), to: axis, color: "#ff0000" };
          } else {
            const t = Math.min(1, Math.max(0, (v - lo) / span));
            // Default length 10%–90% (Excel 2007 look)
            d.bar = { from: 0, to: 0.1 + t * 0.8, color: barColor };
          }
          if (hide) d.hideValue = true;
        }
        break;
      }
      case "iconSet": {
        const set = child(rule, "iconSet");
        const name = set?.getAttribute("iconSet") ?? "3TrafficLights1";
        const cfvos = children(set, "cfvo");
        const icons = iconsFor(name, cfvos.length || 3);
        const reverse = set?.getAttribute("reverse") === "1";
        const hide = set?.getAttribute("showValue") === "0";
        const list = nums();
        if (!list.length) break;
        const limits = cfvos.map((cf) => threshold(cf, list, (f) => evalF(f, base[0], base[1])));
        for (const [r, c] of all) {
          const v = value(r, c);
          if (typeof v !== "number") continue;
          const d = deco(key(r, c));
          if (d.icon) continue;
          let i = 0;
          for (let j = 1; j < limits.length; j++) if (v >= limits[j]) i = j;
          const icon = icons[reverse ? icons.length - 1 - i : i] ?? icons[0];
          d.icon = icon;
          if (hide) d.hideValue = true;
        }
        break;
      }
    }
  }
  return out;
}

/** Thresholds for color scales, data bars and icon sets (cfvo) */
function threshold(cfvo: Element, sorted: number[], evalF: (f: string) => Value): number {
  const type = cfvo.getAttribute("type");
  const raw = cfvo.getAttribute("val") ?? "";
  const min = sorted[0];
  const max = sorted[sorted.length - 1];
  const num = () => {
    const n = Number(raw);
    if (Number.isFinite(n)) return n;
    const v = evalF(raw);
    return typeof v === "number" ? v : 0;
  };
  switch (type) {
    case "min":
    case "autoMin":
      return min;
    case "max":
    case "autoMax":
      return max;
    case "percent":
      return min + ((max - min) * num()) / 100;
    case "percentile":
      return percentile(sorted, num() / 100);
    default:
      return num();
  }
}
