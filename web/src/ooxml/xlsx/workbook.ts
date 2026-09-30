/**
 * .xlsx reading, and write-back that "changes only what's necessary".
 *
 * - Read: cell values, formulas (with shared formulas expanded), merged cells, column widths/row heights, styles (font, fill, borders, alignment, number format)
 * - Write back: instead of rebuilding the file, the original .xlsx is modified:
 *   1. Replay row/column inserts/deletes on the original XML in order, adjusting conditional formats, data validations, hyperlinks, filters, defined names,
 *      chart references, image positions and pivot sources
 *   2. Rewrite only cells whose content or style changed; new styles are copied from the original style and then modified (preserving theme colors and other settings)
 *   3. Merged cells and column widths/row heights are rewritten only when they changed
 *   Everything else (charts, images, pivot tables, all content besides macros) is left untouched
 */
import JSZip from "jszip";
import {
  MAX_COLS,
  cellName,
  key,
  parseCellName,
  colOf,
  rowOf,
  type Borders,
  type Cell,
  type CellStyle,
  type Range,
  type Scalar,
  type Sheet,
  type Workbook,
} from "./model";
import { shiftFormula } from "./formula";
import { parseTheme, rgbaHex, sheetColor, INDEXED_COLORS, type Theme } from "../core/theme";
import { checkZipSizes, readEntry } from "../core/package";
import { breathe } from "../core/yield";
import { OoxmlError } from "../core/errors";
import { adjustFormula, adjustSqref, adjustCell, cloneState, type SheetState, type StructOp } from "./ops";

const NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL_NS = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/** Don't add folder entries automatically: the original usually has none, so keep the same structure as the original */
const NO_FOLDERS = { createFolders: false };

const parseXml = (text: string) => new DOMParser().parseFromString(text, "application/xml");
const byTag = (el: Element | Document, name: string) => Array.from(el.getElementsByTagNameNS(NS, name));
const child = (el: Element, name: string) => Array.from(el.children).find((c) => c.localName === name) ?? null;
const serialize = (doc: Document) => new XMLSerializer().serializeToString(doc);

function ref(r: string): [number, number] {
  const p = parseCellName(r);
  if (!p) throw new OoxmlError("bad-cell-reference", r);
  return p;
}

const pxToWidth = (px: number) => Math.max(0, Math.round(((px - 5) / 7) * 100) / 100);
const widthToPx = (w: number) => Math.round(w * 7 + 5);
const pxToPt = (px: number) => Math.round(px * 0.75 * 100) / 100;

// ───────────── Styles ─────────────

const BUILTIN_NUMFMT: Record<number, string> = {
  1: "0",
  2: "0.00",
  3: "#,##0",
  4: "#,##0.00",
  9: "0%",
  10: "0.00%",
  11: "0.00E+00",
  14: "yyyy/m/d",
  15: "d-mmm-yy",
  16: "d-mmm",
  17: "mmm-yy",
  18: "h:mm AM/PM",
  19: "h:mm:ss AM/PM",
  20: "h:mm",
  21: "h:mm:ss",
  22: "yyyy/m/d h:mm",
  // Built-in formats for the Traditional Chinese (Taiwan) locale; e is the ROC (Minguo) year
  27: '[$-404]e/m/d',
  28: '[$-404]e"年"m"月"d"日"', // i18n-ignore: Excel built-in number format code
  29: '[$-404]e"年"m"月"d"日"', // i18n-ignore: Excel built-in number format code
  30: "m/d/yy",
  31: 'yyyy"年"m"月"d"日"', // i18n-ignore: Excel built-in number format code
  32: 'hh"時"mm"分"', // i18n-ignore: Excel built-in number format code
  33: 'hh"時"mm"分"ss"秒"', // i18n-ignore: Excel built-in number format code
  34: '上午/下午hh"時"mm"分"', // i18n-ignore: Excel built-in number format code
  35: '上午/下午hh"時"mm"分"ss"秒"', // i18n-ignore: Excel built-in number format code
  36: '[$-404]e/m/d',
  50: '[$-404]e/m/d',
  51: '[$-404]e"年"m"月"d"日"', // i18n-ignore: Excel built-in number format code
  52: '上午/下午hh"時"mm"分"', // i18n-ignore: Excel built-in number format code
  53: '上午/下午hh"時"mm"分"ss"秒"', // i18n-ignore: Excel built-in number format code
  54: '[$-404]e"年"m"月"d"日"', // i18n-ignore: Excel built-in number format code
  55: '上午/下午hh"時"mm"分"', // i18n-ignore: Excel built-in number format code
  56: '上午/下午hh"時"mm"分"ss"秒"', // i18n-ignore: Excel built-in number format code
  57: '[$-404]e/m/d',
  58: '[$-404]e"年"m"月"d"日"', // i18n-ignore: Excel built-in number format code
  37: "#,##0 ;(#,##0)",
  38: "#,##0 ;[Red](#,##0)",
  39: "#,##0.00;(#,##0.00)",
  40: "#,##0.00;[Red](#,##0.00)",
  45: "mm:ss",
  46: "[h]:mm:ss",
  47: "mmss.0",
  48: "##0.0E+0",
  49: "@",
};

/** Theme and indexed colors used when reading colors (styles.xml may customize indexed colors) */
let colorCtx: { theme: Theme | null; palette: string[] } = { theme: null, palette: INDEXED_COLORS };

/** SpreadsheetML color (rgb, theme color + tint, indexed color) → #RRGGBB */
function color(el: Element | null) {
  const c = sheetColor(el, colorCtx.theme, colorCtx.palette);
  return c ? rgbaHex(c).toUpperCase() : undefined;
}

const on = (el: Element | null) => !!el && el.getAttribute("val") !== "0" && el.getAttribute("val") !== "false";

function readBorder(el: Element | undefined): Borders | undefined {
  if (!el) return undefined;
  const b: Borders = {};
  for (const side of ["top", "right", "bottom", "left"] as const) {
    const s = child(el, side);
    const style = s?.getAttribute("style");
    if (s && style && style !== "none") b[side] = { style, ...(color(child(s, "color")) ? { color: color(child(s, "color")) } : {}) };
  }
  return Object.keys(b).length ? b : undefined;
}

function readFont(font: Element | undefined): CellStyle {
  const s: CellStyle = {};
  if (!font) return s;
  if (on(child(font, "b"))) s.bold = true;
  if (on(child(font, "i"))) s.italic = true;
  if (child(font, "u") && child(font, "u")?.getAttribute("val") !== "none") s.underline = true;
  if (on(child(font, "strike"))) s.strike = true;
  const sz = Number(child(font, "sz")?.getAttribute("val"));
  if (sz) s.size = sz;
  const name = child(font, "name")?.getAttribute("val");
  if (name) s.font = name;
  const cl = color(child(font, "color"));
  if (cl) s.color = cl;
  return s;
}

/** Foreground color ratio of pattern fills */
const PATTERN_DENSITY: Record<string, number> = {
  gray0625: 0.0625, gray125: 0.125, lightGray: 0.25, mediumGray: 0.5, darkGray: 0.75,
  lightHorizontal: 0.25, lightVertical: 0.25, lightDown: 0.25, lightUp: 0.25, lightGrid: 0.4, lightTrellis: 0.4,
  darkHorizontal: 0.5, darkVertical: 0.5, darkDown: 0.5, darkUp: 0.5, darkGrid: 0.6, darkTrellis: 0.6,
};

function mixHex(a: string, b: string, ratio: number) {
  const ch = (hex: string, i: number) => parseInt(hex.slice(i, i + 2), 16);
  return `#${[1, 3, 5]
    .map((i) => Math.round(ch(a, i) * ratio + ch(b, i) * (1 - ratio)))
    .map((v) => v.toString(16).padStart(2, "0"))
    .join("")
    .toUpperCase()}`;
}

function readStyles(doc: Document | null): CellStyle[] {
  if (!doc) return [{}];
  const custom = byTag(doc, "rgbColor").map((c) => (c.getAttribute("rgb") ?? "").slice(-6));
  colorCtx = { ...colorCtx, palette: custom.length ? custom : INDEXED_COLORS };
  const numFmts = new Map<number, string>();
  for (const n of byTag(doc, "numFmt")) numFmts.set(Number(n.getAttribute("numFmtId")), n.getAttribute("formatCode") ?? "");
  const list = (name: string) => (byTag(doc, name)[0] ? Array.from(byTag(doc, name)[0].children) : []);
  const fonts = list("fonts");
  const fills = list("fills");
  const borders = list("borders");
  const xfs = list("cellXfs");
  if (!xfs.length) return [{}];
  return xfs.map((xf) => {
    const s: CellStyle = readFont(fonts[Number(xf.getAttribute("fontId") ?? 0)]);
    const fill = fills[Number(xf.getAttribute("fillId") ?? 0)];
    const pattern = fill && child(fill, "patternFill");
    const type = pattern?.getAttribute("patternType");
    if (type === "solid") {
      const bg = color(child(pattern!, "fgColor"));
      if (bg) s.bg = bg;
    } else if (type && type !== "none") {
      // Pattern fill: approximate by blending foreground and background by density
      const fg = color(child(pattern!, "fgColor")) ?? "#000000";
      const bgEl = child(pattern!, "bgColor");
      const auto = !bgEl || /^6[45]$/.test(bgEl.getAttribute("indexed") ?? "") || bgEl.getAttribute("auto") === "1";
      const bg = auto ? "#FFFFFF" : (color(bgEl) ?? "#FFFFFF");
      s.bg = mixHex(fg, bg, PATTERN_DENSITY[type] ?? 0.25);
    }
    const gradient = fill && child(fill, "gradientFill");
    if (gradient) {
      // Gradient: use the midpoint of the two end colors
      const stops = Array.from(gradient.children).map((st) => color(child(st, "color"))).filter((c): c is string => !!c);
      if (stops.length) s.bg = stops.length > 1 ? mixHex(stops[0], stops[stops.length - 1], 0.5) : stops[0];
    }
    const border = readBorder(borders[Number(xf.getAttribute("borderId") ?? 0)]);
    if (border) s.border = border;
    const align = child(xf, "alignment");
    if (align) {
      const h = align.getAttribute("horizontal");
      if (h === "left" || h === "center" || h === "right" || h === "justify") s.hAlign = h;
      else if (h === "centerContinuous") s.hAlign = "center";
      const v = align.getAttribute("vertical");
      if (v === "top" || v === "center" || v === "bottom") s.vAlign = v;
      if (align.getAttribute("wrapText") === "1" || align.getAttribute("wrapText") === "true") s.wrap = true;
      const indent = Number(align.getAttribute("indent"));
      if (indent > 0) s.indent = indent;
    }
    const id = Number(xf.getAttribute("numFmtId") ?? 0);
    const fmt = numFmts.get(id) ?? BUILTIN_NUMFMT[id];
    if (fmt && fmt !== "General") s.numFmt = fmt;
    return s;
  });
}

// ───────────── Reading ─────────────

/** Path relative to some file → full path inside the zip */
function resolvePath(base: string, target: string) {
  if (target.startsWith("/")) return target.slice(1);
  const parts = base.split("/").slice(0, -1);
  for (const p of target.split("/")) {
    if (p === "..") parts.pop();
    else if (p && p !== ".") parts.push(p);
  }
  return parts.join("/");
}

const relsPath = (path: string) => {
  const i = path.lastIndexOf("/");
  return `${path.slice(0, i)}/_rels/${path.slice(i + 1)}.rels`;
};

async function related(zip: JSZip, path: string, type: string): Promise<string[]> {
  const xml = await readEntry(zip, relsPath(path), "string");
  if (!xml) return [];
  return Array.from(parseXml(xml).getElementsByTagName("Relationship"))
    .filter((r) => (r.getAttribute("Type") ?? "").endsWith(`/${type}`) && r.getAttribute("TargetMode") !== "External")
    .map((r) => resolvePath(path, r.getAttribute("Target") ?? ""));
}

/** State at open (or last save), used on save to detect what changed; adjusted on insert/delete the same way as the on-screen data */
export type Snapshot = Map<string, SheetState>;

export async function readXlsx(buf: ArrayBuffer): Promise<{ zip: JSZip; book: Workbook; snapshot: Snapshot }> {
  // JSZip's own errors ("Corrupted zip…") are English and cryptic
  const zip = await JSZip.loadAsync(buf).catch(() => {
    throw new OoxmlError("not-ooxml");
  });
  try {
    checkZipSizes(zip);
  } catch {
    throw new OoxmlError("too-large");
  }
  const text = async (path: string) => (await readEntry(zip, path, "string")) ?? null;
  const workbookXml = await text("xl/workbook.xml");
  if (!workbookXml) throw new OoxmlError("no-workbook");
  const workbook = parseXml(workbookXml);
  const rels = parseXml((await text("xl/_rels/workbook.xml.rels")) ?? "<Relationships/>");
  const targets = new Map(Array.from(rels.getElementsByTagName("Relationship")).map((r) => [r.getAttribute("Id"), r.getAttribute("Target") ?? ""]));

  const sstXml = await text("xl/sharedStrings.xml");
  // Phonetic runs (rPh) are not content
  const strings = sstXml
    ? byTag(parseXml(sstXml), "si").map((si) =>
        byTag(si, "t")
          .filter((t) => t.parentElement?.localName !== "rPh")
          .map((t) => t.textContent ?? "")
          .join(""),
      )
    : [];
  const themeTarget = Array.from(rels.getElementsByTagName("Relationship")).find((r) => (r.getAttribute("Type") ?? "").endsWith("/theme"));
  const themeXml = themeTarget ? await text(resolvePath("xl/workbook.xml", themeTarget.getAttribute("Target") ?? "")) : null;
  const stylesXml = await text("xl/styles.xml");
  // colorCtx is module-shared: set it before each synchronous use (so multiple open workbooks don't interfere)
  colorCtx = { theme: themeXml ? parseTheme(parseXml(themeXml)) : null, palette: INDEXED_COLORS };
  const styles = readStyles(stylesXml ? parseXml(stylesXml) : null);
  const ctx = colorCtx;

  const sheets: Sheet[] = [];
  const snapshot: Snapshot = new Map();
  for (const [i, el] of byTag(workbook, "sheet").entries()) {
    const rid = el.getAttributeNS(REL_NS, "id") ?? el.getAttribute("r:id");
    const target = rid ? targets.get(rid) : undefined;
    if (!target || el.getAttribute("state") === "veryHidden") continue;
    const path = resolvePath("xl/workbook.xml", target);
    const xml = await text(path);
    // Skip sheets without cells, such as chart sheets
    if (!xml || !xml.includes("sheetData")) continue;
    const doc = parseXml(xml);

    const cells = new Map<number, Cell>();
    const shared = new Map<string, { r: number; c: number; f: string }>();
    let maxRow = 0;
    let maxCol = 0;
    let n = 0;
    for (const c of byTag(doc, "c")) {
      // Large sheets: let the page breathe every few thousand cells
      if ((++n & 4095) === 0) await breathe();
      const at = c.getAttribute("r");
      if (!at) continue;
      const [r, col] = ref(at);
      const t = c.getAttribute("t");
      const raw = child(c, "v")?.textContent ?? null;
      let v: Scalar = null;
      if (t === "s") v = raw !== null ? (strings[Number(raw)] ?? "") : null;
      else if (t === "inlineStr")
        v = byTag(c, "t")
          .map((x) => x.textContent ?? "")
          .join("");
      else if (t === "b") v = raw === null ? null : raw === "1";
      else if (t === "str" || t === "e") v = raw;
      else if (raw !== null && raw !== "") v = Number(raw);

      let f: string | undefined;
      const fEl = child(c, "f");
      if (fEl) {
        const body = fEl.textContent ?? "";
        const si = fEl.getAttribute("si");
        if (fEl.getAttribute("t") === "shared" && si !== null) {
          if (body) shared.set(si, { r, c: col, f: body });
          const master = shared.get(si);
          if (master) f = "=" + (body || shiftFormula(master.f, r - master.r, col - master.c));
        } else if (body) f = "=" + body;
      }
      const s = Number(c.getAttribute("s") ?? 0) || undefined;
      if (v === null && !f && !s) continue;
      cells.set(key(r, col), { v, ...(f ? { f } : {}), ...(s ? { s } : {}) });
      if (v !== null || f) {
        maxRow = Math.max(maxRow, r);
        maxCol = Math.max(maxCol, col);
      }
    }

    const merges: Range[] = byTag(doc, "mergeCell").flatMap((m) => {
      const [a, b] = (m.getAttribute("ref") ?? "").split(":");
      if (!a || !b) return [];
      const [r1, c1] = ref(a);
      const [r2, c2] = ref(b);
      return [{ r1, c1, r2, c2 }];
    });

    // Column widths are stored in characters and row heights in points; convert to pixels
    const format = byTag(doc, "sheetFormatPr")[0];
    const defaultRowHeight = Math.round(((Number(format?.getAttribute("defaultRowHeight")) || 15) * 4) / 3);
    const baseWidth = Number(format?.getAttribute("defaultColWidth")) || (Number(format?.getAttribute("baseColWidth")) || 8) + 0.71;
    const defaultColWidth = widthToPx(baseWidth);
    const colWidths = new Map<number, number>();
    const colStyles = new Map<number, number>();
    for (const col of byTag(doc, "col")) {
      const w = Number(col.getAttribute("width"));
      const hidden = col.getAttribute("hidden") === "1";
      const style = Number(col.getAttribute("style") ?? 0);
      const min = Number(col.getAttribute("min")) - 1;
      const max = Math.min(Number(col.getAttribute("max")) - 1, MAX_COLS - 1, min + 1000);
      for (let k = min; k <= max; k++) {
        if (hidden) colWidths.set(k, 0);
        else if (w && col.getAttribute("customWidth") !== "0") colWidths.set(k, widthToPx(w));
        if (style) colStyles.set(k, style);
      }
    }
    const rowHeights = new Map<number, number>();
    for (const row of byTag(doc, "row")) {
      const r = Number(row.getAttribute("r")) - 1;
      // An ht without customHeight is the height Excel auto-fitted to the font or wrapping; use it as well
      if (row.getAttribute("hidden") === "1") rowHeights.set(r, 0);
      else if (row.getAttribute("ht")) {
        const h = Math.round((Number(row.getAttribute("ht")) * 4) / 3);
        if (h !== defaultRowHeight) rowHeights.set(r, h);
      }
    }

    // Table (ListObject) ranges: inserting/deleting columns changes table columns, so leave that to Excel
    const tables: Range[] = [];
    for (const t of await related(zip, path, "table")) {
      const tx = await text(t);
      const r = tx && parseXml(tx).documentElement.getAttribute("ref");
      if (!r) continue;
      const [a, b] = r.split(":");
      const [r1, c1] = ref(a);
      const [r2, c2] = ref(b ?? a);
      tables.push({ r1, c1, r2, c2 });
    }

    const id = `s${i}`;
    const sheet: Sheet = {
      id,
      name: el.getAttribute("name") ?? `工作表${i + 1}`, // i18n-ignore: default sheet name is workbook data (formulas refer to it)
      path,
      cells,
      merges,
      colWidths,
      rowHeights,
      colStyles,
      defaultColWidth,
      defaultRowHeight,
      maxRow,
      maxCol,
      tables,
      pivot: (await related(zip, path, "pivotTable")).length > 0,
      ...((colorCtx = ctx), sheetView(doc)),
      ...(el.getAttribute("state") === "hidden" ? { hidden: true } : {}),
    };
    sheets.push(sheet);
    snapshot.set(id, cloneState(sheet));
  }
  if (sheets.length === 0) throw new OoxmlError("no-sheets");
  // Sheet shown on open (activeTab indexes all sheets, including skipped chart sheets)
  const activeTab = Number(byTag(workbook, "workbookView")[0]?.getAttribute("activeTab") ?? 0);
  const activeName = byTag(workbook, "sheet")[activeTab]?.getAttribute("name");
  const active = Math.max(0, sheets.findIndex((sh) => sh.name === activeName && !sh.hidden));
  return { zip, book: { sheets, styles, xfCount: styles.length, styleBase: new Map(), active }, snapshot };
}

/** Sheet view settings: gridlines, frozen panes, tab color */
function sheetView(doc: Document): Partial<Sheet> {
  const out: Partial<Sheet> = {};
  const view = byTag(doc, "sheetView")[0];
  if (view?.getAttribute("showGridLines") === "0") out.noGrid = true;
  const pane = view && child(view, "pane");
  const state = pane?.getAttribute("state");
  if (pane && (state === "frozen" || state === "frozenSplit")) {
    const rows = Math.floor(Number(pane.getAttribute("ySplit") ?? 0));
    const cols = Math.floor(Number(pane.getAttribute("xSplit") ?? 0));
    if (rows > 0 || cols > 0) out.frozen = { rows, cols };
  }
  const tab = color(byTag(doc, "tabColor")[0] ?? null);
  if (tab) out.tabColor = tab;
  return out;
}

/** Height (px) of a row in the file; undefined if default or unset, 0 if hidden */
function heightOf(row: Element, defaultHeight: number): number | undefined {
  if (row.getAttribute("hidden") === "1") return 0;
  const ht = row.getAttribute("ht");
  if (!ht) return undefined;
  const h = Math.round((Number(ht) * 4) / 3);
  return h === defaultHeight ? undefined : h;
}

// ───────────── Write-back: shared ─────────────

const ERRORS = new Set(["#NULL!", "#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#NUM!", "#N/A"]);

function sheetDataOf(doc: Document) {
  const sd = byTag(doc, "sheetData")[0];
  if (!sd) throw new OoxmlError("unknown-sheet");
  return sd;
}

/**
 * Row and cell index for sheetData: avoids rescanning the whole XML for every cell during bulk edits.
 * Built after row/column insert/delete replay finishes; all later additions and removals go through here to maintain the index.
 */
class SheetIndex {
  private rows = new Map<number, Element>();
  private cells = new Map<string, Element>();
  private sheetData: Element;

  constructor(sheetData: Element) {
    this.sheetData = sheetData;
    for (const row of Array.from(sheetData.children)) {
      if (row.localName !== "row") continue;
      this.rows.set(Number(row.getAttribute("r")) - 1, row);
      for (const c of Array.from(row.children)) if (c.localName === "c" && c.getAttribute("r")) this.cells.set(c.getAttribute("r")!, c);
    }
  }

  row(r: number) {
    return this.rows.get(r) ?? null;
  }

  ensureRow(r: number) {
    const existing = this.rows.get(r);
    if (existing) return existing;
    const row = this.sheetData.ownerDocument.createElementNS(NS, "row");
    row.setAttribute("r", String(r + 1));
    // A new row goes before the next existing row (new rows are rare, so a linear search is fine)
    let next: Element | null = null;
    let nextR = Infinity;
    for (const [rr, el] of this.rows) {
      if (rr > r && rr < nextR) {
        nextR = rr;
        next = el;
      }
    }
    this.sheetData.insertBefore(row, next);
    this.rows.set(r, row);
    return row;
  }

  cell(r: number, c: number) {
    return this.cells.get(cellName(r, c)) ?? null;
  }

  ensureCell(r: number, c: number) {
    const name = cellName(r, c);
    const existing = this.cells.get(name);
    if (existing) return existing;
    const row = this.ensureRow(r);
    const cell = row.ownerDocument.createElementNS(NS, "c");
    cell.setAttribute("r", name);
    const next = Array.from(row.children).find((x) => x.localName === "c" && ref(x.getAttribute("r") ?? "A1")[1] > c) ?? null;
    row.insertBefore(cell, next);
    // Row spans are only a performance hint and Excel reports an error when they don't match, so just remove them
    row.removeAttribute("spans");
    this.cells.set(name, cell);
    return cell;
  }

  removeCell(cell: Element) {
    const name = cell.getAttribute("r");
    if (name) this.cells.delete(name);
    cell.parentNode?.removeChild(cell);
  }

  removeRow(r: number) {
    const row = this.rows.get(r);
    if (!row) return;
    this.rows.delete(r);
    row.parentNode?.removeChild(row);
  }

  /** Empty row with no cells and no row settings */
  pruneEmptyRows() {
    for (const [r, row] of this.rows) {
      if (row.children.length === 0 && !row.getAttribute("ht") && !row.getAttribute("hidden") && !row.getAttribute("s")) this.removeRow(r);
    }
  }
}

/** Turn a shared formula group into independent formulas so any cell in it can be safely modified or moved */
function unshare(doc: Document, si: string) {
  const group = byTag(doc, "f").filter((f) => f.getAttribute("t") === "shared" && f.getAttribute("si") === si);
  const master = group.find((f) => f.textContent);
  if (!master) return;
  const [mr, mc] = ref(master.parentElement?.getAttribute("r") ?? "A1");
  const text = master.textContent ?? "";
  for (const f of group) {
    const [r, c] = ref(f.parentElement?.getAttribute("r") ?? "A1");
    f.textContent = shiftFormula(text, r - mr, c - mc);
    for (const a of ["t", "si", "ref"]) f.removeAttribute(a);
  }
}

function unshareAll(doc: Document) {
  const ids = new Set(
    byTag(doc, "f").flatMap((f) => (f.getAttribute("t") === "shared" && f.getAttribute("si") !== null ? [f.getAttribute("si")!] : [])),
  );
  for (const si of ids) unshare(doc, si);
}

function writeContent(cell: Element, v: Scalar, f: string | undefined) {
  const doc = cell.ownerDocument;
  for (const el of Array.from(cell.children)) if (["f", "v", "is"].includes(el.localName)) cell.removeChild(el);
  cell.removeAttribute("t");
  const add = (name: string, text: string) => {
    const el = doc.createElementNS(NS, name);
    el.textContent = text;
    cell.appendChild(el);
  };
  const value = (val: Scalar, formula: boolean) => {
    if (typeof val === "number") add("v", String(val));
    else if (typeof val === "boolean") {
      cell.setAttribute("t", "b");
      add("v", val ? "1" : "0");
    } else if (typeof val === "string" && ERRORS.has(val)) {
      cell.setAttribute("t", "e");
      add("v", val);
    } else if (typeof val === "string" && formula) {
      cell.setAttribute("t", "str");
      add("v", val);
    } else if (typeof val === "string" && val !== "") {
      cell.setAttribute("t", "inlineStr");
      const is = doc.createElementNS(NS, "is");
      const t = doc.createElementNS(NS, "t");
      t.setAttribute("xml:space", "preserve");
      t.textContent = val;
      is.appendChild(t);
      cell.appendChild(is);
    }
  };
  if (f) {
    add("f", f.replace(/^=/, ""));
    // Cached result: lets previews and programs that don't recalculate see the correct number; Excel still recalculates on open
    if (v !== null) value(v, true);
  } else value(v, false);
}

/** Insert a new element at the correct position according to CT_Worksheet element order */
const WORKSHEET_ORDER = [
  "sheetPr",
  "dimension",
  "sheetViews",
  "sheetFormatPr",
  "cols",
  "sheetData",
  "sheetCalcPr",
  "sheetProtection",
  "protectedRanges",
  "scenarios",
  "autoFilter",
  "sortState",
  "dataConsolidate",
  "customSheetViews",
  "mergeCells",
  "phoneticPr",
  "conditionalFormatting",
  "dataValidations",
  "hyperlinks",
  "printOptions",
  "pageMargins",
  "pageSetup",
  "headerFooter",
  "rowBreaks",
  "colBreaks",
  "customProperties",
  "cellWatches",
  "ignoredErrors",
  "smartTags",
  "drawing",
  "legacyDrawing",
  "legacyDrawingHF",
  "drawingHF",
  "picture",
  "oleObjects",
  "controls",
  "webPublishItems",
  "tableParts",
  "extLst",
];

function placeInWorksheet(doc: Document, el: Element) {
  const root = doc.documentElement;
  const idx = WORKSHEET_ORDER.indexOf(el.localName);
  const next = Array.from(root.children).find((c) => WORKSHEET_ORDER.indexOf(c.localName) > idx);
  root.insertBefore(el, next ?? null);
}

// ───────────── Write-back: row/column insert and delete ─────────────

const isRows = (op: StructOp) => op.kind === "insertRows" || op.kind === "deleteRows";

/** Move rows and cells in the sheet XML */
function replayPositions(doc: Document, op: StructOp) {
  const sd = sheetDataOf(doc);
  for (const row of Array.from(sd.children).filter((e) => e.localName === "row")) {
    const r = Number(row.getAttribute("r")) - 1;
    if (isRows(op)) {
      const p = adjustCell(r, 0, op);
      if (!p) {
        sd.removeChild(row);
        continue;
      }
      row.setAttribute("r", String(p[0] + 1));
      for (const c of Array.from(row.children).filter((e) => e.localName === "c")) {
        const [, col] = ref(c.getAttribute("r") ?? "A1");
        c.setAttribute("r", cellName(p[0], col));
      }
    } else {
      row.removeAttribute("spans");
      for (const c of Array.from(row.children).filter((e) => e.localName === "c")) {
        const [, col] = ref(c.getAttribute("r") ?? "A1");
        const p = adjustCell(r, col, op);
        if (!p || p[1] >= MAX_COLS) row.removeChild(c);
        else c.setAttribute("r", cellName(r, p[1]));
      }
    }
  }
  // Array formula ranges
  for (const f of byTag(doc, "f")) {
    const r = f.getAttribute("ref");
    if (r && f.getAttribute("t") === "array") {
      const n = adjustSqref(r, op);
      if (n) f.setAttribute("ref", n);
    }
  }
  // Range-based settings: conditional formats, data validations, hyperlinks, filters, ignored errors, protected ranges
  const sqrefs: [string, string][] = [
    ["conditionalFormatting", "sqref"],
    ["dataValidation", "sqref"],
    ["hyperlink", "ref"],
    ["autoFilter", "ref"],
    ["ignoredError", "sqref"],
    ["protectedRange", "sqref"],
    ["selection", "sqref"],
  ];
  for (const [tag, attr] of sqrefs) {
    for (const el of byTag(doc, tag)) {
      const v = el.getAttribute(attr);
      if (!v) continue;
      const n = adjustSqref(v, op);
      if (n) el.setAttribute(attr, n);
      else if (tag === "selection") {
        el.setAttribute("sqref", "A1");
        el.setAttribute("activeCell", "A1");
      } else el.parentNode?.removeChild(el);
    }
  }
  for (const sel of byTag(doc, "selection")) {
    const ac = sel.getAttribute("activeCell");
    if (ac) sel.setAttribute("activeCell", (adjustSqref(ac, op) ?? "A1").split(" ")[0]);
  }
  // Remove containers that became empty as a result
  for (const tag of ["conditionalFormatting", "dataValidations", "hyperlinks"]) {
    for (const el of byTag(doc, tag))
      if (el.children.length === 0 || (tag === "conditionalFormatting" && !el.getAttribute("sqref"))) el.parentNode?.removeChild(el);
  }
  const dv = byTag(doc, "dataValidations")[0];
  if (dv) dv.setAttribute("count", String(byTag(dv, "dataValidation").length));
}

/** References to the target sheet inside formula text (cells, conditional formats, data validations) */
function adjustFormulasIn(doc: Document, hostName: string, targetName: string, op: StructOp) {
  for (const tag of ["f", "formula", "formula1", "formula2"]) {
    for (const el of byTag(doc, tag)) {
      const t = el.textContent;
      if (!t) continue;
      const n = adjustFormula(t, hostName, targetName, op);
      if (n !== t) el.textContent = n;
    }
  }
}

/** Anchors of images and charts on the sheet (xdr:from/xdr:to) */
function adjustDrawing(doc: Document, op: StructOp) {
  const rows = isRows(op);
  for (const anchor of Array.from(doc.getElementsByTagName("*")).filter((e) => e.localName === "from" || e.localName === "to")) {
    const posEl = Array.from(anchor.children).find((e) => e.localName === (rows ? "row" : "col"));
    if (!posEl) continue;
    const pos = Number(posEl.textContent);
    if (Number.isNaN(pos)) continue;
    let next = pos;
    if (op.kind === "insertRows" || op.kind === "insertCols") next = pos >= op.at ? pos + op.count : pos;
    else if (pos >= op.at + op.count) next = pos - op.count;
    else if (pos >= op.at) next = op.at;
    posEl.textContent = String(next);
  }
}

// ───────────── Write-back: styles ─────────────

const FONT_KEYS = ["bold", "italic", "underline", "strike", "size", "font", "color"] as const;
const sameFont = (a: CellStyle, b: CellStyle) => FONT_KEYS.every((k) => (a[k] ?? null) === (b[k] ?? null));
const sameBorder = (a?: Borders, b?: Borders) => JSON.stringify(a ?? {}) === JSON.stringify(b ?? {});
const argb = (hex: string) => `FF${hex.replace("#", "").toUpperCase()}`;

/** Reorder child elements into the standard font element order */
const FONT_ORDER = [
  "b",
  "i",
  "strike",
  "condense",
  "extend",
  "outline",
  "shadow",
  "u",
  "vertAlign",
  "sz",
  "color",
  "name",
  "family",
  "charset",
  "scheme",
];

function buildFont(doc: Document, base: Element | undefined, s: CellStyle, origin: CellStyle) {
  const font = (base?.cloneNode(true) as Element | undefined) ?? doc.createElementNS(NS, "font");
  const set = (name: string, attrs: Record<string, string> | null) => {
    for (const el of Array.from(font.children)) if (el.localName === name) font.removeChild(el);
    if (!attrs) return;
    const el = doc.createElementNS(NS, name);
    for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
    font.appendChild(el);
  };
  set("b", s.bold ? {} : null);
  set("i", s.italic ? {} : null);
  set("strike", s.strike ? {} : null);
  set("u", s.underline ? {} : null);
  set("sz", { val: String(s.size ?? 11) });
  // If the font color didn't change, keep the original setting (theme color, tint) instead of a fixed rgb
  if (s.color && s.color !== origin.color) set("color", { rgb: argb(s.color) });
  else if (!s.color && origin.color) set("color", { theme: "1" });
  else if (!child(font, "color")) set("color", { theme: "1" });
  if (s.font) set("name", { val: s.font });
  // The font name changed, so the original font scheme no longer applies
  if (s.font && base && readFont(base).font !== s.font) set("scheme", null);
  const kids = Array.from(font.children).sort((a, b) => FONT_ORDER.indexOf(a.localName) - FONT_ORDER.indexOf(b.localName));
  for (const k of kids) font.appendChild(k);
  return font;
}

function buildFill(doc: Document, bg: string) {
  const fill = doc.createElementNS(NS, "fill");
  const p = doc.createElementNS(NS, "patternFill");
  p.setAttribute("patternType", "solid");
  const fg = doc.createElementNS(NS, "fgColor");
  fg.setAttribute("rgb", argb(bg));
  const bgc = doc.createElementNS(NS, "bgColor");
  bgc.setAttribute("indexed", "64");
  p.append(fg, bgc);
  fill.appendChild(p);
  return fill;
}

function buildBorder(doc: Document, b?: Borders) {
  const border = doc.createElementNS(NS, "border");
  for (const side of ["left", "right", "top", "bottom", "diagonal"] as const) {
    const el = doc.createElementNS(NS, side);
    const def = side === "diagonal" ? undefined : b?.[side];
    if (def) {
      el.setAttribute("style", def.style);
      const c = doc.createElementNS(NS, "color");
      if (def.color) c.setAttribute("rgb", argb(def.color));
      else c.setAttribute("auto", "1");
      el.appendChild(c);
    }
    border.appendChild(el);
  }
  return border;
}

/** Write styles added while editing into styles.xml (appended after the original cellXfs, so indices match the screen) */
async function writeStyles(zip: JSZip, book: Workbook) {
  if (book.styles.length <= book.xfCount) return;
  const path = "xl/styles.xml";
  const doc = parseXml((await readEntry(zip, path, "string")) ?? "");
  const container = (name: string) => {
    let el = byTag(doc, name)[0];
    if (!el) {
      el = doc.createElementNS(NS, name);
      doc.documentElement.appendChild(el);
    }
    return el;
  };
  const fonts = container("fonts");
  const fills = container("fills");
  const borders = container("borders");
  const cellXfs = container("cellXfs");
  const xfs = Array.from(cellXfs.children);
  const fontList = Array.from(fonts.children);
  const push = (parent: Element, el: Element) => {
    parent.appendChild(el);
    const n = parent.children.length;
    parent.setAttribute("count", String(n));
    return n - 1;
  };

  // Number format: built-in formats use their id; others are added as custom formats (from 164)
  let numFmts = byTag(doc, "numFmts")[0];
  const fmtIds = new Map<string, number>(
    Object.entries(BUILTIN_NUMFMT)
      .filter(([id]) => Number(id) <= 22 || (Number(id) >= 37 && Number(id) <= 49))
      .map(([id, code]) => [code, Number(id)]),
  );
  let nextFmt = 164;
  for (const n of byTag(doc, "numFmt")) {
    const id = Number(n.getAttribute("numFmtId"));
    fmtIds.set(n.getAttribute("formatCode") ?? "", id);
    nextFmt = Math.max(nextFmt, id + 1);
  }
  const fmtId = (code?: string) => {
    if (!code || code === "General") return 0;
    const known = fmtIds.get(code);
    if (known !== undefined) return known;
    if (!numFmts) {
      numFmts = doc.createElementNS(NS, "numFmts");
      doc.documentElement.insertBefore(numFmts, doc.documentElement.firstElementChild);
    }
    const el = doc.createElementNS(NS, "numFmt");
    el.setAttribute("numFmtId", String(nextFmt));
    el.setAttribute("formatCode", code);
    numFmts.appendChild(el);
    numFmts.setAttribute("count", String(numFmts.children.length));
    fmtIds.set(code, nextFmt);
    return nextFmt++;
  };

  for (let i = book.xfCount; i < book.styles.length; i++) {
    const s = book.styles[i];
    const originIdx = book.styleBase.get(i) ?? 0;
    const origin = book.styles[originIdx] ?? {};
    const baseXf = xfs[originIdx] ?? xfs[0];
    const xf = (baseXf?.cloneNode(true) as Element | undefined) ?? doc.createElementNS(NS, "xf");
    for (const a of ["xfId"]) if (!xf.getAttribute(a)) xf.setAttribute(a, "0");

    if (!sameFont(s, origin)) {
      const baseFont = fontList[Number(xf.getAttribute("fontId") ?? 0)];
      xf.setAttribute("fontId", String(push(fonts, buildFont(doc, baseFont, s, origin))));
      xf.setAttribute("applyFont", "1");
    }
    if ((s.bg ?? null) !== (origin.bg ?? null)) {
      xf.setAttribute("fillId", s.bg ? String(push(fills, buildFill(doc, s.bg))) : "0");
      xf.setAttribute("applyFill", "1");
    }
    if (!sameBorder(s.border, origin.border)) {
      xf.setAttribute("borderId", String(push(borders, buildBorder(doc, s.border))));
      xf.setAttribute("applyBorder", "1");
    }
    if ((s.numFmt ?? null) !== (origin.numFmt ?? null)) {
      xf.setAttribute("numFmtId", String(fmtId(s.numFmt)));
      xf.setAttribute("applyNumberFormat", "1");
    }
    if (s.hAlign !== origin.hAlign || s.vAlign !== origin.vAlign || !!s.wrap !== !!origin.wrap) {
      let align = child(xf, "alignment");
      if (!align) {
        align = doc.createElementNS(NS, "alignment");
        xf.insertBefore(align, xf.firstElementChild);
      }
      const setAttr = (k: string, v?: string) => (v ? align!.setAttribute(k, v) : align!.removeAttribute(k));
      setAttr("horizontal", s.hAlign);
      setAttr("vertical", s.vAlign);
      setAttr("wrapText", s.wrap ? "1" : undefined);
      if (!align.attributes.length) xf.removeChild(align);
      xf.setAttribute("applyAlignment", "1");
    }
    push(cellXfs, xf);
  }
  zip.file(path, serialize(doc), NO_FOLDERS);
}

// ───────────── Write-back: main flow ─────────────

function rewriteMerges(doc: Document, merges: Range[]) {
  for (const el of byTag(doc, "mergeCells")) el.parentNode?.removeChild(el);
  if (!merges.length) return;
  const mc = doc.createElementNS(NS, "mergeCells");
  mc.setAttribute("count", String(merges.length));
  for (const m of merges) {
    const el = doc.createElementNS(NS, "mergeCell");
    el.setAttribute("ref", `${cellName(m.r1, m.c1)}:${cellName(m.r2, m.c2)}`);
    mc.appendChild(el);
  }
  placeInWorksheet(doc, mc);
}

function rewriteCols(doc: Document, sheet: Sheet) {
  for (const el of byTag(doc, "cols")) el.parentNode?.removeChild(el);
  const idx = new Set([...sheet.colWidths.keys(), ...sheet.colStyles.keys()]);
  if (!idx.size) return;
  const cols = doc.createElementNS(NS, "cols");
  const sorted = [...idx].sort((a, b) => a - b);
  // Merge adjacent columns with identical settings into one <col min max>
  let start = sorted[0];
  const sig = (c: number) => `${sheet.colWidths.get(c) ?? ""}|${sheet.colStyles.get(c) ?? ""}`;
  const flush = (from: number, to: number) => {
    const el = doc.createElementNS(NS, "col");
    el.setAttribute("min", String(from + 1));
    el.setAttribute("max", String(to + 1));
    const w = sheet.colWidths.get(from);
    el.setAttribute("width", String(w === undefined ? pxToWidth(sheet.defaultColWidth) : w === 0 ? pxToWidth(sheet.defaultColWidth) : pxToWidth(w)));
    if (w !== undefined) el.setAttribute("customWidth", "1");
    if (w === 0) el.setAttribute("hidden", "1");
    const st = sheet.colStyles.get(from);
    if (st) el.setAttribute("style", String(st));
    cols.appendChild(el);
  };
  for (let i = 1; i <= sorted.length; i++) {
    const prev = sorted[i - 1];
    const cur = sorted[i];
    if (cur === prev + 1 && sig(cur) === sig(start)) continue;
    flush(start, prev);
    start = cur;
  }
  placeInWorksheet(doc, cols);
}

const sameCellContent = (a: Cell | undefined, b: Cell | undefined) => {
  if ((a?.f ?? null) !== (b?.f ?? null)) return false;
  if (b?.f) return true;
  const x = a?.v ?? null;
  const y = b?.v ?? null;
  if (typeof x === "number" && typeof y === "number") return Math.abs(x - y) <= 1e-9 * Math.max(1, Math.abs(x));
  return x === y;
};

const sameScalar = (x: Scalar, y: Scalar | undefined) => {
  if (typeof x === "number" && typeof y === "number") return Math.abs(x - y) <= 1e-9 * Math.max(1, Math.abs(x));
  return x === (y ?? null);
};

/**
 * Produce the new file (modifies the zip in place).
 * ops: inserts/deletes performed since open, in order; valueOf: formula results (return undefined when not computable to keep the original cached value)
 */
export async function buildXlsx(
  zip: JSZip,
  book: Workbook,
  snapshot: Snapshot,
  ops: StructOp[],
  valueOf: (sheet: number, r: number, c: number) => Scalar | undefined,
): Promise<{ blob: Blob; cells: number }> {
  const docs = new Map<string, Document>();
  const load = async (path: string) => {
    let d = docs.get(path);
    if (!d) {
      const xml = await readEntry(zip, path, "string");
      if (!xml) return null;
      d = parseXml(xml);
      docs.set(path, d);
    }
    return d;
  };
  const touchedOps = new Set(ops.map((o) => o.sheet));
  const colOps = new Set(ops.filter((o) => !isRows(o)).map((o) => o.sheet));

  // 1. Row/column inserts and deletes
  if (ops.length) {
    for (const s of book.sheets) unshareAll((await load(s.path))!);
    const wbDoc = (await load("xl/workbook.xml"))!;
    const charts = Object.keys(zip.files).filter((p) => /^xl\/charts\/chart[^/]*\.xml$/.test(p));
    const pivots = Object.keys(zip.files).filter((p) => /^xl\/pivotCache\/pivotCacheDefinition[^/]*\.xml$/.test(p));
    for (const op of ops) {
      const target = book.sheets.find((s) => s.id === op.sheet)!;
      replayPositions((await load(target.path))!, op);
      for (const s of book.sheets) adjustFormulasIn((await load(s.path))!, s.name, target.name, op);
      for (const dn of byTag(wbDoc, "definedName")) {
        const t = dn.textContent ?? "";
        const n = adjustFormula(t, "\u0000", target.name, op);
        if (n !== t) dn.textContent = n;
      }
      for (const d of await related(zip, target.path, "drawing")) {
        const doc = await load(d);
        if (doc) adjustDrawing(doc, op);
      }
      for (const t of await related(zip, target.path, "table")) {
        const doc = await load(t);
        if (!doc) continue;
        for (const el of [doc.documentElement, ...byTag(doc, "autoFilter"), ...byTag(doc, "sortState")]) {
          const r = el.getAttribute("ref");
          const n = r && adjustSqref(r, op);
          if (n) el.setAttribute("ref", n);
        }
      }
      for (const c of charts) {
        const doc = (await load(c))!;
        for (const f of Array.from(doc.getElementsByTagName("*")).filter((e) => e.localName === "f")) {
          const t = f.textContent ?? "";
          const n = adjustFormula(t, "\u0000", target.name, op);
          if (n !== t) f.textContent = n;
        }
      }
      for (const p of pivots) {
        const doc = (await load(p))!;
        for (const src of byTag(doc, "worksheetSource")) {
          if ((src.getAttribute("sheet") ?? "").toLowerCase() !== target.name.toLowerCase()) continue;
          const r = src.getAttribute("ref");
          const n = r && adjustSqref(r, op);
          if (n) src.setAttribute("ref", n);
        }
      }
    }
  }

  // 2. Cell content and styles
  let count = 0;
  for (const [si, sheet] of book.sheets.entries()) {
    const before = snapshot.get(sheet.id)!;
    const doc = (await load(sheet.path))!;
    const index = new SheetIndex(sheetDataOf(doc));
    let touched = touchedOps.has(sheet.id);
    for (const k of new Set([...before.cells.keys(), ...sheet.cells.keys()])) {
      const a = before.cells.get(k);
      const b = sheet.cells.get(k);
      const r = rowOf(k);
      const c = colOf(k);
      const contentChanged = !sameCellContent(a, b);
      const styleChanged = (a?.s ?? 0) !== (b?.s ?? 0);
      let refresh: Scalar | undefined;
      if (!contentChanged && b?.f) {
        // Formula unchanged but its referenced data changed: update the cached result in the file
        const v = valueOf(si, r, c);
        if (v !== undefined && !sameScalar(v, a?.v ?? null)) refresh = v;
      }
      if (!contentChanged && !styleChanged && refresh === undefined) continue;
      touched = true;
      if (contentChanged || styleChanged) count++;
      if (!b) {
        const el = index.cell(r, c);
        if (el) index.removeCell(el);
        continue;
      }
      const el = index.ensureCell(r, c);
      if (b.s) el.setAttribute("s", String(b.s));
      else el.removeAttribute("s");
      if (contentChanged || refresh !== undefined) {
        const v = b.f ? (refresh ?? valueOf(si, r, c) ?? null) : b.v;
        writeContent(el, v, b.f);
      }
      if (!b.f && (b.v === null || b.v === "") && !b.s) index.removeCell(el);
    }

    // Row heights
    for (const r of new Set([...before.rowHeights.keys(), ...sheet.rowHeights.keys()])) {
      const h = sheet.rowHeights.get(r);
      if (h === before.rowHeights.get(r) && !touchedOps.has(sheet.id)) continue;
      const row = h === undefined ? index.row(r) : index.ensureRow(r);
      if (!row) continue;
      // After row/column inserts/deletes every row is checked: if the file's row (already moved by the replay) has the same height it isn't rewritten,
      // preserving Excel's auto-fitted heights (no customHeight) and the original point values
      if (heightOf(row, sheet.defaultRowHeight) === h) continue;
      touched = true;
      for (const a of ["ht", "customHeight", "hidden"]) row.removeAttribute(a);
      if (h === 0) row.setAttribute("hidden", "1");
      else if (h !== undefined) {
        row.setAttribute("ht", String(pxToPt(h)));
        row.setAttribute("customHeight", "1");
      }
    }

    const mergesChanged = JSON.stringify(sheet.merges) !== JSON.stringify(before.merges);
    if (mergesChanged || touchedOps.has(sheet.id)) {
      rewriteMerges(doc, sheet.merges);
      touched = true;
    }
    const colsChanged =
      JSON.stringify([...sheet.colWidths].sort()) !== JSON.stringify([...before.colWidths].sort()) ||
      JSON.stringify([...sheet.colStyles].sort()) !== JSON.stringify([...before.colStyles].sort());
    if (colsChanged || colOps.has(sheet.id)) {
      rewriteCols(doc, sheet);
      touched = true;
    }

    // Remove empty row elements (no cells and no row settings)
    index.pruneEmptyRows();
    const dim = byTag(doc, "dimension")[0];
    if (dim && touched) dim.setAttribute("ref", `A1:${cellName(Math.max(0, sheet.maxRow), Math.max(0, sheet.maxCol))}`);
    if (!touched && !ops.length) docs.delete(sheet.path);
  }

  // 3. New styles, write back all modified XML, and ask Excel to recalculate on open
  for (const [path, doc] of docs) zip.file(path, serialize(doc), NO_FOLDERS);
  await writeStyles(zip, book);
  await forceRecalc(zip);
  // Remove folder entries (only the entries themselves; zip.remove would also delete the files under them)
  for (const [path, entry] of Object.entries(zip.files)) if (entry.dir) delete zip.files[path];
  const blob = await zip.generateAsync({
    type: "blob",
    compression: "DEFLATE",
    mimeType: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  });
  return { blob, cells: count };
}

/** Calculation chain is stale: delete calcChain and ask Excel to fully recalculate on open */
async function forceRecalc(zip: JSZip) {
  if (zip.file("xl/calcChain.xml")) {
    zip.remove("xl/calcChain.xml");
    const rp = "xl/_rels/workbook.xml.rels";
    const rels = await readEntry(zip, rp, "string");
    if (rels) zip.file(rp, rels.replace(/<Relationship\b[^>]*calcChain\.xml"[^>]*\/>/g, ""), NO_FOLDERS);
    const ct = await readEntry(zip, "[Content_Types].xml", "string");
    if (ct) zip.file("[Content_Types].xml", ct.replace(/<Override\b[^>]*\/xl\/calcChain\.xml"[^>]*\/>/g, ""), NO_FOLDERS);
  }
  const wb = parseXml((await readEntry(zip, "xl/workbook.xml", "string")) ?? "");
  let calcPr = byTag(wb, "calcPr")[0];
  if (!calcPr) {
    calcPr = wb.createElementNS(NS, "calcPr");
    // CT_Workbook element order: calcPr comes after definedNames/externalReferences/functionGroups/sheets
    const after = ["definedNames", "externalReferences", "functionGroups", "sheets"].map((n) => byTag(wb, n)[0]).find(Boolean);
    after?.parentNode?.insertBefore(calcPr, after.nextSibling);
  }
  calcPr.setAttribute("fullCalcOnLoad", "1");
  zip.file("xl/workbook.xml", serialize(wb), NO_FOLDERS);
}
