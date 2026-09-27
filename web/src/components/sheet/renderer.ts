/**
 * Spreadsheet canvas rendering.
 *
 * Column widths/row heights are prefix-sum arrays, scrolling binary-searches the visible range, and only visible cells are drawn.
 * There is no scene graph or render cache: the whole visible area is redrawn each time;
 * a typical sheet shows only a few hundred cells at once, so a full redraw takes 1–2 ms.
 */
import { Axis, colName, inRange, key, mergeAt, type CellStyle, type Range, type Sheet } from "@/lib/sheet/model";
import { isErr, type Calculator, type Value } from "@/lib/sheet/formula";
import { formatCell } from "@/lib/sheet/format";
import type { CellDecoration, IconKind } from "@/lib/sheet/conditional";

export const HEADER_W = 46;
export const HEADER_H = 24;
const DEFAULT_FONT = 'Calibri, "Microsoft JhengHei", "PingFang TC", "Noto Sans TC", sans-serif';
const DEFAULT_PT = 11;

export interface View {
  width: number;
  height: number;
  scrollX: number;
  scrollY: number;
  rows: Axis;
  cols: Axis;
  /** Number of frozen rows and columns (for preview; editing doesn't freeze, so hit-testing needn't account for it) */
  frozen?: { rows: number; cols: number };
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

const colX = (v: View, c: number) => HEADER_W + v.cols.offset(c) - v.scrollX;
const rowY = (v: View, r: number) => HEADER_H + v.rows.offset(r) - v.scrollY;

/** Position of a cell (the whole merged range, if merged) on the canvas */
export function cellRect(v: View, sheet: Sheet, r: number, c: number): Rect {
  const m = mergeAt(sheet, r, c);
  const r1 = m ? m.r1 : r;
  const c1 = m ? m.c1 : c;
  const r2 = m ? m.r2 : r;
  const c2 = m ? m.c2 : c;
  const x = colX(v, c1);
  const y = rowY(v, r1);
  return { x, y, w: v.cols.offset(c2 + 1) - v.cols.offset(c1), h: v.rows.offset(r2 + 1) - v.rows.offset(r1) };
}

export type Hit = { area: "corner" } | { area: "col"; c: number } | { area: "row"; r: number } | { area: "cell"; r: number; c: number };

export function hitTest(v: View, x: number, y: number): Hit {
  const c = v.cols.indexAt(Math.max(0, x - HEADER_W + v.scrollX));
  const r = v.rows.indexAt(Math.max(0, y - HEADER_H + v.scrollY));
  if (x < HEADER_W && y < HEADER_H) return { area: "corner" };
  if (y < HEADER_H) return { area: "col", c };
  if (x < HEADER_W) return { area: "row", r };
  return { area: "cell", r, c };
}

export function visibleRange(v: View): Range {
  return {
    r1: v.rows.indexAt(v.scrollY),
    c1: v.cols.indexAt(v.scrollX),
    r2: v.rows.indexAt(v.scrollY + v.height - HEADER_H),
    c2: v.cols.indexAt(v.scrollX + v.width - HEADER_W),
  };
}

// ───────────── Styles and text ─────────────

const fontCache = new Map<string, string>();
export function fontOf(s?: CellStyle, zoom = 1) {
  const id = `${s?.bold ? 1 : 0}${s?.italic ? 1 : 0}${s?.size ?? DEFAULT_PT}${s?.font ?? ""}${zoom}`;
  let f = fontCache.get(id);
  if (!f) {
    const px = ((s?.size ?? DEFAULT_PT) * 4) / 3;
    f = `${s?.italic ? "italic " : ""}${s?.bold ? "bold " : ""}${(px * zoom).toFixed(1)}px ${s?.font ? `"${s.font}", ` : ""}${DEFAULT_FONT}`;
    fontCache.set(id, f);
  }
  return f;
}

export function displayText(value: Value, style?: CellStyle) {
  if (isErr(value)) return { text: value.code, color: undefined };
  return formatCell(value, style?.numFmt);
}

function wrapLines(ctx: CanvasRenderingContext2D, text: string, width: number) {
  const lines: string[] = [];
  for (const para of text.split("\n")) {
    let line = "";
    for (const ch of Array.from(para)) {
      if (line && ctx.measureText(line + ch).width > width) {
        // Avoid breaking English words in the middle where possible
        const sp = line.lastIndexOf(" ");
        if (sp > 0 && /\w/.test(ch)) {
          lines.push(line.slice(0, sp));
          line = line.slice(sp + 1) + ch;
        } else {
          lines.push(line);
          line = ch;
        }
      } else line += ch;
    }
    lines.push(line);
  }
  return lines;
}

// ───────────── Drawing ─────────────

export interface DrawState {
  sheet: Sheet;
  sheetIndex: number;
  styles: CellStyle[];
  /** Get a cell's value (computed result when editing; cached value from the file when previewing) */
  calc: Pick<Calculator, "value">;
  /** null = don't draw a selection (preview) */
  selection: Range | null;
  /** Conditional formatting (for preview): overrides fill and font, adds data bars or icons */
  decorate?(r: number, c: number): CellDecoration | undefined;
  active: [number, number];
  /** Don't draw text for the cell being edited (it's covered by the editor) */
  editing: boolean;
  /** Range being copied (dashed outline) */
  clip: Range | null;
}

const GRID = "#e2e3e5";
const SEL = "#2563eb";

interface Pane {
  /** Scroll offset used by this region (0 for frozen parts) */
  view: View;
  clip: Rect;
  vis: Range;
}

/** Split into up to four regions by frozen panes: main area, frozen rows, frozen columns, top-left corner */
function panes(v: View): Pane[] {
  const fr = Math.min(v.frozen?.rows ?? 0, v.rows.count);
  const fc = Math.min(v.frozen?.cols ?? 0, v.cols.count);
  const fh = v.rows.offset(fr);
  const fw = v.cols.offset(fc);
  const W = v.width;
  const H = v.height;
  const rowsMain = { r1: v.rows.indexAt(v.scrollY + fh), r2: v.rows.indexAt(v.scrollY + H - HEADER_H) };
  const colsMain = { c1: v.cols.indexAt(v.scrollX + fw), c2: v.cols.indexAt(v.scrollX + W - HEADER_W) };
  const out: Pane[] = [
    { view: v, clip: { x: HEADER_W + fw, y: HEADER_H + fh, w: W - HEADER_W - fw, h: H - HEADER_H - fh }, vis: { ...rowsMain, ...colsMain } },
  ];
  if (fr) out.push({ view: { ...v, scrollY: 0 }, clip: { x: HEADER_W + fw, y: HEADER_H, w: W - HEADER_W - fw, h: fh }, vis: { r1: 0, r2: fr - 1, ...colsMain } });
  if (fc) out.push({ view: { ...v, scrollX: 0 }, clip: { x: HEADER_W, y: HEADER_H + fh, w: fw, h: H - HEADER_H - fh }, vis: { ...rowsMain, c1: 0, c2: fc - 1 } });
  if (fr && fc) out.push({ view: { ...v, scrollX: 0, scrollY: 0 }, clip: { x: HEADER_W, y: HEADER_H, w: fw, h: fh }, vis: { r1: 0, r2: fr - 1, c1: 0, c2: fc - 1 } });
  return out;
}

export function draw(ctx: CanvasRenderingContext2D, v: View, s: DrawState) {
  const W = v.width;
  const H = v.height;
  ctx.clearRect(0, 0, W, H);
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, W, H);
  const list = panes(v);
  for (const p of list) drawPane(ctx, p, s);

  // Frozen pane dividers
  if (list.length > 1) {
    const fh = v.rows.offset(Math.min(v.frozen?.rows ?? 0, v.rows.count));
    const fw = v.cols.offset(Math.min(v.frozen?.cols ?? 0, v.cols.count));
    ctx.strokeStyle = "#b5b8bd";
    ctx.lineWidth = 1;
    ctx.beginPath();
    if (fh) {
      ctx.moveTo(HEADER_W, HEADER_H + fh - 0.5);
      ctx.lineTo(W, HEADER_H + fh - 0.5);
    }
    if (fw) {
      ctx.moveTo(HEADER_W + fw - 0.5, HEADER_H);
      ctx.lineTo(HEADER_W + fw - 0.5, H);
    }
    ctx.stroke();
  }
  drawHeaders(ctx, v, s.selection, list);
}

function drawPane(ctx: CanvasRenderingContext2D, pane: Pane, s: DrawState) {
  const { sheet, styles, calc } = s;
  const { view: v, clip, vis } = pane;
  if (clip.w <= 0 || clip.h <= 0) return;

  // Merged cells in the visible range (including ones whose top-left is outside but extend into view)
  const merges = sheet.merges.filter((m) => m.r2 >= vis.r1 && m.r1 <= vis.r2 && m.c2 >= vis.c1 && m.c1 <= vis.c2);
  const covered = (r: number, c: number) => merges.some((m) => inRange(m, r, c));
  ctx.save();
  ctx.beginPath();
  ctx.rect(clip.x, clip.y, clip.w, clip.h);
  ctx.clip();

  // 1. Fills (including conditional-format fills and data bars)
  const bgOf = (r: number, c: number) => {
    const cell = sheet.cells.get(key(r, c));
    return s.decorate?.(r, c)?.bg ?? (cell?.s !== undefined ? styles[cell.s]?.bg : undefined);
  };
  for (let r = vis.r1; r <= vis.r2; r++) {
    const y = rowY(v, r);
    const h = v.rows.sizeOf(r);
    if (h === 0) continue;
    for (let c = vis.c1; c <= vis.c2; c++) {
      if (covered(r, c)) continue;
      const bg = bgOf(r, c);
      if (bg) {
        ctx.fillStyle = bg;
        ctx.fillRect(colX(v, c), y, v.cols.sizeOf(c), h);
      }
      const bar = s.decorate?.(r, c)?.bar;
      if (bar) drawBar(ctx, bar, { x: colX(v, c), y, w: v.cols.sizeOf(c), h });
    }
  }

  // 2. Gridlines (not drawn when the sheet turns gridlines off)
  if (!sheet.noGrid) {
    ctx.strokeStyle = GRID;
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (let c = vis.c1; c <= vis.c2 + 1; c++) {
      const x = Math.round(colX(v, c)) - 0.5;
      ctx.moveTo(x, clip.y);
      ctx.lineTo(x, clip.y + clip.h);
    }
    for (let r = vis.r1; r <= vis.r2 + 1; r++) {
      const y = Math.round(rowY(v, r)) - 0.5;
      ctx.moveTo(clip.x, y);
      ctx.lineTo(clip.x + clip.w, y);
    }
    ctx.stroke();
  }

  // 3. Merged cells cover interior gridlines
  for (const m of merges) {
    const rc = cellRect(v, sheet, m.r1, m.c1);
    ctx.fillStyle = bgOf(m.r1, m.c1) || "#ffffff";
    ctx.fillRect(rc.x, rc.y, rc.w - 1, rc.h - 1);
  }

  // 4. Borders
  const borderWidth = (st: string) => (st === "medium" || st.startsWith("medium") ? 2 : st === "thick" || st === "double" ? 3 : 1);
  const dash = (st: string): number[] => (st.includes("dash") ? [4, 2] : st.includes("dot") || st === "hair" ? [1, 2] : []);
  const line = (side: { style: string; color?: string }, x1: number, y1: number, x2: number, y2: number) => {
    ctx.strokeStyle = side.color ?? "#000000";
    ctx.lineWidth = borderWidth(side.style);
    ctx.setLineDash(dash(side.style));
    ctx.beginPath();
    ctx.moveTo(x1, y1);
    ctx.lineTo(x2, y2);
    ctx.stroke();
  };
  const drawBorders = (r: number, c: number, rect: Rect) => {
    const cell = sheet.cells.get(key(r, c));
    const b = cell?.s !== undefined ? styles[cell.s]?.border : undefined;
    if (!b) return;
    const x1 = Math.round(rect.x) - 0.5;
    const y1 = Math.round(rect.y) - 0.5;
    const x2 = Math.round(rect.x + rect.w) - 0.5;
    const y2 = Math.round(rect.y + rect.h) - 0.5;
    if (b.top) line(b.top, x1, y1, x2, y1);
    if (b.bottom) line(b.bottom, x1, y2, x2, y2);
    if (b.left) line(b.left, x1, y1, x1, y2);
    if (b.right) line(b.right, x2, y1, x2, y2);
  };
  for (let r = vis.r1; r <= vis.r2; r++) {
    if (v.rows.sizeOf(r) === 0) continue;
    for (let c = vis.c1; c <= vis.c2; c++) {
      if (covered(r, c) || v.cols.sizeOf(c) === 0) continue;
      drawBorders(r, c, { x: colX(v, c), y: rowY(v, r), w: v.cols.sizeOf(c), h: v.rows.sizeOf(r) });
    }
  }
  for (const m of merges) drawBorders(m.r1, m.c1, cellRect(v, sheet, m.r1, m.c1));
  ctx.setLineDash([]);
  ctx.lineWidth = 1;

  // 5. Text
  ctx.textBaseline = "alphabetic";
  const active = s.active;
  const drawText = (r: number, c: number, rect: Rect) => {
    if (s.editing && r === active[0] && c === active[1]) return;
    const cell = sheet.cells.get(key(r, c));
    if (!cell || rect.w <= 0 || rect.h <= 0) return;
    const value = calc.value(s.sheetIndex, r, c);
    if (value === null || value === "") return;
    const deco = s.decorate?.(r, c);
    const base = cell.s !== undefined ? styles[cell.s] : undefined;
    // Conditional-format font settings override the cell style
    const style: CellStyle | undefined = deco
      ? {
          ...base,
          ...(deco.bold !== undefined && { bold: deco.bold }),
          ...(deco.italic !== undefined && { italic: deco.italic }),
          ...(deco.underline !== undefined && { underline: deco.underline }),
          ...(deco.strike !== undefined && { strike: deco.strike }),
          ...(deco.color && { color: deco.color }),
        }
      : base;
    if (deco?.icon) drawIcon(ctx, deco.icon.kind, deco.icon.color, rect);
    if (deco?.hideValue) return;
    const { text: raw, color: fmtColor } = displayText(value, style);
    if (!raw) return;
    // Conditional-format font color takes precedence over number-format colors ([Red] etc.)
    const color = deco?.color ?? fmtColor;
    ctx.font = fontOf(style);
    const isNum = typeof value === "number";
    const align = style?.hAlign ?? (isNum ? "right" : typeof value === "boolean" || isErr(value) ? "center" : "left");
    // Indent: about 3 spaces wide per level (about 9px in Calibri 11); icon-set icons take 16px on the left
    const indent = (style?.indent ? style.indent * 9 : 0) + (deco?.icon ? 16 : 0);
    const pad = 3;
    const padL = pad + (align === "left" || deco?.icon ? indent : 0);
    const padR = pad + (align === "right" ? indent : 0);
    const fontPx = ((style?.size ?? DEFAULT_PT) * 4) / 3;
    let text = raw;
    let width = ctx.measureText(text).width;
    // Numbers that don't fit show ###, as in Excel
    if (isNum && width > rect.w - padL - padR && !style?.wrap) {
      const hash = ctx.measureText("#").width;
      text = "#".repeat(Math.max(1, Math.floor((rect.w - pad * 2) / hash)));
      width = ctx.measureText(text).width;
    }
    ctx.fillStyle = color ?? style?.color ?? (isErr(value) ? "#b91c1c" : "#1f2328");

    // Text overflows right into empty neighboring cells (when not wrapped and left-aligned)
    let clipW = rect.w;
    if (!style?.wrap && !isNum && align === "left" && width > rect.w - padL - pad) {
      for (let cc = c + 1; cc <= vis.c2 + 20 && clipW < width + padL + pad; cc++) {
        if (sheet.cells.get(key(r, cc))?.v != null || sheet.cells.get(key(r, cc))?.f || covered(r, cc)) break;
        clipW += v.cols.sizeOf(cc);
      }
    }
    ctx.save();
    ctx.beginPath();
    ctx.rect(rect.x, rect.y, clipW, rect.h);
    ctx.clip();
    const lines = style?.wrap ? wrapLines(ctx, text, rect.w - padL - padR) : [text];
    const lineH = fontPx * 1.25;
    const blockH = lines.length * lineH;
    const vAlign = style?.vAlign ?? "bottom";
    let y =
      vAlign === "top"
        ? rect.y + pad + fontPx
        : vAlign === "center"
          ? rect.y + (rect.h - blockH) / 2 + fontPx
          : rect.y + rect.h - blockH - pad + fontPx;
    if (lines.length === 1 && vAlign !== "top") y = vAlign === "center" ? rect.y + rect.h / 2 + fontPx * 0.35 : rect.y + rect.h - pad - 1;
    for (const line of lines) {
      const lw = ctx.measureText(line).width;
      const x = align === "right" ? rect.x + rect.w - padR - lw : align === "center" ? rect.x + (rect.w - lw) / 2 : rect.x + padL;
      ctx.fillText(line, x, y);
      if (style?.underline || style?.strike) {
        ctx.fillRect(x, style.underline ? y + 2 : y - fontPx * 0.3, lw, 1);
      }
      y += lineH;
    }
    ctx.restore();
  };
  for (let r = vis.r1; r <= vis.r2; r++) {
    if (v.rows.sizeOf(r) === 0) continue;
    for (let c = vis.c1; c <= vis.c2; c++) {
      if (covered(r, c)) continue;
      const w = v.cols.sizeOf(c);
      if (w === 0) continue;
      drawText(r, c, { x: colX(v, c), y: rowY(v, r), w, h: v.rows.sizeOf(r) });
    }
  }
  for (const m of merges) drawText(m.r1, m.c1, cellRect(v, sheet, m.r1, m.c1));

  // 6. Selection
  const sel = s.selection;
  if (sel) {
    const sx = colX(v, sel.c1);
    const sy = rowY(v, sel.r1);
    const sw = v.cols.offset(sel.c2 + 1) - v.cols.offset(sel.c1);
    const sh = v.rows.offset(sel.r2 + 1) - v.rows.offset(sel.r1);
    if (sel.r1 !== sel.r2 || sel.c1 !== sel.c2) {
      ctx.fillStyle = "rgba(37, 99, 235, 0.10)";
      ctx.fillRect(sx, sy, sw, sh);
      // The active cell stays white
      const a = cellRect(v, sheet, active[0], active[1]);
      const cell = sheet.cells.get(key(active[0], active[1]));
      ctx.fillStyle = (cell?.s !== undefined && styles[cell.s]?.bg) || "#ffffff";
      ctx.fillRect(a.x, a.y, a.w - 1, a.h - 1);
      if (!s.editing) drawText(active[0], active[1], a);
    }
    ctx.strokeStyle = SEL;
    ctx.lineWidth = 2;
    ctx.strokeRect(sx - 0.5, sy - 0.5, sw, sh);
    // Fill handle at the bottom-right corner (visual hint)
    ctx.fillStyle = SEL;
    ctx.fillRect(sx + sw - 4, sy + sh - 4, 6, 6);
    ctx.strokeStyle = "#ffffff";
    ctx.lineWidth = 1;
    ctx.strokeRect(sx + sw - 4.5, sy + sh - 4.5, 7, 7);
  }

  if (s.clip) {
    const cx = colX(v, s.clip.c1);
    const cy = rowY(v, s.clip.r1);
    ctx.setLineDash([4, 3]);
    ctx.strokeStyle = SEL;
    ctx.lineWidth = 1.5;
    ctx.strokeRect(
      cx + 1,
      cy + 1,
      v.cols.offset(s.clip.c2 + 1) - v.cols.offset(s.clip.c1) - 2,
      v.rows.offset(s.clip.r2 + 1) - v.rows.offset(s.clip.r1) - 2,
    );
    ctx.setLineDash([]);
  }
  ctx.restore();
}

/** Row/column headers (frozen rows/columns don't scroll) */
function drawHeaders(ctx: CanvasRenderingContext2D, v: View, sel: Range | null, list: Pane[]) {
  const W = v.width;
  const H = v.height;
  const headBg = "#f5f6f7";
  const headSel = "#dbe5fb";
  ctx.font = `12px ${DEFAULT_FONT}`;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillStyle = headBg;
  ctx.fillRect(0, 0, W, HEADER_H);
  ctx.fillRect(0, 0, HEADER_W, H);
  // Column headers follow the region's horizontal scroll, row headers its vertical scroll; the same column (row) range is drawn only once
  const colPanes = list.filter((p, i) => list.findIndex((q) => q.vis.c1 === p.vis.c1 && q.vis.c2 === p.vis.c2) === i);
  const rowPanes = list.filter((p, i) => list.findIndex((q) => q.vis.r1 === p.vis.r1 && q.vis.r2 === p.vis.r2) === i);
  ctx.strokeStyle = "#d0d3d7";
  ctx.lineWidth = 1;
  for (const p of colPanes) {
    ctx.save();
    ctx.beginPath();
    ctx.rect(p.clip.x, 0, p.clip.w, HEADER_H);
    ctx.clip();
    ctx.beginPath();
    for (let c = p.vis.c1; c <= p.vis.c2; c++) {
      const x = colX(p.view, c);
      const w = v.cols.sizeOf(c);
      if (w === 0) continue;
      const on = !!sel && c >= sel.c1 && c <= sel.c2;
      if (on) {
        ctx.fillStyle = headSel;
        ctx.fillRect(x, 0, w, HEADER_H);
      }
      ctx.fillStyle = on ? SEL : "#5f6368";
      ctx.fillText(colName(c), x + w / 2, HEADER_H / 2 + 1);
      ctx.moveTo(Math.round(x + w) - 0.5, 0);
      ctx.lineTo(Math.round(x + w) - 0.5, HEADER_H);
    }
    ctx.stroke();
    if (sel) {
      ctx.fillStyle = SEL;
      ctx.fillRect(colX(p.view, sel.c1), HEADER_H - 2, v.cols.offset(sel.c2 + 1) - v.cols.offset(sel.c1), 2);
    }
    ctx.restore();
  }
  for (const p of rowPanes) {
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, p.clip.y, HEADER_W, p.clip.h);
    ctx.clip();
    ctx.beginPath();
    for (let r = p.vis.r1; r <= p.vis.r2; r++) {
      const y = rowY(p.view, r);
      const h = v.rows.sizeOf(r);
      if (h === 0) continue;
      const on = !!sel && r >= sel.r1 && r <= sel.r2;
      if (on) {
        ctx.fillStyle = headSel;
        ctx.fillRect(0, y, HEADER_W, h);
      }
      ctx.fillStyle = on ? SEL : "#5f6368";
      ctx.fillText(String(r + 1), HEADER_W / 2, y + h / 2 + 1);
      ctx.moveTo(0, Math.round(y + h) - 0.5);
      ctx.lineTo(HEADER_W, Math.round(y + h) - 0.5);
    }
    ctx.stroke();
    if (sel) {
      ctx.fillStyle = SEL;
      ctx.fillRect(HEADER_W - 2, rowY(p.view, sel.r1), 2, v.rows.offset(sel.r2 + 1) - v.rows.offset(sel.r1));
    }
    ctx.restore();
  }
  ctx.fillStyle = headBg;
  ctx.fillRect(0, 0, HEADER_W, HEADER_H);
  ctx.beginPath();
  ctx.moveTo(0, HEADER_H - 0.5);
  ctx.lineTo(W, HEADER_H - 0.5);
  ctx.moveTo(HEADER_W - 0.5, 0);
  ctx.lineTo(HEADER_W - 0.5, H);
  ctx.stroke();
  ctx.textAlign = "start";
}

// ───────────── Conditional-format graphics ─────────────

/** Data bar: left-to-right gradient (close to Excel's gradient fill) */
function drawBar(ctx: CanvasRenderingContext2D, bar: NonNullable<CellDecoration["bar"]>, rect: Rect) {
  const inner = rect.w - 4;
  const x = rect.x + 2 + inner * bar.from;
  const w = Math.max(0, inner * (bar.to - bar.from));
  const h = Math.max(0, rect.h - 4);
  if (w <= 0 || h <= 0) return;
  // Bars for negative values (extending left from the axis) use the reverse gradient direction
  const leftward = bar.from > 0 && bar.color.toLowerCase() === "#ff0000";
  const g = ctx.createLinearGradient(leftward ? x + w : x, 0, leftward ? x : x + w, 0);
  g.addColorStop(0, bar.color);
  g.addColorStop(1, "#ffffff");
  ctx.fillStyle = g;
  ctx.fillRect(x, rect.y + 2, w, h);
  ctx.strokeStyle = bar.color;
  ctx.lineWidth = 1;
  ctx.strokeRect(x + 0.5, rect.y + 2.5, Math.max(0, w - 1), h - 1);
}

/** Icon-set icon: drawn at the left of the cell, about 12px */
function drawIcon(ctx: CanvasRenderingContext2D, kind: IconKind, color: string, rect: Rect) {
  const size = Math.min(12, rect.h - 4);
  if (size <= 2) return;
  const x = rect.x + 3;
  const y = rect.y + (rect.h - size) / 2;
  const cx = x + size / 2;
  const cy = y + size / 2;
  ctx.save();
  ctx.fillStyle = color;
  ctx.strokeStyle = color;
  ctx.lineWidth = 2;
  ctx.beginPath();
  switch (kind) {
    case "up":
      ctx.moveTo(cx, y);
      ctx.lineTo(x + size, cy);
      ctx.lineTo(x + size * 0.68, cy);
      ctx.lineTo(x + size * 0.68, y + size);
      ctx.lineTo(x + size * 0.32, y + size);
      ctx.lineTo(x + size * 0.32, cy);
      ctx.lineTo(x, cy);
      ctx.fill();
      break;
    case "down":
      ctx.moveTo(cx, y + size);
      ctx.lineTo(x + size, cy);
      ctx.lineTo(x + size * 0.68, cy);
      ctx.lineTo(x + size * 0.68, y);
      ctx.lineTo(x + size * 0.32, y);
      ctx.lineTo(x + size * 0.32, cy);
      ctx.lineTo(x, cy);
      ctx.fill();
      break;
    case "right":
      ctx.moveTo(x + size, cy);
      ctx.lineTo(cx, y);
      ctx.lineTo(cx, y + size * 0.32);
      ctx.lineTo(x, y + size * 0.32);
      ctx.lineTo(x, y + size * 0.68);
      ctx.lineTo(cx, y + size * 0.68);
      ctx.lineTo(cx, y + size);
      ctx.fill();
      break;
    case "check":
      ctx.moveTo(x + 1, cy);
      ctx.lineTo(x + size * 0.4, y + size - 2);
      ctx.lineTo(x + size - 1, y + 2);
      ctx.stroke();
      break;
    case "cross":
      ctx.moveTo(x + 2, y + 2);
      ctx.lineTo(x + size - 2, y + size - 2);
      ctx.moveTo(x + size - 2, y + 2);
      ctx.lineTo(x + 2, y + size - 2);
      ctx.stroke();
      break;
    case "bang":
      ctx.fillRect(cx - 1, y, 2, size * 0.65);
      ctx.fillRect(cx - 1, y + size - 2, 2, 2);
      break;
    case "flag":
      ctx.fillRect(x + 1, y, 1.5, size);
      ctx.moveTo(x + 2.5, y);
      ctx.lineTo(x + size, y + size * 0.3);
      ctx.lineTo(x + 2.5, y + size * 0.6);
      ctx.fill();
      break;
    default:
      ctx.arc(cx, cy, size / 2, 0, Math.PI * 2);
      ctx.fill();
  }
  ctx.restore();
}
