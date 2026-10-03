/**
 * Change history: applying cell changes, layout changes, row/column inserts/deletes, and undo/redo.
 * Only data is handled here; which sheet to switch to and what to select is decided by the caller from the returned entry.
 */
import { MAX_ROWS, MAX_COLS, colOf, key, rowOf, type Cell, type Scalar, type Workbook } from "@/ooxml/xlsx/model";
import { applyStructOp, inverseOp } from "@/ooxml/xlsx/ops";
import { restoreStates, type Change, type Entry, type Layout, type Session } from "./session";

const MAX_UNDO = 200;
/** Row/column inserts and deletes keep full copies of the sheets they touch, so far fewer of them are kept */
const MAX_STRUCT_UNDO = 20;

/** Drop the oldest steps beyond the limits (always from the start: undo must replay the remaining steps in order) */
function trimUndo(session: Session) {
  let structs = session.undo.reduce((n, e) => n + (e.struct ? 1 : 0), 0);
  while (session.undo.length > MAX_UNDO || structs > MAX_STRUCT_UNDO) {
    if (session.undo.shift()?.struct) structs--;
  }
}

export function applyChanges(book: Workbook, changes: Change[], dir: "after" | "before") {
  for (const ch of changes) {
    const s = book.sheets[ch.sheet];
    const cell = ch[dir];
    if (cell) {
      s.cells.set(ch.k, cell);
      if (cell.v !== null || cell.f) {
        s.maxRow = Math.max(s.maxRow, rowOf(ch.k));
        s.maxCol = Math.max(s.maxCol, colOf(ch.k));
      }
    } else s.cells.delete(ch.k);
  }
}

export function applyLayout(book: Workbook, sheet: number, l: Layout) {
  const s = book.sheets[sheet];
  s.merges = l.merges.map((m) => ({ ...m }));
  s.colWidths = new Map(l.colWidths);
  s.rowHeights = new Map(l.rowHeights);
}

/** Record a change that has already been applied */
export function pushEntry(session: Session, e: Entry) {
  session.undo.push(e);
  trimUndo(session);
  session.redo = [];
  session.version++;
  session.calc.invalidate();
}

/** Apply and record cell and layout changes; returns false if nothing actually changed */
export function commit(session: Session, changes: Change[], sheet: number, sel: Entry["sel"], layout?: Entry["layout"]) {
  const real = changes.filter((c) => !(c.before?.v === c.after?.v && c.before?.f === c.after?.f && c.before?.s === c.after?.s));
  if (!real.length && !layout) return false;
  applyChanges(session.book, real, "after");
  if (layout) applyLayout(session.book, layout.sheet, layout.after);
  pushEntry(session, { changes: real, sheet, sel, layout });
  return true;
}

/** Build a change that sets (r, c) to content; omitting style keeps the existing style, null clears it */
export function changeAt(book: Workbook, s: number, r: number, c: number, content: { v: Scalar; f?: string } | null, style?: number | null): Change {
  if (!book.sheets[s] || !Number.isInteger(r) || !Number.isInteger(c) || r < 0 || r >= MAX_ROWS || c < 0 || c >= MAX_COLS) throw new RangeError("Cell is outside the worksheet");
  const k = key(r, c);
  const before = book.sheets[s].cells.get(k);
  const st = style === undefined ? before?.s : (style ?? undefined);
  let after: Cell | undefined;
  const has = content && (content.v !== null || content.f);
  if (has || st) after = { v: has ? content!.v : null, ...(has && content!.f ? { f: content!.f } : {}), ...(st ? { s: st } : {}) };
  return { sheet: s, k, before, after };
}

/** Undo the last step; returns the undone entry (undefined if there is nothing to undo) */
export function undo(session: Session): Entry | undefined {
  const e = session.undo.pop();
  if (!e) return undefined;
  if (e.struct) {
    restoreStates(session.book, e.struct.before);
    // Save replays onto the original file: record the inverse operation and apply the same adjustment to the baseline
    const inv = inverseOp(e.struct.op);
    session.ops.push(inv);
    applyStructOp([...session.snapshot.values()], inv, false);
  }
  applyChanges(session.book, [...e.changes].reverse(), "before");
  if (e.layout) applyLayout(session.book, e.layout.sheet, e.layout.before);
  session.redo.push(e);
  session.version++;
  session.calc.invalidate();
  return e;
}

export function redo(session: Session): Entry | undefined {
  const e = session.redo.pop();
  if (!e) return undefined;
  if (e.struct) {
    restoreStates(session.book, e.struct.after);
    session.ops.push(e.struct.op);
    applyStructOp([...session.snapshot.values()], e.struct.op, false);
  }
  applyChanges(session.book, e.changes, "after");
  if (e.layout) applyLayout(session.book, e.layout.sheet, e.layout.after);
  session.undo.push(e);
  trimUndo(session);
  session.version++;
  session.calc.invalidate();
  return e;
}
