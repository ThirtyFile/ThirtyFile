/**
 * Spreadsheet data model.
 *
 * Kept small for lightweight editing in a cloud drive:
 * - Cells are stored in a Map under numeric keys `r * MAX_COLS + c`, which use less memory and look up faster than string keys
 * - Only column widths/row heights that differ from the default are stored; positions use a prefix-sum array + binary search
 */

export const MAX_COLS = 16384; // Excel limit XFD
export const MAX_ROWS = 1048576;

export type Scalar = string | number | boolean | null;

export interface BorderSide {
  /** thin, medium, thick, dashed, dotted, double… */
  style: string;
  color?: string;
}

export interface Borders {
  top?: BorderSide;
  right?: BorderSide;
  bottom?: BorderSide;
  left?: BorderSide;
}

export interface CellStyle {
  bold?: boolean;
  italic?: boolean;
  underline?: boolean;
  strike?: boolean;
  /** Font size (points) */
  size?: number;
  font?: string;
  color?: string;
  bg?: string;
  hAlign?: "left" | "center" | "right" | "justify";
  vAlign?: "top" | "center" | "bottom";
  wrap?: boolean;
  /** Number format, e.g. #,##0.00, yyyy/m/d */
  numFmt?: string;
  border?: Borders;
  /** Indent level (each level is about one character wide) */
  indent?: number;
}

export interface Cell {
  /** Value; for formula cells, the cached result at open time */
  v: Scalar;
  /** Formula (starts with "=") */
  f?: string;
  /** Index into the styles array */
  s?: number;
}

export interface Range {
  r1: number;
  c1: number;
  r2: number;
  c2: number;
}

export interface Sheet {
  id: string;
  name: string;
  /** Path inside the zip, e.g. xl/worksheets/sheet1.xml */
  path: string;
  cells: Map<number, Cell>;
  merges: Range[];
  /** Pixels */
  colWidths: Map<number, number>;
  rowHeights: Map<number, number>;
  /** Default style for the whole column (<col style>) */
  colStyles: Map<number, number>;
  defaultColWidth: number;
  defaultRowHeight: number;
  /** Last row and column containing data (0-based) */
  maxRow: number;
  maxCol: number;
  /** Table (ListObject) ranges */
  tables: Range[];
  /** Contains a pivot table (inserting/deleting rows or columns would scramble its position) */
  pivot: boolean;
  /** Hidden sheet (not shown in preview; kept when editing) */
  hidden?: boolean;
  /** Gridlines turned off (sheetView showGridLines="0") */
  noGrid?: boolean;
  /** Number of frozen rows and columns */
  frozen?: { rows: number; cols: number };
  /** Sheet tab color */
  tabColor?: string;
}

export interface Workbook {
  sheets: Sheet[];
  /** Indices match cellXfs in styles.xml; styles added while editing are appended */
  styles: CellStyle[];
  /** Number of cellXfs in the original file */
  xfCount: number;
  /** New style → which original style it is based on (on save that style is copied and modified, preserving theme colors and other settings) */
  styleBase: Map<number, number>;
  /** Sheet shown on open (index into sheets) */
  active?: number;
}

export const key = (r: number, c: number) => r * MAX_COLS + c;
export const rowOf = (k: number) => Math.floor(k / MAX_COLS);
export const colOf = (k: number) => k % MAX_COLS;

export function colName(index: number) {
  let s = "";
  for (let n = index + 1; n > 0; n = Math.floor((n - 1) / 26)) s = String.fromCharCode(65 + ((n - 1) % 26)) + s;
  return s;
}

export function colIndex(name: string) {
  let c = 0;
  for (const ch of name.toUpperCase()) c = c * 26 + ch.charCodeAt(0) - 64;
  return c - 1;
}

export const cellName = (r: number, c: number) => `${colName(c)}${r + 1}`;

export function parseCellName(ref: string): [number, number] | null {
  const m = /^\$?([A-Z]{1,3})\$?(\d{1,7})$/i.exec(ref.trim());
  if (!m) return null;
  const r = Number(m[2]) - 1;
  const c = colIndex(m[1]);
  return r >= 0 && r < MAX_ROWS && c >= 0 && c < MAX_COLS ? [r, c] : null;
}

export const normRange = (a: Range): Range => ({
  r1: Math.min(a.r1, a.r2),
  c1: Math.min(a.c1, a.c2),
  r2: Math.max(a.r1, a.r2),
  c2: Math.max(a.c1, a.c2),
});

export const inRange = (g: Range, r: number, c: number) => r >= g.r1 && r <= g.r2 && c >= g.c1 && c <= g.c2;

export function rangeName(g: Range) {
  const a = cellName(g.r1, g.c1);
  return g.r1 === g.r2 && g.c1 === g.c2 ? a : `${a}:${cellName(g.r2, g.c2)}`;
}

/** Find the merged range containing (r, c) */
export function mergeAt(sheet: Sheet, r: number, c: number) {
  return sheet.merges.find((m) => inRange(m, r, c)) ?? null;
}

/**
 * Sizes along one axis (column widths or row heights): a prefix-sum array makes both "position of item i" and "which item a position falls in" O(log n)
 */
export class Axis {
  private acc: Float64Array;
  readonly count: number;
  constructor(count: number, size: (i: number) => number) {
    this.count = count;
    this.acc = new Float64Array(count + 1);
    for (let i = 0; i < count; i++) this.acc[i + 1] = this.acc[i] + size(i);
  }
  /** Start positions of every item (and the total at the end), for handing to another context such as the preview frame */
  offsets(): Float64Array {
    return this.acc.slice();
  }
  /** Start position of item i */
  offset(i: number) {
    return this.acc[Math.max(0, Math.min(i, this.count))];
  }
  sizeOf(i: number) {
    return this.acc[i + 1] - this.acc[i];
  }
  get total() {
    return this.acc[this.count];
  }
  /** Which item position px falls in */
  indexAt(px: number) {
    let lo = 0;
    let hi = this.count - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (this.acc[mid] <= px) lo = mid;
      else hi = mid - 1;
    }
    return lo;
  }
}
