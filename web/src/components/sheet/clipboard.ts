/**
 * Clipboard: copy, cut, paste (formulas shift automatically, formats included, Excel content can be pasted) and clearing content.
 */
import { type ClipboardEvent } from "react";
import { key, type Cell, type Range } from "@/ooxml/xlsx/model";
import { shiftFormula } from "@/ooxml/xlsx/formula";
import { deriveStyle } from "@/ooxml/xlsx/ops";
import { displayText } from "./renderer";
import { parseInput, parseTsv, tsvField, type Change } from "./session";
import type { WorkspaceCtx } from "./workspace";

export interface Clip {
  sheet: number;
  range: Range;
  cells: Map<number, Cell | undefined>;
  text: string;
  cut: boolean;
}

/** Most recently copied content (shared across sheets and tabs; lets paste restore formulas and formats) */
export const clipStore: { current: Clip | null } = { current: null };

interface Deps {
  styleRange(): Range;
}

export function createClipboard(ctx: WorkspaceCtx, deps: Deps) {
  const { book, calc, sheet, sheetIdx, range, editing, setSel, setClip, styleAt, commitChanges, changeAt } = ctx;
  const { styleRange } = deps;
  // ───── Clipboard ─────

  const copyRange = (cut: boolean): string => {
    const g = styleRange();
    const lines: string[] = [];
    const cells = new Map<number, Cell | undefined>();
    for (let r = g.r1; r <= g.r2; r++) {
      const fields: string[] = [];
      for (let c = g.c1; c <= g.c2; c++) {
        const cell = sheet.cells.get(key(r, c));
        cells.set(key(r - g.r1, c - g.c1), cell ? { ...cell } : undefined);
        fields.push(tsvField(displayText(calc.value(sheetIdx, r, c), styleAt(r, c)).text));
      }
      lines.push(fields.join("\t"));
    }
    const text = lines.join("\n");
    clipStore.current = { sheet: sheetIdx, range: g, cells, text, cut };
    setClip(g);
    return text;
  };

  const pasteText = (text: string) => {
    const changes: Change[] = [];
    const [r0, c0] = [range.r1, range.c1];
    const internal = clipStore.current && clipStore.current.text === text ? clipStore.current : null;
    if (internal) {
      const h = internal.range.r2 - internal.range.r1 + 1;
      const w = internal.range.c2 - internal.range.c1 + 1;
      // A copied single cell or block can repeat to fill the selection (when the selection is an integer multiple of the source)
      const reps = (range.r2 - range.r1 + 1) % h === 0 && (range.c2 - range.c1 + 1) % w === 0 ? [(range.r2 - range.r1 + 1) / h, (range.c2 - range.c1 + 1) / w] : [1, 1];
      if (internal.cut) {
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
                    f: src.f ? (internal.cut ? src.f : shiftFormula(src.f, tr - internal.range.r1 - r, tc - internal.range.c1 - c)) : undefined,
                  }
                : null;
              // Bring formats along when pasting (same as Excel's default paste)
              changes.push(changeAt(sheetIdx, tr, tc, content, src?.s ?? null));
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
      setSel({ anchor: [r0, c0], focus: [r0 + h * reps[0] - 1, c0 + w * reps[1] - 1] });
      return;
    }
    const grid = parseTsv(text);
    const single = grid.length === 1 && grid[0].length === 1;
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
    if (!single) setSel({ anchor: [r0, c0], focus: [r0 + grid.length - 1, c0 + Math.max(...grid.map((r) => r.length)) - 1] });
  };

  const clearRange = () => {
    const changes: Change[] = [];
    const g = styleRange();
    for (let r = g.r1; r <= Math.min(g.r2, sheet.maxRow); r++)
      for (let c = g.c1; c <= Math.min(g.c2, sheet.maxCol); c++) if (sheet.cells.get(key(r, c))?.v != null || sheet.cells.get(key(r, c))?.f) changes.push(changeAt(sheetIdx, r, c, null));
    commitChanges(changes);
  };

  const onCopy = (e: ClipboardEvent<HTMLTextAreaElement>) => {
    if (editing) return;
    e.preventDefault();
    e.clipboardData.setData("text/plain", copyRange(false));
  };
  const onCut = (e: ClipboardEvent<HTMLTextAreaElement>) => {
    if (editing) return;
    e.preventDefault();
    e.clipboardData.setData("text/plain", copyRange(true));
  };
  const onPaste = (e: ClipboardEvent<HTMLTextAreaElement>) => {
    if (editing) return;
    e.preventDefault();
    pasteText(e.clipboardData.getData("text/plain"));
  };

  return { copyRange, pasteText, clearRange, onCopy, onCut, onPaste };
}
