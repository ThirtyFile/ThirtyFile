/**
 * Changing a Word table's shape (the editor's Insert and Delete menus): rows above or below, columns to the left or
 * right, deleting the row or column of a cell. The table is changed in place in its document.
 *
 * Done the way Word does it:
 * - A new row copies the row next to it: its settings (height, header row) and its cells' settings (widths, spans,
 *   shading, borders), with an empty paragraph in each cell. Inside a vertically merged cell the new row extends it.
 * - A new column takes the width of the column next to it, and a table of fixed width grows by that much. A cell that
 *   spans the place of the new column spans it too.
 * - Deleting a column narrows the cells partly in it; deleting the first row of a vertically merged cell hands the
 *   merge to the row below. Deleting the last row or column deletes the table.
 * - Merging cells grows the range until no merged cell crosses its edge, and moves the text of every cell into the
 *   first one (empty cells add nothing).
 * - Splitting a cell into columns gives the table's grid the new column edges, so the other rows keep lining up, their
 *   cells spanning the new columns. Splitting into rows regroups a cell merged across rows, or adds rows below a single
 *   cell, in which the row's other cells stay one (merged down). The text stays in the first cell.
 *
 * Rows and cells are found by their place in the table's grid (gridBefore, gridSpan). A table this can't follow (rows or
 * cells inside content controls, say) isn't changed at all.
 */

import { attr, kid } from "../core/package";

export type TableOp = "rowAbove" | "rowBelow" | "colLeft" | "colRight" | "deleteRow" | "deleteCol" | "merge" | "split";

/** What merging and splitting need: the other corner of the cells to merge, and how many columns and rows to split into */
export interface TableOpOptions {
  other?: Element;
  cols?: number;
  rows?: number;
}

/** Most columns and rows a cell is split into */
export const MAX_SPLIT = { cols: 20, rows: 50 };

const W14 = "http://schemas.microsoft.com/office/word/2010/wordml";
/** Row content that isn't a cell but may sit in a row; anything else makes the table one this doesn't change */
const ROW_EXTRAS = new Set(["tblPrEx", "trPr", "bookmarkStart", "bookmarkEnd", "proofErr", "permStart", "permEnd"]);
/** The order Word needs a cell's settings in (CT_TcPr) */
const TCPR = [
  "cnfStyle",
  "tcW",
  "gridSpan",
  "hMerge",
  "vMerge",
  "tcBorders",
  "shd",
  "noWrap",
  "tcMar",
  "textDirection",
  "tcFitText",
  "vAlign",
  "hideMark",
  "headers",
  "cellIns",
  "cellDel",
  "cellMerge",
  "tcPrChange",
];
/** Settings about tracked changes, not looks: a new row or cell doesn't copy them */
const TRACKING = new Set(["ins", "del", "cellIns", "cellDel", "cellMerge", "trPrChange", "tcPrChange"]);

interface GridCell {
  tc: Element;
  start: number;
  span: number;
}

interface Row {
  tr: Element;
  cells: GridCell[];
  /** Grid columns before the first cell and after the last */
  before: number;
  after: number;
}

class Tables {
  readonly ns: string;
  readonly prefix: string | null;
  constructor(
    readonly doc: Document,
    sample: Element,
  ) {
    this.ns = sample.namespaceURI ?? "";
    this.prefix = sample.prefix;
  }

  el(name: string, val?: string | number) {
    const e = this.doc.createElementNS(this.ns, this.prefix ? `${this.prefix}:${name}` : name);
    if (val !== undefined) this.set(e, "val", String(val));
    return e;
  }

  /** Sets a w: attribute (an existing one by its local name, for a parser that keeps prefixes in names) */
  set(e: Element, name: string, value: string) {
    const existing = Array.from(e.attributes).find((a) => a.localName === name || a.name.endsWith(`:${name}`));
    if (existing) existing.value = value;
    else e.setAttributeNS(this.ns, this.prefix ? `${this.prefix}:${name}` : name, value);
  }

  /** A cell's settings, made (its first child) when missing */
  tcPr(tc: Element) {
    const found = kid(tc, "tcPr");
    if (found) return found;
    const made = this.el("tcPr");
    tc.prepend(made);
    return made;
  }

  /** The setting `name` of a cell, made in its place among the others when missing */
  cellProp(tc: Element, name: string) {
    const tcPr = this.tcPr(tc);
    return kid(tcPr, name) ?? placeIn(tcPr, this.el(name), TCPR);
  }

  /** The setting `name` of a row, made when missing (a row's settings come after its table exceptions, before its cells) */
  rowProp(tr: Element, name: string) {
    let trPr = kid(tr, "trPr");
    if (!trPr) {
      trPr = this.el("trPr");
      const ex = kid(tr, "tblPrEx");
      if (ex) ex.after(trPr);
      else tr.prepend(trPr);
    }
    return kid(trPr, name) ?? trPr.appendChild(this.el(name));
  }
}

/** Puts `child` into `parent` where `order` says: before the first child that comes after it */
function placeIn(parent: Element, child: Element, order: string[]): Element {
  const i = order.indexOf(child.localName);
  const later = Array.from(parent.children).find((c) => order.indexOf(c.localName) > i);
  if (later) parent.insertBefore(child, later);
  else parent.append(child);
  return child;
}

const num = (el: Element | null, name = "val") => {
  const v = Number(attr(el, name));
  return Number.isFinite(v) ? v : null;
};
const spanOf = (tc: Element) => Math.max(1, num(kid(kid(tc, "tcPr"), "gridSpan")) ?? 1);
/** A cell's vertical merge: "restart" (its first cell), "continue", or null */
function vMergeOf(tc: Element): "restart" | "continue" | null {
  const v = kid(kid(tc, "tcPr"), "vMerge");
  if (!v) return null;
  return attr(v, "val") === "restart" ? "restart" : "continue";
}

function readRow(tr: Element): Row | null {
  const trPr = kid(tr, "trPr");
  const before = num(kid(trPr, "gridBefore")) ?? 0;
  const after = num(kid(trPr, "gridAfter")) ?? 0;
  const cells: GridCell[] = [];
  let col = before;
  for (const c of Array.from(tr.children)) {
    if (c.localName === "tc") {
      const span = spanOf(c);
      cells.push({ tc: c, start: col, span });
      col += span;
    } else if (!ROW_EXTRAS.has(c.localName)) return null;
  }
  return { tr, cells, before, after };
}

function readTable(tbl: Element): Row[] | null {
  const rows: Row[] = [];
  for (const c of Array.from(tbl.children)) {
    if (c.localName === "tr") {
      const row = readRow(c);
      if (!row) return null;
      rows.push(row);
    } else if (!["tblPr", "tblGrid", "bookmarkStart", "bookmarkEnd", "proofErr", "permStart", "permEnd"].includes(c.localName)) return null;
  }
  return rows;
}

const cellAt = (row: Row | undefined, start: number) => row?.cells.find((c) => c.start === start) ?? null;
const cellOver = (row: Row | undefined, col: number) => row?.cells.find((c) => c.start <= col && col < c.start + c.span) ?? null;

function dropIds(e: Element) {
  for (const a of Array.from(e.attributes)) if ((a.namespaceURI === W14 || a.name.startsWith("w14:")) && /(?:^|:)(?:paraId|textId)$/.test(a.name)) e.removeAttributeNode(a);
}

/** A copy of a properties element without its tracked-change settings and the given children */
function copyProps(props: Element | null, without: string[] = []): Element | null {
  if (!props) return null;
  const copy = props.cloneNode(true) as Element;
  for (const c of Array.from(copy.children)) if (TRACKING.has(c.localName) || without.includes(c.localName)) c.remove();
  return copy;
}

/** An empty cell like `like` (its settings, and its first paragraph's), merged vertically as `vMerge` says */
function emptyCell(t: Tables, like: Element, vMerge: "continue" | null, drop: string[] = []): Element {
  const tc = t.el("tc");
  const tcPr = copyProps(kid(like, "tcPr"), ["vMerge", "hMerge", ...drop]) ?? t.el("tcPr");
  if (vMerge) placeIn(tcPr, t.el("vMerge"), TCPR);
  tc.append(tcPr);
  const p = t.el("p");
  const pPr = copyProps(kid(kid(like, "p"), "pPr"), ["sectPr"]);
  if (pPr) p.append(pPr);
  tc.append(p);
  return tc;
}

/** A table removed from its place: the place keeps the paragraph Word needs after a table that ended it */
function removeTable(t: Tables, tbl: Element) {
  const parent = tbl.parentElement;
  tbl.remove();
  if (!parent) return;
  const blocks = Array.from(parent.children).filter((c) => !["sectPr", "tcPr"].includes(c.localName));
  if (!blocks.length || blocks[blocks.length - 1].localName !== "p") {
    const sect = Array.from(parent.children).find((c) => c.localName === "sectPr" && c.parentElement === parent && c === parent.lastElementChild);
    const p = t.el("p");
    if (sect) parent.insertBefore(p, sect);
    else parent.append(p);
  }
}

function gridCols(tbl: Element): Element[] {
  return Array.from(kid(tbl, "tblGrid")?.children ?? []).filter((c) => c.localName === "gridCol");
}

/** A fixed table width (twips) grows or shrinks with its columns; a percentage or automatic width stays as it is */
function changeWidth(t: Tables, tbl: Element, by: number) {
  const w = kid(kid(tbl, "tblPr"), "tblW");
  const type = attr(w, "type");
  const v = num(w, "w");
  if (w && v !== null && (type === "dxa" || type === null) && v > 0) t.set(w, "w", String(Math.max(0, Math.round(v + by))));
}

/**
 * Changes the table of cell `tc` as `op` says. Returns the cell to put the cursor in afterwards (null when the table
 * went), or undefined when the table can't be changed this way (it is then left as it was).
 */
export function changeTable(tc: Element, op: TableOp, opts: TableOpOptions = {}): Element | null | undefined {
  const tr = tc.parentElement;
  const tbl = tr?.parentElement;
  if (!tr || tr.localName !== "tr" || !tbl || tbl.localName !== "tbl" || !tc.ownerDocument) return undefined;
  const rows = readTable(tbl);
  if (!rows) return undefined;
  const r = rows.findIndex((x) => x.tr === tr);
  const cur = rows[r]?.cells.find((c) => c.tc === tc);
  if (!cur) return undefined;
  const t = new Tables(tc.ownerDocument, tc);
  switch (op) {
    case "rowAbove":
    case "rowBelow":
      return insertRow(t, rows, r, cur, op === "rowBelow");
    case "deleteRow":
      return deleteRow(t, tbl, rows, r, cur);
    case "colLeft":
    case "colRight":
      return insertColumn(t, tbl, rows, r, cur, op === "colRight");
    case "deleteCol":
      return deleteColumn(t, tbl, rows, r, cur);
    case "merge":
      return opts.other ? mergeCells(t, rows, tc, opts.other) : undefined;
    case "split":
      return splitCell(t, tbl, rows, r, cur, opts.cols ?? 1, opts.rows ?? 1);
  }
}

// ───────────── Ranges of cells ─────────────

interface Range {
  r1: number;
  r2: number;
  /** Grid columns, both included */
  c1: number;
  c2: number;
}

/** The rows a cell is merged across: from the row starting the merge to the last one continuing it */
function rowsOf(rows: Row[], r: number, cell: GridCell): [number, number] {
  let top = r;
  if (vMergeOf(cell.tc) === "continue") {
    while (top > 0) {
      const up = cellAt(rows[top - 1], cell.start);
      if (!up || !vMergeOf(up.tc)) break;
      top--;
      if (vMergeOf(up.tc) === "restart") break;
    }
  }
  let bottom = r;
  if (vMergeOf(cell.tc)) {
    while (bottom + 1 < rows.length) {
      const down = cellAt(rows[bottom + 1], cell.start);
      if (!down || vMergeOf(down.tc) !== "continue") break;
      bottom++;
    }
  }
  return [top, bottom];
}

const rowOf = (rows: Row[], tc: Element) => rows.findIndex((x) => x.cells.some((c) => c.tc === tc));

/** A range grown until no merged cell crosses its edge; undefined when a row has no cell somewhere in it */
function grow(rows: Row[], range: Range): Range | undefined {
  let { r1, r2, c1, c2 } = range;
  for (let changed = true; changed;) {
    changed = false;
    for (let r = r1; r <= r2; r++) {
      for (const cell of rows[r].cells) {
        const end = cell.start + cell.span - 1;
        if (end < c1 || cell.start > c2) continue;
        const [v1, v2] = rowsOf(rows, r, cell);
        if (cell.start < c1 || end > c2 || v1 < r1 || v2 > r2) changed = true;
        c1 = Math.min(c1, cell.start);
        c2 = Math.max(c2, end);
        r1 = Math.min(r1, v1);
        r2 = Math.max(r2, v2);
      }
    }
  }
  // Every row of the range has cells over all of its columns
  for (let r = r1; r <= r2; r++) {
    const inside = rows[r].cells.filter((c) => c.start + c.span - 1 >= c1 && c.start <= c2);
    if (!inside.length || inside[0].start !== c1 || inside.reduce((n, c) => n + c.span, 0) !== c2 - c1 + 1) return undefined;
  }
  return { r1, r2, c1, c2 };
}

/** The range of cells from one corner to the other, grown as merging would; undefined when they aren't in the same table */
function rangeBetween(rows: Row[], a: Element, b: Element): Range | undefined {
  const ra = rowOf(rows, a);
  const rb = rowOf(rows, b);
  if (ra < 0 || rb < 0) return undefined;
  const ca = rows[ra].cells.find((c) => c.tc === a)!;
  const cb = rows[rb].cells.find((c) => c.tc === b)!;
  return grow(rows, {
    r1: Math.min(ra, rb),
    r2: Math.max(ra, rb),
    c1: Math.min(ca.start, cb.start),
    c2: Math.max(ca.start + ca.span, cb.start + cb.span) - 1,
  });
}

/** The cells from one corner to the other, grown as merging would (cells merged into others included) */
export function cellRange(a: Element, b: Element): Element[] | undefined {
  const tbl = a.parentElement?.parentElement;
  if (!tbl || tbl.localName !== "tbl" || b.parentElement?.parentElement !== tbl) return undefined;
  const rows = readTable(tbl);
  const range = rows && rangeBetween(rows, a, b);
  if (!rows || !range) return undefined;
  const out: Element[] = [];
  for (let r = range.r1; r <= range.r2; r++) for (const c of rows[r].cells) if (c.start >= range.c1 && c.start <= range.c2) out.push(c.tc);
  return out;
}

/** Whether a cell holds nothing: paragraphs without text, pictures or tables */
const isEmpty = (tc: Element) =>
  !Array.from(tc.querySelectorAll("*")).some((e) => ["t", "drawing", "pict", "object", "tbl", "sym", "oMath"].includes(e.localName) && (e.localName !== "t" || !!e.textContent));
const blocksOf = (tc: Element) => Array.from(tc.children).filter((c) => c.localName !== "tcPr");

/** The width (twips) of grid columns c1..c2 */
function widthOf(tbl: Element, c1: number, c2: number) {
  return gridCols(tbl)
    .slice(c1, c2 + 1)
    .reduce((n, g) => n + (num(g, "w") ?? 0), 0);
}

/** A cell's span, width and vertical merge set */
function shape(t: Tables, tc: Element, span: number, width: number, vMerge: "restart" | "continue" | null) {
  const tcPr = t.tcPr(tc);
  if (span > 1) t.set(t.cellProp(tc, "gridSpan"), "val", String(span));
  else kid(tcPr, "gridSpan")?.remove();
  if (width > 0) {
    const tcW = t.cellProp(tc, "tcW");
    t.set(tcW, "w", String(Math.round(width)));
    t.set(tcW, "type", "dxa");
  }
  kid(tcPr, "vMerge")?.remove();
  kid(tcPr, "hMerge")?.remove();
  if (vMerge) {
    const v = placeIn(tcPr, t.el("vMerge"), TCPR);
    if (vMerge === "restart") t.set(v, "val", "restart");
  }
}

/** A cell's content replaced by one empty paragraph (like its first one) */
function clearCell(t: Tables, tc: Element) {
  const p = t.el("p");
  const pPr = copyProps(kid(kid(tc, "p"), "pPr"), ["sectPr"]);
  if (pPr) p.append(pPr);
  for (const b of blocksOf(tc)) b.remove();
  tc.append(p);
}

function mergeCells(t: Tables, rows: Row[], a: Element, b: Element): Element | undefined {
  const range = rangeBetween(rows, a, b);
  if (!range) return undefined;
  const { r1, r2, c1, c2 } = range;
  const tbl = rows[r1].tr.parentElement!;
  const width = widthOf(tbl, c1, c2);
  const inRange = (r: number) => rows[r].cells.filter((c) => c.start >= c1 && c.start <= c2);
  const target = inRange(r1)[0].tc;
  // The text of every cell, in reading order, goes into the first one (cells merged into others hold none of their own)
  const moved: Element[] = [];
  for (let r = r1; r <= r2; r++) for (const c of inRange(r)) if (c.tc !== target && vMergeOf(c.tc) !== "continue" && !isEmpty(c.tc)) moved.push(...blocksOf(c.tc));
  if (moved.length) {
    if (isEmpty(target)) for (const blk of blocksOf(target)) blk.remove();
    target.append(...moved);
  }
  for (let r = r1; r <= r2; r++) {
    const [first, ...rest] = inRange(r);
    for (const c of rest) c.tc.remove();
    if (r === r1) shape(t, target, c2 - c1 + 1, width, r2 > r1 ? "restart" : null);
    else {
      clearCell(t, first.tc);
      shape(t, first.tc, c2 - c1 + 1, width, "continue");
    }
  }
  return target;
}

/** Twips from the table's left edge to each grid column edge (the first is 0) */
function edges(tbl: Element): number[] | undefined {
  const cols = gridCols(tbl);
  const out = [0];
  for (const c of cols) {
    const w = num(c, "w");
    if (w === null || w < 0) return undefined;
    out.push(out[out.length - 1] + w);
  }
  return out;
}

function splitCell(t: Tables, tbl: Element, rows: Row[], r: number, cur: GridCell, cols: number, splitRows: number): Element | undefined {
  if (!(cols >= 1 && cols <= MAX_SPLIT.cols && splitRows >= 1 && splitRows <= MAX_SPLIT.rows) || (cols === 1 && splitRows === 1)) return undefined;
  // The rows the cell is merged across: splitting into rows regroups them, and must share them out evenly
  const [g1, g2] = rowsOf(rows, r, cur);
  const merged = g2 - g1 + 1;
  if (merged > 1 && merged % splitRows !== 0) return undefined;
  const group = () => Array.from({ length: g2 - g1 + 1 }, (_, i) => cellAt(rows[g1 + i], cur.start)!);
  if (group().some((c) => !c || c.span !== cur.span)) return undefined;
  let parts: Element[][] = group().map((c) => [c.tc]);

  if (cols > 1) {
    const old = edges(tbl);
    const width = Math.max(...rows.map((x) => x.before + x.cells.reduce((n, c) => n + c.span, 0) + x.after));
    if (!old || old.length - 1 < width) return undefined;
    // The new grid: the old column edges, and those of the new columns
    const x1 = old[cur.start];
    const x2 = old[cur.start + cur.span];
    const cuts = Array.from({ length: cols - 1 }, (_, i) => Math.round(x1 + ((x2 - x1) * (i + 1)) / cols));
    // A cell too narrow for that many columns isn't split (nothing has changed yet)
    if ([x1, ...cuts, x2].some((x, i, all) => i > 0 && x <= all[i - 1])) return undefined;
    const grid = [...new Set([...old, ...cuts])].sort((a, b) => a - b);
    const at = (x: number) => grid.indexOf(x);
    // Every cell, and every row's empty columns, span the new grid columns between their edges
    for (const row of rows) {
      for (const c of row.cells) {
        const span = at(old[c.start + c.span]) - at(old[c.start]);
        if (span > 1) t.set(t.cellProp(c.tc, "gridSpan"), "val", String(span));
        else kid(kid(c.tc, "tcPr"), "gridSpan")?.remove();
      }
      if (row.before) t.set(t.rowProp(row.tr, "gridBefore"), "val", String(at(old[row.before])));
      if (row.after) t.set(t.rowProp(row.tr, "gridAfter"), "val", String(grid.length - 1 - at(old[old.length - 1 - row.after])));
    }
    // The cell becomes `cols` cells, in each row it is merged across
    const bounds = [x1, ...cuts, x2];
    parts = group().map((c, i) => {
      const out = [c.tc];
      for (let k = 1; k < cols; k++) {
        const tc = emptyCell(t, c.tc, null, ["gridSpan", "tcW"]);
        out[out.length - 1].after(tc);
        out.push(tc);
      }
      out.forEach((tc, k) => shape(t, tc, at(bounds[k + 1]) - at(bounds[k]), bounds[k + 1] - bounds[k], merged > 1 ? (i === 0 ? "restart" : "continue") : null));
      return out;
    });
    const tblGrid = kid(tbl, "tblGrid")!;
    for (const g of gridCols(tbl)) g.remove();
    for (let i = 1; i < grid.length; i++) {
      const g = t.el("gridCol");
      t.set(g, "w", String(grid[i] - grid[i - 1]));
      tblGrid.append(g);
    }
  }

  if (splitRows > 1) {
    if (merged > 1) {
      // Merged across rows: regrouped, each group starting a merge of its own (or not merged, one row each)
      const each = merged / splitRows;
      parts.forEach((row, i) => {
        for (const tc of row) {
          const v = kid(kid(tc, "tcPr"), "vMerge");
          if (each === 1) v?.remove();
          else if (i % each === 0) {
            const made = v ?? placeIn(t.tcPr(tc), t.el("vMerge"), TCPR);
            t.set(made, "val", "restart");
            if (i > 0) clearCell(t, tc);
          }
        }
      });
    } else {
      // A single row: new rows below it, in which the split cell's parts are cells of their own and the row's other
      // cells stay one with the row's (merged down)
      const row = rows[r];
      const mine = new Set(parts[0]);
      const all = Array.from(row.tr.children).filter((c) => c.localName === "tc");
      for (const tc of all) {
        if (mine.has(tc) || vMergeOf(tc)) continue;
        const v = placeIn(t.tcPr(tc), t.el("vMerge"), TCPR);
        t.set(v, "val", "restart");
      }
      let last = row.tr;
      for (let k = 1; k < splitRows; k++) {
        const tr = row.tr.cloneNode(false) as Element;
        dropIds(tr);
        for (const c of Array.from(row.tr.children)) if (c.localName === "tblPrEx" || c.localName === "trPr") tr.append(copyProps(c)!);
        for (const tc of all) tr.append(emptyCell(t, tc, mine.has(tc) ? null : "continue"));
        last.after(tr);
        last = tr;
      }
    }
  }
  return parts[0][0];
}

function insertRow(t: Tables, rows: Row[], r: number, cur: GridCell, below: boolean): Element {
  const ref = rows[r];
  const neighbour = rows[below ? r + 1 : r - 1];
  const row = ref.tr.cloneNode(false) as Element;
  dropIds(row);
  for (const c of Array.from(ref.tr.children)) {
    if (c.localName === "tblPrEx") row.append(c.cloneNode(true));
    else if (c.localName === "trPr") row.append(copyProps(c)!);
  }
  let focus: Element | null = null;
  for (const cell of ref.cells) {
    // Inside a vertically merged cell (the row on the other side continues it) the new row continues it too
    const merge = vMergeOf(cell.tc);
    const across = cellAt(neighbour, cell.start);
    const inside = below ? merge !== null && !!across && vMergeOf(across.tc) === "continue" : merge === "continue";
    const tc = emptyCell(t, cell.tc, inside ? "continue" : null);
    row.append(tc);
    if (cell === cur) focus = tc;
  }
  if (below) ref.tr.after(row);
  else ref.tr.before(row);
  return focus!;
}

function deleteRow(t: Tables, tbl: Element, rows: Row[], r: number, cur: GridCell): Element | null {
  if (rows.length === 1) {
    removeTable(t, tbl);
    return null;
  }
  const row = rows[r];
  const next = rows[r + 1];
  // The first row of a vertically merged cell goes: the row below starts the merge
  for (const cell of row.cells) {
    if (vMergeOf(cell.tc) !== "restart") continue;
    const below = cellAt(next, cell.start);
    if (below && vMergeOf(below.tc) === "continue") t.set(kid(kid(below.tc, "tcPr"), "vMerge")!, "val", "restart");
  }
  row.tr.remove();
  const stay = next ?? rows[r - 1];
  return (cellOver(stay, cur.start) ?? stay.cells[0])?.tc ?? null;
}

function insertColumn(t: Tables, tbl: Element, rows: Row[], r: number, cur: GridCell, right: boolean): Element | null {
  const at = right ? cur.start + cur.span : cur.start;
  const refCol = right ? at - 1 : at;
  const cols = gridCols(tbl);
  const width = num(cols[refCol] ?? null, "w") ?? num(cols[cols.length - 1] ?? null, "w") ?? 1440;
  let focus: Element | null = null;
  for (const [i, row] of rows.entries()) {
    const startsHere = cellAt(row, at);
    const spanning = row.cells.find((c) => c.start < at && at < c.start + c.span);
    const like = cellOver(row, refCol) ?? cellOver(row, at) ?? row.cells[row.cells.length - 1];
    if (spanning) {
      // A cell over the new column's place spans it too
      t.set(t.cellProp(spanning.tc, "gridSpan"), "val", String(spanning.span + 1));
      if (i === r) focus = spanning.tc;
      continue;
    }
    if (!startsHere && at < row.before) {
      // The place is in the row's leading empty columns: they get one more
      t.set(t.rowProp(row.tr, "gridBefore"), "val", String(row.before + 1));
      continue;
    }
    if (!like) continue;
    const tc = emptyCell(t, like.tc, null, ["gridSpan"]);
    const tcW = t.cellProp(tc, "tcW");
    t.set(tcW, "w", String(width));
    t.set(tcW, "type", "dxa");
    if (startsHere) startsHere.tc.before(tc);
    else {
      const last = row.cells[row.cells.length - 1];
      if (last) last.tc.after(tc);
      else row.tr.append(tc);
    }
    if (i === r) focus = tc;
  }
  const grid = kid(tbl, "tblGrid");
  if (grid) {
    const col = t.el("gridCol");
    t.set(col, "w", String(width));
    const before = cols[at];
    if (before) before.before(col);
    else grid.append(col);
  }
  changeWidth(t, tbl, width);
  return focus;
}

function deleteColumn(t: Tables, tbl: Element, rows: Row[], r: number, cur: GridCell): Element | null {
  const a = cur.start;
  const b = cur.start + cur.span;
  const cols = gridCols(tbl);
  // Deleting every column of the table deletes the table
  if (a === 0 && b >= Math.max(cols.length, ...rows.map((x) => x.before + x.cells.reduce((n, c) => n + c.span, 0) + x.after))) {
    removeTable(t, tbl);
    return null;
  }
  let focus: Element | null = null;
  for (const [i, row] of rows.entries()) {
    for (const cell of row.cells) {
      const overlap = Math.min(b, cell.start + cell.span) - Math.max(a, cell.start);
      if (overlap <= 0) continue;
      if (overlap >= cell.span) cell.tc.remove();
      else {
        const span = cell.span - overlap;
        const gs = kid(kid(cell.tc, "tcPr"), "gridSpan")!;
        if (span > 1) t.set(gs, "val", String(span));
        else gs.remove();
      }
    }
    // Empty columns before the first cell that were in the deleted ones
    const lead = Math.min(b, row.before) - Math.max(a, 0);
    if (lead > 0) t.set(kid(kid(row.tr, "trPr"), "gridBefore")!, "val", String(row.before - lead));
    const left = row.cells.filter((c) => c.tc.parentElement === row.tr);
    if (!left.length) row.tr.remove();
    else if (i === r) focus = (left.find((c) => c.start >= b) ?? left[left.length - 1]).tc;
  }
  const removed = cols.slice(a, b);
  changeWidth(t, tbl, -removed.reduce((n, c) => n + (num(c, "w") ?? 0), 0));
  for (const c of removed) c.remove();
  if (!Array.from(tbl.children).some((c) => c.localName === "tr")) {
    removeTable(t, tbl);
    return null;
  }
  return focus;
}
