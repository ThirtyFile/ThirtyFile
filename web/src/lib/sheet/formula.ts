/**
 * Formula engine.
 *
 * A "lexing → syntax tree → interpretation" pipeline, kept small for lightweight editing in a cloud drive:
 * - A single Pratt parser builds the syntax tree; parse results are cached by formula text
 * - No dependency tree: the calculation cache is cleared after each edit and only cells actually shown get computed (lazy evaluation + memoization),
 *   with an "in progress" set to detect circular references; long dependency chains are evaluated in stretches from a worklist
 * - Range operations only visit cells that actually contain data instead of scanning whole columns
 * - About 70 common functions are provided; other functions return #NAME?
 */
import { MAX_COLS, MAX_ROWS, colIndex, colName, colOf, key, rowOf, type Scalar, type Workbook } from "./model";
import { formatValue } from "./format";

// ───────────── Values ─────────────

export class FormulaError {
  readonly code: string;
  constructor(code: string) {
    this.code = code;
  }
  toString() {
    return this.code;
  }
}

const ERR = {
  div0: new FormulaError("#DIV/0!"),
  value: new FormulaError("#VALUE!"),
  ref: new FormulaError("#REF!"),
  name: new FormulaError("#NAME?"),
  num: new FormulaError("#NUM!"),
  na: new FormulaError("#N/A"),
  cycle: new FormulaError("#CYCLE!"),
};
const ERROR_CODES = new Map(Object.values(ERR).map((e) => [e.code, e]));
ERROR_CODES.set("#NULL!", new FormulaError("#NULL!"));

export type Value = Scalar | FormulaError;
interface RangeRef {
  kind: "range";
  sheet: number;
  r1: number;
  c1: number;
  r2: number;
  c2: number;
}
type Result = Value | RangeRef;

const isRange = (v: unknown): v is RangeRef => typeof v === "object" && v !== null && (v as RangeRef).kind === "range";
const isErr = (v: unknown): v is FormulaError => v instanceof FormulaError;

// ───────────── Syntax tree ─────────────

type Node =
  | { t: "num"; v: number }
  | { t: "str"; v: string }
  | { t: "bool"; v: boolean }
  | { t: "err"; v: FormulaError }
  | { t: "empty" }
  | { t: "ref"; sheet?: string; r1: number; c1: number; r2: number; c2: number }
  | { t: "un"; op: string; a: Node }
  | { t: "pct"; a: Node }
  | { t: "bin"; op: string; a: Node; b: Node }
  | { t: "call"; name: string; args: Node[] };

// ───────────── Lexing ─────────────

type Tok =
  | { k: "num"; v: number }
  | { k: "str"; v: string }
  | { k: "err"; v: string }
  | { k: "ref"; sheet?: string; a: string; b?: string }
  | { k: "func"; v: string }
  | { k: "name"; v: string }
  | { k: "op"; v: string };

const SHEET_PREFIX = String.raw`(?:'((?:[^']|'')+)'|([A-Za-z_À-￿][\w.À-￿]*))!`;
const CELL = String.raw`\$?[A-Za-z]{1,3}\$?\d+`;
const COL = String.raw`\$?[A-Za-z]{1,3}`;
const ROW = String.raw`\$?\d+`;
const REF_RE = new RegExp(String.raw`^(?:${SHEET_PREFIX})?(?:(${CELL})(?::(${CELL}))?|(${COL}):(${COL})|(${ROW}):(${ROW}))(?![\w(])`);

function tokenize(src: string): Tok[] {
  const out: Tok[] = [];
  let i = 0;
  while (i < src.length) {
    const ch = src[i];
    if (/\s/.test(ch)) {
      i++;
      continue;
    }
    const rest = src.slice(i);
    if (ch === '"') {
      let j = i + 1;
      let s = "";
      for (; j < src.length; j++) {
        if (src[j] === '"') {
          if (src[j + 1] === '"') {
            s += '"';
            j++;
          } else break;
        } else s += src[j];
      }
      if (j >= src.length) throw ERR.value;
      out.push({ k: "str", v: s });
      i = j + 1;
      continue;
    }
    if (ch === "#") {
      const m = /^#(?:DIV\/0!|VALUE!|REF!|NAME\?|NUM!|N\/A|NULL!)/i.exec(rest);
      if (!m) throw ERR.name;
      out.push({ k: "err", v: m[0].toUpperCase() });
      i += m[0].length;
      continue;
    }
    const num = /^(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?/.exec(rest);
    // "1:3" is a whole-row reference, not a number
    if (num && !/^\d+:\$?\d/.test(rest)) {
      out.push({ k: "num", v: Number(num[0]) });
      i += num[0].length;
      continue;
    }
    const ref = REF_RE.exec(rest);
    if (ref) {
      const sheet = ref[1] !== undefined ? ref[1].replace(/''/g, "'") : ref[2];
      const a = ref[3] ?? ref[5] ?? ref[7];
      const b = ref[4] ?? ref[6] ?? ref[8];
      out.push({ k: "ref", sheet, a, b });
      i += ref[0].length;
      continue;
    }
    const id = /^[A-Za-z_À-￿][\w.À-￿]*/.exec(rest);
    if (id) {
      const after = src.slice(i + id[0].length).trimStart();
      out.push(after.startsWith("(") ? { k: "func", v: id[0].toUpperCase() } : { k: "name", v: id[0].toUpperCase() });
      i += id[0].length;
      continue;
    }
    const op = /^(?:<>|<=|>=|[-+*/^&=<>%(),;])/.exec(rest);
    if (!op) throw ERR.name;
    out.push({ k: "op", v: op[0] === ";" ? "," : op[0] });
    i += op[0].length;
  }
  return out;
}

// ───────────── Parsing (Pratt) ─────────────

const BIN_PREC: Record<string, number> = { "=": 1, "<>": 1, "<": 1, ">": 1, "<=": 1, ">=": 1, "&": 2, "+": 3, "-": 3, "*": 4, "/": 4, "^": 5 };

function parseRefPart(s: string, whole: "col" | "row" | null): [number, number] {
  const clean = s.replace(/\$/g, "");
  if (whole === "col") return [0, colIndex(clean)];
  if (whole === "row") return [Number(clean) - 1, 0];
  const m = /^([A-Za-z]+)(\d+)$/.exec(clean)!;
  return [Number(m[2]) - 1, colIndex(m[1])];
}

function refNode(t: Extract<Tok, { k: "ref" }>): Node {
  const whole = /^\$?[A-Za-z]+$/.test(t.a) ? "col" : /^\$?\d+$/.test(t.a) ? "row" : null;
  const [r1, c1] = parseRefPart(t.a, whole);
  let [r2, c2] = t.b ? parseRefPart(t.b, whole) : [r1, c1];
  if (whole === "col") r2 = MAX_ROWS - 1;
  if (whole === "row") c2 = MAX_COLS - 1;
  if (r1 < 0 || c1 < 0 || r2 < 0 || c2 < 0 || c1 >= MAX_COLS || c2 >= MAX_COLS) return { t: "err", v: ERR.ref };
  return { t: "ref", sheet: t.sheet, r1: Math.min(r1, r2), c1: Math.min(c1, c2), r2: Math.max(r1, r2), c2: Math.max(c1, c2) };
}

function parse(src: string): Node {
  const toks = tokenize(src);
  let pos = 0;
  const peek = () => toks[pos];
  const isOp = (v: string) => toks[pos]?.k === "op" && (toks[pos] as { v: string }).v === v;
  const expect = (v: string) => {
    if (!isOp(v)) throw ERR.value;
    pos++;
  };

  const primary = (): Node => {
    const t = toks[pos++];
    if (!t) throw ERR.value;
    switch (t.k) {
      case "num":
        return { t: "num", v: t.v };
      case "str":
        return { t: "str", v: t.v };
      case "err":
        return { t: "err", v: ERROR_CODES.get(t.v) ?? ERR.value };
      case "ref":
        return refNode(t);
      case "name":
        if (t.v === "TRUE" || t.v === "FALSE") return { t: "bool", v: t.v === "TRUE" };
        return { t: "err", v: ERR.name };
      case "func": {
        expect("(");
        const args: Node[] = [];
        if (!isOp(")")) {
          for (;;) {
            args.push(isOp(",") || isOp(")") ? { t: "empty" } : expr(0));
            if (isOp(",")) {
              pos++;
              continue;
            }
            break;
          }
        }
        expect(")");
        return { t: "call", name: t.v.replace(/^_XLFN\./, ""), args };
      }
      case "op":
        if (t.v === "(") {
          const e = expr(0);
          expect(")");
          return e;
        }
        if (t.v === "-" || t.v === "+") return { t: "un", op: t.v, a: unary() };
        throw ERR.value;
    }
  };

  // Prefix sign binds tighter than ^ (Excel: -2^2 = 4); postfix % binds tighter still
  const unary = (): Node => {
    let n = primary();
    while (isOp("%")) {
      pos++;
      n = { t: "pct", a: n };
    }
    return n;
  };

  const expr = (min: number): Node => {
    let left = unary();
    for (;;) {
      const t = peek();
      if (!t || t.k !== "op") break;
      const prec = BIN_PREC[t.v];
      if (prec === undefined || prec < min) break;
      pos++;
      // Parse the right side at higher precedence → operators of equal precedence are left-associative (Excel's ^ is left-associative too: 2^3^2 = 64)
      const right = expr(prec + 1);
      left = { t: "bin", op: t.v, a: left, b: right };
    }
    return left;
  };

  const node = expr(0);
  if (pos < toks.length) throw ERR.value;
  return node;
}

const parseCache = new Map<string, Node | FormulaError>();
function parsed(formula: string): Node | FormulaError {
  let n = parseCache.get(formula);
  if (!n) {
    try {
      n = parse(formula.replace(/^=/, ""));
    } catch (e) {
      // Running out of stack says nothing about the formula: don't remember it as an error
      if (e instanceof RangeError) throw e;
      n = isErr(e) ? e : ERR.value;
    }
    if (parseCache.size > 5000) parseCache.clear();
    parseCache.set(formula, n);
  }
  return n;
}

/** Formula syntax check: returns an error code or null */
export function checkFormula(formula: string) {
  const n = parsed(formula);
  return isErr(n) ? n.code : null;
}

// ───────────── Formula shifting (shared formulas, copy/paste) ─────────────

/** Shift relative references in a formula by (dr, dc); absolute references ($), quoted strings and function names are left untouched */
export function shiftFormula(formula: string, dr: number, dc: number) {
  if (dr === 0 && dc === 0) return formula;
  return formula
    .split(/("(?:[^"]|"")*"|'(?:[^']|'')*'!)/)
    .map((part, i) =>
      i % 2 === 1
        ? part
        : part.replace(/(^|[^A-Za-z0-9_.$])(\$?)([A-Za-z]{1,3})(\$?)(\d+)(?![A-Za-z0-9_(])/g, (_m, pre, cAbs, col, rAbs, row) => {
            const r = Number(row) - 1;
            const c = colIndex(col);
            const nr = rAbs ? r : r + dr;
            const nc = cAbs ? c : c + dc;
            if (nr < 0 || nc < 0 || nr >= MAX_ROWS || nc >= MAX_COLS) return `${pre}#REF!`;
            return `${pre}${cAbs}${colName(nc)}${rAbs}${nr + 1}`;
          }),
    )
    .join("");
}

// ───────────── Type conversion and comparison ─────────────

function toNumber(v: Value): number | FormulaError {
  if (isErr(v)) return v;
  if (typeof v === "number") return v;
  if (typeof v === "boolean") return v ? 1 : 0;
  if (v === null || v === "") return 0;
  const n = parseNumber(v);
  return n === null ? ERR.value : n;
}

/** Text to number: supports thousands separators and percentages */
export function parseNumber(s: string): number | null {
  const t = s.trim().replace(/,/g, "");
  if (!t) return null;
  const pct = t.endsWith("%");
  const body = pct ? t.slice(0, -1) : t;
  if (!/^[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?$/.test(body)) return null;
  const n = Number(body);
  return pct ? n / 100 : n;
}

function toText(v: Value): string | FormulaError {
  if (isErr(v)) return v;
  if (v === null) return "";
  if (typeof v === "boolean") return v ? "TRUE" : "FALSE";
  if (typeof v === "number") return formatValue(v);
  return v;
}

function toBool(v: Value): boolean | FormulaError {
  if (isErr(v)) return v;
  if (typeof v === "boolean") return v;
  if (typeof v === "number") return v !== 0;
  if (v === null || v === "") return false;
  const u = v.toUpperCase();
  if (u === "TRUE") return true;
  if (u === "FALSE") return false;
  return ERR.value;
}

/** Excel comparison rules: number < text < logical; text is case-insensitive; blank counts as 0 or empty string */
function compare(a: Value, b: Value): number {
  if (a === null) a = typeof b === "string" ? "" : typeof b === "boolean" ? false : 0;
  if (b === null) b = typeof a === "string" ? "" : typeof a === "boolean" ? false : 0;
  const rank = (v: Value) => (typeof v === "number" ? 0 : typeof v === "string" ? 1 : 2);
  const ra = rank(a);
  const rb = rank(b);
  if (ra !== rb) return ra - rb;
  if (typeof a === "string" && typeof b === "string") return a.localeCompare(b, "zh-Hant", { sensitivity: "accent" });
  return (a as number) < (b as number) ? -1 : (a as number) > (b as number) ? 1 : 0;
}

// ───────────── Dates ─────────────

const DAY = 86400000;
const EPOCH = Date.UTC(1899, 11, 30);

/** Excel serial number → year/month/day (1900 date system) */
export function serialToDate(serial: number) {
  return new Date(EPOCH + Math.round(serial * DAY));
}

export function dateToSerial(y: number, m: number, d: number) {
  return (Date.UTC(y, m - 1, d) - EPOCH) / DAY;
}

// ───────────── Evaluation ─────────────

export interface EvalContext {
  book: Workbook;
  /** Sheet and cell of the formula being evaluated */
  sheet: number;
  row: number;
  col: number;
  /** Get a cell's value (formula cells are computed recursively) */
  get(sheet: number, r: number, c: number): Value;
  /** Keys of all cells in the sheet (ascending); cached by the calculator and rebuilt only when data changes */
  sortedKeys?(sheet: number): number[];
}

/** Index of the first element >= target in a sorted array */
function lowerBound(arr: number[], target: number) {
  let lo = 0;
  let hi = arr.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (arr[mid] < target) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

function sheetIndex(ctx: EvalContext, name?: string) {
  if (name === undefined) return ctx.sheet;
  const lower = name.toLowerCase();
  return ctx.book.sheets.findIndex((s) => s.name.toLowerCase() === lower);
}

function evalNode(n: Node, ctx: EvalContext): Result {
  switch (n.t) {
    case "num":
    case "str":
    case "bool":
      return n.v;
    case "err":
      return n.v;
    case "empty":
      return null;
    case "ref": {
      const s = sheetIndex(ctx, n.sheet);
      if (s < 0) return ERR.ref;
      if (n.r1 === n.r2 && n.c1 === n.c2) return ctx.get(s, n.r1, n.c1);
      return { kind: "range", sheet: s, r1: n.r1, c1: n.c1, r2: n.r2, c2: n.c2 };
    }
    case "un": {
      const v = toNumber(scalar(evalNode(n.a, ctx), ctx));
      return isErr(v) ? v : n.op === "-" ? -v : v;
    }
    case "pct": {
      const v = toNumber(scalar(evalNode(n.a, ctx), ctx));
      return isErr(v) ? v : v / 100;
    }
    case "bin": {
      const a = scalar(evalNode(n.a, ctx), ctx);
      const b = scalar(evalNode(n.b, ctx), ctx);
      if (isErr(a)) return a;
      if (isErr(b)) return b;
      if (n.op === "&") {
        const x = toText(a);
        const y = toText(b);
        return isErr(x) ? x : isErr(y) ? y : x + y;
      }
      if (["=", "<>", "<", ">", "<=", ">="].includes(n.op)) {
        const c = compare(a, b);
        return n.op === "=" ? c === 0 : n.op === "<>" ? c !== 0 : n.op === "<" ? c < 0 : n.op === ">" ? c > 0 : n.op === "<=" ? c <= 0 : c >= 0;
      }
      const x = toNumber(a);
      const y = toNumber(b);
      if (isErr(x)) return x;
      if (isErr(y)) return y;
      switch (n.op) {
        case "+":
          return x + y;
        case "-":
          return x - y;
        case "*":
          return x * y;
        case "/":
          return y === 0 ? ERR.div0 : x / y;
        case "^": {
          const p = Math.pow(x, y);
          return Number.isFinite(p) ? p : ERR.num;
        }
      }
      return ERR.value;
    }
    case "call": {
      const fn = FUNCTIONS[n.name];
      if (!fn) return ERR.name;
      return fn(n.args, ctx);
    }
  }
}

/** Range in a single-value position: a single cell yields its value; same row or column yields the intersection (Excel's implicit intersection) */
function scalar(v: Result, ctx: EvalContext): Value {
  if (!isRange(v)) return v;
  if (v.r1 === v.r2 && v.c1 === v.c2) return ctx.get(v.sheet, v.r1, v.c1);
  if (v.c1 === v.c2 && ctx.row >= v.r1 && ctx.row <= v.r2) return ctx.get(v.sheet, ctx.row, v.c1);
  if (v.r1 === v.r2 && ctx.col >= v.c1 && ctx.col <= v.c2) return ctx.get(v.sheet, v.r1, ctx.col);
  return ERR.value;
}

/** Effective bounds of a range: anything beyond the sheet's data range is blank */
function bounded(ctx: EvalContext, g: RangeRef) {
  const s = ctx.book.sheets[g.sheet];
  return { r2: Math.min(g.r2, s.maxRow), c2: Math.min(g.c2, s.maxCol) };
}

/** Visit non-empty cells in a range; when the range exceeds the data size, iterate the data instead to avoid scanning whole columns */
function eachInRange(ctx: EvalContext, g: RangeRef, fn: (v: Value, r: number, c: number) => void) {
  const sheet = ctx.book.sheets[g.sheet];
  const { r2, c2 } = bounded(ctx, g);
  if (r2 < g.r1 || c2 < g.c1) return;
  const area = (r2 - g.r1 + 1) * (c2 - g.c1 + 1);
  if (area > sheet.cells.size * 2) {
    // Keys are sorted row-major: binary-search straight to the range's first row and visit only rows inside the range
    const keys = ctx.sortedKeys?.(g.sheet) ?? Array.from(sheet.cells.keys()).sort((a, b) => a - b);
    const last = key(r2, MAX_COLS - 1);
    for (let i = lowerBound(keys, key(g.r1, 0)); i < keys.length && keys[i] <= last; i++) {
      const c = colOf(keys[i]);
      if (c >= g.c1 && c <= c2) fn(ctx.get(g.sheet, rowOf(keys[i]), c), rowOf(keys[i]), c);
    }
    return;
  }
  for (let r = g.r1; r <= r2; r++) for (let c = g.c1; c <= c2; c++) if (sheet.cells.has(key(r, c))) fn(ctx.get(g.sheet, r, c), r, c);
}

/** Range to a 2D array (lookup functions need positions) */
function matrix(ctx: EvalContext, g: RangeRef): Value[][] {
  const { r2, c2 } = bounded(ctx, g);
  const rows = Math.max(0, Math.min(g.r2, Math.max(r2, g.r1)) - g.r1 + 1);
  const cols = Math.max(0, Math.min(g.c2, Math.max(c2, g.c1)) - g.c1 + 1);
  const out: Value[][] = [];
  for (let r = 0; r < rows; r++) {
    const row: Value[] = [];
    for (let c = 0; c < cols; c++) row.push(ctx.get(g.sheet, g.r1 + r, g.c1 + c));
    out.push(row);
  }
  return out;
}

// ───────────── Functions ─────────────

type Fn = (args: Node[], ctx: EvalContext) => Result;

const val = (n: Node | undefined, ctx: EvalContext): Value => (n ? scalar(evalNode(n, ctx), ctx) : null);
const num = (n: Node | undefined, ctx: EvalContext, dflt?: number): number | FormulaError => {
  if (!n || n.t === "empty") return dflt ?? 0;
  return toNumber(val(n, ctx));
};
const str = (n: Node | undefined, ctx: EvalContext): string | FormulaError => toText(val(n, ctx));

/** Collect numbers per Excel rules: text numbers and logicals typed directly are counted; in ranges only numbers count */
function collectNumbers(args: Node[], ctx: EvalContext): number[] | FormulaError {
  const out: number[] = [];
  for (const a of args) {
    const v = evalNode(a, ctx);
    if (isRange(v)) {
      let err: FormulaError | null = null;
      eachInRange(ctx, v, (x) => {
        if (isErr(x)) err ??= x;
        else if (typeof x === "number") out.push(x);
      });
      if (err) return err;
    } else if (isErr(v)) return v;
    else if (v !== null) {
      const n = toNumber(v);
      if (isErr(n)) return n;
      out.push(n);
    }
  }
  return out;
}

const numbersFn =
  (f: (xs: number[]) => Value): Fn =>
  (args, ctx) => {
    const xs = collectNumbers(args, ctx);
    return isErr(xs) ? xs : f(xs);
  };

const math1 =
  (f: (x: number) => number): Fn =>
  (args, ctx) => {
    const x = num(args[0], ctx);
    if (isErr(x)) return x;
    const y = f(x);
    return Number.isFinite(y) ? y : ERR.num;
  };

function roundTo(x: number, digits: number, mode: "round" | "up" | "down") {
  const f = Math.pow(10, digits);
  const v = Math.abs(x) * f;
  // Fix floating-point error (e.g. 1.005 * 100 = 100.49999…)
  const fixed = Number(v.toPrecision(15));
  const r = mode === "round" ? Math.round(fixed) : mode === "up" ? Math.ceil(fixed) : Math.floor(fixed);
  return (Math.sign(x) * r) / f;
}

const roundFn =
  (mode: "round" | "up" | "down"): Fn =>
  (args, ctx) => {
    const x = num(args[0], ctx);
    const d = num(args[1], ctx);
    if (isErr(x)) return x;
    if (isErr(d)) return d;
    return roundTo(x, Math.trunc(d), mode);
  };

/** SUMIF/COUNTIF criteria, e.g. ">=10", "<>done", "wang*" */
function criteria(c: Value): (v: Value) => boolean {
  if (typeof c === "number" || typeof c === "boolean") return (v) => compare(v, c) === 0 && v !== null;
  const text = c === null || isErr(c) ? "" : String(c);
  const m = /^(<=|>=|<>|<|>|=)?(.*)$/s.exec(text)!;
  const op = m[1] ?? "=";
  const operand = m[2];
  const n = parseNumber(operand);
  if (n !== null) {
    return (v) => {
      if (typeof v !== "number") return op === "<>";
      return op === "=" ? v === n : op === "<>" ? v !== n : op === "<" ? v < n : op === ">" ? v > n : op === "<=" ? v <= n : v >= n;
    };
  }
  if (op === "=" || op === "<>") {
    if (operand === "") return (v) => (op === "=" ? v === null || v === "" : v !== null && v !== "");
    const re = new RegExp(
      "^" +
        operand
          .replace(/[.+^${}()|[\]\\]/g, "\\$&")
          .replace(/~\*/g, "\u0001")
          .replace(/~\?/g, "\u0002")
          .replace(/\*/g, ".*")
          .replace(/\?/g, ".")
          // oxlint-disable-next-line no-control-regex -- placeholders for escaped wildcards, set just above
          .replace(/\u0001/g, "\\*")
          // oxlint-disable-next-line no-control-regex
          .replace(/\u0002/g, "\\?") +
        "$",
      "is",
    );
    return (v) => {
      const hit = typeof v === "string" && re.test(v);
      return op === "=" ? hit : !hit;
    };
  }
  return (v) => {
    if (typeof v !== "string") return false;
    const cmp = compare(v, operand);
    return op === "<" ? cmp < 0 : op === ">" ? cmp > 0 : op === "<=" ? cmp <= 0 : cmp >= 0;
  };
}

/** Range argument: references are always treated as ranges (a single cell is a 1×1 range); other expressions must evaluate to a range */
function rangeArg(n: Node | undefined, ctx: EvalContext): RangeRef | FormulaError {
  if (!n) return ERR.value;
  if (n.t === "ref") {
    const sheet = sheetIndex(ctx, n.sheet);
    if (sheet < 0) return ERR.ref;
    return { kind: "range", sheet, r1: n.r1, c1: n.c1, r2: n.r2, c2: n.c2 };
  }
  const v = evalNode(n, ctx);
  return isRange(v) ? v : isErr(v) ? v : ERR.value;
}

/**
 * Multi-criteria functions like SUMIFS: return the offsets (dr, dc) matching all criteria.
 * Only the populated area (used ranges of each criteria range and the sum range) is visited; cells outside are all blank,
 * and if every criterion matches blanks they are only counted (extra), so whole-column/row ranges are never scanned cell by cell.
 */
function matchAll(
  args: Node[],
  ctx: EvalContext,
  target?: RangeRef,
): { base: RangeRef; hits: [number, number][]; extra: number } | FormulaError {
  const pairs: [RangeRef, (v: Value) => boolean][] = [];
  for (let i = 0; i + 1 < args.length; i += 2) {
    const g = rangeArg(args[i], ctx);
    if (isErr(g)) return g;
    // A criterion referring to an empty cell is treated as 0 (Excel's behavior), not as "blank"
    const c = val(args[i + 1], ctx);
    pairs.push([g, criteria(c === null && args[i + 1]?.t === "ref" ? 0 : c)]);
  }
  if (!pairs.length) return ERR.value;
  const [base] = pairs[0];
  const height = base.r2 - base.r1 + 1;
  const width = base.c2 - base.c1 + 1;
  // All criteria ranges must be the same size (COUNTIFS, SUMIFS)
  if (pairs.some(([g]) => g.r2 - g.r1 + 1 !== height || g.c2 - g.c1 + 1 !== width)) return ERR.value;
  let maxDr = -1;
  let maxDc = -1;
  for (const g of [...pairs.map(([g]) => g), ...(target ? [target] : [])]) {
    const b = bounded(ctx, g);
    maxDr = Math.max(maxDr, b.r2 - g.r1);
    maxDc = Math.max(maxDc, b.c2 - g.c1);
  }
  const rows = Math.max(0, Math.min(height, maxDr + 1));
  const cols = Math.max(0, Math.min(width, maxDc + 1));
  const hits: [number, number][] = [];
  for (let dr = 0; dr < rows; dr++)
    for (let dc = 0; dc < cols; dc++) {
      if (pairs.every(([g, test]) => test(ctx.get(g.sheet, g.r1 + dr, g.c1 + dc)))) hits.push([dr, dc]);
    }
  const blankMatches = pairs.every(([, test]) => test(null));
  return { base, hits, extra: blankMatches ? height * width - rows * cols : 0 };
}

function sumAt(ctx: EvalContext, g: RangeRef, hits: [number, number][]) {
  let s = 0;
  let n = 0;
  for (const [dr, dc] of hits) {
    const v = ctx.get(g.sheet, g.r1 + dr, g.c1 + dc);
    if (isErr(v)) return { err: v, s, n };
    if (typeof v === "number") {
      s += v;
      n++;
    }
  }
  return { err: null, s, n };
}

function lookupIndex(list: Value[], target: Value, mode: number): number {
  if (mode === 0) {
    const test = typeof target === "string" ? criteria(target) : (v: Value) => compare(v, target) === 0 && v !== null;
    return list.findIndex(test);
  }
  // Approximate match: in a sorted list, find the last <= target (mode 1) or >= target (mode -1)
  let found = -1;
  for (let i = 0; i < list.length; i++) {
    const v = list[i];
    if (v === null || isErr(v)) continue;
    const c = compare(v, target);
    if (mode === 1 ? c <= 0 : c >= 0) found = i;
    else break;
  }
  return found;
}

const dateParts = (args: Node[], ctx: EvalContext) => {
  const n = num(args[0], ctx);
  return isErr(n) ? n : serialToDate(n);
};

const FUNCTIONS: Record<string, Fn> = {
  // Math
  SUM: numbersFn((xs) => xs.reduce((a, b) => a + b, 0)),
  PRODUCT: numbersFn((xs) => (xs.length ? xs.reduce((a, b) => a * b, 1) : 0)),
  AVERAGE: numbersFn((xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : ERR.div0)),
  MIN: numbersFn((xs) => (xs.length ? Math.min(...xs) : 0)),
  MAX: numbersFn((xs) => (xs.length ? Math.max(...xs) : 0)),
  MEDIAN: numbersFn((xs) => {
    if (!xs.length) return ERR.num;
    const s = [...xs].sort((a, b) => a - b);
    const m = s.length >> 1;
    return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
  }),
  COUNT: (args, ctx) => {
    let n = 0;
    for (const a of args) {
      const v = evalNode(a, ctx);
      if (isRange(v)) eachInRange(ctx, v, (x) => typeof x === "number" && n++);
      else if (typeof v === "number" || (typeof v === "string" && parseNumber(v) !== null) || typeof v === "boolean") n++;
    }
    return n;
  },
  COUNTA: (args, ctx) => {
    let n = 0;
    for (const a of args) {
      if (a.t === "empty") continue;
      const v = evalNode(a, ctx);
      if (isRange(v)) eachInRange(ctx, v, (x) => x !== null && x !== "" && n++);
      else if (v !== null) n++;
    }
    return n;
  },
  COUNTBLANK: (args, ctx) => {
    const g = evalNode(args[0], ctx);
    if (!isRange(g)) return g === null || g === "" ? 1 : 0;
    let filled = 0;
    eachInRange(ctx, g, (x) => x !== null && x !== "" && filled++);
    return (g.r2 - g.r1 + 1) * (g.c2 - g.c1 + 1) - filled;
  },
  ABS: math1(Math.abs),
  INT: math1(Math.floor),
  SQRT: (args, ctx) => {
    const x = num(args[0], ctx);
    return isErr(x) ? x : x < 0 ? ERR.num : Math.sqrt(x);
  },
  EXP: math1(Math.exp),
  LN: (args, ctx) => {
    const x = num(args[0], ctx);
    return isErr(x) ? x : x <= 0 ? ERR.num : Math.log(x);
  },
  LOG10: (args, ctx) => {
    const x = num(args[0], ctx);
    return isErr(x) ? x : x <= 0 ? ERR.num : Math.log10(x);
  },
  PI: () => Math.PI,
  SIGN: math1(Math.sign),
  POWER: (args, ctx) => {
    const x = num(args[0], ctx);
    const y = num(args[1], ctx);
    if (isErr(x)) return x;
    if (isErr(y)) return y;
    const p = Math.pow(x, y);
    return Number.isFinite(p) ? p : ERR.num;
  },
  MOD: (args, ctx) => {
    const x = num(args[0], ctx);
    const y = num(args[1], ctx);
    if (isErr(x)) return x;
    if (isErr(y)) return y;
    if (y === 0) return ERR.div0;
    return x - y * Math.floor(x / y);
  },
  ROUND: roundFn("round"),
  ROUNDUP: roundFn("up"),
  ROUNDDOWN: roundFn("down"),
  TRUNC: (args, ctx) => {
    const x = num(args[0], ctx);
    const d = num(args[1], ctx);
    if (isErr(x)) return x;
    if (isErr(d)) return d;
    return roundTo(x, Math.trunc(d), "down");
  },
  CEILING: (args, ctx) => {
    const x = num(args[0], ctx);
    const s = num(args[1], ctx, 1);
    if (isErr(x)) return x;
    if (isErr(s)) return s;
    return s === 0 ? 0 : Math.ceil(x / s) * s;
  },
  FLOOR: (args, ctx) => {
    const x = num(args[0], ctx);
    const s = num(args[1], ctx, 1);
    if (isErr(x)) return x;
    if (isErr(s)) return s;
    return s === 0 ? ERR.div0 : Math.floor(x / s) * s;
  },
  SUMPRODUCT: (args, ctx) => {
    const ms = args.map((a) => {
      const v = evalNode(a, ctx);
      return isRange(v) ? v : null;
    });
    if (ms.some((m) => !m)) return ERR.value;
    const [first] = ms as RangeRef[];
    const rows = first.r2 - first.r1;
    const cols = first.c2 - first.c1;
    if ((ms as RangeRef[]).some((m) => m.r2 - m.r1 !== rows || m.c2 - m.c1 !== cols)) return ERR.value;
    const { r2, c2 } = bounded(ctx, first);
    let total = 0;
    for (let r = 0; r <= Math.min(rows, r2 - first.r1); r++)
      for (let c = 0; c <= Math.min(cols, c2 - first.c1); c++) {
        let p = 1;
        for (const m of ms as RangeRef[]) {
          const v = ctx.get(m.sheet, m.r1 + r, m.c1 + c);
          if (isErr(v)) return v;
          p *= typeof v === "number" ? v : 0;
        }
        total += p;
      }
    return total;
  },
  SUMIF: (args, ctx) => {
    const g = rangeArg(args[0], ctx);
    if (isErr(g)) return g;
    const target = args[2] ? rangeArg(args[2], ctx) : g;
    if (isErr(target)) return target;
    const m = matchAll([args[0], args[1]], ctx, target);
    if (isErr(m)) return m;
    const { err, s } = sumAt(ctx, target, m.hits);
    return err ?? s;
  },
  SUMIFS: (args, ctx) => {
    const target = rangeArg(args[0], ctx);
    if (isErr(target)) return target;
    const m = matchAll(args.slice(1), ctx, target);
    if (isErr(m)) return m;
    const { err, s } = sumAt(ctx, target, m.hits);
    return err ?? s;
  },
  COUNTIF: (args, ctx) => {
    const m = matchAll(args, ctx);
    return isErr(m) ? m : m.hits.length + m.extra;
  },
  COUNTIFS: (args, ctx) => {
    const m = matchAll(args, ctx);
    return isErr(m) ? m : m.hits.length + m.extra;
  },
  AVERAGEIF: (args, ctx) => {
    const g = rangeArg(args[0], ctx);
    if (isErr(g)) return g;
    const target = args[2] ? rangeArg(args[2], ctx) : g;
    if (isErr(target)) return target;
    const m = matchAll([args[0], args[1]], ctx, target);
    if (isErr(m)) return m;
    const { err, s, n } = sumAt(ctx, target, m.hits);
    return err ?? (n ? s / n : ERR.div0);
  },
  AVERAGEIFS: (args, ctx) => {
    const target = rangeArg(args[0], ctx);
    if (isErr(target)) return target;
    const m = matchAll(args.slice(1), ctx, target);
    if (isErr(m)) return m;
    const { err, s, n } = sumAt(ctx, target, m.hits);
    return err ?? (n ? s / n : ERR.div0);
  },

  // Logical
  IF: (args, ctx) => {
    const c = toBool(val(args[0], ctx));
    if (isErr(c)) return c;
    const branch = c ? args[1] : args[2];
    if (!branch) return c ? true : false;
    return branch.t === "empty" ? 0 : evalNode(branch, ctx);
  },
  IFS: (args, ctx) => {
    for (let i = 0; i + 1 < args.length; i += 2) {
      const c = toBool(val(args[i], ctx));
      if (isErr(c)) return c;
      if (c) return evalNode(args[i + 1], ctx);
    }
    return ERR.na;
  },
  IFERROR: (args, ctx) => {
    const v = val(args[0], ctx);
    return isErr(v) ? val(args[1], ctx) : v;
  },
  IFNA: (args, ctx) => {
    const v = val(args[0], ctx);
    return isErr(v) && v.code === "#N/A" ? val(args[1], ctx) : v;
  },
  AND: (args, ctx) => {
    let all = true;
    for (const a of args) {
      const v = evalNode(a, ctx);
      if (isRange(v)) {
        let err: FormulaError | null = null;
        eachInRange(ctx, v, (x) => {
          if (isErr(x)) err ??= x;
          else if (typeof x === "number" || typeof x === "boolean") all &&= !!x;
        });
        if (err) return err;
      } else {
        const b = toBool(v);
        if (isErr(b)) return b;
        all &&= b;
      }
    }
    return all;
  },
  OR: (args, ctx) => {
    let any = false;
    for (const a of args) {
      const v = evalNode(a, ctx);
      if (isRange(v)) {
        let err: FormulaError | null = null;
        eachInRange(ctx, v, (x) => {
          if (isErr(x)) err ??= x;
          else if (typeof x === "number" || typeof x === "boolean") any ||= !!x;
        });
        if (err) return err;
      } else {
        const b = toBool(v);
        if (isErr(b)) return b;
        any ||= b;
      }
    }
    return any;
  },
  NOT: (args, ctx) => {
    const b = toBool(val(args[0], ctx));
    return isErr(b) ? b : !b;
  },
  TRUE: () => true,
  FALSE: () => false,

  // Text
  CONCAT: (args, ctx) => {
    let s = "";
    for (const a of args) {
      const v = evalNode(a, ctx);
      if (isRange(v)) {
        for (const row of matrix(ctx, v))
          for (const x of row) {
            const t = toText(x);
            if (isErr(t)) return t;
            s += t;
          }
      } else {
        const t = toText(v);
        if (isErr(t)) return t;
        s += t;
      }
    }
    return s;
  },
  CONCATENATE: (args, ctx) => {
    let s = "";
    for (const a of args) {
      const t = str(a, ctx);
      if (isErr(t)) return t;
      s += t;
    }
    return s;
  },
  TEXTJOIN: (args, ctx) => {
    const sep = str(args[0], ctx);
    const skip = toBool(val(args[1], ctx));
    if (isErr(sep)) return sep;
    if (isErr(skip)) return skip;
    const parts: string[] = [];
    for (const a of args.slice(2)) {
      const v = evalNode(a, ctx);
      const list = isRange(v) ? matrix(ctx, v).flat() : [v];
      for (const x of list) {
        const t = toText(x);
        if (isErr(t)) return t;
        if (!skip || t !== "") parts.push(t);
      }
    }
    return parts.join(sep);
  },
  LEFT: (args, ctx) => {
    const s = str(args[0], ctx);
    const n = num(args[1], ctx, 1);
    if (isErr(s)) return s;
    if (isErr(n)) return n;
    return n < 0 ? ERR.value : Array.from(s).slice(0, n).join("");
  },
  RIGHT: (args, ctx) => {
    const s = str(args[0], ctx);
    const n = num(args[1], ctx, 1);
    if (isErr(s)) return s;
    if (isErr(n)) return n;
    if (n < 0) return ERR.value;
    const chars = Array.from(s);
    return chars.slice(Math.max(0, chars.length - n)).join("");
  },
  MID: (args, ctx) => {
    const s = str(args[0], ctx);
    const start = num(args[1], ctx);
    const n = num(args[2], ctx);
    if (isErr(s)) return s;
    if (isErr(start)) return start;
    if (isErr(n)) return n;
    if (start < 1 || n < 0) return ERR.value;
    return Array.from(s)
      .slice(start - 1, start - 1 + n)
      .join("");
  },
  LEN: (args, ctx) => {
    const s = str(args[0], ctx);
    return isErr(s) ? s : Array.from(s).length;
  },
  TRIM: (args, ctx) => {
    const s = str(args[0], ctx);
    return isErr(s) ? s : s.trim().replace(/ {2,}/g, " ");
  },
  UPPER: (args, ctx) => {
    const s = str(args[0], ctx);
    return isErr(s) ? s : s.toUpperCase();
  },
  LOWER: (args, ctx) => {
    const s = str(args[0], ctx);
    return isErr(s) ? s : s.toLowerCase();
  },
  PROPER: (args, ctx) => {
    const s = str(args[0], ctx);
    return isErr(s) ? s : s.toLowerCase().replace(/(^|[^A-Za-z])([a-z])/g, (_m, a, b) => a + b.toUpperCase());
  },
  REPT: (args, ctx) => {
    const s = str(args[0], ctx);
    const n = num(args[1], ctx);
    if (isErr(s)) return s;
    if (isErr(n)) return n;
    return n < 0 || s.length * n > 32767 ? ERR.value : s.repeat(Math.floor(n));
  },
  SUBSTITUTE: (args, ctx) => {
    const s = str(args[0], ctx);
    const from = str(args[1], ctx);
    const to = str(args[2], ctx);
    if (isErr(s)) return s;
    if (isErr(from)) return from;
    if (isErr(to)) return to;
    if (!from) return s;
    if (!args[3]) return s.split(from).join(to);
    const nth = num(args[3], ctx);
    if (isErr(nth)) return nth;
    let idx = -1;
    for (let i = 0; i < nth; i++) {
      idx = s.indexOf(from, idx + 1);
      if (idx < 0) return s;
    }
    return s.slice(0, idx) + to + s.slice(idx + from.length);
  },
  FIND: (args, ctx) => {
    const find = str(args[0], ctx);
    const within = str(args[1], ctx);
    const start = num(args[2], ctx, 1);
    if (isErr(find)) return find;
    if (isErr(within)) return within;
    if (isErr(start)) return start;
    const i = within.indexOf(find, start - 1);
    return i < 0 ? ERR.value : i + 1;
  },
  SEARCH: (args, ctx) => {
    const find = str(args[0], ctx);
    const within = str(args[1], ctx);
    const start = num(args[2], ctx, 1);
    if (isErr(find)) return find;
    if (isErr(within)) return within;
    if (isErr(start)) return start;
    const i = within.toLowerCase().indexOf(find.toLowerCase(), start - 1);
    return i < 0 ? ERR.value : i + 1;
  },
  EXACT: (args, ctx) => {
    const a = str(args[0], ctx);
    const b = str(args[1], ctx);
    return isErr(a) ? a : isErr(b) ? b : a === b;
  },
  TEXT: (args, ctx) => {
    const v = val(args[0], ctx);
    const f = str(args[1], ctx);
    if (isErr(v)) return v;
    if (isErr(f)) return f;
    return formatValue(v, f);
  },
  VALUE: (args, ctx) => {
    const v = val(args[0], ctx);
    if (typeof v === "number" || isErr(v)) return v;
    const n = parseNumber(String(v ?? ""));
    return n === null ? ERR.value : n;
  },

  // Date
  TODAY: () => {
    const d = new Date();
    return dateToSerial(d.getFullYear(), d.getMonth() + 1, d.getDate());
  },
  NOW: () => {
    const d = new Date();
    return dateToSerial(d.getFullYear(), d.getMonth() + 1, d.getDate()) + (d.getHours() * 3600 + d.getMinutes() * 60 + d.getSeconds()) / 86400;
  },
  DATE: (args, ctx) => {
    const y = num(args[0], ctx);
    const m = num(args[1], ctx);
    const d = num(args[2], ctx);
    if (isErr(y)) return y;
    if (isErr(m)) return m;
    if (isErr(d)) return d;
    return dateToSerial(y < 1900 ? y + 1900 : y, m, d);
  },
  YEAR: (args, ctx) => {
    const d = dateParts(args, ctx);
    return isErr(d) ? d : d.getUTCFullYear();
  },
  MONTH: (args, ctx) => {
    const d = dateParts(args, ctx);
    return isErr(d) ? d : d.getUTCMonth() + 1;
  },
  DAY: (args, ctx) => {
    const d = dateParts(args, ctx);
    return isErr(d) ? d : d.getUTCDate();
  },
  WEEKDAY: (args, ctx) => {
    const d = dateParts(args, ctx);
    const type = num(args[1], ctx, 1);
    if (isErr(d)) return d;
    if (isErr(type)) return type;
    const w = d.getUTCDay();
    return type === 2 ? ((w + 6) % 7) + 1 : type === 3 ? (w + 6) % 7 : w + 1;
  },
  EDATE: (args, ctx) => {
    const d = dateParts(args, ctx);
    const m = num(args[1], ctx);
    if (isErr(d)) return d;
    if (isErr(m)) return m;
    const target = new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + Math.trunc(m), 1));
    const last = new Date(Date.UTC(target.getUTCFullYear(), target.getUTCMonth() + 1, 0)).getUTCDate();
    return dateToSerial(target.getUTCFullYear(), target.getUTCMonth() + 1, Math.min(d.getUTCDate(), last));
  },
  EOMONTH: (args, ctx) => {
    const d = dateParts(args, ctx);
    const m = num(args[1], ctx);
    if (isErr(d)) return d;
    if (isErr(m)) return m;
    return dateToSerial(d.getUTCFullYear(), d.getUTCMonth() + Math.trunc(m) + 2, 0);
  },

  // Lookup
  VLOOKUP: (args, ctx) => lookup(args, ctx, "v"),
  HLOOKUP: (args, ctx) => lookup(args, ctx, "h"),
  MATCH: (args, ctx) => {
    const target = val(args[0], ctx);
    const g = evalNode(args[1], ctx);
    const mode = num(args[2], ctx, 1);
    if (isErr(target)) return target;
    if (!isRange(g)) return ERR.na;
    if (isErr(mode)) return mode;
    const m = matrix(ctx, g);
    const list = g.r1 === g.r2 ? (m[0] ?? []) : m.map((r) => r[0]);
    const i = lookupIndex(list, target, Math.sign(mode));
    return i < 0 ? ERR.na : i + 1;
  },
  INDEX: (args, ctx) => {
    const g = evalNode(args[0], ctx);
    const r = num(args[1], ctx);
    const c = num(args[2], ctx, g && isRange(g) && g.r1 === g.r2 ? 0 : 1);
    if (!isRange(g)) return isErr(g) ? g : r === 1 || r === 0 ? g : ERR.ref;
    if (isErr(r)) return r;
    if (isErr(c)) return c;
    let row = r;
    let col = c;
    if (g.r1 === g.r2 && !args[2]) {
      col = r;
      row = 1;
    }
    if (row < 1 || col < 1 || g.r1 + row - 1 > g.r2 || g.c1 + col - 1 > g.c2) return ERR.ref;
    return ctx.get(g.sheet, g.r1 + row - 1, g.c1 + col - 1);
  },
  XLOOKUP: (args, ctx) => {
    const target = val(args[0], ctx);
    const look = evalNode(args[1], ctx);
    const ret = evalNode(args[2], ctx);
    if (isErr(target)) return target;
    if (!isRange(look) || !isRange(ret)) return ERR.value;
    const vertical = look.c1 === look.c2;
    const m = matrix(ctx, look);
    const list = vertical ? m.map((r) => r[0]) : (m[0] ?? []);
    const i = lookupIndex(list, target, 0);
    if (i < 0) return args[3] ? val(args[3], ctx) : ERR.na;
    return vertical ? ctx.get(ret.sheet, ret.r1 + i, ret.c1) : ctx.get(ret.sheet, ret.r1, ret.c1 + i);
  },
  CHOOSE: (args, ctx) => {
    const i = num(args[0], ctx);
    if (isErr(i)) return i;
    const pick = args[Math.trunc(i)];
    return i < 1 || !pick ? ERR.value : evalNode(pick, ctx);
  },
  ROW: (args, ctx) => {
    if (!args.length) return ctx.row + 1;
    const g = args[0].t === "ref" ? args[0] : null;
    return g ? g.r1 + 1 : ERR.value;
  },
  COLUMN: (args, ctx) => {
    if (!args.length) return ctx.col + 1;
    const g = args[0].t === "ref" ? args[0] : null;
    return g ? g.c1 + 1 : ERR.value;
  },
  ROWS: (args) => (args[0]?.t === "ref" ? args[0].r2 - args[0].r1 + 1 : ERR.value),
  COLUMNS: (args) => (args[0]?.t === "ref" ? args[0].c2 - args[0].c1 + 1 : ERR.value),

  // Information
  ISBLANK: (args, ctx) => val(args[0], ctx) === null,
  ISNUMBER: (args, ctx) => typeof val(args[0], ctx) === "number",
  ISTEXT: (args, ctx) => typeof val(args[0], ctx) === "string",
  ISLOGICAL: (args, ctx) => typeof val(args[0], ctx) === "boolean",
  ISERROR: (args, ctx) => isErr(val(args[0], ctx)),
  ISERR: (args, ctx) => {
    const v = val(args[0], ctx);
    return isErr(v) && v.code !== "#N/A";
  },
  ISNA: (args, ctx) => {
    const v = val(args[0], ctx);
    return isErr(v) && v.code === "#N/A";
  },
  NA: () => ERR.na,
};

function lookup(args: Node[], ctx: EvalContext, dir: "v" | "h"): Result {
  const target = val(args[0], ctx);
  const g = evalNode(args[1], ctx);
  const idx = num(args[2], ctx);
  const approx = args[3] ? toBool(val(args[3], ctx)) : true;
  if (isErr(target)) return target;
  if (!isRange(g)) return isErr(g) ? g : ERR.value;
  if (isErr(idx)) return idx;
  if (isErr(approx)) return approx;
  const span = dir === "v" ? g.c2 - g.c1 + 1 : g.r2 - g.r1 + 1;
  if (idx < 1) return ERR.value;
  if (idx > span) return ERR.ref;
  const m = matrix(ctx, g);
  const list = dir === "v" ? m.map((r) => r[0]) : (m[0] ?? []);
  const i = lookupIndex(list, target, approx ? 1 : 0);
  if (i < 0) return ERR.na;
  return dir === "v" ? ctx.get(g.sheet, g.r1 + i, g.c1 + idx - 1) : ctx.get(g.sheet, g.r1 + idx - 1, g.c1 + i);
}

// ───────────── Calculation engine ─────────────

/**
 * Lazy evaluation + memoization: call invalidate() after an edit; afterwards each cell is computed only when read
 */
/** Dependency levels evaluated by recursion; deeper chains continue from a worklist so the call stack can't overflow */
const MAX_DEPTH = 150;

/** Thrown when a cell at MAX_DEPTH has to be evaluated: the top-level call evaluates it first, then tries again */
class TooDeep {
  sheet: number;
  row: number;
  col: number;
  /** Cells that were being evaluated when the limit was reached, innermost first */
  path: [number, number, number][] = [];
  constructor(sheet: number, row: number, col: number) {
    this.sheet = sheet;
    this.row = row;
    this.col = col;
  }
}

export class Calculator {
  private depth = 0;
  private memo: Map<number, Value>[] = [];
  private visiting: Set<number>[] = [];
  private book: Workbook;
  constructor(book: Workbook) {
    this.book = book;
    this.invalidate();
  }

  private keys: (number[] | undefined)[] = [];

  invalidate() {
    this.keys = [];
    this.memo = this.book.sheets.map(() => new Map());
    this.visiting = this.book.sheets.map(() => new Set());
  }

  /**
   * Get the value for display.
   * A chain of dependencies thousands of cells long (a running total down a column) is evaluated in stretches: when
   * the recursion gets too deep, the cell it stopped at is evaluated first (its result is remembered), then the rest.
   */
  value(sheet: number, r: number, c: number): Value {
    if (this.depth > 0) return this.compute(sheet, r, c);
    const work: [number, number, number][] = [[sheet, r, c]];
    const waiting = new Set<string>([`${sheet}:${key(r, c)}`]);
    let result: Value = null;
    while (work.length) {
      const [si, rr, cc] = work[work.length - 1];
      try {
        result = this.compute(si, rr, cc);
        work.pop();
        waiting.delete(`${si}:${key(rr, cc)}`);
      } catch (e) {
        if (!(e instanceof TooDeep)) throw e;
        const id = `${e.sheet}:${key(e.row, e.col)}`;
        if (waiting.has(id)) {
          // Came back to a cell that is already waiting for its own dependencies: a circular reference longer than MAX_DEPTH
          this.memo[e.sheet].set(key(e.row, e.col), ERR.cycle);
        } else {
          // The cells on the way wait too (outermost first), so evaluation continues from the nearest one instead of
          // starting over from the top: a SUM over thousands of unevaluated cells deep in a chain stays linear
          for (let i = e.path.length - 1; i >= 0; i--) {
            const [ps, pr, pc] = e.path[i];
            const pid = `${ps}:${key(pr, pc)}`;
            if (waiting.has(pid)) continue;
            waiting.add(pid);
            work.push([ps, pr, pc]);
          }
          waiting.add(id);
          work.push([e.sheet, e.row, e.col]);
        }
      } finally {
        this.depth = 0;
      }
    }
    return result;
  }

  private compute(sheet: number, r: number, c: number): Value {
    const s = this.book.sheets[sheet];
    if (!s) return ERR.ref;
    const k = key(r, c);
    const cell = s.cells.get(k);
    if (!cell) return null;
    if (!cell.f) return cell.v;
    const memo = this.memo[sheet];
    if (memo.has(k)) return memo.get(k)!;
    const visiting = this.visiting[sheet];
    if (visiting.has(k)) return ERR.cycle;
    if (this.depth >= MAX_DEPTH) throw new TooDeep(sheet, r, c);
    visiting.add(k);
    this.depth++;
    let v: Value;
    try {
      const n = parsed(cell.f);
      if (isErr(n)) v = n;
      else {
        const ctx: EvalContext = {
          book: this.book,
          sheet,
          row: r,
          col: c,
          get: (si, rr, cc) => this.value(si, rr, cc),
          sortedKeys: (si) => (this.keys[si] ??= Array.from(this.book.sheets[si].cells.keys()).sort((a, b) => a - b)),
        };
        const res = evalNode(n, ctx);
        v = scalar(res, ctx);
        if (typeof v === "number" && !Number.isFinite(v)) v = ERR.num;
      }
    } catch (e) {
      if (e instanceof TooDeep) {
        // Not computed yet: nothing is remembered, the top-level call comes back to this cell
        e.path.push([sheet, r, c]);
        throw e;
      }
      // Should the stack overflow anyway (deeply nested expressions), keep the value Excel stored in the file
      // rather than turning the whole chain into #VALUE!
      if (e instanceof RangeError) v = cell.v !== null && cell.v !== undefined ? cell.v : ERR.value;
      else v = isErr(e) ? e : ERR.value;
    } finally {
      // Whatever happened, this cell is no longer being evaluated
      visiting.delete(k);
      this.depth--;
    }
    // Unsupported function: reuse the cached value Excel stored in the file instead of showing #NAME?
    if (isErr(v) && v.code === "#NAME?" && cell.v !== null && cell.v !== undefined) v = cell.v;
    memo.set(k, v);
    return v;
  }
}

/**
 * Evaluate a formula relative to a given cell (used by conditional formatting):
 * relative references in the formula are written against (baseRow, baseCol) and shifted to (row, col) before evaluation
 */
export function evaluateAt(
  book: Workbook,
  formula: string,
  sheet: number,
  base: [number, number],
  at: [number, number],
  get: EvalContext["get"],
  sortedKeys?: EvalContext["sortedKeys"],
): Value {
  const n = parsed(shiftFormula(formula, at[0] - base[0], at[1] - base[1]));
  if (isErr(n)) return n;
  const ctx: EvalContext = { book, sheet, row: at[0], col: at[1], get, sortedKeys };
  try {
    const v = scalar(evalNode(n, ctx), ctx);
    return typeof v === "number" && !Number.isFinite(v) ? ERR.num : v;
  } catch (e) {
    return isErr(e) ? e : ERR.value;
  }
}

/** Convert a value to a saveable scalar (errors are stored as error codes) */
export const toScalar = (v: Value): Scalar => (isErr(v) ? v.code : v);
export { isErr };
