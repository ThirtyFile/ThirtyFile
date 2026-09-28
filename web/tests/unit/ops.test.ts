import { describe, expect, test } from "vitest";
import { key } from "@/lib/sheet/model";
import { adjustFormula, adjustRange, adjustSqref, applyStructOp, cloneState, deriveStyle, inverseOp, type StructOp } from "@/lib/sheet/ops";
import { sheet, workbook } from "./sheet";

const op = (kind: StructOp["kind"], at: number, count = 1, target = "S"): StructOp => ({ kind, sheet: target, at, count });

describe("adjusting references", () => {
  test("inserting rows", () => {
    const ins = op("insertRows", 2, 2);
    expect(adjustFormula("=A1+A3+$A$3", "S", "S", ins)).toBe("=A1+A5+$A$5");
    // A range across the insertion point grows
    expect(adjustFormula("=SUM(A1:A10)", "S", "S", ins)).toBe("=SUM(A1:A12)");
    // Whole columns don't change
    expect(adjustFormula("=SUM(A:A)", "S", "S", ins)).toBe("=SUM(A:A)");
    // References to other sheets are left alone, references to this one from another sheet move
    expect(adjustFormula("=Other!A3+A3", "S", "S", ins)).toBe("=Other!A3+A5");
    expect(adjustFormula("='My S'!A3+A3", "Other", "My S", ins)).toBe("='My S'!A5+A3");
    // Text in quotes is not a reference
    expect(adjustFormula('="A3"&A3', "S", "S", ins)).toBe('="A3"&A5');
  });

  test("deleting columns", () => {
    const del = op("deleteCols", 1, 2);
    expect(adjustFormula("=A1+B1+D1", "S", "S", del)).toBe("=A1+#REF!+B1");
    expect(adjustFormula("=SUM(A1:E1)", "S", "S", del)).toBe("=SUM(A1:C1)");
    expect(adjustFormula("=SUM(B1:C1)", "S", "S", del)).toBe("=SUM(#REF!)");
  });

  test("ranges and sqref lists", () => {
    expect(adjustRange({ r1: 1, c1: 0, r2: 3, c2: 0 }, op("deleteRows", 1, 3))).toBeNull();
    expect(adjustRange({ r1: 1, c1: 0, r2: 5, c2: 0 }, op("deleteRows", 0, 2))).toEqual({ r1: 0, c1: 0, r2: 3, c2: 0 });
    expect(adjustSqref("A1:A3 C5", op("insertRows", 0))).toBe("A2:A4 C6");
    expect(adjustSqref("B2", op("deleteRows", 1))).toBeNull();
  });

  test("inverseOp undoes an operation", () => {
    const f = "=SUM(A2:A9)+B5";
    const ins = op("insertRows", 3, 2);
    expect(adjustFormula(adjustFormula(f, "S", "S", ins), "S", "S", inverseOp(ins))).toBe(f);
  });
});

describe("applying to sheets", () => {
  test("cells, merges, sizes and formulas in other sheets move", () => {
    const s = sheet("S", { A1: 1, A2: 2, A3: "=A1+A2" });
    s.merges = [{ r1: 1, c1: 0, r2: 2, c2: 1 }];
    s.rowHeights = new Map([[1, 40]]);
    s.cells.set(key(0, 0), { v: 1, s: 3 });
    const other = sheet("Other", { A1: "=S!A3*2" });
    const before = cloneState(s);
    applyStructOp([s, other], op("insertRows", 1));
    expect(s.cells.get(key(2, 0))?.v).toBe(2);
    expect(s.cells.get(key(3, 0))?.f).toBe("=A1+A3");
    // The new row takes the format of the row above
    expect(s.cells.get(key(1, 0))).toEqual({ v: null, s: 3 });
    expect(s.merges).toEqual([{ r1: 2, c1: 0, r2: 3, c2: 1 }]);
    expect(s.rowHeights.get(2)).toBe(40);
    expect(other.cells.get(key(0, 0))?.f).toBe("=S!A4*2");
    expect(s.maxRow).toBe(3);
    // The clone taken before is untouched
    expect(before.cells.get(key(2, 0))?.f).toBe("=A1+A2");
  });

  test("merges that shrink to one cell are dropped", () => {
    const s = sheet("S", {});
    s.merges = [{ r1: 0, c1: 0, r2: 0, c2: 1 }];
    applyStructOp([s], op("deleteCols", 1));
    expect(s.merges).toEqual([]);
  });
});

describe("styles", () => {
  test("deriveStyle shares styles with the same look", () => {
    const book = workbook(sheet("S", {}));
    book.styles = [{}, { bold: true }];
    book.xfCount = 2;
    // Making the default style bold gives a new style based on it, not style 1 (which may differ in ways the editor can't see)
    const bold = deriveStyle(book, 0, (s) => ({ ...s, bold: true }));
    expect(bold).toBe(2);
    expect(book.styleBase.get(2)).toBe(0);
    expect(deriveStyle(book, undefined, (s) => ({ ...s, bold: true }))).toBe(2);
    // Removing the change again goes back to the original style
    expect(deriveStyle(book, 2, (s) => ({ ...s, bold: false }))).toBe(0);
    expect(deriveStyle(book, 1, (s) => ({ ...s, italic: true }))).toBe(3);
    expect(book.styles[3]).toEqual({ bold: true, italic: true });
  });
});
