/**
 * Mouse: selecting cells/whole rows/whole columns, click-to-insert references while typing formulas, drag-resizing columns and rows, auto-fitting column width.
 */
import { type MouseEvent, useEffect } from "react";
import { MAX_COLS, MAX_ROWS, cellName, key, mergeAt, normRange, rangeName } from "@/lib/sheet/model";
import { HEADER_H, HEADER_W, displayText, fontOf, hitTest } from "./renderer";
import { cloneLayout, editText } from "./session";
import type { WorkspaceCtx } from "./workspace";

const RESIZE_ZONE = 4;

export function useGridMouse(ctx: WorkspaceCtx) {
  const {
    calc,
    sheet,
    sheetIdx,
    sel,
    range,
    wholeRows,
    wholeCols,
    editing,
    size,
    rows,
    cols,
    view,
    inputRef,
    scrollRef,
    canvasRef,
    drag,
    refAnchor,
    setSel,
    setSelState,
    setMenuTarget,
    cursor,
    setCursor,
    bumpLayout,
    styleAt,
    focusGrid,
    commitChanges,
    applyLayout,
    startEdit,
    commitEdit,
    expectsRef,
    insertRef,
  } = ctx;
  // ───── Column widths and row heights ─────

  /** Whether the mouse is on a row/column header border (drag to resize) */
  const resizeHit = (x: number, y: number): { kind: "resize-col" | "resize-row"; index: number } | null => {
    const v = view();
    if (y < HEADER_H && x > HEADER_W) {
      const c = cols.indexAt(x - HEADER_W + v.scrollX);
      const right = HEADER_W + cols.offset(c + 1) - v.scrollX;
      const left = HEADER_W + cols.offset(c) - v.scrollX;
      if (Math.abs(x - right) <= RESIZE_ZONE) return { kind: "resize-col", index: c };
      if (Math.abs(x - left) <= RESIZE_ZONE && c > 0) return { kind: "resize-col", index: c - 1 };
    }
    if (x < HEADER_W && y > HEADER_H) {
      const r = rows.indexAt(y - HEADER_H + v.scrollY);
      const bottom = HEADER_H + rows.offset(r + 1) - v.scrollY;
      const top = HEADER_H + rows.offset(r) - v.scrollY;
      if (Math.abs(y - bottom) <= RESIZE_ZONE) return { kind: "resize-row", index: r };
      if (Math.abs(y - top) <= RESIZE_ZONE && r > 0) return { kind: "resize-row", index: r - 1 };
    }
    return null;
  };

  /** Auto-fit column width: measure the displayed text of every cell in the column */
  const autofit = (c: number) => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!ctx) return;
    let w = 0;
    for (let r = 0; r <= Math.min(sheet.maxRow, 5000); r++) {
      const cell = sheet.cells.get(key(r, c));
      if (!cell || mergeAt(sheet, r, c)) continue;
      const style = styleAt(r, c);
      ctx.font = fontOf(style);
      const t = displayText(calc.value(sheetIdx, r, c), style).text;
      w = Math.max(w, ...t.split("\n").map((l) => ctx.measureText(l).width));
    }
    const before = cloneLayout(sheet);
    const after = cloneLayout(sheet);
    after.colWidths.set(c, Math.max(20, Math.ceil(w + 12)));
    commitChanges([], { sheet: sheetIdx, before, after });
  };

  // ───── Mouse ─────

  const pointer = (e: { clientX: number; clientY: number }) => {
    const rect = scrollRef.current!.getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top };
  };

  const onMouseDown = (e: MouseEvent<HTMLDivElement>) => {
    const el = scrollRef.current!;
    const { x, y } = pointer(e);
    // Clicked on a scrollbar
    if (x > el.clientWidth || y > el.clientHeight) return;
    const hit = hitTest(view(), x, y);
    if (e.button === 2) {
      setMenuTarget(hit.area === "row" ? "row" : hit.area === "col" ? "col" : "cell");
      if (editing) commitEdit(null);
      if (hit.area === "cell" && !(hit.r >= range.r1 && hit.r <= range.r2 && hit.c >= range.c1 && hit.c <= range.c2))
        setSel({ anchor: [hit.r, hit.c], focus: [hit.r, hit.c] }, false);
      else if (hit.area === "row" && !(wholeRows && hit.r >= range.r1 && hit.r <= range.r2))
        setSel({ anchor: [hit.r, 0], focus: [hit.r, MAX_COLS - 1] }, false);
      else if (hit.area === "col" && !(wholeCols && hit.c >= range.c1 && hit.c <= range.c2))
        setSel({ anchor: [0, hit.c], focus: [MAX_ROWS - 1, hit.c] }, false);
      return;
    }
    if (e.button !== 0) return;
    e.preventDefault();
    const rz = resizeHit(x, y);
    if (rz) {
      if (editing) commitEdit(null);
      if (e.detail === 2) {
        if (rz.kind === "resize-col") autofit(rz.index);
        return;
      }
      drag.current = {
        ...rz,
        start: rz.kind === "resize-col" ? x : y,
        size: rz.kind === "resize-col" ? cols.sizeOf(rz.index) : rows.sizeOf(rz.index),
        before: cloneLayout(sheet),
      };
      return;
    }
    if (hit.area === "cell" && expectsRef()) {
      const start = insertRef(cellName(hit.r, hit.c));
      drag.current = { kind: "ref", refStart: start };
      refAnchor.current = [hit.r, hit.c];
      return;
    }
    if (editing) commitEdit(null);
    focusGrid();
    if (hit.area === "corner") {
      setSel({ anchor: [0, 0], focus: [MAX_ROWS - 1, MAX_COLS - 1] }, false);
      return;
    }
    if (hit.area === "col") {
      setSel(e.shiftKey ? { anchor: [0, sel.anchor[1]], focus: [MAX_ROWS - 1, hit.c] } : { anchor: [0, hit.c], focus: [MAX_ROWS - 1, hit.c] }, false);
      drag.current = { kind: "col" };
    } else if (hit.area === "row") {
      setSel(e.shiftKey ? { anchor: [sel.anchor[0], 0], focus: [hit.r, MAX_COLS - 1] } : { anchor: [hit.r, 0], focus: [hit.r, MAX_COLS - 1] }, false);
      drag.current = { kind: "row" };
    } else {
      const m = mergeAt(sheet, hit.r, hit.c);
      const at: [number, number] = m ? [m.r1, m.c1] : [hit.r, hit.c];
      setSel(e.shiftKey ? { anchor: sel.anchor, focus: at } : { anchor: at, focus: m ? [m.r2, m.c2] : at }, false);
      drag.current = { kind: "cell" };
    }
  };

  const onHover = (e: MouseEvent<HTMLDivElement>) => {
    if (drag.current) return;
    const { x, y } = pointer(e);
    const rz = resizeHit(x, y);
    const next = rz ? (rz.kind === "resize-col" ? "col-resize" : "row-resize") : y < HEADER_H || x < HEADER_W ? "default" : "cell";
    if (next !== cursor) setCursor(next);
  };

  useEffect(() => {
    const move = (e: globalThis.MouseEvent) => {
      const d = drag.current;
      if (!d) return;
      const el = scrollRef.current;
      if (!el) return;
      const { x, y } = pointer(e);
      if (d.kind === "resize-col" || d.kind === "resize-row") {
        const delta = (d.kind === "resize-col" ? x : y) - d.start;
        const next = Math.max(d.kind === "resize-col" ? 8 : 6, Math.round(d.size + delta));
        if (d.kind === "resize-col") sheet.colWidths.set(d.index, next);
        else sheet.rowHeights.set(d.index, next);
        bumpLayout();
        return;
      }
      // Auto-scroll when dragging to the edge
      if (x > el.clientWidth - 10) el.scrollLeft += 24;
      else if (x < HEADER_W && d.kind !== "row") el.scrollLeft -= 24;
      if (y > el.clientHeight - 10) el.scrollTop += 24;
      else if (y < HEADER_H && d.kind !== "col") el.scrollTop -= 24;
      const v = view();
      const c = cols.indexAt(Math.max(0, x - HEADER_W + v.scrollX));
      const r = rows.indexAt(Math.max(0, y - HEADER_H + v.scrollY));
      if (d.kind === "ref") {
        const [ar, ac] = refAnchor.current;
        const ref = ar === r && ac === c ? cellName(r, c) : rangeName(normRange({ r1: ar, c1: ac, r2: r, c2: c }));
        const input = inputRef.current!;
        input.setSelectionRange(d.refStart, input.selectionEnd ?? input.value.length);
        insertRef(ref, d.refStart);
        return;
      }
      setSelState((s) =>
        d.kind === "col" ? { ...s, focus: [MAX_ROWS - 1, c] } : d.kind === "row" ? { ...s, focus: [r, MAX_COLS - 1] } : { ...s, focus: [r, c] },
      );
    };
    const up = () => {
      const d = drag.current;
      drag.current = null;
      if (!d) return;
      if (d.kind === "ref") inputRef.current?.focus();
      if (d.kind === "resize-col" || d.kind === "resize-row") {
        const after = cloneLayout(sheet);
        // Revert to the pre-drag state first, then apply via commitChanges so it can be undone
        applyLayout(sheetIdx, d.before);
        if (JSON.stringify([...after.colWidths, ...after.rowHeights]) !== JSON.stringify([...d.before.colWidths, ...d.before.rowHeights]))
          commitChanges([], { sheet: sheetIdx, before: d.before, after });
        else bumpLayout();
        focusGrid();
      }
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rows, cols, size, sheet, sheetIdx]);

  const onDoubleClick = (e: MouseEvent<HTMLDivElement>) => {
    const { x, y } = pointer(e);
    if (resizeHit(x, y)) return;
    const hit = hitTest(view(), x, y);
    if (hit.area !== "cell") return;
    const [r, c] = sel.anchor;
    startEdit(editText(sheet.cells.get(key(r, c)), styleAt(r, c)), "edit");
  };

  return { resizeHit, autofit, pointer, onMouseDown, onHover, onDoubleClick };
}
