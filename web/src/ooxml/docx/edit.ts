/**
 * Editing the text of a Word document (the view is in editor.ts): which body paragraphs can be edited, the text each
 * run holds, reading the edited view back, and writing the changed paragraphs into document.xml.
 *
 * Like saving a workbook, only what changed is rewritten:
 * - A paragraph whose text is the same keeps its XML exactly as it was.
 * - A changed paragraph keeps its paragraph settings and the formatting of each run. Text typed into a run takes its
 *   formatting; text typed into an empty paragraph takes the paragraph mark's.
 * - A paragraph split in two (Enter) gives both parts its settings. The section break it may end is kept by the last part.
 * - Every other block (tables, pictures, fields, tracked changes, content controls…) is shown as it is and kept. It can
 *   only be deleted as a whole, by selecting across it. A section break is never lost: a deleted paragraph that ended a
 *   section leaves an empty one with its settings.
 *
 * Paragraphs are editable when they only hold runs of text (with tabs and line breaks), links around such runs,
 * bookmarks, and fields (page numbers, dates…). A field is kept as it is, as one piece the view shows but doesn't let
 * anyone type into: it can only be deleted as a whole. Anything else in a paragraph makes the whole paragraph a block
 * that is kept as it is.
 *
 * Table cells hold blocks of their own (each cell is a container, like the body): their paragraphs are edited the same
 * way, while the table's rows, columns and settings stay as they are. A cell always keeps a paragraph, as Word needs.
 *
 * A header or footer part is edited the same way: its root holds its blocks as the body does.
 */

import { attr, kid } from "../core/package";
import { applyFmt, rPrOf, type Fmt } from "./formatOps";
import { storyRoot, WordXml } from "./xmlEdit";

const W14 = "http://schemas.microsoft.com/office/word/2010/wordml";
const XML_NS = "http://www.w3.org/XML/1998/namespace";

/** Run content the editor shows as text and can write back */
const TEXT_KIDS = new Set(["rPr", "t", "tab", "br", "cr", "softHyphen", "noBreakHyphen", "lastRenderedPageBreak"]);
/** Markers without text that may sit between runs: bookmarks are kept with the run after them, spelling marks are dropped */
const MARKERS = new Set(["bookmarkStart", "bookmarkEnd", "proofErr"]);
/** Paragraph-mark formatting that is about tracked changes, not looks: not given to text typed into an empty paragraph */
const TRACKING = new Set(["ins", "del", "moveFrom", "moveTo", "rPrChange"]);
/** The characters the view shows for w:softHyphen and w:noBreakHyphen (as the preview does) */
const SOFT_HYPHEN = String.fromCharCode(0xad);
const NON_BREAKING_HYPHEN = String.fromCharCode(0x2011);
/** Characters an XML file can't hold, and halves of characters without their other half (the u flag matches only those): never written */
// oxlint-disable-next-line no-control-regex -- control characters are removed on purpose
const UNWRITABLE = /[\u{0}-\u{8}\u{b}\u{c}\u{e}-\u{1f}\u{fffe}\u{ffff}\u{d800}-\u{dfff}]/gu;

/** Text of one run in the edited view: `run` is its id in the plan, null for text that came from nowhere (typed into an empty view) */
export interface Piece {
  run: number | null;
  text: string;
  /** Character formatting the view gave this text, differing from what its run had */
  fmt?: Fmt;
  /** A field kept as it is (its index in the plan's `atoms`), in place of text */
  atom?: number;
}

/** The formatting the view gives a text node of run `run`, differing from what the run had (editor.ts works it out from what is shown) */
export type FmtOf = (text: Node, run: number | null) => Fmt | undefined;

const fmtKey = (f: Fmt | undefined) => (f ? JSON.stringify(Object.entries(f).sort(([a], [b]) => a.localeCompare(b))) : "");

/** What the edited view holds, in order: blocks kept as they are, and paragraphs with their text */
export type EditedBlock = { kind: "keep"; block: number } | { kind: "para"; source: number | null; pieces: Piece[] };

/** Where blocks are: the body (always container 0) and every table cell */
export interface Container {
  el: Element;
  /** Its blocks in order, by index in the plan's `blocks` (a cell's properties and the body's last section left out) */
  blocks: number[];
}

export interface EditPlan {
  doc: Document;
  body: Element;
  /** Namespace and prefix of the document's elements (transitional or strict) */
  ns: string;
  prefix: string | null;
  /** The blocks of every container, the body's first */
  blocks: Element[];
  containers: Container[];
  containerOf: Map<Element, number>;
  editable: boolean[];
  /** Every run of the editable paragraphs, by id */
  runs: Element[];
  runIds: Map<Element, number>;
  /** Bookmarks before a run or a field, and those at the end of each editable paragraph */
  before: Map<Element, Element[]>;
  after: Map<number, Element[]>;
  /** The fields of the editable paragraphs, each the elements it is made of, by id; and the id of each one's first element */
  atoms: Element[][];
  atomIds: Map<Element, number>;
}

const sectPrOf = (p: Element | null | undefined) => kid(kid(p, "pPr"), "sectPr");

/** A paragraph's w14:paraId or w14:textId (by name too, for a parser that keeps prefixes in names) */
const isParagraphId = (a: Attr) => /^(?:w14:)?(?:paraId|textId)$/.test(a.name) && (a.namespaceURI === W14 || a.name.startsWith("w14:"));

function isTextRun(r: Element): boolean {
  for (const c of Array.from(r.children)) {
    if (!TEXT_KIDS.has(c.localName)) return false;
    if (c.localName === "br") {
      const type = attr(c, "type");
      const clear = attr(c, "clear");
      if ((type && type !== "textWrapping") || (clear && clear !== "none")) return false;
    }
    // Hidden text isn't shown, so it can't be edited: the paragraph is kept as it is
    if (c.localName === "rPr" && (kid(c, "vanish") || kid(c, "specVanish") || kid(c, "webHidden"))) return false;
  }
  return true;
}

/** How many fields a run starts and ends (+1 for each start, -1 for each end) */
function fieldDepth(r: Element): number {
  let d = 0;
  for (const c of Array.from(r.children)) {
    if (c.localName !== "fldChar") continue;
    const t = attr(c, "fldCharType");
    if (t === "begin") d++;
    else if (t === "end") d--;
  }
  return d;
}

/** Whether a run only holds field marks and field code (the start or the end of a field, nothing shown) */
const isFieldMarkRun = (r: Element) => r.localName === "r" && Array.from(r.children).every((c) => c.localName === "rPr" || c.localName === "fldChar" || c.localName === "instrText");

/**
 * A paragraph's fields: each w:fldSimple, and the elements from a run starting a field to the run ending it (with
 * whatever is between, nested fields too). Null when a field isn't whole in the paragraph, or starts or ends in a run
 * that also holds text.
 */
export function fieldsOf(p: Element): Element[][] | null {
  const out: Element[][] = [];
  let open: Element[] | null = null;
  let depth = 0;
  for (const c of Array.from(p.children)) {
    if (open) {
      open.push(c);
      if (c.localName === "r") depth += fieldDepth(c);
      if (depth <= 0) {
        if (!isFieldMarkRun(c)) return null;
        out.push(open);
        open = null;
      }
      continue;
    }
    if (c.localName === "fldSimple") out.push([c]);
    else if (c.localName === "r" && fieldDepth(c) !== 0 && Array.from(c.children).some((k) => k.localName === "fldChar")) {
      depth = fieldDepth(c);
      if (depth < 0 || !isFieldMarkRun(c)) return null;
      open = [c];
    }
  }
  return open ? null : out;
}

/** Whether a body paragraph can be edited (see the module notes) */
export function isEditableParagraph(p: Element): boolean {
  const fields = fieldsOf(p);
  if (!fields) return false;
  const inField = new Set(fields.flat());
  for (const c of Array.from(p.children)) {
    if (inField.has(c)) continue;
    switch (c.localName) {
      case "pPr": {
        // A paragraph mark deleted in a tracked change joins it with the next one: joining or splitting it here would mix them up
        const mark = kid(c, "rPr");
        if (kid(mark, "del") || kid(mark, "moveFrom")) return false;
        break;
      }
      case "r":
        if (!isTextRun(c)) return false;
        break;
      case "hyperlink":
        for (const k of Array.from(c.children)) {
          if (k.localName === "r" ? !isTextRun(k) : !MARKERS.has(k.localName)) return false;
        }
        break;
      default:
        if (!MARKERS.has(c.localName)) return false;
    }
  }
  return true;
}

/** A run's text as the editor shows it: tabs as \t, line breaks as \n, soft and non-breaking hyphens as their characters */
export function runText(r: Element): string {
  let s = "";
  for (const c of Array.from(r.children)) {
    switch (c.localName) {
      case "t":
        // As the preview shows it: line breaks inside w:t aren't line breaks in Word
        s += (c.textContent ?? "").replace(/[\r\n]/g, "");
        break;
      case "tab":
        s += "\t";
        break;
      case "br":
      case "cr":
        s += "\n";
        break;
      case "softHyphen":
        s += SOFT_HYPHEN;
        break;
      case "noBreakHyphen":
        s += NON_BREAKING_HYPHEN;
        break;
    }
  }
  return s;
}

/** A container's own property elements: a cell's (first), the body's last section (last) */
function propsOf(el: Element, body: Element): { head: Element[]; tail: Element[] } {
  if (el === body)
    return {
      head: [],
      tail: Array.from(el.children)
        .filter((c) => c.localName === "sectPr")
        .slice(-1),
    };
  return { head: Array.from(el.children).filter((c) => c.localName === "tcPr"), tail: [] };
}

export function planEdits(doc: Document | null): EditPlan | null {
  // document.xml's body, or a header's or footer's root (which holds its blocks the same way)
  const body = storyRoot(doc);
  if (!doc || !body) return null;
  const plan: EditPlan = {
    doc,
    body,
    ns: body.namespaceURI ?? "",
    prefix: body.prefix,
    blocks: [],
    containers: [],
    containerOf: new Map(),
    editable: [],
    runs: [],
    runIds: new Map(),
    before: new Map(),
    after: new Map(),
    atoms: [],
    atomIds: new Map(),
  };
  // The body, then every table cell in it (tables inside cells too), in document order
  const cells = Array.from(body.querySelectorAll("*")).filter((e) => e.localName === "tc");
  for (const el of [body, ...cells]) {
    const { head, tail } = propsOf(el, body);
    const own = Array.from(el.children).filter((c) => !head.includes(c) && !tail.includes(c));
    plan.containerOf.set(el, plan.containers.length);
    plan.containers.push({ el, blocks: own.map((b) => plan.blocks.push(b) - 1) });
  }
  plan.blocks.forEach((b, i) => {
    const editable = b.localName === "p" && isEditableParagraph(b);
    plan.editable.push(editable);
    if (!editable) return;
    let pending: Element[] = [];
    const fields = fieldsOf(b) ?? [];
    const starts = new Map(fields.map((f) => [f[0], f]));
    const inField = new Set(fields.flat());
    const add = (el: Element) => {
      const field = starts.get(el);
      if (field) {
        plan.atomIds.set(el, plan.atoms.length);
        plan.atoms.push(field);
        if (pending.length) plan.before.set(el, pending);
        pending = [];
      } else if (inField.has(el)) return;
      else if (el.localName === "r") {
        plan.runIds.set(el, plan.runs.length);
        plan.runs.push(el);
        if (pending.length) plan.before.set(el, pending);
        pending = [];
      } else if (el.localName === "bookmarkStart" || el.localName === "bookmarkEnd") pending.push(el);
    };
    for (const c of Array.from(b.children)) {
      if (c.localName === "hyperlink") Array.from(c.children).forEach(add);
      else add(c);
    }
    if (pending.length) plan.after.set(i, pending);
  });
  return plan;
}

/** The runs of a paragraph that show text, and its fields, as pieces (what the editor shows for it before any change) */
export function originalPieces(plan: EditPlan, block: number): Piece[] {
  const out: Piece[] = [];
  const add = (r: Element) => {
    const text = runText(r);
    if (text) out.push({ run: plan.runIds.get(r) ?? null, text });
  };
  for (const c of Array.from(plan.blocks[block].children)) {
    const atom = plan.atomIds.get(c);
    if (atom !== undefined) out.push({ run: null, text: "", atom });
    else if (c.localName === "r" && plan.runIds.has(c)) add(c);
    else if (c.localName === "hyperlink") for (const r of Array.from(c.children)) if (r.localName === "r") add(r);
  }
  return out;
}

/** Joins consecutive pieces of the same run, and drops empty ones (a field is a piece of its own) */
export function mergePieces(pieces: Piece[]): Piece[] {
  const out: Piece[] = [];
  for (const p of pieces) {
    if (!p.text && p.atom === undefined) continue;
    const last = out[out.length - 1];
    if (last && last.atom === undefined && p.atom === undefined && last.run === p.run && fmtKey(last.fmt) === fmtKey(p.fmt)) last.text += p.text;
    else out.push({ ...p });
  }
  return out;
}

export const samePieces = (a: Piece[], b: Piece[]) =>
  a.length === b.length && a.every((p, i) => p.run === b[i].run && p.text === b[i].text && p.atom === b[i].atom && fmtKey(p.fmt) === fmtKey(b[i].fmt));

// ───────────── Reading the edited view back ─────────────

/** Elements a browser may make a paragraph of while editing */
const BLOCK_TAGS = new Set(["P", "DIV", "LI", "UL", "OL", "H1", "H2", "H3", "H4", "H5", "H6", "BLOCKQUOTE", "PRE", "SECTION", "ARTICLE"]);

interface Token extends Piece {
  /** A line break element; `end` is the empty line the view adds to a paragraph that ends without text */
  br?: "br" | "end";
}

const isElement = (n: Node): n is HTMLElement => n.nodeType === 1;
const keptBlock = (n: Node) => isElement(n) && n.dataset.block !== undefined;
const isBlock = (n: Node) => isElement(n) && BLOCK_TAGS.has(n.tagName);

function runOf(el: Element | null, stop: Element): number | null {
  for (let e: Element | null = el; e && e !== stop; e = e.parentElement) {
    const r = (e as HTMLElement).dataset?.r;
    if (r !== undefined) return Number(r);
  }
  return null;
}

/** The pieces of one paragraph's tokens: one trailing line break is the browser's empty line, not one of the text's */
function finish(tokens: Token[]): Piece[] {
  const last = tokens[tokens.length - 1];
  if (last?.br) tokens.pop();
  const kept = tokens.filter((t) => t.br !== "end");
  // Text before the first run (typed at the start of the paragraph) takes the formatting of the run after it
  const first = kept.find((t) => t.run !== null)?.run ?? null;
  for (const t of kept) {
    if (t.run !== null) break;
    if (t.atom === undefined) t.run = first;
  }
  return mergePieces(kept.map(({ run, text, fmt, atom }) => (atom !== undefined ? { run: null, text: "", atom } : fmt ? { run, text, fmt } : { run, text })));
}

/**
 * One paragraph element of the view: its text by run. Nested paragraph elements and kept blocks inside it (a browser
 * may leave them there) split it into several paragraphs.
 */
function readBlock(el: HTMLElement, source: number | null, out: EditedBlock[], fmtOf?: FmtOf) {
  let tokens: Token[] = [];
  let lastRun: number | null = null;
  let produced = false;
  const close = (always: boolean) => {
    if (!tokens.length && !always) return;
    out.push({ kind: "para", source, pieces: finish(tokens) });
    produced = true;
    tokens = [];
    lastRun = null;
  };
  const walk = (n: Node) => {
    if (n.nodeType === 3) {
      const text = n.nodeValue ?? "";
      if (!text) return;
      const run = runOf(n.parentElement, el) ?? lastRun;
      lastRun = run;
      const fmt = fmtOf?.(n, run);
      tokens.push(fmt ? { run, text, fmt } : { run, text });
      return;
    }
    if (!isElement(n)) return;
    // A field, kept as it is
    if (n.dataset.atom !== undefined) {
      tokens.push({ run: null, text: "", atom: Number(n.dataset.atom) });
      return;
    }
    // List numbers and other parts of the view that aren't the paragraph's text
    if (n.dataset.ro !== undefined) return;
    if (keptBlock(n)) {
      close(false);
      out.push({ kind: "keep", block: Number(n.dataset.block) });
      produced = true;
      return;
    }
    if (isBlock(n)) {
      close(false);
      readBlock(n, n.dataset.para !== undefined ? Number(n.dataset.para) : source, out, fmtOf);
      produced = true;
      return;
    }
    if (n.tagName === "BR") {
      if (n.classList.contains("tf-docx-eol")) {
        tokens.push({ run: lastRun, text: "", br: "end" });
        return;
      }
      const run = runOf(n, el) ?? lastRun;
      lastRun = run;
      tokens.push({ run, text: "\n", br: "br" });
      return;
    }
    for (const c of Array.from(n.childNodes)) walk(c);
  };
  for (const c of Array.from(el.childNodes)) walk(c);
  close(!produced);
}

/** The pieces of a single paragraph element (nested blocks aren't expected here) */
export function readParagraph(el: HTMLElement, fmtOf?: FmtOf): Piece[] {
  const out: EditedBlock[] = [];
  readBlock(el, null, out, fmtOf);
  return out.flatMap((b) => (b.kind === "para" ? b.pieces : []));
}

/**
 * Reads the edited view back: its kept blocks (`data-block`) and paragraphs (`data-para` names the paragraph a
 * paragraph element came from; a browser copies it into both halves of a split one). Paragraphs that came from nowhere
 * (text typed straight into the view) take the paragraph before them, or after them, as their model.
 */
export function readEdits(root: HTMLElement, fmtOf?: FmtOf): EditedBlock[] {
  const out: EditedBlock[] = [];
  let loose: HTMLElement | null = null;
  const flush = () => {
    if (loose) readBlock(loose, null, out);
    loose = null;
  };
  for (const n of Array.from(root.childNodes)) {
    if (keptBlock(n)) {
      flush();
      out.push({ kind: "keep", block: Number((n as HTMLElement).dataset.block) });
    } else if (isBlock(n)) {
      flush();
      const el = n as HTMLElement;
      readBlock(el, el.dataset.para !== undefined ? Number(el.dataset.para) : null, out, fmtOf);
    } else if (n.nodeType === 3 || isElement(n)) {
      // Inline content straight in the view: one paragraph for each run of it
      if (n.nodeType === 3 && !(n.nodeValue ?? "")) continue;
      loose ??= root.ownerDocument.createElement("p");
      loose.append(n.cloneNode(true));
    }
  }
  flush();
  const paras = out.filter((b): b is Extract<EditedBlock, { kind: "para" }> => b.kind === "para");
  paras.forEach((b, i) => {
    if (b.source !== null) return;
    for (let j = i - 1; j >= 0 && b.source === null; j--) b.source = paras[j].source;
    for (let j = i + 1; j < paras.length && b.source === null; j++) b.source = paras[j].source;
  });
  return out;
}

/**
 * Every container of the view: the body (the view itself, container 0), and each table cell it shows as one of its own
 * (`data-cell` names the container). A cell's text is read only as its cell's, never as part of the body.
 */
export function readAllEdits(root: HTMLElement, fmtOf?: FmtOf): Map<number, EditedBlock[]> {
  const out = new Map<number, EditedBlock[]>([[0, readEdits(root, fmtOf)]]);
  for (const host of Array.from(root.querySelectorAll<HTMLElement>("[data-cell]"))) out.set(Number(host.dataset.cell), readEdits(host, fmtOf));
  return out;
}

// ───────────── Writing the edits back ─────────────

class Writer {
  private readonly anchored = new Set<Element>();
  readonly x: WordXml;
  constructor(private plan: EditPlan) {
    this.x = new WordXml(plan.doc, plan.body);
  }

  el(name: string) {
    const { doc, ns, prefix } = this.plan;
    return doc.createElementNS(ns, prefix ? `${prefix}:${name}` : name);
  }

  /** The paragraph mark's formatting, for text typed into an empty paragraph */
  private markRPr(pPr: Element | null): Element | null {
    const rPr = kid(pPr, "rPr");
    if (!rPr) return null;
    const copy = rPr.cloneNode(true) as Element;
    for (const c of Array.from(copy.children)) if (TRACKING.has(c.localName)) c.remove();
    return copy.children.length ? copy : null;
  }

  run(r: Element | null, text: string, pPr: Element | null, fmt?: Fmt): Element {
    // A run whose text is the same keeps everything it had (and gets the formatting the view gave it)
    if (r && text === runText(r)) {
      const same = r.cloneNode(true) as Element;
      if (fmt) applyFmt(this.x, rPrOf(this.x, same), fmt);
      return same;
    }
    const run = r ? (r.cloneNode(false) as Element) : this.el("r");
    const rPr = r ? kid(r, "rPr")?.cloneNode(true) : this.markRPr(pPr);
    if (rPr) run.append(rPr);
    let buf = "";
    const flush = () => {
      if (!buf) return;
      const t = this.el("t");
      if (/^\s|\s$|\s\s/.test(buf)) t.setAttributeNS(XML_NS, "xml:space", "preserve");
      t.textContent = buf;
      run.append(t);
      buf = "";
    };
    const mark = (name: string) => {
      flush();
      run.append(this.el(name));
    };
    for (const ch of text.replace(UNWRITABLE, "")) {
      if (ch === "\t") mark("tab");
      else if (ch === "\n") mark("br");
      else if (ch === SOFT_HYPHEN) mark("softHyphen");
      else if (ch === NON_BREAKING_HYPHEN) mark("noBreakHyphen");
      else buf += ch;
    }
    flush();
    if (fmt) applyFmt(this.x, rPrOf(this.x, run), fmt);
    return run;
  }

  /**
   * A paragraph with new text, made from the one it came from (`source`): its settings, and its runs' formatting.
   * `first` keeps its ids (the other parts of a split paragraph get none); `last` keeps the section break it may end.
   */
  paragraph(source: number | null, pieces: Piece[], first: boolean, last: boolean): Element {
    const { plan } = this;
    const src = source !== null ? plan.blocks[source] : null;
    const p = src ? (src.cloneNode(false) as Element) : this.el("p");
    // Ids are each paragraph's own: the other parts of a split paragraph get none (Word gives them new ones)
    if (!first) for (const a of Array.from(p.attributes)) if (isParagraphId(a)) p.removeAttributeNode(a);
    const pPr = kid(src, "pPr");
    if (pPr) {
      const copy = pPr.cloneNode(true) as Element;
      if (!last) kid(copy, "sectPr")?.remove();
      p.append(copy);
    }
    let host: Element = p;
    let link: Element | null = null;
    for (const piece of pieces) {
      if (piece.atom !== undefined) {
        // A field goes in as it was, once, in the paragraph itself (never in a link)
        const field = plan.atoms[piece.atom];
        if (!field || this.anchored.has(field[0])) continue;
        this.anchored.add(field[0]);
        link = null;
        host = p;
        for (const m of plan.before.get(field[0]) ?? []) p.append(m.cloneNode(true));
        for (const el of field) p.append(el.cloneNode(true));
        continue;
      }
      const r = piece.run !== null ? (plan.runs[piece.run] ?? null) : null;
      const parent = r?.parentElement ?? null;
      const hl = parent?.localName === "hyperlink" ? parent : null;
      if (hl !== link) {
        link = hl;
        host = hl ? p.appendChild(hl.cloneNode(false) as Element) : p;
      }
      if (r && !this.anchored.has(r)) {
        this.anchored.add(r);
        for (const m of plan.before.get(r) ?? []) host.append(m.cloneNode(true));
      }
      host.append(this.run(r, piece.text, pPr, piece.fmt));
    }
    if (last && source !== null) for (const m of plan.after.get(source) ?? []) p.append(m.cloneNode(true));
    return p;
  }

  /** What stays of a block deleted in the view: the section break it ended, as an empty paragraph with its settings */
  sectionBreak(block: Element): Element | null {
    const p =
      block.localName === "p" && sectPrOf(block)
        ? block
        : Array.from(block.querySelectorAll("*"))
            .filter((e) => e.localName === "sectPr")
            .map((s) => s.parentElement?.parentElement)
            .find((x) => x?.localName === "p");
    const pPr = kid(p, "pPr");
    if (!p || !pPr) return null;
    const out = this.el("p");
    out.append(pPr.cloneNode(true));
    return out;
  }
}

/**
 * A container's new blocks for its edited view (container 0 is the body), or null when nothing changed. `shown` are
 * the blocks the view showed: the others (a paragraph holding only a section break, say) stay where they were.
 */
export function editedBlocks(plan: EditPlan, edited: EditedBlock[], shown: ReadonlySet<number>, container = 0, w = new Writer(plan)): Element[] | null {
  const own = plan.containers[container]?.blocks ?? [];
  const n = own.length;
  const at = new Map(own.map((b, i) => [b, i]));
  const referenced = new Set<number>();
  const occurrences = new Map<number, number[]>();
  edited.forEach((e, i) => {
    if (e.kind === "keep") referenced.add(e.block);
    else if (e.source !== null) {
      referenced.add(e.source);
      const list = occurrences.get(e.source) ?? [];
      list.push(i);
      occurrences.set(e.source, list);
    }
  });
  const out: Element[] = [];
  let next = 0;
  // Blocks before the n-th that the view didn't show, or that were deleted but end a section, stay in their place
  const passTo = (local: number | undefined) => {
    if (local === undefined) return;
    for (; next < Math.min(local, n); next++) {
      const b = own[next];
      if (referenced.has(b)) continue;
      const kept = shown.has(b) ? w.sectionBreak(plan.blocks[b]) : plan.blocks[b];
      if (kept) out.push(kept);
    }
    next = Math.max(next, local + 1);
  };
  const kept = new Set<number>();
  edited.forEach((e, i) => {
    if (e.kind === "keep") {
      // A block is written once, and only in its own container, wherever the view may have repeated it
      if (!at.has(e.block) || kept.has(e.block)) return;
      kept.add(e.block);
      passTo(at.get(e.block));
      out.push(plan.blocks[e.block]);
      return;
    }
    const occ = e.source !== null ? (occurrences.get(e.source) ?? []) : [];
    const first = occ[0] === i;
    const last = occ[occ.length - 1] === i;
    if (first) passTo(at.get(e.source!));
    const pieces = mergePieces(e.pieces);
    const src = e.source !== null ? plan.blocks[e.source] : null;
    if (first && src && at.has(e.source!) && plan.editable[e.source!] && samePieces(pieces, originalPieces(plan, e.source!)) && (last || !sectPrOf(src))) {
      out.push(src);
      return;
    }
    out.push(w.paragraph(e.source, pieces, first, last));
  });
  passTo(n);
  // Word needs a paragraph in the body and in every cell, and one after a table that ends them
  const template = [...own].reverse().find((b) => plan.blocks[b].localName === "p");
  const lastIsP = n > 0 && plan.blocks[own[n - 1]].localName === "p";
  if (!out.some((b) => b.localName === "p") || (lastIsP && out[out.length - 1]?.localName !== "p")) {
    out.push(w.paragraph(template ?? null, [], false, false));
  }
  if (out.length === n && out.every((b, i) => b === plan.blocks[own[i]])) return null;
  return out;
}

const DECLARATION = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\r\n';

/** A document part as Word writes it, with its XML declaration */
export function writeXml(doc: Document): string {
  const xml = new XMLSerializer().serializeToString(doc);
  return xml.startsWith("<?xml") ? xml : DECLARATION + xml;
}

/** What the view holds for each container it showed: the body's (container 0), and each cell's */
export type Edits = Map<number, EditedBlock[]>;

/** document.xml with the edited view's changes (`edits`: the body's, or each container's), or null when nothing changed */
export function saveEdits(plan: EditPlan, edits: EditedBlock[] | Edits, shown: ReadonlySet<number>): string | null {
  const byContainer: Edits = Array.isArray(edits) ? new Map([[0, edits]]) : edits;
  const w = new Writer(plan);
  const changes: { el: Element; kids: Node[] }[] = [];
  for (const [c, edited] of byContainer) {
    const blocks = editedBlocks(plan, edited, shown, c, w);
    if (!blocks) continue;
    const el = plan.containers[c].el;
    const { head, tail } = propsOf(el, plan.body);
    changes.push({ el, kids: [...head, ...blocks, ...tail] });
  }
  if (!changes.length) return null;
  // The new blocks go into their containers only while the document is written out, so the plan keeps describing it as opened
  const old = changes.map(({ el }) => Array.from(el.childNodes));
  changes.forEach(({ el, kids }) => el.replaceChildren(...kids));
  try {
    return writeXml(plan.doc);
  } finally {
    for (let i = changes.length - 1; i >= 0; i--) changes[i].el.replaceChildren(...old[i]);
  }
}
