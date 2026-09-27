/**
 * Tables (a:tbl): grid, merged cells, cell fill/borders/margins,
 * and table styles (ppt/tableStyles.xml; falls back to the built-in styles in tablestyles.ts when not found).
 */

import { attr, css, h, kid, kids, numAttr, s } from "@/lib/office/ooxml";
import { colorIn, rgbaCss, type Rgba } from "@/lib/office/theme";
import { fillElIn, readFill, resolveLine, themeFill, cssGradient, type Fill, type Line } from "./paint";
import { readBodyPr, renderParagraphs, type TextCtx } from "./text";
import { C } from "./styles";
import { builtinTableStyle } from "./tablestyles";
import type { Ctx, Mapper } from "./shapes";

export interface TableCtx extends Ctx {
  textFor: (style: Element | null) => TextCtx;
}

// ───────────── Style application ─────────────

interface Region {
  part: Element;
  /** Region extent (rows, columns, inclusive) */
  r0: number;
  r1: number;
  c0: number;
  c1: number;
}

interface CellStyle {
  fill: Fill | null;
  bold?: boolean;
  italic?: boolean;
  color?: Rgba | null;
  face?: string | null;
  borders: Record<"l" | "r" | "t" | "b", Line | null | undefined>;
}

const SIDE = { l: ["left", "insideV"], r: ["right", "insideV"], t: ["top", "insideH"], b: ["bottom", "insideH"] } as const;

// ───────────── Table ─────────────

export function renderTable(tbl: Element, tctx: TableCtx, _w: number, _h: number, m: Mapper): HTMLElement {
  const info = tctx.sr.info;
  const cc = info.cc;
  const theme = info.theme;
  const tblPr = kid(tbl, "tblPr");
  const flag = (n: string) => attr(tblPr, n) === "1" || attr(tblPr, n) === "true";
  const firstRow = flag("firstRow");
  const lastRow = flag("lastRow");
  const firstCol = flag("firstCol");
  const lastCol = flag("lastCol");
  const bandRow = flag("bandRow");
  const bandCol = flag("bandCol");
  const styleId = (kid(tblPr, "tableStyleId")?.textContent ?? "").trim().toUpperCase();
  const style = styleId ? (info.pres.tableStyles.get(styleId) ?? builtinTableStyle(styleId)) : null;

  const cols = kids(kid(tbl, "tblGrid"), "gridCol").map((g) => (numAttr(g, "w") ?? 0) * m.sx);
  const rows = kids(tbl, "tr");
  const R = rows.length;
  const Cn = cols.length;

  // Regions (application order: later ones override earlier ones)
  const regions: Region[] = [];
  const add = (name: string, r0: number, r1: number, c0: number, c1: number) => {
    const part = kid(style, name);
    if (part && r0 <= r1 && c0 <= c1) regions.push({ part, r0, r1, c0, c1 });
  };
  const bodyR0 = firstRow ? 1 : 0;
  const bodyR1 = lastRow ? R - 2 : R - 1;
  const bodyC0 = firstCol ? 1 : 0;
  const bodyC1 = lastCol ? Cn - 2 : Cn - 1;
  add("wholeTbl", 0, R - 1, 0, Cn - 1);
  if (bandCol) for (let c = bodyC0; c <= bodyC1; c++) add((c - bodyC0) % 2 ? "band2V" : "band1V", 0, R - 1, c, c);
  if (bandRow) for (let r = bodyR0; r <= bodyR1; r++) add((r - bodyR0) % 2 ? "band2H" : "band1H", r, r, 0, Cn - 1);
  if (lastCol) add("lastCol", 0, R - 1, Cn - 1, Cn - 1);
  if (firstCol) add("firstCol", 0, R - 1, 0, 0);
  if (lastRow) add("lastRow", R - 1, R - 1, 0, Cn - 1);
  if (firstRow) add("firstRow", 0, 0, 0, Cn - 1);
  if (firstRow && firstCol) add("nwCell", 0, 0, 0, 0);
  if (firstRow && lastCol) add("neCell", 0, 0, Cn - 1, Cn - 1);
  if (lastRow && firstCol) add("swCell", R - 1, R - 1, 0, 0);
  if (lastRow && lastCol) add("seCell", R - 1, R - 1, Cn - 1, Cn - 1);

  const lineOf = (el: Element | null): Line | null | undefined => {
    if (!el) return undefined;
    const ln = kid(el, "ln");
    const ref = kid(el, "lnRef");
    if (!ln && !ref) return undefined;
    return resolveLine([ln], ref, cc, theme, info.path);
  };

  const styleFor = (r: number, c: number, rs: number, cs: number): CellStyle => {
    const st: CellStyle = { fill: null, borders: { l: undefined, r: undefined, t: undefined, b: undefined } };
    for (const g of regions) {
      if (r > g.r1 || r + rs - 1 < g.r0 || c > g.c1 || c + cs - 1 < g.c0) continue;
      const tcStyle = kid(g.part, "tcStyle");
      const fillHost = kid(tcStyle, "fill");
      const fEl = fillElIn(fillHost);
      if (fEl) st.fill = readFill(fEl, cc, info.path);
      else if (kid(tcStyle, "fillRef")) st.fill = themeFill(kid(tcStyle, "fillRef"), cc, theme, info.path);
      const bdr = kid(tcStyle, "tcBdr");
      const edge = { l: c <= g.c0, r: c + cs - 1 >= g.c1, t: r <= g.r0, b: r + rs - 1 >= g.r1 };
      for (const side of ["l", "r", "t", "b"] as const) {
        const [outer, inner] = SIDE[side];
        const v = lineOf(kid(bdr, edge[side] ? outer : inner));
        if (v !== undefined) st.borders[side] = v;
      }
      const tx = kid(g.part, "tcTxStyle");
      if (tx) {
        const b = attr(tx, "b");
        const i = attr(tx, "i");
        if (b) st.bold = b === "on";
        if (i) st.italic = i === "on";
        const col = colorIn(tx, cc);
        if (col) st.color = col;
        const font = kid(tx, "font");
        const face = attr(kid(font, "latin"), "typeface");
        if (face) st.face = face;
        const fr = attr(kid(tx, "fontRef"), "idx");
        if (!face && fr) st.face = fr === "major" ? "+mj-lt" : "+mn-lt";
      }
    }
    return st;
  };

  const table = h("table", { class: C.table, style: css({ left: "0", top: "0", width: `${Math.round(cols.reduce((a, b) => a + b, 0) * 100) / 100}px` }) });
  // Background of the whole table
  const tblFill = readFill(fillElIn(tblPr), cc, info.path);
  if (tblFill?.t === "solid") table.style.backgroundColor = rgbaCss(tblFill.c)!;
  const colgroup = h("colgroup", null, ...cols.map((w) => h("col", { style: `width:${Math.round(w * 100) / 100}px` })));
  table.append(colgroup);
  const tbody = h("tbody");
  table.append(tbody);

  rows.forEach((tr, r) => {
    const rowH = (numAttr(tr, "h") ?? 0) * m.sy;
    const trEl = h("tr", { style: `height:${Math.round(rowH * 100) / 100}px` });
    let c = 0;
    for (const tc of kids(tr, "tc")) {
      const col = c;
      c++;
      // Skip continuation cells of merges
      if (attr(tc, "hMerge") === "1" || attr(tc, "vMerge") === "1" || attr(tc, "hMerge") === "true" || attr(tc, "vMerge") === "true") continue;
      const cs = Math.max(1, numAttr(tc, "gridSpan") ?? 1);
      const rs = Math.max(1, numAttr(tc, "rowSpan") ?? 1);
      const tcPr = kid(tc, "tcPr");
      const st = styleFor(r, col, rs, cs);
      // The cell's own settings
      const ownFill = fillElIn(tcPr);
      const fill = ownFill ? readFill(ownFill, cc, info.path) : st.fill;
      const own = { l: "lnL", r: "lnR", t: "lnT", b: "lnB" } as const;
      const border = (side: "l" | "r" | "t" | "b") => {
        const el = kid(tcPr, own[side]);
        const line = el ? resolveLine([el], null, cc, theme, info.path) : st.borders[side];
        if (!line) return undefined;
        const color = line.fill.t === "solid" ? rgbaCss(line.fill.c) : line.fill.t === "grad" ? rgbaCss(line.fill.stops[0].c) : "#000";
        const dash = line.dash ? (line.dash[0] <= 1 ? "dotted" : "dashed") : line.compound === "dbl" ? "double" : "solid";
        return `${Math.round(line.width * 100) / 100}px ${dash} ${color}`;
      };
      const mar = (n: string, d: number) => ((numAttr(tcPr, n) ?? d) * m.sx);
      const anchor = attr(tcPr, "anchor") ?? "t";
      const td = h("td", {
        colspan: cs > 1 ? cs : undefined,
        rowspan: rs > 1 ? rs : undefined,
        style: css({
          "border-left": border("l"),
          "border-right": border("r"),
          "border-top": border("t"),
          "border-bottom": border("b"),
          "background-color": fill?.t === "solid" ? rgbaCss(fill.c) : undefined,
          "background-image": fill?.t === "grad" ? cssGradient(fill) : undefined,
          padding: `${r2(mar("marT", 45720))}px ${r2(mar("marR", 91440))}px ${r2(mar("marB", 45720))}px ${r2(mar("marL", 91440))}px`,
          "vertical-align": anchor === "ctr" ? "middle" : anchor === "b" ? "bottom" : "top",
          "writing-mode": attr(tcPr, "vert") === "vert" || attr(tcPr, "vert") === "eaVert" ? "vertical-rl" : undefined,
          position: kid(tcPr, "lnTlToBr") || kid(tcPr, "lnBlToTr") ? "relative" : undefined,
        }),
      });
      const txBody = kid(tc, "txBody");
      if (txBody) {
        const base = tctx.textFor(null);
        const tcx: TextCtx = { ...base, cellBold: st.bold, cellItalic: st.italic, cellColor: st.color ?? null, cellFace: st.face ?? null };
        const bp = { ...readBodyPr([kid(txBody, "bodyPr")]), l: 0, r: 0, t: 0, b: 0 };
        for (const p of renderParagraphs(txBody, tcx, { bp })) td.append(p);
      }
      // Diagonal lines
      for (const [name, d] of [["lnTlToBr", "M0 0L100 100"], ["lnBlToTr", "M0 100L100 0"]] as const) {
        const line = kid(tcPr, name) ? resolveLine([kid(tcPr, name)], null, cc, theme, info.path) : null;
        if (!line || line.fill.t !== "solid") continue;
        td.append(
          s("svg", { viewBox: "0 0 100 100", preserveAspectRatio: "none", style: "position:absolute;inset:0;width:100%;height:100%;pointer-events:none" },
            s("path", { d, stroke: rgbaCss(line.fill.c), "stroke-width": line.width, "vector-effect": "non-scaling-stroke", fill: "none" })),
        );
      }
      trEl.append(td);
    }
    tbody.append(trEl);
  });
  return table;
}

const r2 = (n: number) => Math.round(n * 100) / 100;
