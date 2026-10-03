/**
 * Clipboard: copy, cut, paste (formulas shift automatically, formats included, Excel content can be pasted) and clearing content.
 */
import { type ClipboardEvent } from "react";
import { toast } from "sonner";
import { t } from "@/lib/i18n";
import { MAX_COLS, MAX_ROWS, colOf, rowOf, key, type Cell, type CellStyle, type Range, type Workbook } from "@/ooxml/xlsx/model";
import { shiftFormula } from "@/ooxml/xlsx/formula";
import { deriveStyle } from "@/ooxml/xlsx/ops";
import { displayText } from "./renderer";
import { parseInput, parseTsv, tsvField, type Change } from "./session";
import type { WorkspaceCtx } from "./workspace";

export interface Clip {
  workbook: symbol;
  styles: Map<number, CellStyle>;
  sheet: number;
  range: Range;
  cells: Map<number, Cell | undefined>;
  text: string;
  cut: boolean;
}

/** Most recently copied content (shared across sheets and tabs; lets paste restore formulas and formats) */
export const clipStore: { current: Clip | null } = { current: null };
export const MAX_CLIP_CELLS = 100000;
const MAX_CLIP_TEXT = 8 * 1024 * 1024;
const identities = new WeakMap<Workbook, symbol>();
const identity = (book: Workbook) => {
  let id = identities.get(book);
  if (!id) identities.set(book, (id = Symbol()));
  return id;
};

/** A cut/copy outline belongs only to its original workbook, without retaining that workbook in the clipboard. */
export function clipboardRange(book: Workbook, sheet: number): Range | null {
  const clip = clipStore.current;
  return clip?.workbook === identities.get(book) && clip?.sheet === sheet ? clip.range : null;
}

function withinSheet(g: Range) {
  if ([g.r1, g.r2, g.c1, g.c2].every(Number.isInteger) && g.r1 >= 0 && g.c1 >= 0 && g.r2 >= g.r1 && g.c2 >= g.c1 && g.r2 < MAX_ROWS && g.c2 < MAX_COLS) return true;
  toast.error(t("The pasted cells would extend beyond the worksheet. Select another cell and try again."));
  return false;
}

function bounded(g: Range) {
  if ((g.r2 - g.r1 + 1) * (g.c2 - g.c1 + 1) <= MAX_CLIP_CELLS) return true;
  toast.error(t("The selection is too large. Select a smaller range and try again."));
  return false;
}

interface Deps {
  styleRange(): Range;
}

export function createClipboard(ctx: WorkspaceCtx, deps: Deps) {
  const { book, calc, sheet, sheetIdx, range, editing, setSel, setClip, styleAt, commitChanges, changeAt } = ctx;
  const { styleRange } = deps;
  // ───── Clipboard ─────

  const copyRange = (cut: boolean): string | null => {
    const g = styleRange();
    if (!withinSheet(g) || !bounded(g)) return null;
    const lines: string[] = [];
    const cells = new Map<number, Cell | undefined>();
    const styles = new Map<number, CellStyle>([[0, structuredClone(book.styles[0] ?? {})]]);
    let length = 0;
    for (let r = g.r1; r <= g.r2; r++) {
      const fields: string[] = [];
      for (let c = g.c1; c <= g.c2; c++) {
        const cell = sheet.cells.get(key(r, c));
        if (cell?.s && !styles.has(cell.s)) styles.set(cell.s, structuredClone(book.styles[cell.s] ?? {}));
        cells.set(key(r - g.r1, c - g.c1), cell ? { ...cell } : undefined);
        const field = tsvField(displayText(calc.value(sheetIdx, r, c), styleAt(r, c)).text);
        length += field.length + 1;
        if (length > MAX_CLIP_TEXT) {
          toast.error(t("The clipboard content is too large. Copy a smaller range and try again."));
          return null;
        }
        fields.push(field);
      }
      lines.push(fields.join("\t"));
    }
    const text = lines.join("\n");
    clipStore.current = { workbook: identity(book), styles, sheet: sheetIdx, range: g, cells, text, cut };
    setClip(g);
    return text;
  };

  const pasteText = (text: string) => {
    if (text.length > MAX_CLIP_TEXT) {
      toast.error(t("The clipboard content is too large. Copy a smaller range and try again."));
      return;
    }
    const changes: Change[] = [];
    const [r0, c0] = [range.r1, range.c1];
    const internal = clipStore.current && clipStore.current.text === text ? clipStore.current : null;
    if (internal) {
      const h = internal.range.r2 - internal.range.r1 + 1;
      const w = internal.range.c2 - internal.range.c1 + 1;
      // A copied single cell or block can repeat to fill the selection (when the selection is an integer multiple of the source)
      const reps = (range.r2 - range.r1 + 1) % h === 0 && (range.c2 - range.c1 + 1) % w === 0 ? [(range.r2 - range.r1 + 1) / h, (range.c2 - range.c1 + 1) / w] : [1, 1];
      const target = { r1: r0, c1: c0, r2: r0 + h * reps[0] - 1, c2: c0 + w * reps[1] - 1 };
      if (!withinSheet(target) || !bounded(target)) return;
      const sameBook = internal.workbook === identities.get(book);
      const cut = internal.cut && sameBook;
      if (cut) {
        for (let r = 0; r < h; r++) for (let c = 0; c < w; c++) changes.push(changeAt(internal.sheet, internal.range.r1 + r, internal.range.c1 + c, null, null));
      }
      for (let rr = 0; rr < reps[0]; rr++)
        for (let cc = 0; cc < reps[1]; cc++)
          for (let r = 0; r < h; r++)
            for (let c = 0; c < w; c++) {
              const src = internal.cells.get(key(r, c));
              const tr = r0 + rr * h + r;
              const tc = c0 + cc * w + c;
              const content = src
                ? {
                    v: src.f ? null : src.v,
                    f: src.f ? (cut ? src.f : shiftFormula(src.f, tr - internal.range.r1 - r, tc - internal.range.c1 - c)) : undefined,
                  }
                : null;
              // Bring formats along when pasting (same as Excel's default paste)
              const style = sameBook ? (src?.s ?? null) : deriveStyle(book, undefined, () => internal.styles.get(src?.s ?? 0) ?? {});
              changes.push(changeAt(sheetIdx, tr, tc, content, style || null));
            }
      if (internal.cut) {
        clipStore.current = null;
        setClip(null);
      }
      // When a cell is cleared (cut) and then written, the last write wins
      const merged = new Map<string, Change>();
      for (const ch of changes) {
        const id = `${ch.sheet}:${ch.k}`;
        const prev = merged.get(id);
        merged.set(id, prev ? { ...ch, before: prev.before } : ch);
      }
      commitChanges([...merged.values()]);
      if (internal.cut && !sameBook) toast.info(t("Cells pasted into another workbook are copied; the source is kept."));
      setSel({ anchor: [r0, c0], focus: [r0 + h * reps[0] - 1, c0 + w * reps[1] - 1] });
      return;
    }
    let grid: string[][];
    try {
      grid = parseTsv(text, MAX_CLIP_CELLS);
    } catch {
      toast.error(t("The selection is too large. Select a smaller range and try again."));
      return;
    }
    const single = grid.length === 1 && grid[0].length === 1;
    const width = grid.reduce((w, row) => Math.max(w, row.length), 0);
    const target = single ? styleRange() : { r1: r0, c1: c0, r2: r0 + grid.length - 1, c2: c0 + width - 1 };
    if (!withinSheet(target) || !bounded(target)) return;
    const put = (r: number, c: number, v: string) => {
      const style = styleAt(r, c);
      const p = parseInput(v, style);
      const s = p.fmt ? deriveStyle(book, sheet.cells.get(key(r, c))?.s, (st) => ({ ...st, numFmt: p.fmt })) : undefined;
      changes.push(changeAt(sheetIdx, r, c, p, s === undefined ? undefined : s || null));
    };
    if (single) {
      const g = styleRange();
      for (let r = g.r1; r <= g.r2; r++) for (let c = g.c1; c <= g.c2; c++) put(r, c, grid[0][0]);
    } else grid.forEach((row, r) => row.forEach((v, c) => put(r0 + r, c0 + c, v)));
    commitChanges(changes);
    if (!single) setSel({ anchor: [r0, c0], focus: [target.r2, target.c2] });
  };

  const clearRange = () => {
    const changes: Change[] = [];
    const g = styleRange();
    for (const [k, cell] of sheet.cells) {
      const r = rowOf(k),
        c = colOf(k);
      if (r < g.r1 || r > g.r2 || c < g.c1 || c > g.c2 || (cell.v == null && !cell.f)) continue;
      if (changes.length === MAX_CLIP_CELLS) {
        toast.error(t("The selection is too large. Select a smaller range and try again."));
        return;
      }
      changes.push(changeAt(sheetIdx, r, c, null));
    }
    commitChanges(changes);
  };

  const onCopy = (e: ClipboardEvent<HTMLTextAreaElement>) => {
    if (editing) return;
    e.preventDefault();
    const text = copyRange(false);
    if (text !== null) e.clipboardData.setData("text/plain", text);
  };
  const onCut = (e: ClipboardEvent<HTMLTextAreaElement>) => {
    if (editing) return;
    e.preventDefault();
    const text = copyRange(true);
    if (text !== null) e.clipboardData.setData("text/plain", text);
  };
  const onPaste = (e: ClipboardEvent<HTMLTextAreaElement>) => {
    if (editing) return;
    e.preventDefault();
    pasteText(e.clipboardData.getData("text/plain"));
  };

  return { copyRange, pasteText, clearRange, onCopy, onCut, onPaste };
}
