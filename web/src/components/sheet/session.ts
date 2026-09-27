/**
 * Spreadsheet editing session: the opened data, change history and undo stack.
 * Switching tabs and coming back reuses the same session, so unsaved changes aren't lost.
 */
import JSZip from "jszip";
import { fetchOffice, type FileSource, type Node } from "@/api";
import { hasDraft, onDraftRemoved } from "@/lib/drafts";
import { dateToSerial, parseNumber, serialToDate, Calculator } from "@/lib/sheet/formula";
import { isDatePattern } from "@/lib/sheet/format";
import type { Cell, CellStyle, Range, Scalar, Sheet, Workbook } from "@/lib/sheet/model";
import { cloneState, type SheetState, type StructOp } from "@/lib/sheet/ops";
import { readXlsx, type Snapshot } from "@/lib/sheet/xlsx";

// ───────────── Editing session (unsaved changes survive switching tabs and coming back) ─────────────

export interface Change {
  sheet: number;
  k: number;
  before?: Cell;
  after?: Cell;
}
export interface Layout {
  merges: Range[];
  colWidths: Map<number, number>;
  rowHeights: Map<number, number>;
}
export interface Entry {
  changes: Change[];
  sheet: number;
  sel: Sel;
  /** Merged cells, column widths and row heights */
  layout?: { sheet: number; before: Layout; after: Layout };
  /** Row/column insert/delete: stores before/after state of all sheets (formula references are adjusted across sheets) */
  struct?: { op: StructOp; before: SheetState[]; after: SheetState[] };
}
export interface Sel {
  anchor: [number, number];
  focus: [number, number];
}
export interface Session {
  zip: JSZip;
  book: Workbook;
  snapshot: Snapshot;
  calc: Calculator;
  /** updated_at at open (or last save), used on save to detect conflicts */
  base: number;
  /** Inserts/deletes since the last save, replayed in order on the original XML when saving */
  ops: StructOp[];
  undo: Entry[];
  redo: Entry[];
  version: number;
  saved: number;
  sheet: number;
  sel: Sel;
}

const sessions = new Map<string, Session>();

// Draft removed while the session still has unsaved changes = changes were discarded (e.g. chose discard when closing the tab): free memory now instead of waiting for the same file to be reopened
onDraftRemoved((nodeId) => {
  const s = sessions.get(nodeId);
  if (s && s.version !== s.saved) sessions.delete(nodeId);
});

// ───────────── Input and display ─────────────

/** Uppercase function names and references (quoted text untouched) so saved formulas match Excel */
export function normalizeFormula(f: string) {
  return f
    .split(/("(?:[^"]|"")*"|'(?:[^']|'')*')/)
    .map((p, i) => (i % 2 ? p : p.toUpperCase()))
    .join("");
}

/** User-typed text → cell content; fmt is the number format Excel would apply automatically (when typing dates or percentages) */
export function parseInput(text: string, style?: CellStyle): { v: Scalar; f?: string; fmt?: string } {
  if (text === "") return { v: null };
  if (text.startsWith("'")) return { v: text.slice(1) };
  if (text.startsWith("=") && text.length > 1) return { v: null, f: normalizeFormula(text) };
  const n = parseNumber(text);
  if (n !== null) return { v: n, ...(text.trim().endsWith("%") && !style?.numFmt?.includes("%") ? { fmt: "0%" } : {}) };
  const upper = text.trim().toUpperCase();
  if (upper === "TRUE" || upper === "FALSE") return { v: upper === "TRUE" };
  const d = /^(\d{4})[/-](\d{1,2})[/-](\d{1,2})(?:\s+(\d{1,2}):(\d{2})(?::(\d{2}))?)?$/.exec(text.trim());
  if (d && +d[2] >= 1 && +d[2] <= 12 && +d[3] >= 1 && +d[3] <= 31) {
    const time = (+(d[4] ?? 0) * 3600 + +(d[5] ?? 0) * 60 + +(d[6] ?? 0)) / 86400;
    const fmt = isDatePattern(style?.numFmt) ? undefined : d[4] ? "yyyy/m/d h:mm" : "yyyy/m/d";
    return { v: dateToSerial(+d[1], +d[2], +d[3]) + time, ...(fmt ? { fmt } : {}) };
  }
  return { v: text };
}

/** Content shown while editing: formula, full-precision number, date */
export function editText(cell: Cell | undefined, style?: CellStyle) {
  if (!cell) return "";
  if (cell.f) return cell.f;
  const v = cell.v;
  if (v === null) return "";
  if (typeof v === "boolean") return v ? "TRUE" : "FALSE";
  if (typeof v === "number") {
    if (isDatePattern(style?.numFmt)) {
      const d = serialToDate(v);
      const date = `${d.getUTCFullYear()}/${d.getUTCMonth() + 1}/${d.getUTCDate()}`;
      return v % 1 ? `${date} ${d.getUTCHours()}:${String(d.getUTCMinutes()).padStart(2, "0")}` : date;
    }
    if (style?.numFmt?.includes("%")) return `${Number((v * 100).toPrecision(15))}%`;
    return String(v);
  }
  return v;
}

/** TSV copied from Excel (including quoted line breaks) */
export function parseTsv(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let field = "";
  let quoted = false;
  const src = text.replace(/\r\n/g, "\n").replace(/\n$/, "");
  for (let i = 0; i < src.length; i++) {
    const ch = src[i];
    if (quoted) {
      if (ch === '"' && src[i + 1] === '"') {
        field += '"';
        i++;
      } else if (ch === '"') quoted = false;
      else field += ch;
    } else if (ch === '"' && field === "") quoted = true;
    else if (ch === "\t") {
      row.push(field);
      field = "";
    } else if (ch === "\n") {
      row.push(field);
      rows.push(row);
      row = [];
      field = "";
    } else field += ch;
  }
  row.push(field);
  rows.push(row);
  return rows;
}

export const tsvField = (s: string) => (/[\t\n"]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);

export const cloneLayout = (s: Sheet): Layout => ({
  merges: s.merges.map((m) => ({ ...m })),
  colWidths: new Map(s.colWidths),
  rowHeights: new Map(s.rowHeights),
});

export function restoreStates(book: Workbook, states: SheetState[]) {
  for (const st of states) {
    const s = book.sheets.find((x) => x.id === st.id);
    if (!s) continue;
    const c = cloneState(st);
    s.cells = c.cells;
    s.merges = c.merges;
    s.colWidths = c.colWidths;
    s.rowHeights = c.rowHeights;
    s.colStyles = c.colStyles;
    s.maxRow = c.maxRow ?? s.maxRow;
    s.maxCol = c.maxCol ?? s.maxCol;
  }
}

// ───────────── Loading and releasing ─────────────

/**
 * A session can be reused if it has unsaved changes (and the draft wasn't discarded), or has no changes and the file wasn't updated by someone else.
 * If the draft was discarded (e.g. chose discard when closing the tab) or the file was updated, drop the old session and reload.
 */
export function reusableSession(node: Node): Session | undefined {
  const s = sessions.get(node.id);
  if (!s) return undefined;
  const dirty = s.version !== s.saved;
  if (dirty ? hasDraft(node.id) : s.base === node.updated_at) return s;
  sessions.delete(node.id);
  return undefined;
}

export async function openSession(node: Node, source: FileSource): Promise<Session> {
  const { zip, book, snapshot } = await readXlsx(await fetchOffice(source.contentUrl(node)));
  const s: Session = {
    zip,
    book,
    snapshot,
    calc: new Calculator(book),
    base: node.updated_at,
    ops: [],
    undo: [],
    redo: [],
    version: 0,
    saved: 0,
    sheet: 0,
    sel: { anchor: [0, 0], focus: [0, 0] },
  };
  sessions.set(node.id, s);
  return s;
}

/** Release memory when there are no unsaved changes (the workbook and original zip can be large) */
export function releaseIfClean(nodeId: string, s: Session) {
  if (s.version === s.saved && sessions.get(nodeId) === s) sessions.delete(nodeId);
}

export function dropSession(nodeId: string) {
  sessions.delete(nodeId);
}
