/**
 * Formatting text and paragraphs in a Word document (the editor's formatting toolbar).
 *
 * - Character formatting the view changed (bold, italic, underline, strikethrough, color, highlight) is written into a
 *   run's properties as what differs from what it had (`applyFmt`): turning off what came from a style writes it off, so
 *   Word shows what the editor showed. A highlight in one of Word's highlight colors is a highlight; any other color is
 *   shading.
 * - Font size, paragraph style and alignment are applied to a range of the document marked by two characters the editor
 *   puts at the selection's ends (`formatRange`): runs are split at the marks, the runs between them get the size, the
 *   paragraphs they touch the style or alignment, and the marks go.
 */

import { kid } from "../core/package";
import { placeIn, WordXml } from "./xmlEdit";

/** Character formatting differing from what a run had (a color is "RRGGBB"; a highlight null when taken off) */
export interface Fmt {
  b?: boolean;
  i?: boolean;
  u?: boolean;
  s?: boolean;
  color?: string;
  hl?: string | null;
}

/** The characters the editor marks a selection's start and end with (Unicode private use, so never in a document's text) */
export const MARK_START = String.fromCharCode(0xe000);
export const MARK_END = String.fromCharCode(0xe001);
const MARKS = new RegExp(`[${MARK_START}${MARK_END}]`, "g");
const HAS_MARK = new RegExp(`[${MARK_START}${MARK_END}]`);

/** Word's highlight colors (w:highlight), by their hex color */
export const HIGHLIGHTS: Record<string, string> = {
  FFFF00: "yellow",
  "00FF00": "green",
  "00FFFF": "cyan",
  FF00FF: "magenta",
  "0000FF": "blue",
  FF0000: "red",
  "000080": "darkBlue",
  "008080": "darkCyan",
  "008000": "darkGreen",
  "800080": "darkMagenta",
  "800000": "darkRed",
  "808000": "darkYellow",
  "808080": "darkGray",
  C0C0C0: "lightGray",
  "000000": "black",
};

/** The order Word needs a run's properties in (CT_RPr) */
const RPR = [
  "rStyle",
  "rFonts",
  "b",
  "bCs",
  "i",
  "iCs",
  "caps",
  "smallCaps",
  "strike",
  "dstrike",
  "outline",
  "shadow",
  "emboss",
  "imprint",
  "noProof",
  "snapToGrid",
  "vanish",
  "webHidden",
  "color",
  "spacing",
  "w",
  "kern",
  "position",
  "sz",
  "szCs",
  "highlight",
  "u",
  "effect",
  "bdr",
  "shd",
  "fitText",
  "vertAlign",
  "rtl",
  "cs",
  "em",
  "lang",
  "eastAsianLayout",
  "specVanish",
  "oMath",
  "rPrChange",
];
/** The order Word needs a paragraph's properties in (CT_PPr) */
const PPR = [
  "pStyle",
  "keepNext",
  "keepLines",
  "pageBreakBefore",
  "framePr",
  "widowControl",
  "numPr",
  "suppressLineNumbers",
  "pBdr",
  "shd",
  "tabs",
  "suppressAutoHyphens",
  "kinsoku",
  "wordWrap",
  "overflowPunct",
  "topLinePunct",
  "autoSpaceDE",
  "autoSpaceDN",
  "bidi",
  "adjustRightInd",
  "snapToGrid",
  "spacing",
  "ind",
  "contextualSpacing",
  "mirrorIndents",
  "suppressOverlap",
  "jc",
  "textDirection",
  "textAlignment",
  "textboxTightWrap",
  "outlineLvl",
  "divId",
  "cnfStyle",
  "rPr",
  "sectPr",
  "pPrChange",
];

/** A properties element's child `name`, made in its place when missing, its attributes cleared to be set afresh */
function prop(x: WordXml, props: Element, name: string, order: string[]) {
  return x.clear(kid(props, name) ?? placeIn(props, x.el(name), order));
}

/** A run's (or paragraph mark's) properties element, made when missing (it comes first) */
export function rPrOf(x: WordXml, r: Element): Element {
  const found = kid(r, "rPr");
  if (found) return found;
  const made = x.el("rPr");
  r.prepend(made);
  return made;
}

/** A paragraph's properties element, made when missing (it comes first) */
function pPrOf(x: WordXml, p: Element): Element {
  const found = kid(p, "pPr");
  if (found) return found;
  const made = x.el("pPr");
  p.prepend(made);
  return made;
}

/** Character formatting written into a run's properties: on, or off where a style or the run had it on */
export function applyFmt(x: WordXml, rPr: Element, fmt: Fmt) {
  const toggle = (names: string[], on: boolean) => {
    for (const n of names) {
      const el = prop(x, rPr, n, RPR);
      if (!on) x.set(el, "val", "0");
    }
  };
  if (fmt.b !== undefined) toggle(["b", "bCs"], fmt.b);
  if (fmt.i !== undefined) toggle(["i", "iCs"], fmt.i);
  if (fmt.s !== undefined) {
    toggle(["strike"], fmt.s);
    kid(rPr, "dstrike")?.remove();
  }
  if (fmt.u !== undefined) x.set(prop(x, rPr, "u", RPR), "val", fmt.u ? "single" : "none");
  if (fmt.color !== undefined) x.set(prop(x, rPr, "color", RPR), "val", fmt.color.toUpperCase());
  if (fmt.hl !== undefined) {
    const hex = fmt.hl?.toUpperCase() ?? null;
    const name = hex ? HIGHLIGHTS[hex] : "none";
    kid(rPr, "shd")?.remove();
    kid(rPr, "highlight")?.remove();
    if (name) x.set(prop(x, rPr, "highlight", RPR), "val", name);
    else {
      const shd = prop(x, rPr, "shd", RPR);
      x.set(shd, "val", "clear");
      x.set(shd, "color", "auto");
      x.set(shd, "fill", hex!);
    }
  }
}

/** A change of the marked range: font size (points), paragraph style (id), or alignment (left, center, right, both) */
export type RangeFormat = { size: number } | { style: string; isDefault?: boolean } | { align: "left" | "center" | "right" | "both" };

const byName = (root: Element, name: string) => Array.from(root.querySelectorAll("*")).filter((e) => e.localName === name);
/** The paragraph holding an element */
function paragraphOf(el: Element | null): Element | null {
  for (let e = el; e; e = e.parentElement) if (e.localName === "p") return e;
  return null;
}

/**
 * Splits the run of text element `t` where character `k` of it is (taking that character, a mark, out). Returns the
 * first run after the split, or null when the split is at the run's end.
 */
function splitAt(x: WordXml, t: Element, k: number): Element | null {
  const r = t.parentElement!;
  const text = t.textContent ?? "";
  const before = text.slice(0, k);
  const after = text.slice(k + 1);
  const rest = r.cloneNode(false) as Element;
  const rPr = kid(r, "rPr");
  if (rPr) rest.append(rPr.cloneNode(true));
  if (after) {
    const t2 = t.cloneNode(false) as Element;
    t2.textContent = after;
    if (/^\s|\s$/.test(after)) t2.setAttributeNS("http://www.w3.org/XML/1998/namespace", "xml:space", "preserve");
    rest.append(t2);
  }
  for (let n = t.nextElementSibling; n;) {
    const next = n.nextElementSibling;
    rest.append(n);
    n = next;
  }
  if (before) t.textContent = before;
  else t.remove();
  const hasContent = Array.from(rest.children).some((c) => c.localName !== "rPr");
  if (!hasContent) return null;
  r.after(rest);
  return rest;
}

/**
 * Applies `f` to the range between the marks in `doc`, and takes the marks out. False when the marks aren't both there
 * (nothing is changed, and any mark left is taken out).
 */
export function formatRange(doc: Document, f: RangeFormat): boolean {
  const body = byName(doc.documentElement, "body")[0];
  if (!body) return false;
  const texts = () => byName(body, "t");
  const find = (mark: string) => texts().find((t) => (t.textContent ?? "").includes(mark));
  const endT = find(MARK_END);
  const startT = find(MARK_START);
  if (!startT || !endT) {
    for (const t of texts()) if (HAS_MARK.test(t.textContent ?? "")) t.textContent = (t.textContent ?? "").replace(MARKS, "");
    return false;
  }
  const x = new WordXml(doc, body);
  // The paragraphs from the start's to the end's are those the selection touches
  const startP = paragraphOf(startT);
  const endP = paragraphOf(endT);
  // A style or alignment is the paragraphs': the runs stay as they are, the marks just go
  if (!("size" in f)) {
    for (const t of texts()) if (HAS_MARK.test(t.textContent ?? "")) t.textContent = (t.textContent ?? "").replace(MARKS, "");
    const ps = byName(body, "p");
    const i = startP ? ps.indexOf(startP) : -1;
    const j = endP ? ps.indexOf(endP) : -1;
    for (const p of i >= 0 && j >= i ? ps.slice(i, j + 1) : []) {
      const pPr = pPrOf(x, p);
      if ("align" in f) x.set(prop(x, pPr, "jc", PPR), "val", f.align);
      else if (f.isDefault) kid(pPr, "pStyle")?.remove();
      else x.set(prop(x, pPr, "pStyle", PPR), "val", f.style);
    }
    return true;
  }
  // The range's edges, as placeholders between the runs split at the marks (the end first, so the start's place stays
  // as it was): the runs between them are the range's, however the splits moved the text
  const edge = () => doc.createElementNS(null, "tf-edge");
  const endEdge = edge();
  const endRun = endT.parentElement!;
  splitAt(x, endT, (endT.textContent ?? "").indexOf(MARK_END));
  endRun.after(endEdge);
  const startT2 = find(MARK_START)!;
  const startRun = startT2.parentElement!;
  const startEdge = edge();
  splitAt(x, startT2, (startT2.textContent ?? "").indexOf(MARK_START));
  startRun.after(startEdge);
  const all = Array.from(body.querySelectorAll("*"));
  const from = all.indexOf(startEdge);
  const to = all.indexOf(endEdge);
  let inRange = all.filter((e, i) => e.localName === "r" && i > from && i < to);
  startEdge.remove();
  endEdge.remove();
  // A run the marks emptied goes
  for (const r of [startRun, endRun]) if (r.parentElement && !Array.from(r.children).some((c) => c.localName !== "rPr")) r.remove();
  inRange = inRange.filter((r) => r.parentElement && Array.from(r.children).some((c) => c.localName !== "rPr"));
  const paragraphs = () => {
    const ps = byName(body, "p");
    const i = startP ? ps.indexOf(startP) : -1;
    const j = endP ? ps.indexOf(endP) : -1;
    return i >= 0 && j >= i ? ps.slice(i, j + 1) : [];
  };
  const half = String(Math.round(f.size * 2));
  // A selection without text (an empty paragraph): the paragraph mark gets the size, for the text typed there
  const targets = inRange.length ? inRange.map((r) => rPrOf(x, r)) : paragraphs().map((p) => rPrOf(x, pPrOf(x, p)));
  for (const rPr of targets) for (const n of ["sz", "szCs"]) x.set(prop(x, rPr, n, RPR), "val", half);
  return true;
}
