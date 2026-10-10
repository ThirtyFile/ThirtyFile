// Adding and deleting the rows and columns of a Word table, the way Word does it: settings copied from the row or column
// next to it, merged cells kept merged, the grid and a fixed width kept in step, and nothing changed half-way
import { describe, expect, test } from "vitest";
import { attr } from "@/ooxml/core/package";
import { cellRange, changeTable } from "@/ooxml/docx/tableOps";

const W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const W14 = "http://schemas.microsoft.com/office/word/2010/wordml";

/** Descendants by local name (happy-dom's getElementsByTagNameNS doesn't look below the first level) */
const find = (el: Element | Document, name: string) => Array.from(el.querySelectorAll("*")).filter((e) => e.localName === name);
const kids = (el: Element, name: string) => Array.from(el.children).filter((e) => e.localName === name);
const text = (el: Element) =>
  find(el, "t")
    .map((t) => t.textContent)
    .join("");

const cell = (text: string, props = "") =>
  `<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/>${props}</w:tcPr><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>${text}</w:t></w:r></w:p></w:tc>`;
const row = (cells: string, props = "") => `<w:tr w14:paraId="0000AAAA">${props}${cells}</w:tr>`;

function table(rows: string, cols = 2, extra = "") {
  const grid = Array.from({ length: cols }, () => '<w:gridCol w:w="2000"/>').join("");
  const xml = `<w:document xmlns:w="${W}" xmlns:w14="${W14}"><w:body><w:tbl><w:tblPr><w:tblW w:w="${2000 * cols}" w:type="dxa"/></w:tblPr><w:tblGrid>${grid}</w:tblGrid>${rows}</w:tbl>${extra}<w:sectPr/></w:body></w:document>`;
  const doc = new DOMParser().parseFromString(xml, "application/xml");
  const tbl = find(doc, "tbl")[0];
  const at = (r: number, c: number) => kids(kids(tbl, "tr")[r], "tc")[c];
  const grid2 = () => kids(find(tbl, "tblGrid")[0], "gridCol").map((g) => attr(g, "w"));
  return { doc, tbl, at, rows: () => kids(tbl, "tr"), grid: grid2, width: () => attr(find(tbl, "tblW")[0], "w") };
}

const texts = (tbl: Element) => kids(tbl, "tr").map((tr) => kids(tr, "tc").map(text));

describe("rows", () => {
  test("a row inserted below copies the row's and cells' settings, with empty cells, and the cursor goes to its cell", () => {
    const t = table(row(cell("A", '<w:shd w:val="clear" w:fill="FFFF00"/>') + cell("B"), '<w:trPr><w:trHeight w:val="500"/><w:ins w:id="1"/></w:trPr>') + row(cell("C") + cell("D")));
    const focus = changeTable(t.at(0, 1), "rowBelow");
    expect(texts(t.tbl)).toEqual([
      ["A", "B"],
      ["", ""],
      ["C", "D"],
    ]);
    const added = t.rows()[1];
    expect(added.getAttribute("w14:paraId")).toBeNull();
    expect(find(added, "trHeight")).toHaveLength(1);
    expect(find(added, "ins")).toHaveLength(0);
    expect(find(kids(added, "tc")[0], "shd")).toHaveLength(1);
    expect(kids(added, "tc").every((tc) => find(tc, "jc").length === 1 && find(tc, "r").length === 0)).toBe(true);
    expect(focus).toBe(kids(added, "tc")[1]);
  });

  test("a row inserted inside a vertically merged cell extends the merge, in Word's order of a cell's settings", () => {
    const merged = (text: string, v: string) => cell(text, `<w:vMerge${v}/><w:shd w:fill="EEEEEE"/>`);
    const t = table(row(merged("M", ' w:val="restart"') + cell("1")) + row(merged("", "") + cell("2")) + row(cell("x") + cell("3")));
    changeTable(t.at(0, 1), "rowBelow");
    const added = kids(t.rows()[1], "tc")[0];
    expect(Array.from(find(added, "tcPr")[0].children).map((c) => c.localName)).toEqual(["tcW", "vMerge", "shd"]);
    expect(attr(find(added, "vMerge")[0], "val")).toBeNull();
    // Above the first row of the merge, or below its last, the new row isn't in it
    changeTable(t.at(0, 1), "rowAbove");
    expect(find(kids(t.rows()[0], "tc")[0], "vMerge")).toHaveLength(0);
    changeTable(t.at(3, 1), "rowBelow");
    expect(find(kids(t.rows()[4], "tc")[0], "vMerge")).toHaveLength(0);
  });

  test("deleting the first row of a merged cell hands the merge to the row below", () => {
    const t = table(row(cell("M", '<w:vMerge w:val="restart"/>') + cell("1")) + row(cell("", "<w:vMerge/>") + cell("2")));
    const focus = changeTable(t.at(0, 1), "deleteRow");
    expect(texts(t.tbl)).toEqual([["", "2"]]);
    expect(attr(find(t.at(0, 0), "vMerge")[0], "val")).toBe("restart");
    expect(focus).toBe(t.at(0, 1));
  });

  test("deleting the last row deletes the table, and a paragraph stays where it ended its place", () => {
    const t = table(row(cell("A") + cell("B")));
    expect(changeTable(t.at(0, 0), "deleteRow")).toBeNull();
    const body = find(t.doc, "body")[0];
    expect(Array.from(body.children).map((c) => c.localName)).toEqual(["p", "sectPr"]);
  });
});

describe("columns", () => {
  test("a column inserted to the right takes the next column's width, grows a fixed table, and widens a cell over its place", () => {
    const t = table(row(cell("A") + cell("B") + cell("C")) + row(cell("wide", '<w:gridSpan w:val="2"/>') + cell("F")), 3);
    const focus = changeTable(t.at(0, 0), "colRight");
    expect(texts(t.tbl)).toEqual([
      ["A", "", "B", "C"],
      ["wide", "F"],
    ]);
    expect(attr(find(t.at(1, 0), "gridSpan")[0], "val")).toBe("3");
    expect(t.grid()).toEqual(["2000", "2000", "2000", "2000"]);
    expect(t.width()).toBe("8000");
    expect(Array.from(find(t.at(0, 1), "tcPr")[0].children).map((c) => c.localName)).toEqual(["tcW"]);
    expect(focus).toBe(t.at(0, 1));
  });

  test("a column inserted to the left of the first goes first", () => {
    const t = table(row(cell("A") + cell("B")));
    changeTable(t.at(0, 0), "colLeft");
    expect(texts(t.tbl)).toEqual([["", "A", "B"]]);
  });

  test("deleting a column removes its cells and narrows the cells partly in it", () => {
    const t = table(row(cell("A") + cell("B") + cell("C")) + row(cell("wide", '<w:gridSpan w:val="2"/>') + cell("F")), 3);
    const focus = changeTable(t.at(0, 1), "deleteCol");
    expect(texts(t.tbl)).toEqual([
      ["A", "C"],
      ["wide", "F"],
    ]);
    expect(find(t.at(1, 0), "gridSpan")).toHaveLength(0);
    expect(t.grid()).toEqual(["2000", "2000"]);
    expect(t.width()).toBe("4000");
    expect(focus).toBe(t.at(0, 1));
  });

  test("deleting the only column deletes the table", () => {
    const t = table(row(cell("A")) + row(cell("B")), 1, "<w:p/>");
    expect(changeTable(t.at(0, 0), "deleteCol")).toBeNull();
    expect(find(t.doc, "tbl")).toHaveLength(0);
  });
});

test("a table whose cells are inside content controls isn't changed at all", () => {
  const t = table(row(cell("A") + `<w:sdt><w:sdtContent>${cell("B")}</w:sdtContent></w:sdt>`));
  const before = new XMLSerializer().serializeToString(t.tbl);
  for (const op of ["rowBelow", "colRight", "deleteRow", "deleteCol"] as const) expect(changeTable(t.at(0, 0), op)).toBeUndefined();
  expect(new XMLSerializer().serializeToString(t.tbl)).toBe(before);
});

const spans = (tbl: Element) => kids(tbl, "tr").map((tr) => kids(tr, "tc").map((tc) => Number(attr(find(tc, "gridSpan")[0], "val") ?? 1)));
const merges = (tbl: Element) => kids(tbl, "tr").map((tr) => kids(tr, "tc").map((tc) => (find(tc, "vMerge")[0] ? (attr(find(tc, "vMerge")[0], "val") ?? "continue") : "-")));

describe("merging", () => {
  test("cells merged across a row and down hold the text of all of them, and the range grows over merged cells", () => {
    const t = table(row(cell("A") + cell("B") + cell("C")) + row(cell("D") + cell("E", '<w:gridSpan w:val="2"/>')) + row(cell("") + cell("H") + cell("I")), 3);
    // From B to D: grown to E's span, so columns 0–2 of the first two rows
    expect(cellRange(t.at(0, 1), t.at(1, 0))?.length).toBe(5);
    const merged = changeTable(t.at(0, 1), "merge", { other: t.at(1, 0) });
    expect(merged).toBe(t.at(0, 0));
    expect(find(merged!, "p").map(text)).toEqual(["A", "B", "C", "D", "E"]);
    expect(spans(t.tbl)).toEqual([[3], [3], [1, 1, 1]]);
    expect(merges(t.tbl)).toEqual([["restart"], ["continue"], ["-", "-", "-"]]);
    expect(attr(find(merged!, "tcW")[0], "w")).toBe("6000");
    expect(Array.from(find(merged!, "tcPr")[0].children).map((c) => c.localName)).toEqual(["tcW", "gridSpan", "vMerge"]);
    expect(text(t.at(1, 0))).toBe("");
  });

  test("merging empty cells into one with text keeps just its text", () => {
    const t = table(row(cell("A") + cell("")));
    const merged = changeTable(t.at(0, 0), "merge", { other: t.at(0, 1) });
    expect(find(merged!, "p").map(text)).toEqual(["A"]);
    expect(spans(t.tbl)).toEqual([[2]]);
  });
});

describe("splitting", () => {
  test("a cell split into columns gives the grid new edges, and the other rows span them", () => {
    const t = table(row(cell("A") + cell("B")) + row(cell("C") + cell("D")));
    const first = changeTable(t.at(0, 1), "split", { cols: 2 });
    expect(texts(t.tbl)).toEqual([
      ["A", "B", ""],
      ["C", "D"],
    ]);
    expect(spans(t.tbl)).toEqual([
      [1, 1, 1],
      [1, 2],
    ]);
    expect(t.grid()).toEqual(["2000", "1000", "1000"]);
    expect(first).toBe(t.at(0, 1));
    expect(attr(find(t.at(0, 2), "tcW")[0], "w")).toBe("1000");
  });

  test("a cell split into rows gets new rows below, in which the row's other cells stay merged down", () => {
    const t = table(row(cell("A") + cell("B")));
    changeTable(t.at(0, 1), "split", { rows: 3 });
    expect(texts(t.tbl)).toEqual([
      ["A", "B"],
      ["", ""],
      ["", ""],
    ]);
    expect(merges(t.tbl)).toEqual([
      ["restart", "-"],
      ["continue", "-"],
      ["continue", "-"],
    ]);
  });

  test("a cell merged across rows splits back into groups of them, and the split can be undone into one row each", () => {
    const four = () => table([0, 1, 2, 3].map((i) => row(cell(i ? "" : "M", `<w:vMerge${i ? "" : ' w:val="restart"'}/>`) + cell(String(i)))).join(""));
    const t = four();
    changeTable(t.at(2, 0), "split", { rows: 2 });
    expect(merges(t.tbl).map((r) => r[0])).toEqual(["restart", "continue", "restart", "continue"]);
    const u = four();
    changeTable(u.at(0, 0), "split", { rows: 4 });
    expect(merges(u.tbl).map((r) => r[0])).toEqual(["-", "-", "-", "-"]);
    // Three rows can't share four out evenly
    const v = four();
    expect(changeTable(v.at(0, 0), "split", { rows: 3 })).toBeUndefined();
  });

  test("a merged cell split into columns splits in every row it is merged across", () => {
    const t = table(row(cell("M", '<w:gridSpan w:val="2"/><w:vMerge w:val="restart"/>')) + row(cell("", '<w:gridSpan w:val="2"/><w:vMerge/>')));
    changeTable(t.at(0, 0), "split", { cols: 2 });
    expect(spans(t.tbl)).toEqual([
      [1, 1],
      [1, 1],
    ]);
    expect(merges(t.tbl)).toEqual([
      ["restart", "restart"],
      ["continue", "continue"],
    ]);
    expect(texts(t.tbl)).toEqual([
      ["M", ""],
      ["", ""],
    ]);
  });

  test("a split that can't be made changes nothing", () => {
    const t = table(row(cell("A") + cell("B")));
    const before = new XMLSerializer().serializeToString(t.tbl);
    expect(changeTable(t.at(0, 0), "split", { cols: 1, rows: 1 })).toBeUndefined();
    expect(changeTable(t.at(0, 0), "split", { cols: 99 })).toBeUndefined();
    expect(new XMLSerializer().serializeToString(t.tbl)).toBe(before);
  });
});
