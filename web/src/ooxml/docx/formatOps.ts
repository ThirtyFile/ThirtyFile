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
import { placeIn, storyRoot, WordXml } from "./xmlEdit";

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

/** A change of the marked range: font size (points), font, paragraph style (id), alignment (left, center, right, both) or spacing */
export type RangeFormat =
  | { size: number }
  /** A font for the runs: East Asian (`eastAsia`) for their East Asian text too, otherwise for the rest */
  | { font: string; eastAsia: boolean }
  | { style: string; isDefault?: boolean }
  | { align: "left" | "center" | "right" | "both" }
  /** Line spacing in 240ths of a line (240: single) */
  | { line: number }
  /** 12 points of space before or after the paragraphs, or none */
  | { spaceBefore: boolean }
  | { spaceAfter: boolean };

/** A paragraph's own spacing, made in its place when missing: the attributes not changed stay */
function spacingOf(x: WordXml, pPr: Element): Element {
  return kid(pPr, "spacing") ?? placeIn(pPr, x.el("spacing"), PPR);
}

/** A font written into a run's properties: the other slots stay, the theme fonts of those it sets go (they win over names) */
function applyFont(x: WordXml, rPr: Element, font: string, eastAsia: boolean) {
  const fonts = kid(rPr, "rFonts") ?? placeIn(rPr, x.el("rFonts"), RPR);
  const slots = eastAsia ? ["ascii", "hAnsi", "eastAsia"] : ["ascii", "hAnsi", "cs"];
  for (const n of slots) x.set(fonts, n, font);
  x.unset(fonts, ...slots.map((n) => (n === "cs" ? "cstheme" : `${n}Theme`)));
}

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
  if (before) {
    t.textContent = before;
    // Spaces at an edge the split made are kept only when the text says so
    if (/^\s|\s$/.test(before)) t.setAttributeNS("http://www.w3.org/XML/1998/namespace", "xml:space", "preserve");
  } else t.remove();
  const hasContent = Array.from(rest.children).some((c) => c.localName !== "rPr");
  if (!hasContent) return null;
  r.after(rest);
  return rest;
}

/**
 * Applies `f` to the range between the marks in `doc`, and takes the marks out. False when the marks aren't both there
 * (nothing is changed, and any mark left is taken out).
 */
/**
 * The paragraphs the marks are in and those between them (the paragraphs the selection touches), with the marks taken
 * out; null when the marks aren't both there (any mark left is taken out too)
 */
export function markedParagraphs(doc: Document): Element[] | null {
  const body = storyRoot(doc);
  if (!body) return null;
  const texts = byName(body, "t");
  const startP = paragraphOf(texts.find((t) => (t.textContent ?? "").includes(MARK_START)) ?? null);
  const endP = paragraphOf(texts.find((t) => (t.textContent ?? "").includes(MARK_END)) ?? null);
  for (const t of texts) if (HAS_MARK.test(t.textContent ?? "")) t.textContent = (t.textContent ?? "").replace(MARKS, "");
  const ps = byName(body, "p");
  const i = startP ? ps.indexOf(startP) : -1;
  const j = endP ? ps.indexOf(endP) : -1;
  return i >= 0 && j >= i ? ps.slice(i, j + 1) : null;
}

/** A paragraph's properties element, made when missing (it comes first) */
export function paragraphProps(x: WordXml, p: Element): Element {
  return pPrOf(x, p);
}

/** A paragraph property `name` in its place among the others, its attributes cleared to be set afresh */
export function paragraphProp(x: WordXml, pPr: Element, name: string): Element {
  return prop(x, pPr, name, PPR);
}

export function formatRange(doc: Document, f: RangeFormat): boolean {
  const body = storyRoot(doc);
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
  // A style, alignment or spacing is the paragraphs': the runs stay as they are, the marks just go
  if (!("size" in f) && !("font" in f)) {
    for (const p of markedParagraphs(doc) ?? []) {
      const pPr = pPrOf(x, p);
      if ("align" in f) x.set(prop(x, pPr, "jc", PPR), "val", f.align);
      else if ("line" in f) {
        const sp = spacingOf(x, pPr);
        x.set(sp, "line", String(f.line));
        x.set(sp, "lineRule", "auto");
      } else if ("spaceBefore" in f || "spaceAfter" in f) {
        const side = "spaceBefore" in f ? "before" : "after";
        const sp = spacingOf(x, pPr);
        x.set(sp, side, ("spaceBefore" in f ? f.spaceBefore : f.spaceAfter) ? "240" : "0");
        // Spacing in lines, or HTML's automatic spacing, would win over the points
        x.unset(sp, `${side}Lines`, `${side}Autospacing`);
      } else if (f.isDefault) kid(pPr, "pStyle")?.remove();
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
  // A selection without text (an empty paragraph): the paragraph mark gets the size or font, for the text typed there
  const targets = inRange.length ? inRange.map((r) => rPrOf(x, r)) : paragraphs().map((p) => rPrOf(x, pPrOf(x, p)));
  if ("font" in f) {
    for (const rPr of targets) applyFont(x, rPr, f.font, f.eastAsia);
    return true;
  }
  const half = String(Math.round(f.size * 2));
  for (const rPr of targets) for (const n of ["sz", "szCs"]) x.set(prop(x, rPr, n, RPR), "val", half);
  return true;
}

/** The fields the editor inserts: the page number, and the number of pages */
export type FieldKind = "PAGE" | "NUMPAGES";

/**
 * Inserts a field where the start mark is (the end mark, at the same place, goes too), formatted as the text there, as
 * Word does: a w:fldSimple whose result Word works out again. False when the mark isn't there, or is in a link.
 */
export function insertField(doc: Document, kind: FieldKind): boolean {
  const body = storyRoot(doc);
  if (!body) return false;
  const texts = byName(body, "t");
  for (const t of texts) if ((t.textContent ?? "").includes(MARK_END)) t.textContent = (t.textContent ?? "").replace(MARK_END, "");
  const t = texts.find((e) => (e.textContent ?? "").includes(MARK_START));
  const r = t?.parentElement;
  if (!t || !r || r.localName !== "r" || r.parentElement?.localName !== "p") {
    for (const e of texts) if (HAS_MARK.test(e.textContent ?? "")) e.textContent = (e.textContent ?? "").replace(MARKS, "");
    return false;
  }
  const x = new WordXml(doc, body);
  const field = x.el("fldSimple");
  x.set(field, "instr", ` ${kind} `);
  const run = x.el("r");
  const rPr = kid(r, "rPr");
  if (rPr) run.append(rPr.cloneNode(true));
  const result = x.el("t");
  result.textContent = "1";
  run.append(result);
  field.append(run);
  // The run is split at the mark: the field goes between its halves (a half left without text goes)
  splitAt(x, t, (t.textContent ?? "").indexOf(MARK_START));
  r.after(field);
  if (!Array.from(r.children).some((c) => c.localName !== "rPr")) r.remove();
  return true;
}
