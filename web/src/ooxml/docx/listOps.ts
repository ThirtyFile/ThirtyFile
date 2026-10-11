/**
 * Lists in a Word document (the editor's list buttons): bulleted and numbered lists, and moving a paragraph a level
 * in or out, applied to the paragraphs between the editor's marks (formatOps.ts).
 *
 * Done the way Word does it:
 * - A paragraph joins a list through its numbering properties (w:numPr: the list's w:num, and its level). A new
 *   numbered list starts at 1 (its w:num restarts the numbering); one right after a list of the same kind continues it.
 *   Bulleted lists share a definition.
 * - The definitions are like Word's own (bullets •, ○, ▪ and numbers 1., a., i., over nine levels, half an inch
 *   apiece). When the document has none of the kind needed, one is added to its numbering part, which is made, with its
 *   relationship and content type, when the document has none.
 * - Turning a list off takes the numbering away, or turns off numbering that comes from the paragraph's style.
 * - A list item moves a level in or out; any other paragraph's left indent changes by half an inch.
 */

import { attr, kid, kids } from "../core/package";
import { markedParagraphs, paragraphProp, paragraphProps } from "./formatOps";
import { WordXml } from "./xmlEdit";

export type ListFormat = { list: "bullet" | "number" | "none" } | { level: 1 | -1 };
export type ListKind = "bullet" | "number";

/** The parts a list change reads and may change (null when the document has none of that part) */
export interface ListParts {
  doc: Document;
  numbering: Document | null;
  styles: Document | null;
  /** The document's relationships and the package's content types: changed only when the numbering part is made */
  rels: Document | null;
  types: Document | null;
  /** Where a numbering part made goes (next to document.xml): word/numbering.xml when not given */
  numberingPath?: string;
}

/** What a list change made: the numbering part when there was none (with its relationship and content type) */
export interface ListResult {
  numbering: Document | null;
  rels: Document | null;
  types: Document | null;
  /** The parts that changed besides document.xml */
  changed: { numbering: boolean; rels: boolean; types: boolean };
}

const RELS = "http://schemas.openxmlformats.org/package/2006/relationships";
/** The numbering part's relationship type, in a transitional or a strict document */
const NUMBERING_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
const NUMBERING_REL_STRICT = "http://purl.oclc.org/ooxml/officeDocument/relationships/numbering";
const STRICT = "http://purl.oclc.org/ooxml/wordprocessingml/main";
const NUMBERING_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";
/** Word's half inch, the indent of each level (twips) */
const STEP = 720;
const MAX_LEVEL = 8;

/** Word's bullets for each level: •, ○ and ▪, in its own fonts */
const BULLETS: [string, string][] = [
  [String.fromCharCode(0xf0b7), "Symbol"],
  ["o", "Courier New"],
  [String.fromCharCode(0xf0a7), "Wingdings"],
];
/** Word's numbers for each level: 1., a. and i. */
const NUMBERS = ["decimal", "lowerLetter", "lowerRoman"];

/** The kind of list a paragraph is in at its level (from the numbering definitions), or null when it is in none */
export function listKind(numbering: Document | null, numId: string | null, ilvl: number): ListKind | null {
  if (!numbering?.documentElement || !numId || numId === "0") return null;
  const root = numbering.documentElement;
  const num = kids(root, "num").find((n) => attr(n, "numId") === numId);
  const absId = attr(kid(num, "abstractNumId"), "val");
  const abs = kids(root, "abstractNum").find((a) => attr(a, "abstractNumId") === absId);
  const override = kids(num, "lvlOverride").find((o) => Number(attr(o, "ilvl")) === ilvl);
  const lvl = kid(override, "lvl") ?? kids(abs, "lvl").find((l) => Number(attr(l, "ilvl")) === ilvl);
  const fmt = attr(kid(lvl, "numFmt"), "val");
  if (!fmt) return null;
  return fmt === "bullet" ? "bullet" : fmt === "none" ? null : "number";
}

/** A paragraph's own numbering: list and level (from w:numPr) */
function numPrOf(p: Element): { numId: string | null; ilvl: number } {
  const numPr = kid(kid(p, "pPr"), "numPr");
  return { numId: attr(kid(numPr, "numId"), "val"), ilvl: Number(attr(kid(numPr, "ilvl"), "val") ?? 0) || 0 };
}

/** Whether a paragraph's style gives it numbering (a list style such as List Bullet) */
function styleNumbered(styles: Document | null, p: Element): boolean {
  const id = attr(kid(kid(p, "pPr"), "pStyle"), "val");
  if (!id || !styles?.documentElement) return false;
  const seen = new Set<string>();
  for (let sid: string | null = id; sid && !seen.has(sid);) {
    seen.add(sid);
    const st = kids(styles.documentElement, "style").find((s) => attr(s, "styleId") === sid);
    const numId = attr(kid(kid(kid(st, "pPr"), "numPr"), "numId"), "val");
    if (numId) return numId !== "0";
    sid = attr(kid(st, "basedOn"), "val");
  }
  return false;
}

/** A definition of nine levels like Word's own, for bullets or numbers */
function abstractNum(x: WordXml, id: number, kind: ListKind): Element {
  const abs = x.el("abstractNum");
  x.set(abs, "abstractNumId", String(id));
  abs.append(x.el("multiLevelType", "hybridMultilevel"));
  for (let i = 0; i <= MAX_LEVEL; i++) {
    const lvl = x.el("lvl");
    x.set(lvl, "ilvl", String(i));
    lvl.append(x.el("start", 1));
    if (kind === "bullet") {
      const [char, font] = BULLETS[i % 3];
      lvl.append(x.el("numFmt", "bullet"), x.el("lvlText", char), x.el("lvlJc", "left"));
      const pPr = x.el("pPr");
      const ind = x.el("ind");
      x.set(ind, "left", String(STEP * (i + 1)));
      x.set(ind, "hanging", "360");
      pPr.append(ind);
      const rPr = x.el("rPr");
      const fonts = x.el("rFonts");
      for (const k of ["ascii", "hAnsi"]) x.set(fonts, k, font);
      x.set(fonts, "hint", "default");
      rPr.append(fonts);
      lvl.append(pPr, rPr);
    } else {
      lvl.append(x.el("numFmt", NUMBERS[i % 3]), x.el("lvlText", `%${i + 1}.`), x.el("lvlJc", "left"));
      const pPr = x.el("pPr");
      const ind = x.el("ind");
      x.set(ind, "left", String(STEP * (i + 1)));
      x.set(ind, "hanging", "360");
      pPr.append(ind);
      lvl.append(pPr);
    }
    abs.append(lvl);
  }
  return abs;
}

class Lists {
  readonly x: WordXml;
  numbering: Document | null;
  readonly changed = { numbering: false, rels: false, types: false };
  constructor(private parts: ListParts) {
    const body = parts.doc.documentElement;
    this.x = new WordXml(parts.doc, body);
    this.numbering = parts.numbering;
  }

  /** The numbering part's root, made (with its relationship and content type) when the document has none */
  private root(): Element {
    if (this.numbering?.documentElement) return this.numbering.documentElement;
    const { ns, prefix } = this.x;
    const path = this.parts.numberingPath ?? "word/numbering.xml";
    const name = prefix ? `${prefix}:numbering` : "numbering";
    const doc = new DOMParser().parseFromString(`<${name} ${prefix ? `xmlns:${prefix}` : "xmlns"}="${ns}"/>`, "application/xml");
    this.numbering = doc;
    this.changed.numbering = true;
    const rels = this.parts.rels?.documentElement;
    if (rels) {
      const ids = new Set(kids(rels, "Relationship").map((r) => attr(r, "Id")));
      let n = 1;
      while (ids.has(`rId${n}`)) n++;
      const rel = this.parts.rels!.createElementNS(RELS, "Relationship");
      rel.setAttribute("Id", `rId${n}`);
      rel.setAttribute("Type", ns === STRICT ? NUMBERING_REL_STRICT : NUMBERING_REL);
      rel.setAttribute("Target", path.slice(path.lastIndexOf("/") + 1));
      rels.append(rel);
      this.changed.rels = true;
    }
    const types = this.parts.types?.documentElement;
    if (types && !kids(types, "Override").some((o) => attr(o, "PartName") === `/${path}`)) {
      const over = this.parts.types!.createElementNS(types.namespaceURI, "Override");
      over.setAttribute("PartName", `/${path}`);
      over.setAttribute("ContentType", NUMBERING_TYPE);
      types.append(over);
      this.changed.types = true;
    }
    return doc.documentElement;
  }

  /** A definition for `kind` that the document has (its first level of that kind), or one made like Word's own */
  private abstractFor(kind: ListKind): string {
    const root = this.root();
    for (const a of kids(root, "abstractNum")) {
      const lvl0 = kids(a, "lvl").find((l) => attr(l, "ilvl") === "0");
      const fmt = attr(kid(lvl0, "numFmt"), "val");
      if (!kid(a, "numStyleLink") && !kid(a, "styleLink") && (kind === "bullet" ? fmt === "bullet" : fmt === "decimal")) return attr(a, "abstractNumId")!;
    }
    const id = Math.max(-1, ...kids(root, "abstractNum").map((a) => Number(attr(a, "abstractNumId")) || 0)) + 1;
    const nx = new WordXml(this.numbering!, root);
    const abs = abstractNum(nx, id, kind);
    // Definitions come before the lists that use them
    const after = kids(root, "abstractNum").pop() ?? kids(root, "numPicBullet").pop();
    if (after) after.after(abs);
    else root.prepend(abs);
    this.changed.numbering = true;
    return String(id);
  }

  /** A new list (w:num) of `kind`; a numbered one starts at 1. A bulleted list uses one of the document's own when it can */
  newList(kind: ListKind): string {
    const root = this.root();
    if (kind === "bullet") {
      const existing = kids(root, "num").find((n) => listKind(this.numbering, attr(n, "numId"), 0) === "bullet" && !kids(n, "lvlOverride").length);
      if (existing) return attr(existing, "numId")!;
    }
    const absId = this.abstractFor(kind);
    const id = Math.max(0, ...kids(root, "num").map((n) => Number(attr(n, "numId")) || 0)) + 1;
    const nx = new WordXml(this.numbering!, root);
    const num = nx.el("num");
    nx.set(num, "numId", String(id));
    num.append(nx.el("abstractNumId", absId));
    if (kind === "number") {
      const o = nx.el("lvlOverride");
      nx.set(o, "ilvl", "0");
      o.append(nx.el("startOverride", 1));
      num.append(o);
    }
    const last = kids(root, "num").pop();
    if (last) last.after(num);
    else root.append(num);
    this.changed.numbering = true;
    return String(id);
  }

  setNumPr(p: Element, numId: string, ilvl: number) {
    const numPr = paragraphProp(this.x, paragraphProps(this.x, p), "numPr");
    numPr.append(this.x.el("ilvl", ilvl), this.x.el("numId", numId));
  }

  /** Takes a paragraph out of its list (and out of its style's, when the style gives it one) */
  clearList(p: Element) {
    const pPr = kid(p, "pPr");
    kid(pPr, "numPr")?.remove();
    if (styleNumbered(this.parts.styles, p)) this.setNumPr(p, "0", 0);
  }
}

/** The paragraph before `p` in the same place (body or cell), skipping nothing but non-paragraphs between */
function previousParagraph(p: Element): Element | null {
  for (let e = p.previousElementSibling; e; e = e.previousElementSibling) if (e.localName === "p") return e;
  return null;
}

/**
 * Applies `f` to the paragraphs between the marks in `parts.doc` (taking the marks out). Undefined when the marks
 * aren't both there; otherwise the numbering part and the other parts, saying which changed.
 */
export function formatList(parts: ListParts, f: ListFormat): ListResult | undefined {
  const paragraphs = markedParagraphs(parts.doc);
  if (!paragraphs?.length) return undefined;
  const lists = new Lists(parts);
  const kindOf = (p: Element) => {
    const { numId, ilvl } = numPrOf(p);
    return listKind(lists.numbering, numId, ilvl);
  };
  if ("level" in f) {
    for (const p of paragraphs) {
      const { numId, ilvl } = numPrOf(p);
      if (numId && numId !== "0") {
        const level = Math.max(0, Math.min(MAX_LEVEL, ilvl + f.level));
        // The level comes first in the numbering properties
        const numPr = kid(kid(p, "pPr"), "numPr")!;
        const el = kid(numPr, "ilvl") ?? numPr.insertBefore(lists.x.el("ilvl"), numPr.firstChild);
        lists.x.set(el, "val", String(level));
        continue;
      }
      // Not in a list: the left indent, by half an inch
      const ind = kid(kid(p, "pPr"), "ind");
      const left = Number(attr(ind, "left") ?? attr(ind, "start") ?? 0) || 0;
      const next = Math.max(0, left + f.level * STEP);
      if (next === left) continue;
      const pPr = paragraphProps(lists.x, p);
      const el = kid(pPr, "ind") ?? paragraphProp(lists.x, pPr, "ind");
      lists.x.set(el, attr(el, "start") !== null ? "start" : "left", String(next));
    }
  } else if (f.list === "none" || paragraphs.every((p) => kindOf(p) === f.list)) {
    // The same kind again turns it off, as Word does
    for (const p of paragraphs) lists.clearList(p);
  } else {
    const kind = f.list;
    // Right after a list of the same kind: the paragraphs join it (its numbering goes on)
    const before = previousParagraph(paragraphs[0]);
    const joined = before && kindOf(before) === kind ? numPrOf(before).numId : null;
    const numId = joined ?? lists.newList(kind);
    for (const p of paragraphs) {
      const { numId: own, ilvl } = numPrOf(p);
      kid(kid(p, "pPr"), "numPr")?.remove();
      lists.setNumPr(p, numId, own && own !== "0" ? ilvl : 0);
    }
  }
  return { numbering: lists.numbering, rels: parts.rels, types: parts.types, changed: lists.changed };
}
