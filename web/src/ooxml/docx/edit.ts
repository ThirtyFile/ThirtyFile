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
 * Paragraphs are editable when they only hold runs of text (with tabs and line breaks), links around such runs, and
 * bookmarks. Anything else in a paragraph makes the whole paragraph a block that is kept as it is.
 */

import { attr, kid } from "../core/package";

const W14 = "http://schemas.microsoft.com/office/word/2010/wordml";
const XML_NS = "http://www.w3.org/XML/1998/namespace";

/** Run content the editor shows as text and can write back */
const TEXT_KIDS = new Set(["rPr", "t", "tab", "br", "cr", "softHyphen", "noBreakHyphen", "lastRenderedPageBreak"]);
/** Markers without text that may sit between runs: bookmarks are kept with the run after them, spelling marks are dropped */
const MARKERS = new Set(["bookmarkStart", "bookmarkEnd", "proofErr"]);
/** Paragraph-mark formatting that is about tracked changes, not looks: not given to text typed into an empty paragraph */
const TRACKING = new Set(["ins", "del", "moveFrom", "moveTo", "rPrChange"]);
/** Characters an XML file can't hold, and halves of characters without their other half: never written */
// oxlint-disable-next-line no-control-regex -- control characters are removed on purpose
const UNWRITABLE = /[\u0000-\u0008\u000b\u000c\u000e-\u001f￾￿]|[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/g;

/** Text of one run in the edited view: `run` is its id in the plan, null for text that came from nowhere (typed into an empty view) */
export interface Piece {
  run: number | null;
  text: string;
}

/** What the edited view holds, in order: blocks kept as they are, and paragraphs with their text */
export type EditedBlock = { kind: "keep"; block: number } | { kind: "para"; source: number | null; pieces: Piece[] };

export interface EditPlan {
  doc: Document;
  body: Element;
  /** Namespace and prefix of the document's elements (transitional or strict) */
  ns: string;
  prefix: string | null;
  /** The body's blocks in order, its final section properties left out */
  blocks: Element[];
  editable: boolean[];
  /** Every run of the editable paragraphs, by id */
  runs: Element[];
  runIds: Map<Element, number>;
  /** Bookmarks before a run, and those at the end of each editable paragraph */
  before: Map<Element, Element[]>;
  after: Map<number, Element[]>;
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

/** Whether a body paragraph can be edited (see the module notes) */
export function isEditableParagraph(p: Element): boolean {
  for (const c of Array.from(p.children)) {
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
        s += "­";
        break;
      case "noBreakHyphen":
        s += "‑";
        break;
    }
  }
  return s;
}

export function planEdits(doc: Document | null): EditPlan | null {
  const root = doc?.documentElement;
  const body = kid(root, "body");
  if (!doc || !body) return null;
  const sect =
    Array.from(body.children)
      .filter((c) => c.localName === "sectPr")
      .pop() ?? null;
  const blocks = Array.from(body.children).filter((c) => c !== sect);
  const plan: EditPlan = {
    doc,
    body,
    ns: body.namespaceURI ?? "",
    prefix: body.prefix,
    blocks,
    editable: [],
    runs: [],
    runIds: new Map(),
    before: new Map(),
    after: new Map(),
  };
  blocks.forEach((b, i) => {
    const editable = b.localName === "p" && isEditableParagraph(b);
    plan.editable.push(editable);
    if (!editable) return;
    let pending: Element[] = [];
    const add = (el: Element) => {
      if (el.localName === "r") {
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

/** The runs of a paragraph that show text, as pieces (what the editor shows for it before any change) */
export function originalPieces(plan: EditPlan, block: number): Piece[] {
  const out: Piece[] = [];
  const add = (r: Element) => {
    const text = runText(r);
    if (text) out.push({ run: plan.runIds.get(r) ?? null, text });
  };
  for (const c of Array.from(plan.blocks[block].children)) {
    if (c.localName === "r") add(c);
    else if (c.localName === "hyperlink") for (const r of Array.from(c.children)) if (r.localName === "r") add(r);
  }
  return out;
}

/** Joins consecutive pieces of the same run, and drops empty ones */
export function mergePieces(pieces: Piece[]): Piece[] {
  const out: Piece[] = [];
  for (const p of pieces) {
    if (!p.text) continue;
    const last = out[out.length - 1];
    if (last && last.run === p.run) last.text += p.text;
    else out.push({ ...p });
  }
  return out;
}

export const samePieces = (a: Piece[], b: Piece[]) => a.length === b.length && a.every((p, i) => p.run === b[i].run && p.text === b[i].text);

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
    t.run = first;
  }
  return mergePieces(kept.map(({ run, text }) => ({ run, text })));
}

/**
 * One paragraph element of the view: its text by run. Nested paragraph elements and kept blocks inside it (a browser
 * may leave them there) split it into several paragraphs.
 */
function readBlock(el: HTMLElement, source: number | null, out: EditedBlock[]) {
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
      tokens.push({ run, text });
      return;
    }
    if (!isElement(n)) return;
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
      readBlock(n, n.dataset.para !== undefined ? Number(n.dataset.para) : source, out);
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
export function readParagraph(el: HTMLElement): Piece[] {
  const out: EditedBlock[] = [];
  readBlock(el, null, out);
  return out.flatMap((b) => (b.kind === "para" ? b.pieces : []));
}

/**
 * Reads the edited view back: its kept blocks (`data-block`) and paragraphs (`data-para` names the paragraph a
 * paragraph element came from; a browser copies it into both halves of a split one). Paragraphs that came from nowhere
 * (text typed straight into the view) take the paragraph before them, or after them, as their model.
 */
export function readEdits(root: HTMLElement): EditedBlock[] {
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
      readBlock(el, el.dataset.para !== undefined ? Number(el.dataset.para) : null, out);
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

// ───────────── Writing the edits back ─────────────

class Writer {
  private readonly anchored = new Set<Element>();
  constructor(private plan: EditPlan) {}

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

  run(r: Element | null, text: string, pPr: Element | null): Element {
    // A run whose text is the same keeps everything it had
    if (r && text === runText(r)) return r.cloneNode(true) as Element;
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
      else if (ch === "­") mark("softHyphen");
      else if (ch === "‑") mark("noBreakHyphen");
      else buf += ch;
    }
    flush();
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
      host.append(this.run(r, piece.text, pPr));
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
 * The body's new blocks for the edited view, or null when nothing changed. `shown` are the blocks the view showed:
 * the others (a paragraph holding only a section break, say) stay where they were.
 */
export function editedBlocks(plan: EditPlan, edited: EditedBlock[], shown: ReadonlySet<number>): Element[] | null {
  const n = plan.blocks.length;
  const w = new Writer(plan);
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
  // Blocks before `b` that the view didn't show, or that were deleted but end a section, stay in their place
  const passTo = (b: number) => {
    for (; next < Math.min(b, n); next++) {
      if (referenced.has(next)) continue;
      const block = plan.blocks[next];
      const kept = shown.has(next) ? w.sectionBreak(block) : block;
      if (kept) out.push(kept);
    }
    next = Math.max(next, b + 1);
  };
  const kept = new Set<number>();
  edited.forEach((e, i) => {
    if (e.kind === "keep") {
      // A block is written once, wherever the view may have repeated it
      if (e.block < 0 || e.block >= n || kept.has(e.block)) return;
      kept.add(e.block);
      passTo(e.block);
      out.push(plan.blocks[e.block]);
      return;
    }
    const occ = e.source !== null ? (occurrences.get(e.source) ?? []) : [];
    const first = occ[0] === i;
    const last = occ[occ.length - 1] === i;
    if (first) passTo(e.source!);
    const pieces = mergePieces(e.pieces);
    const src = e.source !== null ? plan.blocks[e.source] : null;
    if (first && src && plan.editable[e.source!] && samePieces(pieces, originalPieces(plan, e.source!)) && (last || !sectPrOf(src))) {
      out.push(src);
      return;
    }
    out.push(w.paragraph(e.source, pieces, first, last));
  });
  passTo(n);
  // Word needs a paragraph in the body, and one after a table that ends it
  const template = [...plan.blocks].reverse().find((b) => b.localName === "p") ?? null;
  const lastIsP = plan.blocks[n - 1]?.localName === "p";
  if (!out.some((b) => b.localName === "p") || (lastIsP && out[out.length - 1]?.localName !== "p")) {
    out.push(w.paragraph(template ? plan.blocks.indexOf(template) : null, [], false, false));
  }
  if (out.length === n && out.every((b, i) => b === plan.blocks[i])) return null;
  return out;
}

const DECLARATION = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\r\n';

/** document.xml with the edited view's changes, or null when nothing changed */
export function saveEdits(plan: EditPlan, edited: EditedBlock[], shown: ReadonlySet<number>): string | null {
  const blocks = editedBlocks(plan, edited, shown);
  if (!blocks) return null;
  const { body, doc } = plan;
  // The new blocks go into the body only while it is written out, so the plan keeps describing the document as opened
  const old = Array.from(body.childNodes);
  const sect = Array.from(body.children)
    .filter((c) => c.localName === "sectPr")
    .pop();
  body.replaceChildren(...blocks, ...(sect ? [sect] : []));
  try {
    const xml = new XMLSerializer().serializeToString(doc);
    return xml.startsWith("<?xml") ? xml : DECLARATION + xml;
  } finally {
    body.replaceChildren(...old);
  }
}
