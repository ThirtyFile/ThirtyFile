/**
 * Sections (w:sectPr): page size, margins, columns, header/footer references, page number format, document grid, page borders.
 */

import { attr, kid, kids, numAttr, toggle } from "../core/package";
import { tw, twAttr } from "./xml";

export interface HeaderRefs {
  default?: string;
  first?: string;
  even?: string;
}

export interface Section {
  el: Element | null;
  /** nextPage/continuous/evenPage/oddPage/nextColumn */
  type: string;
  /** px */
  width: number;
  height: number;
  top: number;
  right: number;
  bottom: number;
  left: number;
  header: number;
  footer: number;
  gutter: number;
  cols: { num: number; space: number; sep: boolean; widths: number[] };
  /** r:id (relative to document.xml); unspecified types inherit from the previous section */
  headers: HeaderRefs;
  footers: HeaderRefs;
  titlePg: boolean;
  pgNumFmt: string | null;
  pgNumStart: number | null;
  /** Vertical alignment of the page (cover pages often use center) */
  vAlign: string;
  grid: { type: string; pitch: number };
  pgBorders: Element | null;
  bidi: boolean;
  /** Footnote number format */
  footnoteFmt: string | null;
  endnoteFmt: string | null;
}

/** Body width (px) */
export const contentWidth = (s: Section) => Math.max(48, s.width - s.left - s.right - s.gutter);
/** Body height (px) */
export const contentHeight = (s: Section) => Math.max(48, s.height - s.top - s.bottom);

function refs(sectPr: Element | null, name: string): HeaderRefs {
  const out: HeaderRefs = {};
  for (const r of kids(sectPr, name)) {
    const t = (attr(r, "type") ?? "default") as keyof HeaderRefs;
    const id = attr(r, "id");
    if (id && (t === "default" || t === "first" || t === "even")) out[t] = id;
  }
  return out;
}

export function parseSection(sectPr: Element | null, prev: Section | null): Section {
  const pgSz = kid(sectPr, "pgSz");
  const pgMar = kid(sectPr, "pgMar");
  let w = twAttr(pgSz, "w") ?? 11906;
  let h = twAttr(pgSz, "h") ?? 16838;
  // Swap width and height ourselves for landscape pages that weren't swapped
  if (attr(pgSz, "orient") === "landscape" && w < h) [w, h] = [h, w];
  const m = (name: string, def: number) => tw(Math.abs(twAttr(pgMar, name) ?? def));
  const colsEl = kid(sectPr, "cols");
  const colNum = Math.max(1, Math.min(10, numAttr(colsEl, "num") ?? 1));
  const pgNum = kid(sectPr, "pgNumType");
  const grid = kid(sectPr, "docGrid");
  const headers = refs(sectPr, "headerReference");
  const footers = refs(sectPr, "footerReference");
  return {
    el: sectPr,
    type: attr(kid(sectPr, "type"), "val") ?? "nextPage",
    width: tw(w),
    height: tw(h),
    top: m("top", 1440),
    right: m("right", 1440),
    bottom: m("bottom", 1440),
    left: m("left", 1440),
    header: m("header", 851),
    footer: m("footer", 992),
    gutter: m("gutter", 0),
    cols: {
      num: colNum,
      space: tw(twAttr(colsEl, "space") ?? 720),
      sep: toggle(kid(colsEl, "sep")) ?? /^(1|true|on)$/i.test(attr(colsEl, "sep") ?? ""),
      widths: kids(colsEl, "col").map((c) => tw(twAttr(c, "w") ?? 0)),
    },
    headers: { ...prev?.headers, ...headers },
    footers: { ...prev?.footers, ...footers },
    titlePg: toggle(kid(sectPr, "titlePg")) ?? false,
    pgNumFmt: attr(pgNum, "fmt"),
    pgNumStart: numAttr(pgNum, "start"),
    vAlign: attr(kid(sectPr, "vAlign"), "val") ?? "top",
    grid: { type: attr(grid, "type") ?? "default", pitch: twAttr(grid, "linePitch") ?? 0 },
    pgBorders: kid(sectPr, "pgBorders"),
    bidi: toggle(kid(sectPr, "bidi")) ?? false,
    footnoteFmt: attr(kid(kid(sectPr, "footnotePr"), "numFmt"), "val"),
    endnoteFmt: attr(kid(kid(sectPr, "endnotePr"), "numFmt"), "val"),
  };
}

/** Whether the line grid is enabled (common in CJK documents: each line height snaps to linePitch) */
export const gridPitchPt = (s: Section) => ((s.grid.type === "lines" || s.grid.type === "linesAndChars") && s.grid.pitch > 0 ? s.grid.pitch / 20 : 0);
