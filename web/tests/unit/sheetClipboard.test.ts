import { beforeEach, expect, test, vi } from "vitest";
import { clipboardRange, clipStore, createClipboard, MAX_CLIP_CELLS } from "@/components/sheet/clipboard";
import { applyChanges, changeAt } from "@/components/sheet/history";
import { parseTsv, type Change } from "@/components/sheet/session";
import type { WorkspaceCtx } from "@/components/sheet/workspace";
import { Calculator } from "@/ooxml/xlsx/formula";
import { colOf, key, MAX_COLS, MAX_ROWS, rowOf, type Range, type Workbook } from "@/ooxml/xlsx/model";
import { sheet, workbook } from "./sheet";

vi.mock("sonner", () => ({ toast: { info: vi.fn<() => void>(), error: vi.fn<() => void>() } }));
const range = (r: number, c: number): Range => ({ r1: r, r2: r, c1: c, c2: c });
function clipboard(book: Workbook, g: Range, sheetIdx = 0) {
  const calc = new Calculator(book);
  const changes: Change[][] = [];
  const s = book.sheets[sheetIdx];
  const ctx = {
    book,
    calc,
    sheet: s,
    sheetIdx,
    range: g,
    editing: null,
    setSel: vi.fn<() => void>(),
    setClip: vi.fn<() => void>(),
    styleAt: (r: number, c: number) => book.styles[s.cells.get(key(r, c))?.s ?? 0],
    changeAt: (i: number, r: number, c: number, v: Parameters<typeof changeAt>[4], style?: number | null) => changeAt(book, i, r, c, v, style),
    commitChanges: (ch: Change[]) => {
      changes.push(ch);
      applyChanges(book, ch, "after");
      calc.invalidate();
    },
  } as unknown as WorkspaceCtx;
  return { ...createClipboard(ctx, { styleRange: () => g }), changes };
}
beforeEach(() => {
  clipStore.current = null;
});

test("cross-workbook cut/paste keeps the source and unrelated destination cells", () => {
  const a = workbook(sheet("Source", { A1: "from A" }));
  const b = workbook(sheet("Destination", { A1: "keep B", B1: "replace B" }));
  const text = clipboard(a, range(0, 0)).copyRange(true)!;
  expect(clipboardRange(b, 0)).toBeNull();
  clipboard(b, range(0, 1)).pasteText(text);
  expect(b.sheets[0].cells.get(key(0, 0))?.v).toBe("keep B");
  expect(b.sheets[0].cells.get(key(0, 1))?.v).toBe("from A");
  expect(a.sheets[0].cells.get(key(0, 0))?.v).toBe("from A");
  expect(clipStore.current).toBeNull();
});

test("cross-workbook paste remaps styles and never looks up the source sheet in the destination", () => {
  const a = workbook(sheet("One", {}), sheet("Two", { A1: "value" }));
  a.styles.push({ bold: true });
  a.xfCount = 2;
  a.sheets[1].cells.get(key(0, 0))!.s = 1;
  const b = workbook(sheet("Only", { A1: "untouched" }));
  b.styles.push({ italic: true });
  b.xfCount = 2;
  const text = clipboard(a, range(0, 0), 1).copyRange(true)!;
  clipboard(b, range(0, 1)).pasteText(text);
  const cell = b.sheets[0].cells.get(key(0, 1))!;
  expect(b.styles[cell.s!]).toEqual({ bold: true });
  expect(b.sheets[0].cells.get(key(0, 0))?.v).toBe("untouched");
});

test("same-workbook cut moves cells and records a reversible overlapping change", () => {
  const book = workbook(sheet("Sheet1", { A1: "a", B1: "b" }));
  const text = clipboard(book, { r1: 0, r2: 0, c1: 0, c2: 1 }).copyRange(true)!;
  const dest = clipboard(book, range(0, 1));
  dest.pasteText(text);
  expect(book.sheets[0].cells.get(key(0, 0))).toBeUndefined();
  expect(book.sheets[0].cells.get(key(0, 1))?.v).toBe("a");
  expect(book.sheets[0].cells.get(key(0, 2))?.v).toBe("b");
  applyChanges(book, [...dest.changes[0]].reverse(), "before");
  expect(book.sheets[0].cells.get(key(0, 0))?.v).toBe("a");
  expect(book.sheets[0].cells.get(key(0, 1))?.v).toBe("b");
  expect(book.sheets[0].cells.get(key(0, 2))).toBeUndefined();
});

test.each([
  [0, MAX_COLS - 1, "x\ty"],
  [MAX_ROWS - 1, 0, "x\ny"],
])("overflow paste at %i,%i changes no cells or history", (r, c, text) => {
  const book = workbook(sheet("Sheet1", { A2: "keep A2" }));
  const dest = clipboard(book, range(r, c));
  dest.pasteText(text);
  expect(book.sheets[0].cells.get(key(1, 0))?.v).toBe("keep A2");
  expect(dest.changes).toHaveLength(0);
});

test("an overflowing internal cut leaves source and clipboard intact", () => {
  const book = workbook(sheet("Sheet1", { A1: "a", B1: "b" }));
  const text = clipboard(book, { r1: 0, r2: 0, c1: 0, c2: 1 }).copyRange(true)!;
  clipboard(book, range(0, MAX_COLS - 1)).pasteText(text);
  expect(book.sheets[0].cells.get(key(0, 0))?.v).toBe("a");
  expect(clipStore.current?.cut).toBe(true);
});

test("the shared mutation boundary rejects invalid coordinates before key collisions", () => {
  const book = workbook(sheet("Sheet1", { A2: "keep" }));
  expect(() => changeAt(book, 0, 0, MAX_COLS, { v: "x" })).toThrow(RangeError);
  expect(() => changeAt(book, 0, MAX_ROWS, 0, { v: "x" })).toThrow(RangeError);
  expect(() => changeAt(book, 0, -1, 0, { v: "x" })).toThrow(RangeError);
});

test("huge sparse copy/fill reject before walking the rectangle; clear visits only stored cells", () => {
  const book = workbook(sheet("Sheet1", { A1: "a", XFD1048576: "far" }));
  const g = { r1: 0, r2: MAX_ROWS - 1, c1: 0, c2: MAX_COLS - 1 };
  const get = vi.spyOn(book.sheets[0].cells, "get");
  const op = clipboard(book, g);
  expect(op.copyRange(false)).toBeNull();
  op.pasteText("fill");
  expect(get).not.toHaveBeenCalled();
  expect(op.changes).toHaveLength(0);
  op.clearRange();
  expect(op.changes[0]).toHaveLength(2);
  expect(book.sheets[0].cells.size).toBe(0);
});

test("oversized TSV is bounded while parsing, including quoted separators", () => {
  expect(() => parseTsv("\t".repeat(MAX_CLIP_CELLS), MAX_CLIP_CELLS)).toThrow(RangeError);
  expect(parseTsv('"a\tb\nc"\tvalue', 2)).toEqual([["a\tb\nc", "value"]]);
  const book = workbook(sheet("Sheet1", {}));
  const op = clipboard(book, range(0, 0));
  op.pasteText("\t".repeat(MAX_CLIP_CELLS));
  expect(op.changes).toHaveLength(0);
});

test("an exact-boundary single-cell paste succeeds", () => {
  const book = workbook(sheet("Sheet1", {}));
  clipboard(book, range(MAX_ROWS - 1, MAX_COLS - 1)).pasteText("last");
  const [k, cell] = [...book.sheets[0].cells][0];
  expect([rowOf(k), colOf(k), cell.v]).toEqual([MAX_ROWS - 1, MAX_COLS - 1, "last"]);
});

test("oversized text rejects copy and paste without replacing the clipboard or changing cells", () => {
  const source = workbook(sheet("Source", { A1: "kept clipboard" }));
  clipboard(source, range(0, 0)).copyRange(false);
  const previous = clipStore.current;
  const huge = "x".repeat(8 * 1024 * 1024 + 1);
  const book = workbook(sheet("Destination", { A1: huge }));
  const op = clipboard(book, range(0, 0));
  expect(op.copyRange(false)).toBeNull();
  expect(clipStore.current).toBe(previous);
  op.pasteText(huge);
  expect(op.changes).toHaveLength(0);
  expect(book.sheets[0].cells.get(0)?.v).toBe(huge);
});

test("clearing too many populated cells is refused atomically", () => {
  const book = workbook(sheet("Sheet1", {}));
  for (let k = 0; k <= MAX_CLIP_CELLS; k++) book.sheets[0].cells.set(k, { v: "keep" });
  const op = clipboard(book, { r1: 0, c1: 0, r2: Math.floor(MAX_CLIP_CELLS / MAX_COLS), c2: MAX_COLS - 1 });
  op.clearRange();
  expect(op.changes).toHaveLength(0);
  expect(book.sheets[0].cells.size).toBe(MAX_CLIP_CELLS + 1);
  expect(book.sheets[0].cells.get(0)?.v).toBe("keep");
});
