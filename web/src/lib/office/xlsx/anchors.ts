/**
 * Positions of the drawing objects on an Excel worksheet (xl/drawings/drawingN.xml): pictures, charts, shapes, groups.
 * Only anchors are read here (cell positions + offsets), which is all the host page needs to size the sheet;
 * the objects themselves are rendered by `drawings.ts` inside the sandboxed preview frame.
 */

import { attr, emuToPx, kid, kids, numAttr, type OoxmlPackage } from "@/lib/office/ooxml";

/** Anchor: cell position (0-based) + offset (EMU), or an absolute position */
export interface Anchor {
  from?: { col: number; colOff: number; row: number; rowOff: number };
  to?: { col: number; colOff: number; row: number; rowOff: number };
  /** Size for oneCellAnchor/absoluteAnchor (EMU) */
  ext?: { cx: number; cy: number };
  /** Position for absoluteAnchor (EMU) */
  pos?: { x: number; y: number };
}

export interface DrawingItem {
  anchor: Anchor;
  /** Content element under the anchor (xdr:sp, xdr:pic, xdr:graphicFrame, xdr:grpSp, xdr:cxnSp) */
  el: Element;
  /** Part used to resolve relationships (drawingN.xml) */
  part: string;
}

/** Box computed from the anchor and column/row positions (px, relative to the top-left of the first cell) */
export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

const CONTENT = new Set(["sp", "pic", "graphicFrame", "grpSp", "cxnSp"]);
/** Namespaces required by mc:AlternateContent Choice that we can handle (chart extension cx, shape extension a14) */
const SUPPORTED_NS = new Set(["cx", "cx1", "cx2", "cx4", "a14", "c14"]);

function cellPos(el: Element | null) {
  return {
    col: Number(kid(el, "col")?.textContent ?? 0),
    colOff: Number(kid(el, "colOff")?.textContent ?? 0),
    row: Number(kid(el, "row")?.textContent ?? 0),
    rowOff: Number(kid(el, "rowOff")?.textContent ?? 0),
  };
}

/** Extract the content to use from AlternateContent */
function pickAlternate(el: Element): Element[] {
  for (const choice of kids(el, "Choice")) {
    const req = (attr(choice, "Requires") ?? "").split(/\s+/).filter(Boolean);
    if (req.every((r) => SUPPORTED_NS.has(r))) return kids(choice);
  }
  return kids(kid(el, "Fallback"));
}

/** The worksheet's drawing objects (in file order, which is the z-order) */
export async function readDrawings(pkg: OoxmlPackage, sheetPath: string): Promise<DrawingItem[]> {
  const rel = await pkg.relOfType(sheetPath, "/drawing");
  if (!rel) return [];
  const doc = await pkg.xml(rel.target);
  const root = doc?.documentElement;
  if (!root) return [];
  const items: DrawingItem[] = [];
  const visit = (anchors: Element[]) => {
    for (const a of anchors) {
      if (a.localName === "AlternateContent") {
        visit(pickAlternate(a));
        continue;
      }
      const anchor: Anchor = {};
      if (a.localName === "twoCellAnchor") {
        anchor.from = cellPos(kid(a, "from"));
        anchor.to = cellPos(kid(a, "to"));
      } else if (a.localName === "oneCellAnchor") {
        anchor.from = cellPos(kid(a, "from"));
        anchor.ext = { cx: numAttr(kid(a, "ext"), "cx") ?? 0, cy: numAttr(kid(a, "ext"), "cy") ?? 0 };
      } else if (a.localName === "absoluteAnchor") {
        anchor.pos = { x: numAttr(kid(a, "pos"), "x") ?? 0, y: numAttr(kid(a, "pos"), "y") ?? 0 };
        anchor.ext = { cx: numAttr(kid(a, "ext"), "cx") ?? 0, cy: numAttr(kid(a, "ext"), "cy") ?? 0 };
      } else continue;
      let content = kids(a).find((c) => CONTENT.has(c.localName));
      // The content itself may also be wrapped in AlternateContent (newer charts, slicers)
      if (!content) {
        const alt = kids(a, "AlternateContent")[0];
        content = alt ? pickAlternate(alt).find((c) => CONTENT.has(c.localName)) : undefined;
      }
      if (content) items.push({ anchor, el: content, part: rel.target });
    }
  };
  visit(kids(root));
  return items;
}

/** Anchor → box; colOffset/rowOffset give the start (px) of the n-th column (row) */
export function anchorBox(a: Anchor, colOffset: (c: number) => number, rowOffset: (r: number) => number): Box {
  if (a.pos && a.ext) return { x: emuToPx(a.pos.x), y: emuToPx(a.pos.y), w: emuToPx(a.ext.cx), h: emuToPx(a.ext.cy) };
  const f = a.from ?? { col: 0, colOff: 0, row: 0, rowOff: 0 };
  const x = colOffset(f.col) + emuToPx(f.colOff);
  const y = rowOffset(f.row) + emuToPx(f.rowOff);
  if (a.to) {
    const x2 = colOffset(a.to.col) + emuToPx(a.to.colOff);
    const y2 = rowOffset(a.to.row) + emuToPx(a.to.rowOff);
    return { x, y, w: Math.max(0, x2 - x), h: Math.max(0, y2 - y) };
  }
  return { x, y, w: emuToPx(a.ext?.cx ?? 0), h: emuToPx(a.ext?.cy ?? 0) };
}
