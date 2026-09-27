/**
 * Structural and style operations: inserting/deleting rows and columns, adjusting formula references, adding styles.
 *
 * Reference adjustment on insert/delete matches Excel:
 * - Insert: references at or after the insertion point shift; ranges spanning the insertion point grow
 * - Delete: references entirely inside the deleted area become #REF!; partially deleted ranges shrink
 * - Absolute references ($) are adjusted too, because the cells they point to have moved
 */
import { MAX_COLS, MAX_ROWS, colIndex, colName, colOf, key, rowOf, type Cell, type CellStyle, type Range, type Workbook } from "./model";

export interface StructOp {
  kind: "insertRows" | "deleteRows" | "insertCols" | "deleteCols";
  /** Sheet id */
  sheet: string;
  at: number;
  count: number;
}

export const inverseOp = (op: StructOp): StructOp => ({
  ...op,
  kind: op.kind === "insertRows" ? "deleteRows" : op.kind === "deleteRows" ? "insertRows" : op.kind === "insertCols" ? "deleteCols" : "insertCols",
});

const isRowOp = (op: StructOp) => op.kind === "insertRows" || op.kind === "deleteRows";

/** One-dimensional adjustment: returns the new [a, b], or null if the whole span is deleted */
function adjust1(a: number, b: number, op: StructOp, whole: boolean): [number, number] | null {
  if (whole) return [a, b];
  const { at, count } = op;
  if (op.kind === "insertRows" || op.kind === "insertCols") {
    return [a >= at ? a + count : a, b >= at ? b + count : b];
  }
  const end = at + count - 1;
  if (a >= at && b <= end) return null;
  const na = a < at ? a : a > end ? a - count : at;
  const nb = b < at ? b : b > end ? b - count : at - 1;
  return [na, nb];
}

/** Adjust a range; returns null if it is deleted entirely */
export function adjustRange(g: Range, op: StructOp): Range | null {
  if (isRowOp(op)) {
    const r = adjust1(g.r1, g.r2, op, g.r1 === 0 && g.r2 >= MAX_ROWS - 1);
    return r ? { ...g, r1: r[0], r2: Math.min(r[1], MAX_ROWS - 1) } : null;
  }
  const c = adjust1(g.c1, g.c2, op, g.c1 === 0 && g.c2 >= MAX_COLS - 1);
  return c ? { ...g, c1: c[0], c2: Math.min(c[1], MAX_COLS - 1) } : null;
}

/** Where a position moves to; returns null if deleted */
export function adjustCell(r: number, c: number, op: StructOp): [number, number] | null {
  const g = adjustRange({ r1: r, c1: c, r2: r, c2: c }, op);
  return g ? [g.r1, g.c1] : null;
}

// ───────────── Formula references ─────────────

const REF = String.raw`((?:'(?:[^']|'')+'|[A-Za-z_À-￿][\w.À-￿]*)!)?(\$?[A-Za-z]{1,3}\$?\d+(?::\$?[A-Za-z]{1,3}\$?\d+)?|\$?[A-Za-z]{1,3}:\$?[A-Za-z]{1,3}|\$?\d+:\$?\d+)(?![\w(!])`;
const REF_RE = new RegExp(String.raw`(^|[^A-Za-z0-9_.$'!À-￿])` + REF, "g");

interface Part {
  col?: { abs: boolean; i: number };
  row?: { abs: boolean; i: number };
}

function parsePart(s: string): Part {
  const m = /^(\$?)([A-Za-z]{1,3})?(\$?)(\d+)?$/.exec(s)!;
  const p: Part = {};
  if (m[2]) p.col = { abs: !!m[1], i: colIndex(m[2]) };
  if (m[4]) p.row = { abs: !!(m[2] ? m[3] : m[1]), i: Number(m[4]) - 1 };
  return p;
}

const fmtPart = (p: Part) => (p.col ? `${p.col.abs ? "$" : ""}${colName(p.col.i)}` : "") + (p.row ? `${p.row.abs ? "$" : ""}${p.row.i + 1}` : "");

const unquote = (prefix: string) => {
  const name = prefix.slice(0, -1);
  return name.startsWith("'") ? name.slice(1, -1).replace(/''/g, "'") : name;
};

/**
 * Adjust references in a formula that point to targetSheet, according to an insert/delete.
 * hostSheet is the sheet containing the formula (references without a sheet name point to it).
 */
export function adjustFormula(formula: string, hostSheet: string, targetSheet: string, op: StructOp): string {
  const target = targetSheet.toLowerCase();
  const host = hostSheet.toLowerCase();
  return formula
    .split(/("(?:[^"]|"")*")/)
    .map((seg, i) =>
      i % 2 === 1
        ? seg
        : seg.replace(REF_RE, (m, pre: string, prefix: string | undefined, body: string) => {
            const sheet = prefix ? unquote(prefix).toLowerCase() : host;
            if (sheet !== target) return m;
            const [a, b] = body.split(":");
            const pa = parsePart(a);
            const pb = b ? parsePart(b) : pa;
            const g: Range = {
              r1: pa.row?.i ?? 0,
              r2: pb.row?.i ?? MAX_ROWS - 1,
              c1: pa.col?.i ?? 0,
              c2: pb.col?.i ?? MAX_COLS - 1,
            };
            const n = adjustRange(g, op);
            if (!n) return `${pre}${prefix ?? ""}#REF!`;
            const na: Part = { col: pa.col && { ...pa.col, i: n.c1 }, row: pa.row && { ...pa.row, i: n.r1 } };
            if (!b) return `${pre}${prefix ?? ""}${fmtPart(na)}`;
            const nb: Part = { col: pb.col && { ...pb.col, i: n.c2 }, row: pb.row && { ...pb.row, i: n.r2 } };
            return `${pre}${prefix ?? ""}${fmtPart(na)}:${fmtPart(nb)}`;
          }),
    )
    .join("");
}

/** sqref/ref attributes (multiple space-separated ranges, no sheet name) */
export function adjustSqref(sqref: string, op: StructOp): string | null {
  const out = sqref
    .split(/\s+/)
    .filter(Boolean)
    .flatMap((part) => {
      const f = adjustFormula(part, "x", "x", op);
      return f.includes("#REF!") ? [] : [f];
    });
  return out.length ? out.join(" ") : null;
}

// ───────────── Applying to the data model ─────────────

/** Minimal data affected by insert/delete: a sheet's cells, merges, column widths and row heights */
export interface SheetState {
  id: string;
  name: string;
  cells: Map<number, Cell>;
  merges: Range[];
  colWidths: Map<number, number>;
  rowHeights: Map<number, number>;
  colStyles: Map<number, number>;
  maxRow?: number;
  maxCol?: number;
}

function shiftMap<T>(m: Map<number, T>, op: StructOp): Map<number, T> {
  const out = new Map<number, T>();
  for (const [i, v] of m) {
    const g = adjust1(i, i, op, false);
    if (g) out.set(g[0], v);
  }
  return out;
}

/**
 * Apply an insert/delete to a set of sheets (in place): move the target sheet's cells, merges and sizes, and adjust formulas in all sheets.
 * inheritStyle: on insert, reuse the format of the row above (column to the left), as Excel does
 */
export function applyStructOp(sheets: SheetState[], op: StructOp, inheritStyle = true) {
  const target = sheets.find((s) => s.id === op.sheet);
  if (!target) return;
  const rowsOp = isRowOp(op);

  const moved = new Map<number, Cell>();
  for (const [k, cell] of target.cells) {
    const p = adjustCell(rowOf(k), colOf(k), op);
    if (p && p[0] < MAX_ROWS && p[1] < MAX_COLS) moved.set(key(p[0], p[1]), cell);
  }
  if (inheritStyle && (op.kind === "insertRows" || op.kind === "insertCols") && op.at > 0) {
    for (const [k, cell] of target.cells) {
      if (cell.s === undefined) continue;
      const r = rowOf(k);
      const c = colOf(k);
      if (rowsOp ? r !== op.at - 1 : c !== op.at - 1) continue;
      for (let i = 0; i < op.count; i++) {
        const nk = rowsOp ? key(op.at + i, c >= op.at ? c + op.count : c) : key(r >= op.at ? r + op.count : r, op.at + i);
        if (!moved.has(nk)) moved.set(nk, { v: null, s: cell.s });
      }
    }
  }
  target.cells = moved;
  target.merges = target.merges.flatMap((m) => {
    const n = adjustRange(m, op);
    return n && (n.r1 !== n.r2 || n.c1 !== n.c2) ? [n] : [];
  });
  if (rowsOp) target.rowHeights = shiftMap(target.rowHeights, op);
  else {
    target.colWidths = shiftMap(target.colWidths, op);
    target.colStyles = shiftMap(target.colStyles, op);
  }

  for (const s of sheets) {
    for (const [k, cell] of s.cells) {
      if (!cell.f) continue;
      const f = adjustFormula(cell.f, s.name, target.name, op);
      if (f !== cell.f) s.cells.set(k, { ...cell, f });
    }
  }

  if (target.maxRow !== undefined) {
    let mr = 0;
    let mc = 0;
    for (const [k, cell] of target.cells) {
      if (cell.v === null && !cell.f) continue;
      mr = Math.max(mr, rowOf(k));
      mc = Math.max(mc, colOf(k));
    }
    target.maxRow = mr;
    target.maxCol = mc;
  }
}

export function cloneState(s: SheetState): SheetState {
  return {
    id: s.id,
    name: s.name,
    cells: new Map(s.cells),
    merges: s.merges.map((m) => ({ ...m })),
    colWidths: new Map(s.colWidths),
    rowHeights: new Map(s.rowHeights),
    colStyles: new Map(s.colStyles),
    maxRow: s.maxRow,
    maxCol: s.maxCol,
  };
}

// ───────────── Styles ─────────────

const styleKey = (s: CellStyle) => JSON.stringify(s, Object.keys(s).sort());

/** Drop unset fields like undefined/false so styles with the same appearance can be shared */
function clean(s: CellStyle): CellStyle {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(s)) {
    if (v === undefined || v === false || v === null) continue;
    if (k === "border") {
      const b = Object.fromEntries(Object.entries(v as object).filter(([, x]) => x));
      if (Object.keys(b).length) out.border = b;
      continue;
    }
    out[k] = v;
  }
  return out as CellStyle;
}

/**
 * Get the style index for "base style plus changes"; styles with the same appearance share one index
 */
export function deriveStyle(book: Workbook, base: number | undefined, change: (s: CellStyle) => CellStyle): number {
  const from = base ?? 0;
  const next = clean(change({ ...(book.styles[from] ?? {}) }));
  const origin = from >= book.xfCount ? (book.styleBase.get(from) ?? 0) : from;
  const k = styleKey(next);
  // Reuse an existing original-file style with exactly the same appearance and the same base
  if (styleKey(clean(book.styles[origin] ?? {})) === k) return origin;
  for (let i = book.xfCount; i < book.styles.length; i++) {
    if (book.styleBase.get(i) === origin && styleKey(book.styles[i]) === k) return i;
  }
  book.styles.push(next);
  book.styleBase.set(book.styles.length - 1, origin);
  return book.styles.length - 1;
}
