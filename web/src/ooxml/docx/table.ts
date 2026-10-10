/**
 * Tables: grid and column widths, merged cells (gridSpan/vMerge), border precedence (table < conditional format < cell),
 * cell margins, shading, table style conditional formatting (tblLook: header row, first column, banded rows…), vertical alignment, text direction, row height, repeated header rows.
 */

import { attr, css, h, kid, kids, numAttr, s } from "../core/package";
import { fillBlocks } from "./blocks";
import { childFlow, type Flow } from "./context";
import { borderCss, shadeColor } from "./props";
import type { StyleDef } from "./styles";
import { flatKids, pctValue, tw, twAttr, val } from "./xml";

/** Maximum number of table columns (Word itself allows at most 63) */
const MAX_TABLE_COLS = 63;

type Side = "top" | "bottom" | "left" | "right" | "insideH" | "insideV";
const SIDE_NAMES: Record<string, Side> = { top: "top", bottom: "bottom", left: "left", start: "left", right: "right", end: "right", insideH: "insideH", insideV: "insideV" };

const r2 = (n: number) => Math.round(n * 100) / 100;

function readBorders(el: Element | null, into: Partial<Record<Side, Element>>) {
  for (const b of kids(el)) {
    const side = SIDE_NAMES[b.localName];
    if (side) into[side] = b;
  }
  return into;
}

interface Look {
  firstRow: boolean;
  lastRow: boolean;
  firstColumn: boolean;
  lastColumn: boolean;
  noHBand: boolean;
  noVBand: boolean;
}

function parseLook(el: Element | null): Look {
  const v = parseInt(attr(el, "val") ?? "04A0", 16) || 0;
  const flag = (name: string, bit: number) => {
    const a = attr(el, name);
    return a !== null ? /^(1|true|on)$/i.test(a) : !!(v & bit);
  };
  return {
    firstRow: flag("firstRow", 0x20),
    lastRow: flag("lastRow", 0x40),
    firstColumn: flag("firstColumn", 0x80),
    lastColumn: flag("lastColumn", 0x100),
    noHBand: flag("noHBand", 0x200),
    noVBand: flag("noVBand", 0x400),
  };
}

interface CellRec {
  tc: Element;
  tcPr: Element | null;
  row: number;
  col: number;
  span: number;
  rowspan: number;
  td?: HTMLTableCellElement;
  /** Border per side and its source level (0 table, 1 conditional format, 2 cell) */
  sides: Record<"top" | "bottom" | "left" | "right", { css: string | null; level: number }>;
}

/** Table rows (content controls unwrapped) */
function rowsOf(tbl: Element): Element[] {
  const out: Element[] = [];
  const walk = (el: Element) => {
    for (const c of flatKids(el)) {
      if (c.localName === "tr") out.push(c);
      else if (c.localName === "sdt") walk(kid(c, "sdtContent") ?? c);
      else if (c.localName === "customXml") walk(c);
    }
  };
  walk(tbl);
  return out;
}

function cellsOf(tr: Element): Element[] {
  const out: Element[] = [];
  const walk = (el: Element) => {
    for (const c of flatKids(el)) {
      if (c.localName === "tc") out.push(c);
      else if (c.localName === "sdt") walk(kid(c, "sdtContent") ?? c);
      else if (c.localName === "customXml") walk(c);
    }
  };
  walk(tr);
  return out;
}

const ROW_CONDS = new Set(["firstRow", "lastRow", "band1Horz", "band2Horz"]);
const COL_CONDS = new Set(["firstCol", "lastCol", "band1Vert", "band2Vert"]);

export function renderTable(tbl: Element, f: Flow): HTMLElement | null {
  if (f.depth > 12) return null;
  const doc = f.doc;
  const st = doc.styles;
  const theme = doc.theme;
  const tblPr = kid(tbl, "tblPr");
  const styleId = val(tblPr, "tblStyle") ?? st.defaults.table;
  const chain: StyleDef[] = st.chain(styleId);
  /** Table property: direct formatting first, then the style chain (derived styles first) */
  const tprop = (name: string): Element | null => {
    const d = kid(tblPr, name);
    if (d) return d;
    for (let i = chain.length - 1; i >= 0; i--) {
      const v = kid(chain[i].tblPr, name);
      if (v) return v;
    }
    return null;
  };
  const look = parseLook(kid(tblPr, "tblLook"));
  const rowBand = Math.max(1, numAttr(tprop("tblStyleRowBandSize"), "val") ?? 1);
  const colBand = Math.max(1, numAttr(tprop("tblStyleColBandSize"), "val") ?? 1);

  // Table borders: style chain → direct formatting (per side)
  const tBorders: Partial<Record<Side, Element>> = {};
  for (const s of chain) readBorders(kid(s.tblPr, "tblBorders"), tBorders);
  readBorders(kid(tblPr, "tblBorders"), tBorders);

  // Default cell margins
  const defMar = { top: 0, bottom: 0, left: tw(108), right: tw(108) };
  const applyMar = (el: Element | null, into: typeof defMar) => {
    for (const m of kids(el)) {
      const side = SIDE_NAMES[m.localName];
      if (!side || side === "insideH" || side === "insideV") continue;
      const type = attr(m, "type");
      const v = twAttr(m, "w");
      if (v !== null && type !== "pct" && type !== "nil") into[side] = tw(v);
      else if (type === "nil") into[side] = 0;
    }
  };
  for (const s of chain) applyMar(kid(s.tblPr, "tblCellMar"), defMar);
  applyMar(kid(tblPr, "tblCellMar"), defMar);

  // Grid and cell positions
  const grid = kids(kid(tbl, "tblGrid"), "gridCol").map((c) => tw(twAttr(c, "w") ?? 0));
  const trs = rowsOf(tbl).filter((tr) => !kid(kid(tr, "trPr"), "hidden"));
  const rows: CellRec[][] = [];
  let ncols = grid.length;
  trs.forEach((tr, r) => {
    const trPr = kid(tr, "trPr");
    // Word tables have at most 63 columns: clamp unreasonable numbers from the file to avoid allocating huge arrays
    let col = Math.min(MAX_TABLE_COLS, Math.max(0, numAttr(kid(trPr, "gridBefore"), "val") ?? 0));
    const list: CellRec[] = [];
    for (const tc of cellsOf(tr)) {
      const tcPr = kid(tc, "tcPr");
      const span = Math.min(MAX_TABLE_COLS, Math.max(1, numAttr(kid(tcPr, "gridSpan"), "val") ?? 1));
      list.push({
        tc,
        tcPr,
        row: r,
        col,
        span,
        rowspan: 1,
        sides: { top: { css: null, level: -1 }, bottom: { css: null, level: -1 }, left: { css: null, level: -1 }, right: { css: null, level: -1 } },
      });
      col += span;
    }
    ncols = Math.min(MAX_TABLE_COLS * 4, Math.max(ncols, col));
    rows.push(list);
  });
  if (!rows.length) return null;

  // Vertical merge: count continue cells below each restart
  const vm = (c: CellRec) => {
    const el = kid(c.tcPr, "vMerge");
    if (!el) return null;
    return attr(el, "val") === "restart" ? "restart" : "continue";
  };
  const skip = new Set<CellRec>();
  for (let r = 0; r < rows.length; r++) {
    for (const c of rows[r]) {
      if (vm(c) !== "restart") continue;
      for (let k = r + 1; k < rows.length; k++) {
        const below = rows[k].find((x) => x.col === c.col);
        if (!below || vm(below) !== "continue") break;
        c.rowspan++;
        skip.add(below);
      }
    }
  }
  const lastRow = rows.length - 1;

  // Occupancy map (for finding adjacent cells)
  const occ: (CellRec | undefined)[][] = rows.map(() => []);
  for (const list of rows)
    for (const c of list) {
      if (skip.has(c)) continue;
      for (let rr = c.row; rr < c.row + c.rowspan && rr < rows.length; rr++) for (let cc = c.col; cc < c.col + c.span; cc++) occ[rr][cc] = c;
    }

  // Conditional formatting
  const condsOf = (c: CellRec): string[] => {
    const out = ["wholeTable"];
    const isFirstRow = look.firstRow && c.row === 0;
    const isLastRow = look.lastRow && c.row + c.rowspan - 1 === lastRow && rows.length > 1;
    const isFirstCol = look.firstColumn && c.col === 0;
    const isLastCol = look.lastColumn && c.col + c.span === ncols;
    if (!look.noVBand && !isFirstCol && !isLastCol) {
      const ci = c.col - (look.firstColumn ? 1 : 0);
      out.push(Math.floor(ci / colBand) % 2 ? "band2Vert" : "band1Vert");
    }
    if (!look.noHBand && !isFirstRow && !isLastRow) {
      const ri = c.row - (look.firstRow ? 1 : 0);
      out.push(Math.floor(ri / rowBand) % 2 ? "band2Horz" : "band1Horz");
    }
    if (isLastCol) out.push("lastCol");
    if (isFirstCol) out.push("firstCol");
    if (isLastRow) out.push("lastRow");
    if (isFirstRow) out.push("firstRow");
    if (isFirstRow && isLastCol) out.push("neCell");
    if (isFirstRow && isFirstCol) out.push("nwCell");
    if (isLastRow && isLastCol) out.push("seCell");
    if (isLastRow && isFirstCol) out.push("swCell");
    return out.filter((t) => t === "wholeTable" || chain.some((s) => s.cond.has(t)));
  };
  /** tcPr children from conditional formats (in priority order; later wins) */
  const condTcPr = (conds: string[], name: string): { el: Element; cond: string }[] => {
    const out: { el: Element; cond: string }[] = [];
    for (const s of chain) {
      const v = kid(s.tcPr, name);
      if (v) out.push({ el: v, cond: "" });
    }
    for (const c of conds)
      for (const s of chain) {
        const v = kid(s.cond.get(c)?.tcPr, name);
        if (v) out.push({ el: v, cond: c });
      }
    return out;
  };

  // Table element
  const table = h("table", { class: "tf-docx-tbl" });
  const tbody = h("tbody");
  const gridSum = grid.reduce((a, b) => a + b, 0);
  const gridComplete = grid.length >= ncols && grid.every((w) => w > 0);
  const tblW = kid(tblPr, "tblW");
  const wType = attr(tblW, "type");
  const wVal = attr(tblW, "w");
  const tstyle: Record<string, string | undefined> = {};
  if (gridComplete) {
    tstyle["table-layout"] = "fixed";
    if (wType === "pct") tstyle.width = `${r2(pctValue(wVal) ?? 100)}%`;
    else tstyle.width = `${r2(gridSum)}px`;
    const cg = h("colgroup");
    for (let i = 0; i < ncols; i++) cg.append(h("col", { style: `width:${r2(grid[i] ?? 0)}px` }));
    table.append(cg);
  } else if (wType === "pct") tstyle.width = `${r2(pctValue(wVal) ?? 100)}%`;
  else if (wType === "dxa" && wVal) tstyle.width = `${r2(tw(Number(wVal) || 0))}px`;

  // Position: indent and alignment
  const jc = attr(tprop("jc"), "val");
  const ind = tw(twAttr(tprop("tblInd"), "w") ?? 0);
  const firstCellLeft = (() => {
    const c = rows[0]?.[0];
    const m = { ...defMar };
    applyMar(kid(c?.tcPr, "tcMar"), m);
    return m.left;
  })();
  const shift = doc.settings.compat >= 15 ? 0 : firstCellLeft;
  if (jc === "center") {
    tstyle["margin-left"] = "auto";
    tstyle["margin-right"] = "auto";
  } else if (jc === "right" || jc === "end") tstyle["margin-left"] = "auto";
  else if (ind - shift) tstyle["margin-left"] = `${r2(ind - shift)}px`;
  const spacing = twAttr(tprop("tblCellSpacing"), "w");
  if (spacing) {
    tstyle["border-collapse"] = "separate";
    tstyle["border-spacing"] = `${r2(tw(spacing) * 2)}px`;
  }
  const tshade = shadeColor(tprop("shd"), theme);
  if (tshade) tstyle["background-color"] = tshade;
  if (tprop("bidiVisual")) tstyle.direction = "rtl";
  table.setAttribute("style", css(tstyle));

  // Border resolution
  const setSide = (c: CellRec, side: "top" | "bottom" | "left" | "right", el: Element | null | undefined, level: number) => {
    if (!el) return;
    const v = borderCss(el, theme);
    if (v === null) return;
    if (level >= c.sides[side].level) c.sides[side] = { css: v, level };
  };
  let headerRows = 0;
  trs.forEach((tr, r) => {
    const trPr = kid(tr, "trPr");
    if (r === headerRows && kid(trPr, "tblHeader") && attr(kid(trPr, "tblHeader"), "val") !== "0") headerRows++;
  });

  const cellWidth = (c: CellRec) => {
    let w = 0;
    for (let i = c.col; i < c.col + c.span; i++) w += grid[i] ?? 0;
    if (!w) w = f.width / Math.max(1, ncols);
    return w;
  };

  rows.forEach((list, r) => {
    const tr = trs[r];
    const trPr = kid(tr, "trPr");
    const row = h("tr");
    const ht = kid(trPr, "trHeight");
    const hv = twAttr(ht, "val");
    if (hv) row.style.height = `${r2(tw(hv))}px`;
    if (attr(ht, "hRule") === "exact") row.classList.add("tf-docx-exact");
    if (r < headerRows) row.dataset.h = "1";
    if (kid(trPr, "cantSplit")) row.dataset.k = "1";
    const before = numAttr(kid(trPr, "gridBefore"), "val") ?? 0;
    if (before > 0) row.append(h("td", { colspan: before, class: "tf-docx-gap" }));

    for (const c of list) {
      if (skip.has(c)) continue;
      const conds = condsOf(c);
      const td = h("td");
      c.td = td;
      td.dataset.col = String(c.col);
      if (c.span > 1) td.colSpan = c.span;
      if (c.rowspan > 1) td.rowSpan = c.rowspan;

      // Borders: table level (by position)
      const atTop = c.row === 0;
      const atBottom = c.row + c.rowspan - 1 === lastRow;
      const atLeft = c.col === 0;
      const atRight = c.col + c.span >= ncols;
      setSide(c, "top", atTop ? tBorders.top : tBorders.insideH, 0);
      setSide(c, "bottom", atBottom ? tBorders.bottom : tBorders.insideH, 0);
      setSide(c, "left", atLeft ? tBorders.left : tBorders.insideV, 0);
      setSide(c, "right", atRight ? tBorders.right : tBorders.insideV, 0);
      // Conditional formats: left/right edges of row-type regions, top/bottom edges of column-type regions
      for (const { el, cond } of condTcPr(conds, "tcBorders")) {
        const b = readBorders(el, {});
        const rowish = ROW_CONDS.has(cond);
        const colish = COL_CONDS.has(cond);
        setSide(c, "top", colish ? (atTop ? b.top : b.insideH) : rowish ? b.top : atTop ? b.top : (b.insideH ?? b.top), 1);
        setSide(c, "bottom", colish ? (atBottom ? b.bottom : b.insideH) : rowish ? b.bottom : atBottom ? b.bottom : (b.insideH ?? b.bottom), 1);
        setSide(c, "left", rowish ? (atLeft ? b.left : b.insideV) : colish ? b.left : atLeft ? b.left : (b.insideV ?? b.left), 1);
        setSide(c, "right", rowish ? (atRight ? b.right : b.insideV) : colish ? b.right : atRight ? b.right : (b.insideV ?? b.right), 1);
      }
      const tcb = kid(c.tcPr, "tcBorders");
      const direct = readBorders(tcb, {});
      // Diagonal borders
      const diag = ["tl2br", "tr2bl"].map((n) => [n, borderCss(kid(tcb, n), theme)] as const).filter(([, v]) => v && v !== "none");
      if (diag.length) {
        const svg = s("svg", { class: "tf-docx-diag", viewBox: "0 0 100 100", preserveAspectRatio: "none" });
        for (const [n, v] of diag) {
          const parts = v!.split(" ");
          svg.append(
            s("line", { x1: 0, y1: n === "tl2br" ? 0 : 100, x2: 100, y2: n === "tl2br" ? 100 : 0, stroke: parts[2], "stroke-width": parts[0], "vector-effect": "non-scaling-stroke" }),
          );
        }
        td.classList.add("tf-docx-rel");
        td.append(svg);
      }
      setSide(c, "top", direct.top, 2);
      setSide(c, "bottom", direct.bottom, 2);
      setSide(c, "left", direct.left, 2);
      setSide(c, "right", direct.right, 2);

      // Margins
      const mar = { ...defMar };
      for (const { el } of condTcPr(conds, "tcMar")) applyMar(el, mar);
      applyMar(kid(c.tcPr, "tcMar"), mar);
      const cs: Record<string, string | undefined> = {
        padding: `${r2(mar.top)}px ${r2(mar.right)}px ${r2(mar.bottom)}px ${r2(mar.left)}px`,
      };
      // Shading
      const shades = condTcPr(conds, "shd");
      const shdEl = kid(c.tcPr, "shd") ?? shades[shades.length - 1]?.el;
      const bg = shadeColor(shdEl, theme);
      if (bg) cs["background-color"] = bg;
      // Vertical alignment
      const vAlign = attr(kid(c.tcPr, "vAlign"), "val") ?? attr(condTcPr(conds, "vAlign").pop()?.el, "val");
      if (vAlign === "center" || vAlign === "both") cs["vertical-align"] = "middle";
      else if (vAlign === "bottom") cs["vertical-align"] = "bottom";
      if (kid(c.tcPr, "noWrap") && !gridComplete) cs["white-space"] = "nowrap";
      if (!gridComplete) {
        const tcW = kid(c.tcPr, "tcW");
        const wv = twAttr(tcW, "w");
        if (wv && attr(tcW, "type") === "dxa") cs.width = `${r2(tw(wv))}px`;
        else if (wv && attr(tcW, "type") === "pct") cs.width = `${r2(pctValue(attr(tcW, "w")) ?? 0)}%`;
      }
      td.setAttribute("style", css(cs));

      // Content
      const width = Math.max(10, cellWidth(c) - mar.left - mar.right);
      const flow = childFlow(f, {
        inCell: true,
        tableStyle: styleId ? { id: styleId, conds } : undefined,
        width,
        depth: f.depth + 1,
        fields: f.fields,
        comments: f.comments,
        floats: f.floats,
        notes: f.notes,
      });
      const content = flatKids(c.tc).filter((x) => x.localName !== "tcPr");
      const dir = attr(kid(c.tcPr, "textDirection"), "val");
      if (dir && dir !== "lrTb" && dir !== "tb") {
        const box = h("div", { class: "tf-docx-vert" + (dir === "btLr" || dir === "tbLrV" ? " tf-docx-btlr" : "") });
        fillBlocks(box, content, flow);
        td.append(box);
      } else if (!flow.edit?.cell?.(c.tc, td, content, flow)) fillBlocks(td, content, flow);
      if (!td.childNodes.length) td.append(h("p", { class: "tf-docx-p" }, h("br")));
      row.append(td);
    }
    tbody.append(row);
  });

  // Borders explicitly removed at the cell level (nil) also remove the adjacent cell's table-level border
  const clear = (c: CellRec, side: "top" | "bottom" | "left" | "right", n: CellRec | undefined) => {
    const opp = side === "top" ? "bottom" : side === "bottom" ? "top" : side === "left" ? "right" : "left";
    if (!n || n === c) return;
    if (c.sides[side].level === 2 && c.sides[side].css === "none" && n.sides[opp].level < 2) n.sides[opp] = { css: "none", level: 2 };
  };
  for (const list of rows)
    for (const c of list) {
      if (skip.has(c)) continue;
      clear(c, "bottom", occ[c.row + c.rowspan]?.[c.col]);
      clear(c, "top", occ[c.row - 1]?.[c.col]);
      clear(c, "right", occ[c.row]?.[c.col + c.span]);
      clear(c, "left", occ[c.row]?.[c.col - 1]);
    }
  for (const list of rows)
    for (const c of list) {
      if (!c.td) continue;
      for (const side of ["top", "bottom", "left", "right"] as const) {
        const v = c.sides[side].css;
        if (v && v !== "none") c.td.style.setProperty(`border-${side}`, v);
      }
    }

  table.append(tbody);
  if (headerRows) doc.meta(table).headerRows = headerRows;
  return table;
}
