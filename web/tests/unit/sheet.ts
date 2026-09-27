// Workbooks for the formula and structure tests, written as { "A1": value or "=formula" }
import { key, parseCellName, type Cell, type Scalar, type Sheet, type Workbook } from "@/lib/sheet/model";

export function sheet(name: string, data: Record<string, Scalar>, id = name): Sheet {
  const cells = new Map<number, Cell>();
  let maxRow = 0;
  let maxCol = 0;
  for (const [ref, v] of Object.entries(data)) {
    const [r, c] = parseCellName(ref)!;
    cells.set(key(r, c), typeof v === "string" && v.startsWith("=") ? { v: null, f: v } : { v });
    maxRow = Math.max(maxRow, r);
    maxCol = Math.max(maxCol, c);
  }
  return {
    id,
    name,
    path: `xl/worksheets/${id}.xml`,
    cells,
    merges: [],
    colWidths: new Map(),
    rowHeights: new Map(),
    colStyles: new Map(),
    defaultColWidth: 64,
    defaultRowHeight: 20,
    maxRow,
    maxCol,
    tables: [],
    pivot: false,
  };
}

export const workbook = (...sheets: Sheet[]): Workbook => ({ sheets, styles: [{}], xfCount: 1, styleBase: new Map() });
