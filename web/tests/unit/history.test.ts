// The spreadsheet editor's change history (components/sheet/history.ts): committing changes, undo and redo
import JSZip from "jszip";
import { describe, expect, test } from "vitest";
import { changeAt, commit, redo, undo } from "@/components/sheet/history";
import type { Sel, Session } from "@/components/sheet/session";
import { key } from "@/ooxml/xlsx/model";
import { sheet, workbook } from "./sheet";

function session(data: Record<string, string | number>): Session {
  return {
    zip: new JSZip(),
    book: workbook(sheet("Sheet1", data)),
    snapshot: new Map(),
    calc: { invalidate: () => {} } as unknown as Session["calc"],
    base: 0,
    ops: [],
    undo: [],
    redo: [],
    version: 0,
    saved: 0,
    sheet: 0,
    sel: { anchor: [0, 0], focus: [0, 0] },
  };
}

const value = (s: Session, r: number, c: number) => s.book.sheets[0].cells.get(key(r, c))?.v ?? null;
const sel: Sel = { anchor: [0, 0], focus: [0, 0] };

describe("sheet history", () => {
  test("a change is applied and counted, and undo and redo take it back and forth", () => {
    const s = session({ A1: 1 });
    expect(commit(s, [changeAt(s.book, 0, 0, 0, { v: 2 }), changeAt(s.book, 0, 1, 0, { v: "new" })], 0, sel)).toBe(true);
    expect([value(s, 0, 0), value(s, 1, 0), s.version]).toEqual([2, "new", 1]);
    expect(s.book.sheets[0].maxRow).toBe(1);

    expect(undo(s)?.changes).toHaveLength(2);
    expect([value(s, 0, 0), value(s, 1, 0), s.version]).toEqual([1, null, 2]);
    expect(redo(s)).toBeDefined();
    expect([value(s, 0, 0), value(s, 1, 0)]).toEqual([2, "new"]);
    expect(redo(s)).toBeUndefined();
  });

  test("a change that changes nothing isn't recorded", () => {
    const s = session({ A1: 1 });
    expect(commit(s, [changeAt(s.book, 0, 0, 0, { v: 1 })], 0, sel)).toBe(false);
    expect([s.undo.length, s.version]).toEqual([0, 0]);
  });

  test("a new change after undo drops what could be redone", () => {
    const s = session({});
    commit(s, [changeAt(s.book, 0, 0, 0, { v: "a" })], 0, sel);
    undo(s);
    expect(s.redo).toHaveLength(1);
    commit(s, [changeAt(s.book, 0, 0, 1, { v: "b" })], 0, sel);
    expect(s.redo).toHaveLength(0);
    expect(redo(s)).toBeUndefined();
  });

  test("clearing a cell keeps its style unless the style is cleared too", () => {
    const s = session({});
    s.book.sheets[0].cells.set(key(0, 0), { v: 5, s: 3 });
    commit(s, [changeAt(s.book, 0, 0, 0, null)], 0, sel);
    expect(s.book.sheets[0].cells.get(key(0, 0))).toEqual({ v: null, s: 3 });
    commit(s, [changeAt(s.book, 0, 0, 0, null, null)], 0, sel);
    expect(s.book.sheets[0].cells.has(key(0, 0))).toBe(false);
  });

  test("at most 200 steps are kept, the oldest dropped first", () => {
    const s = session({});
    for (let i = 1; i <= 205; i++) commit(s, [changeAt(s.book, 0, 0, 0, { v: i })], 0, sel);
    expect(s.undo).toHaveLength(200);
    while (undo(s));
    // The first five steps can't be taken back any more
    expect(value(s, 0, 0)).toBe(5);
  });
});
