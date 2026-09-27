// Open, edit and save round trips: saving must change only what was edited and keep everything else in the file.
import JSZip from "jszip";
import { describe, expect, test } from "vitest";
import { Calculator, isErr, toScalar } from "@/lib/sheet/formula";
import { key, type Workbook } from "@/lib/sheet/model";
import { applyStructOp, deriveStyle, type StructOp } from "@/lib/sheet/ops";
import { buildXlsx, readXlsx, type Snapshot } from "@/lib/sheet/xlsx";
import { buildWorkbook, UNKNOWN_CONTENT, UNKNOWN_PART } from "../fixtures";

const SAMPLE = {
  name: "Data",
  rows:
    '<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" t="inlineStr"><is><t>Inline</t></is></c></row>' +
    '<row r="2"><c r="A2"><v>10</v></c><c r="B2" s="2"><v>2.5</v></c><c r="C2"><f>A2*B2</f><v>25</v></c></row>' +
    '<row r="3"><c r="A3"><v>20</v></c><c r="B3" s="2"><v>1.5</v></c><c r="C3"><f t="shared" ref="C3:C4" si="0">A3*B3</f><v>30</v></c></row>' +
    '<row r="4"><c r="A4"><v>30</v></c><c r="B4" s="2"><v>2</v></c><c r="C4"><f t="shared" si="0"/><v>60</v></c></row>' +
    '<row r="5"><c r="A5" t="b"><v>1</v></c><c r="C5" s="1"><f>SUM(C2:C4)</f><v>115</v></c></row>',
  after: '<mergeCells count="1"><mergeCell ref="A7:B8"/></mergeCells>',
};
const OTHER = { name: "Other Sheet", rows: '<row r="1"><c r="A1"><f>Data!C5*2</f><v>230</v></c></row>' };

async function open(sheets = [SAMPLE, OTHER]) {
  return readXlsx(await buildWorkbook(sheets, ["Name", "Price"], '<definedName name="Total">Data!$C$5</definedName>'));
}

/** Same steps as the editor's save: compute formula results, write, and read the file back */
async function save(zip: JSZip, book: Workbook, snapshot: Snapshot, ops: StructOp[] = []) {
  const calc = new Calculator(book);
  const valueOf = (si: number, r: number, c: number) => {
    const v = calc.value(si, r, c);
    return isErr(v) && (v.code === "#NAME?" || v.code === "#CYCLE!") ? undefined : toScalar(v);
  };
  const { blob, cells } = await buildXlsx(zip, book, snapshot, ops, valueOf);
  const buf = await blob.arrayBuffer();
  return { buf, cells, reopened: await readXlsx(buf), files: await JSZip.loadAsync(buf) };
}

const at = (book: Workbook, sheet: number, ref: string) => {
  const m = /^([A-Z]+)(\d+)$/.exec(ref)!;
  const c = m[1].charCodeAt(0) - 65;
  return book.sheets[sheet].cells.get(key(Number(m[2]) - 1, c));
};

describe("reading", () => {
  test("values, formulas and merges", async () => {
    const { book } = await open();
    expect(book.sheets.map((s) => s.name)).toEqual(["Data", "Other Sheet"]);
    expect(at(book, 0, "A1")?.v).toBe("Name");
    expect(at(book, 0, "C1")?.v).toBe("Inline");
    expect(at(book, 0, "B2")).toEqual({ v: 2.5, s: 2 });
    expect(at(book, 0, "C2")).toEqual({ v: 25, f: "=A2*B2" });
    // The shared formula is expanded for the second cell
    expect(at(book, 0, "C4")?.f).toBe("=A4*B4");
    expect(at(book, 0, "A5")?.v).toBe(true);
    expect(book.sheets[0].merges).toEqual([{ r1: 6, c1: 0, r2: 7, c2: 1 }]);
    expect(book.styles[1].bold).toBe(true);
    expect(book.styles[2].numFmt).toBe("0.00");
  });

  test("formulas give the values Excel stored", async () => {
    const { book } = await open();
    const calc = new Calculator(book);
    expect(calc.value(0, 4, 2)).toBe(115);
    expect(calc.value(1, 0, 0)).toBe(230);
  });

  test("a file that isn't a workbook is refused", async () => {
    const zip = new JSZip();
    zip.file("hello.txt", "hi");
    await expect(readXlsx(await zip.generateAsync({ type: "arraybuffer" }))).rejects.toThrow();
  });
});

describe("saving", () => {
  test("unchanged workbook keeps its sheets and unknown parts", async () => {
    const { zip, book, snapshot } = await open();
    const before = await zip.file("xl/worksheets/sheet1.xml")!.async("string");
    const { cells, files } = await save(zip, book, snapshot);
    expect(cells).toBe(0);
    expect(await files.file("xl/worksheets/sheet1.xml")!.async("string")).toBe(before);
    expect(await files.file(UNKNOWN_PART)!.async("string")).toBe(UNKNOWN_CONTENT);
  });

  test("an edited value is written, formulas that depend on it are refreshed", async () => {
    const { zip, book, snapshot } = await open();
    book.sheets[0].cells.set(key(1, 0), { v: 100 });
    const { cells, reopened, files } = await save(zip, book, snapshot);
    expect(cells).toBe(1);
    const b = reopened.book;
    expect(at(b, 0, "A2")?.v).toBe(100);
    expect(at(b, 0, "C2")).toEqual({ v: 250, f: "=A2*B2" });
    expect(at(b, 0, "C5")).toEqual({ v: 340, f: "=SUM(C2:C4)", s: 1 });
    expect(at(b, 1, "A1")?.v).toBe(680);
    // Untouched content stays as it was
    expect(at(b, 0, "A1")?.v).toBe("Name");
    expect(at(b, 0, "C4")?.f).toBe("=A4*B4");
    expect(await files.file(UNKNOWN_PART)!.async("string")).toBe(UNKNOWN_CONTENT);
    // The calculation chain is stale: removed everywhere, and Excel recalculates on open
    expect(files.file("xl/calcChain.xml")).toBeNull();
    expect(await files.file("xl/_rels/workbook.xml.rels")!.async("string")).not.toContain("calcChain");
    expect(await files.file("[Content_Types].xml")!.async("string")).not.toContain("calcChain");
    expect(await files.file("xl/workbook.xml")!.async("string")).toMatch(/fullCalcOnLoad="1"/);
  });

  test("text, new formulas, booleans and cleared cells", async () => {
    const { zip, book, snapshot } = await open();
    const cells = book.sheets[0].cells;
    cells.set(key(0, 3), { v: 'Quote " & <tag>' });
    cells.set(key(5, 0), { v: null, f: "=A2+A3" });
    cells.set(key(5, 1), { v: false });
    cells.delete(key(0, 2));
    const { reopened } = await save(zip, book, snapshot);
    const b = reopened.book;
    expect(at(b, 0, "D1")?.v).toBe('Quote " & <tag>');
    expect(at(b, 0, "A6")).toEqual({ v: 30, f: "=A2+A3" });
    expect(at(b, 0, "B6")?.v).toBe(false);
    expect(at(b, 0, "C1")).toBeUndefined();
  });

  test("a new style is based on the original one", async () => {
    const { zip, book, snapshot } = await open();
    const s = deriveStyle(book, 2, (st) => ({ ...st, italic: true }));
    expect(s).toBe(3);
    // The same change again reuses the style
    expect(deriveStyle(book, 2, (st) => ({ ...st, italic: true }))).toBe(3);
    book.sheets[0].cells.set(key(1, 1), { v: 2.5, s });
    const { reopened } = await save(zip, book, snapshot);
    const b = reopened.book;
    const cell = at(b, 0, "B2")!;
    expect(b.styles[cell.s!]).toMatchObject({ italic: true, numFmt: "0.00" });
    // Existing styles keep their numbers
    expect(b.styles[1].bold).toBe(true);
  });

  test("inserting rows moves cells, formulas, merges and defined names", async () => {
    const { zip, book, snapshot } = await open();
    const op: StructOp = { kind: "insertRows", sheet: book.sheets[0].id, at: 2, count: 2 };
    applyStructOp(book.sheets, op);
    applyStructOp([...snapshot.values()], op, false);
    const { reopened, files } = await save(zip, book, snapshot, [op]);
    const b = reopened.book;
    expect(at(b, 0, "A2")?.v).toBe(10);
    expect(at(b, 0, "A5")?.v).toBe(20);
    expect(at(b, 0, "C7")?.f).toBe("=SUM(C2:C6)");
    expect(at(b, 0, "C6")?.f).toBe("=A6*B6");
    expect(at(b, 1, "A1")?.f).toBe("=Data!C7*2");
    expect(b.sheets[0].merges).toEqual([{ r1: 8, c1: 0, r2: 9, c2: 1 }]);
    expect(await files.file("xl/workbook.xml")!.async("string")).toContain("Data!$C$7");
  });

  test("deleting the rows a formula points to gives #REF!", async () => {
    const { zip, book, snapshot } = await open();
    const op: StructOp = { kind: "deleteRows", sheet: book.sheets[0].id, at: 4, count: 1 };
    applyStructOp(book.sheets, op);
    applyStructOp([...snapshot.values()], op, false);
    const { reopened } = await save(zip, book, snapshot, [op]);
    expect(at(reopened.book, 1, "A1")?.f).toBe("=Data!#REF!*2");
  });
});
