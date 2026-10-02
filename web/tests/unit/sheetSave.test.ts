// Saving a spreadsheet from the editor (components/sheet/save.ts): what is saved is the workbook as it was when saving
// started; edits made while it uploads stay unsaved, and a save that fails changes nothing
import { afterEach, describe, expect, test, vi } from "vitest";
import { api, type Node } from "@/api";
import type { Session } from "@/components/sheet/session";
import { saveSession } from "@/components/sheet/save";
import { Calculator } from "@/ooxml/xlsx/formula";
import { key } from "@/ooxml/xlsx/model";
import type { StructOp } from "@/ooxml/xlsx/ops";
import { readXlsx } from "@/ooxml/xlsx/workbook";
import { buildWorkbook } from "../fixtures";

const ROWS =
  '<row r="1"><c r="A1"><v>10</v></c><c r="B1"><v>2.5</v></c><c r="C1"><f>A1*B1</f><v>25</v></c></row>' +
  '<row r="2"><c r="A2"><v>20</v></c><c r="B2"><v>1</v></c><c r="C2"><f>NOSUCHFUNCTION(A2)</f><v>7</v></c></row>';

async function session(): Promise<Session> {
  const { zip, book, snapshot } = await readXlsx(await buildWorkbook([{ name: "Data", rows: ROWS }]));
  return { zip, book, snapshot, calc: new Calculator(book), base: 100, ops: [], undo: [], redo: [], version: 0, saved: 0, sheet: 0, sel: { anchor: [0, 0], focus: [0, 0] } };
}
const saved = (updated_at: number) => ({ id: "book", name: "Book.xlsx", updated_at }) as Node;
const value = (s: Pick<Session, "book">, ref: [number, number]) => s.book.sheets[0].cells.get(key(...ref))?.v;

afterEach(() => vi.restoreAllMocks());

describe("saving a spreadsheet", () => {
  test("saves the workbook as it was when saving started, with formulas worked out again", async () => {
    const s = await session();
    const sheetId = s.book.sheets[0].id;
    // A1: 10 → 40, so C1 (A1*B1) becomes 100
    s.book.sheets[0].cells.set(key(0, 0), { v: 40 });
    s.version = 1;
    const op: StructOp = { kind: "insertRows", sheet: sheetId, at: 0, count: 1 };
    let uploaded: Blob | null = null;
    const saveContent = vi.spyOn(api, "saveContent").mockImplementation(async (_id, content) => {
      uploaded = content as Blob;
      // Edits while it uploads: a cell, and a row inserted at the top
      s.book.sheets[0].cells.set(key(1, 0), { v: 99 });
      s.ops.push(op);
      s.version = 2;
      return saved(200);
    });
    const { node, cells } = await saveSession(s, "book");
    expect(node.updated_at).toBe(200);
    expect(cells).toBeGreaterThan(0);
    // Saved over the version it was opened at
    expect(saveContent).toHaveBeenCalledWith("book", expect.any(Blob), 100);

    const back = await readXlsx(await uploaded!.arrayBuffer());
    expect(value(back, [0, 0])).toBe(40);
    expect(value(back, [0, 2])).toBe(100);
    expect(value(back, [1, 0])).toBe(20);
    // A function this editor doesn't know keeps the result Excel worked out
    expect(value(back, [1, 2])).toBe(7);

    // What was saved is the new starting point; the edits made meanwhile are still to save
    expect([s.saved, s.version, s.base]).toEqual([1, 2, 200]);
    expect(s.ops).toEqual([op]);
    const baseline = s.snapshot.get(sheetId)!.cells;
    // The baseline has the row inserted meanwhile too, so the next save compares like with like
    expect(baseline.get(key(1, 0))?.v).toBe(40);
    expect(baseline.get(key(1, 2))?.v).toBe(100);
    expect(baseline.get(key(2, 0))?.v).toBe(20);
  });

  test("a save that fails leaves the session as it was, to try again", async () => {
    const s = await session();
    s.book.sheets[0].cells.set(key(0, 0), { v: 40 });
    s.version = 1;
    const op: StructOp = { kind: "deleteRows", sheet: s.book.sheets[0].id, at: 1, count: 1 };
    s.ops.push(op);
    const before = { zip: s.zip, snapshot: s.snapshot, xfCount: s.book.xfCount };
    vi.spyOn(api, "saveContent").mockRejectedValue(new Error("The file was changed by someone else"));
    await expect(saveSession(s, "book")).rejects.toThrow("changed by someone else");
    expect([s.saved, s.version, s.base]).toEqual([0, 1, 100]);
    expect(s.zip).toBe(before.zip);
    expect(s.snapshot).toBe(before.snapshot);
    expect(s.ops).toEqual([op]);
    expect(s.book.xfCount).toBe(before.xfCount);
    // The original archive wasn't touched: the next save starts from the file as opened
    expect(Object.keys(s.zip.files)).toContain("xl/worksheets/sheet1.xml");
    expect(value(await readXlsx(await s.zip.generateAsync({ type: "arraybuffer" })), [0, 0])).toBe(10);
  });
});
