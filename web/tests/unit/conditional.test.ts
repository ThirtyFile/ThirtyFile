// @vitest-environment jsdom
// (jsdom: happy-dom's getElementsByTagNameNS doesn't take "*" for any namespace, which the sheet code uses)
import { describe, expect, test } from "vitest";
import { computeConditional } from "@/lib/sheet/conditional";
import { key } from "@/lib/sheet/model";
import { sheet, workbook } from "./sheet";

/** Decorated cells of a sheet with one "expression" rule over A1:A10 */
function decorated(data: Record<string, number>, formula: string) {
  const book = workbook(sheet("S", data));
  const xml = `<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><conditionalFormatting sqref="A1:A10"><cfRule type="expression" dxfId="0" priority="1"><formula>${formula}</formula></cfRule></conditionalFormatting></worksheet>`;
  const out = computeConditional({
    book,
    sheetIndex: 0,
    sheetDoc: new DOMParser().parseFromString(xml, "application/xml"),
    dxfs: [{ bg: "#ff0000" }],
    theme: null,
    value: (r, c) => book.sheets[0].cells.get(key(r, c))?.v ?? null,
  });
  return out.size;
}

describe("conditional formatting formulas", () => {
  const data: Record<string, number> = { J100000: 1 };
  for (let r = 1; r <= 10; r++) data[`A${r}`] = r;

  test("an ordinary rule formula applies", () => {
    expect(decorated(data, "$A1&gt;5")).toBe(5);
    expect(decorated(data, "COUNTIF($A$1:$J$10,1)&gt;0")).toBe(10);
  });

  test("the rule formulas of a sheet share one work limit, and a rule that runs out isn't applied", () => {
    // Each check visits 1,000,000 positions: allowed for one formula, but not ten times over
    expect(decorated(data, "COUNTIF($A$1:$J$100000,1)&gt;0")).toBe(0);
  });
});
