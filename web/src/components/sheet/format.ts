/**
 * Formatting, merging cells, inserting/deleting rows and columns.
 */
import { toast } from "sonner";
import { t } from "@/lib/i18n";
import { key, type Borders, type CellStyle, type Range } from "@/ooxml/xlsx/model";
import { applyStructOp, cloneState, deriveStyle, adjustRange, type StructOp } from "@/ooxml/xlsx/ops";
import { changeDecimals, type BorderKind, type ToolbarActions } from "./SheetToolbar";
import { cloneLayout, type Change } from "./session";
import type { WorkspaceCtx } from "./workspace";

export function createFormatting(ctx: WorkspaceCtx) {
  const { session, book, calc, sheet, sheetIdx, sel, range, active, wholeRows, wholeCols, editing, setSel, styleAt, commitChanges, changeAt, pushEntry, commitEdit } = ctx;
  // ───── Formatting ─────

  /** Range to apply formatting to: whole columns/rows only go as far as the data */
  const styleRange = (): Range => ({
    r1: range.r1,
    c1: range.c1,
    r2: wholeCols ? Math.max(sheet.maxRow, range.r1) : range.r2,
    c2: wholeRows ? Math.max(sheet.maxCol, range.c1) : range.c2,
  });

  const restyle = (fn: (s: CellStyle, r: number, c: number, g: Range) => CellStyle) => {
    if (editing) commitEdit(null);
    const g = styleRange();
    if ((g.r2 - g.r1 + 1) * (g.c2 - g.c1 + 1) > 100000) {
      toast.error(t("The selection is too large. Select a smaller range and try again."));
      return;
    }
    const changes: Change[] = [];
    for (let r = g.r1; r <= g.r2; r++)
      for (let c = g.c1; c <= g.c2; c++) {
        const cell = sheet.cells.get(key(r, c));
        const s = deriveStyle(book, cell?.s, (st) => fn(st, r, c, g));
        if (s !== (cell?.s ?? 0)) changes.push(changeAt(sheetIdx, r, c, cell ? { v: cell.v, f: cell.f } : null, s || null));
      }
    commitChanges(changes);
  };

  const applyBorder = (kind: BorderKind) => {
    const thin = { style: "thin" };
    const thick = { style: "medium" };
    restyle((s, r, c, g) => {
      const b: Borders = { ...s.border };
      const top = r === g.r1;
      const bottom = r === g.r2;
      const left = c === g.c1;
      const right = c === g.c2;
      switch (kind) {
        case "none":
          return { ...s, border: undefined };
        case "all":
          return { ...s, border: { top: thin, right: thin, bottom: thin, left: thin } };
        case "outside":
        case "thickOutside": {
          const side = kind === "outside" ? thin : thick;
          if (top) b.top = side;
          if (bottom) b.bottom = side;
          if (left) b.left = side;
          if (right) b.right = side;
          return { ...s, border: b };
        }
        default:
          if ((kind === "top" && top) || (kind === "bottom" && bottom) || (kind === "left" && left) || (kind === "right" && right)) b[kind] = thin;
          return { ...s, border: b };
      }
    });
  };

  const toggleMerge = () => {
    if (editing) commitEdit(null);
    const g = styleRange();
    const hit = sheet.merges.filter((m) => m.r1 <= g.r2 && m.r2 >= g.r1 && m.c1 <= g.c2 && m.c2 >= g.c1);
    const before = cloneLayout(sheet);
    if (hit.length) {
      const after = { ...before, merges: before.merges.filter((m) => !hit.some((h) => h.r1 === m.r1 && h.c1 === m.c1)) };
      commitChanges([], { sheet: sheetIdx, before, after });
      return;
    }
    if (g.r1 === g.r2 && g.c1 === g.c2) return;
    if ((g.r2 - g.r1 + 1) * (g.c2 - g.c1 + 1) > 10000) {
      toast.error(t("The range to merge is too large"));
      return;
    }
    // Same as Excel: keep only the top-left value, and center it
    const changes: Change[] = [];
    let dropped = false;
    for (let r = g.r1; r <= g.r2; r++)
      for (let c = g.c1; c <= g.c2; c++) {
        const cell = sheet.cells.get(key(r, c));
        if (r === g.r1 && c === g.c1) {
          const s = deriveStyle(book, cell?.s, (st) => ({ ...st, hAlign: "center", vAlign: st.vAlign ?? "center" }));
          changes.push(changeAt(sheetIdx, r, c, cell ? { v: cell.v, f: cell.f } : null, s || null));
        } else if (cell && (cell.v !== null || cell.f)) {
          dropped = true;
          changes.push(changeAt(sheetIdx, r, c, null));
        }
      }
    const after = { ...before, merges: [...before.merges, g] };
    commitChanges(changes, { sheet: sheetIdx, before, after });
    if (dropped) toast.info(t("Merging cells keeps only the upper-left value"));
  };

  const actions: ToolbarActions = {
    style: (change) => restyle((s) => change(s)),
    border: applyBorder,
    merge: toggleMerge,
    decimals: (delta) => {
      const sample = calc.value(sheetIdx, active[0], active[1]);
      const next = changeDecimals(styleAt(active[0], active[1])?.numFmt, delta, sample);
      restyle((s) => ({ ...s, numFmt: next }));
    },
    clearFormat: () => restyle(() => ({})),
    insertRows: () => structural("insertRows"),
    insertCols: () => structural("insertCols"),
    deleteRows: () => structural("deleteRows"),
    deleteCols: () => structural("deleteCols"),
  };

  // ───── Insert/delete rows and columns ─────

  const structureBlocked = sheet.pivot ? t("This sheet has a PivotTable. Insert or delete rows and columns in Excel.") : undefined;

  const structural = (kind: StructOp["kind"]) => {
    if (editing) commitEdit(null);
    if (structureBlocked) {
      toast.error(structureBlocked);
      return;
    }
    const rowsOp = kind === "insertRows" || kind === "deleteRows";
    const at = rowsOp ? range.r1 : range.c1;
    // With whole columns selected, inserting rows inserts just one row (and vice versa)
    const count = rowsOp ? (wholeCols ? 1 : range.r2 - range.r1 + 1) : wholeRows ? 1 : range.c2 - range.c1 + 1;
    const op: StructOp = { kind, sheet: sheet.id, at, count };
    // Table (ListObject) columns are maintained by Excel: if inserting/deleting columns would change a table's column count, ask the user to do it in Excel
    if (!rowsOp) {
      const end = at + count - 1;
      const inside = sheet.tables.some((t) => (kind === "insertCols" ? at > t.c1 && at <= t.c2 : at <= t.c2 && end >= t.c1));
      if (inside) {
        toast.error(t("This range contains an Excel table. Inserting or deleting columns would change the table's columns, so do this in Excel."));
        return;
      }
    } else if (kind === "deleteRows" && sheet.tables.some((t) => at <= t.r1 && at + count - 1 >= t.r1)) {
      toast.error(t("Can't delete the header row of an Excel table. Do this in Excel."));
      return;
    }
    const before = book.sheets.map((s) => cloneState(s));
    applyStructOp(book.sheets, op);
    applyStructOp([...session.snapshot.values()], op, false);
    for (const s of book.sheets)
      if (s.id === op.sheet)
        s.tables = s.tables.flatMap((t) => {
          const n = adjustRange(t, op);
          return n ? [n] : [];
        });
    session.ops.push(op);
    const after = book.sheets.map((s) => cloneState(s));
    pushEntry({ changes: [], sheet: sheetIdx, sel, struct: { op, before, after } });
    if (kind === "deleteRows" || kind === "deleteCols") {
      const a: [number, number] = [range.r1, range.c1];
      setSel({ anchor: a, focus: [rowsOp ? a[0] : range.r2, rowsOp ? range.c2 : a[1]] }, false);
    }
  };

  return { styleRange, restyle, applyBorder, toggleMerge, actions, structureBlocked, structural };
}
