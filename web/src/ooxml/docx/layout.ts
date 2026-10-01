/**
 * Pagination and pages:
 * 1. Blocks between hard page breaks are first placed into a measuring container (galley) as wide as the body, laid out once, then all positions are read;
 * 2. Page breaks are chosen from each section's body height (minus headers/footers that exceed the margins and that page's footnotes); tables split by row and repeat header rows;
 * 3. Build pages: headers/footers (first/even/default, page number fields), footnotes, floating objects, page borders.
 * Kept-together paragraphs move whole to the next page; keep-with-next (keepNext) paragraphs move together.
 */

import { breathe } from "../core/yield";
import { attr, css, h, kids } from "../core/package";
import { fillBlocks } from "./blocks";
import type { DocCtx, Flow, PageFloat } from "./context";
import { formatNumber } from "./numfmt";
import { borderCss, borderSpace } from "./props";
import { contentWidth, type Section } from "./section";
import { flatKids } from "./xml";

export interface BodyBlock {
  el: HTMLElement;
  pageBreak: boolean;
  columnBreak: boolean;
  sec: number;
}

export interface HfInstance {
  el: HTMLElement;
  floats: PageFloat[];
  height: number;
}

type HfType = "default" | "first" | "even";

type Item = { el: HTMLElement } | { table: HTMLTableElement; from: number; to: number } | { para: HTMLElement; part: number };

interface Piece {
  sec: number;
  items: Item[];
}

interface Page {
  sec: number;
  pieces: Piece[];
  notes: string[];
  hard: boolean;
  number: number;
  indexInSection: number;
  header: HfInstance | null;
  footer: HfInstance | null;
  top: number;
  bottom: number;
  blank?: boolean;
}

interface Seg {
  sec: number;
  el: HTMLElement;
  blocks: BodyBlock[];
}

interface Run {
  segs: Seg[];
  galley: HTMLElement;
  sectionStart: boolean;
}

const r2 = (n: number) => Math.round(n * 100) / 100;
const PT = 96 / 72;

/** Headers/footers: built once per section and type, cloned for every page */
export class HeaderFooters {
  private cache = new Map<string, HfInstance | null>();

  constructor(
    private doc: DocCtx,
    private sections: Section[],
    private host: HTMLElement,
    private parts: Map<string, Document | null>,
  ) {}

  get(sec: number, kind: "header" | "footer", type: HfType): HfInstance | null {
    const s = this.sections[sec];
    const refs = kind === "header" ? s.headers : s.footers;
    // When first/even are not specified: the first page is blank, even pages use the default
    const id = type === "first" ? refs.first : type === "even" ? (refs.even ?? refs.default) : refs.default;
    if (!id) return null;
    const rel = (this.doc.rels.get("word/document.xml") ?? []).find((r) => r.id === id);
    if (!rel) return null;
    const width = contentWidth(s);
    const key = `${rel.target}|${Math.round(width)}|${sec}`;
    if (this.cache.has(key)) return this.cache.get(key)!;
    const xml = this.parts.get(rel.target);
    const root = xml?.documentElement;
    let inst: HfInstance | null = null;
    if (root) {
      const el = h("div", { class: kind === "header" ? "tf-docx-hdr" : "tf-docx-ftr", style: `width:${r2(width)}px` });
      const flow: Flow = {
        doc: this.doc,
        part: rel.target,
        rels: this.doc.rels.get(rel.target) ?? [],
        section: s,
        width,
        story: kind,
        depth: 0,
        fields: [],
        comments: new Set(),
        floats: [],
      };
      fillBlocks(el, flatKids(root), flow);
      this.host.append(el);
      inst = { el, floats: flow.floats, height: 0 };
    }
    this.cache.set(key, inst);
    return inst;
  }

  measure() {
    for (const v of this.cache.values()) if (v) v.height = v.el.offsetHeight;
  }

  /** Prebuild every header/footer that will be used (so their tabs can be resolved together) */
  prepare() {
    this.sections.forEach((_, i) => {
      for (const kind of ["header", "footer"] as const) for (const t of ["default", "first", "even"] as const) this.get(i, kind, t);
    });
  }
}

export interface LayoutInput {
  doc: DocCtx;
  sections: Section[];
  blocks: BodyBlock[];
  bodyFloats: PageFloat[];
  footnotes: Map<string, HTMLElement>;
  hf: HeaderFooters;
  host: HTMLElement;
  background: string | null;
  evenAndOdd: boolean;
}

export class Paginator {
  private runs: Run[] = [];
  private noteHost: HTMLElement;
  private noteHeights = new Map<string, number>();
  /** Split positions of paragraphs that span pages (in order) */
  private splits = new Map<HTMLElement, { node: Text; offset: number }[]>();

  constructor(private o: LayoutInput) {
    this.noteHost = h("div", { class: "tf-docx-fn", style: `width:${r2(contentWidth(o.sections[0]))}px` });
  }

  /** Phase 1: place blocks into the galley */
  buildGalleys() {
    const { blocks, sections, host } = this.o;
    let run: Run | null = null;
    let seg: Seg | null = null;
    let prevSec = -1;
    for (const b of blocks) {
      const secChange = b.sec !== prevSec;
      const type = sections[b.sec].type;
      // Word also starts a new page for a continuous section break when the page size differs
      const sizeChange = secChange && prevSec >= 0 && (Math.abs(sections[b.sec].width - sections[prevSec].width) > 1 || Math.abs(sections[b.sec].height - sections[prevSec].height) > 1);
      const hardSection = secChange && prevSec >= 0 && ((type !== "continuous" && type !== "nextColumn") || sizeChange);
      if (!run || b.pageBreak || hardSection) {
        const s = sections[b.sec];
        run = { segs: [], galley: h("div", { class: "tf-docx-galley", style: `width:${r2(contentWidth(s))}px` }), sectionStart: secChange };
        this.runs.push(run);
        seg = null;
        host.append(run.galley);
      }
      if (!seg || seg.sec !== b.sec) {
        const s = sections[b.sec];
        const cols = s.cols.num;
        const colW = cols > 1 ? (contentWidth(s) - s.cols.space * (cols - 1)) / cols : contentWidth(s);
        seg = { sec: b.sec, el: h("div", { class: "tf-docx-seg", style: `width:${r2(colW)}px` }), blocks: [] };
        run.segs.push(seg);
        run.galley.append(seg.el);
      }
      seg.el.append(b.el);
      seg.blocks.push(b);
      prevSec = b.sec;
    }
    for (const n of this.o.footnotes.values()) this.noteHost.append(n);
    host.append(this.noteHost);
  }

  private noteHeight(ids: Iterable<string>) {
    let sum = 0;
    let any = false;
    for (const id of ids) {
      any = true;
      sum += this.noteHeights.get(id) ?? 0;
    }
    return any ? sum + 14 : 0;
  }

  /** Phase 2: measure and choose page breaks */
  async paginate(): Promise<Page[]> {
    const { sections, hf, evenAndOdd } = this.o;
    for (const [id, el] of this.o.footnotes) this.noteHeights.set(id, el.offsetHeight);
    hf.measure();
    const pages: Page[] = [];
    let number = 0;
    let secIndex = -1;
    let sectionPage = 0;

    const newPage = (sec: number, hard: boolean, sectionStart: boolean): Page => {
      const s = sections[sec];
      if (sectionStart && sec !== secIndex) {
        // Section restarts page numbering
        if (s.pgNumStart !== null) number = s.pgNumStart - 1;
        // Odd/even page section break: insert a blank page
        if (pages.length && ((s.type === "oddPage" && (number + 1) % 2 === 0) || (s.type === "evenPage" && (number + 1) % 2 === 1))) {
          number++;
          pages.push(this.blankPage(sec, number));
        }
        secIndex = sec;
        sectionPage = 0;
      }
      number++;
      const first = s.titlePg && sectionPage === 0;
      const type: HfType = first ? "first" : evenAndOdd && number % 2 === 0 ? "even" : "default";
      const header = hf.get(sec, "header", type);
      const footer = hf.get(sec, "footer", type);
      const top = Math.max(s.top, header ? s.header + header.height : 0);
      const bottom = Math.max(s.bottom, footer ? s.footer + footer.height : 0);
      const p: Page = { sec, pieces: [], notes: [], hard, number, indexInSection: sectionPage++, header, footer, top, bottom };
      pages.push(p);
      return p;
    };
    const capacity = (p: Page) => Math.max(40, sections[p.sec].height - p.top - p.bottom);

    for (const run of this.runs) {
      const firstSec = run.segs[0]?.sec ?? 0;
      let page = newPage(firstSec, true, run.sectionStart || secIndex < 0);
      let empty = true;
      let used = 0;
      for (const seg of run.segs) {
        if (seg.sec !== secIndex) secIndex = seg.sec;
        const cols = sections[seg.sec].cols.num;
        const segTop = seg.el.getBoundingClientRect().top;
        let piece: Piece = { sec: seg.sec, items: [] };
        page.pieces.push(piece);
        let base = used;
        let startY: number | null = null;
        let lastBottom = 0;
        const blocks = seg.blocks;
        const rect = (el: Element) => {
          const r = el.getBoundingClientRect();
          return { top: r.top - segTop, bottom: r.bottom - segTop };
        };
        const openPage = () => {
          page = newPage(seg.sec, false, false);
          piece = { sec: seg.sec, items: [] };
          page.pieces.push(piece);
          empty = true;
          base = 0;
          used = 0;
        };
        for (let i = 0; i < blocks.length; i++) {
          if ((i & 31) === 0) await breathe();
          const b = blocks[i];
          const m = rect(b.el);
          const meta = this.o.doc.blockMeta.get(b.el);
          // Top of page: hard breaks keep the paragraph's space before (handled the same way when building pages)
          const topGap = page.hard && b.el.tagName === "P" ? (meta?.before ?? 0) : 0;
          if (startY === null || empty) startY = empty ? m.top - topGap : m.top - (parseFloat(b.el.style.marginTop) || 0);
          const notes = new Set(page.notes);
          for (const n of meta?.notes ?? []) notes.add(n);
          const cap = capacity(page);
          const need = base + (m.bottom - startY) / cols + this.noteHeight(notes);
          const splittable = b.el.tagName === "P" && !meta?.keepLines && !b.el.querySelector(".tf-docx-obj");
          if (need <= cap + 0.5 || (empty && !(b.el instanceof HTMLTableElement) && !splittable)) {
            piece.items.push({ el: b.el });
            page.notes = [...notes];
            empty = false;
            lastBottom = m.bottom;
            continue;
          }
          const table = b.el instanceof HTMLTableElement ? b.el : null;
          const rows = table ? Array.from(table.tBodies[0]?.rows ?? []) : [];
          const hdr = Math.min(meta?.headerRows ?? 0, Math.max(0, rows.length - 1));
          const rr = rows.map((r) => rect(r));
          // Table: split on this page only if the header rows plus the first row fit; otherwise move it all to the next page (together with the preceding heading paragraph)
          const firstFits = !!rows.length && base + (rr[Math.min(rows.length - 1, hdr)].bottom - startY) / cols + this.noteHeight(notes) <= cap + 0.5;
          if (table && rows.length && (firstFits || empty)) {
            const hdrH = hdr ? rr[hdr - 1].bottom - rr[0].top : 0;
            let from = 0;
            let tableNotesDone = false;
            let guard = 0;
            while (from < rows.length && guard++ < 10000) {
              const capNow = capacity(page);
              const noteH = this.noteHeight(tableNotesDone ? page.notes : [...page.notes, ...(meta?.notes ?? [])]);
              let k = from - 1;
              while (k + 1 < rows.length && base + (rr[k + 1].bottom - startY!) / cols + noteH <= capNow + 0.5) k++;
              const minRows = from === 0 ? Math.max(hdr, 0) : from;
              if (k < minRows) {
                if (!empty) {
                  openPage();
                  startY = rr[from].top - (from > 0 && hdr ? hdrH : 0);
                  continue;
                }
                k = Math.min(rows.length - 1, minRows);
              }
              piece.items.push({ table, from, to: k + 1 });
              if (!tableNotesDone) {
                page.notes = [...new Set([...page.notes, ...(meta?.notes ?? [])])];
                tableNotesDone = true;
              }
              empty = false;
              lastBottom = rr[k].bottom;
              from = k + 1;
              if (from < rows.length) {
                openPage();
                startY = rr[from].top - (hdr ? hdrH : 0);
              }
            }
            continue;
          }
          // Split the paragraph by line onto the next page (widow/orphan control: at least two lines on each side)
          if (splittable) {
            const lines = lineBoxes(b.el, segTop);
            let partStart = m.top;
            let parts = 0;
            let placed = false;
            for (let guard = 0; guard < 500; guard++) {
              const limit = startY! + (capacity(page) - base - this.noteHeight(notes)) * cols;
              if (m.bottom <= limit + 0.5) {
                placed = true;
                break;
              }
              const bnd = splitAt(lines, partStart, limit, !!meta?.widow);
              const pos = bnd === null ? null : findSplit(b.el, bnd + segTop);
              if (bnd === null || !pos) {
                if (empty) placed = true;
                break;
              }
              const list = this.splits.get(b.el) ?? [];
              list.push(pos);
              this.splits.set(b.el, list);
              piece.items.push({ para: b.el, part: parts++ });
              page.notes = [...notes];
              openPage();
              startY = bnd;
              partStart = bnd;
            }
            if (placed || parts) {
              piece.items.push(parts ? { para: b.el, part: parts } : { el: b.el });
              if (!parts) page.notes = [...notes];
              empty = false;
              lastBottom = m.bottom;
              continue;
            }
          }
          // Keep with next: move the preceding run of keepNext paragraphs to the next page too
          let moveBack = 0;
          for (let j = piece.items.length - 1; j >= 0; j--) {
            const it = piece.items[j];
            if (!("el" in it) || !this.o.doc.blockMeta.get(it.el)?.keepNext) break;
            moveBack++;
          }
          if (moveBack >= piece.items.length || moveBack > 5) moveBack = 0;
          piece.items.splice(piece.items.length - moveBack, moveBack);
          i -= moveBack;
          openPage();
          startY = null;
          i--;
        }
        used = base + (startY === null ? 0 : (lastBottom - startY) / cols);
      }
    }
    // Remove empty segments
    for (const p of pages) p.pieces = p.pieces.filter((x) => x.items.length);
    return pages;
  }

  private blankPage(sec: number, number: number): Page {
    const s = this.o.sections[sec];
    return { sec, pieces: [], notes: [], hard: true, number, indexInSection: 0, header: null, footer: null, top: s.top, bottom: s.bottom, blank: true };
  }

  /** Phase 3: build pages */
  build(pages: Page[]): HTMLElement[] {
    const { sections, doc, background } = this.o;
    const out: HTMLElement[] = [];
    // Split tables (handle vertical merges across the break first, then move rows)
    const slices = new Map<HTMLTableElement, { from: number; to: number }[]>();
    for (const p of pages)
      for (const pc of p.pieces)
        for (const it of pc.items) {
          if (!("table" in it)) continue;
          const list = slices.get(it.table) ?? [];
          list.push({ from: it.from, to: it.to });
          slices.set(it.table, list);
        }
    const partsOf = new Map<HTMLTableElement, HTMLTableElement[]>();
    for (const [table, list] of slices) {
      if (list.length < 2) continue;
      const rows = Array.from(table.tBodies[0].rows);
      const hdr = doc.blockMeta.get(table)?.headerRows ?? 0;
      for (const sl of list.slice(1)) splitRowspans(rows, sl.from);
      const parts: HTMLTableElement[] = [];
      for (const sl of list.slice(1)) {
        const t = table.cloneNode(false) as HTMLTableElement;
        const cg = table.querySelector("colgroup");
        if (cg) t.append(cg.cloneNode(true));
        const body = h("tbody");
        for (let k = 0; k < hdr && k < sl.from; k++) body.append(rows[k].cloneNode(true));
        for (const r of rows.slice(sl.from, sl.to)) body.append(r);
        t.append(body);
        parts.push(t);
      }
      partsOf.set(table, parts);
    }

    const paraParts = new Map<HTMLElement, HTMLElement[]>();
    for (const [p, list] of this.splits) paraParts.set(p, splitParagraph(p, list));

    const total = pages.length;
    const perSection = new Map<number, number>();
    for (const p of pages) perSection.set(p.sec, (perSection.get(p.sec) ?? 0) + 1);
    const partIndex = new Map<HTMLTableElement, number>();

    for (const p of pages) {
      const s = sections[p.sec];
      const page = h("div", {
        class: "tf-docx-page",
        style: css({
          width: `${r2(s.width)}px`,
          "min-height": `${r2(s.height)}px`,
          padding: `${r2(p.top)}px ${r2(s.right)}px ${r2(p.bottom)}px ${r2(s.left + s.gutter)}px`,
          "background-color": background ?? undefined,
        }),
      });
      const body = h("div", { class: "tf-docx-body", style: `min-height:${r2(Math.max(40, s.height - p.top - p.bottom))}px` });
      const flow = h("div", { class: "tf-docx-flow" });
      if (s.vAlign === "center" || s.vAlign === "both") flow.classList.add("tf-docx-vcenter");
      else if (s.vAlign === "bottom") flow.classList.add("tf-docx-vbottom");
      let firstItem = true;
      for (const pc of p.pieces) {
        const ps = sections[pc.sec];
        const segEl = h("div", { class: "tf-docx-seg" });
        if (ps.cols.num > 1) {
          segEl.setAttribute("style", css({ "column-count": String(ps.cols.num), "column-gap": `${r2(ps.cols.space)}px`, "column-rule": ps.cols.sep ? "0.75pt solid #000" : undefined }));
        }
        for (const it of pc.items) {
          let el: HTMLElement;
          if ("table" in it) {
            const parts = partsOf.get(it.table);
            if (!parts || it.from === 0) el = it.table;
            else {
              const idx = partIndex.get(it.table) ?? 0;
              el = parts[idx];
              partIndex.set(it.table, idx + 1);
            }
          } else if ("para" in it) el = paraParts.get(it.para)?.[it.part] ?? it.para;
          else el = it.el;
          if (firstItem) {
            // Top of page: natural breaks drop the paragraph's space before, hard breaks keep it
            if (el.tagName === "P") el.style.marginTop = p.hard ? `${r2(doc.blockMeta.get(el)?.before ?? 0)}px` : "0";
            firstItem = false;
          }
          segEl.append(el);
        }
        flow.append(segEl);
      }
      body.append(flow);
      // Footnotes
      if (p.notes.length) {
        const fn = h("div", { class: "tf-docx-fn" }, h("div", { class: "tf-docx-fnrule" }));
        for (const id of p.notes) {
          const n = this.o.footnotes.get(id);
          if (n) fn.append(n);
        }
        body.append(fn);
      }
      page.append(body);
      // Headers/footers
      const addHf = (inst: HfInstance | null, kind: "header" | "footer") => {
        if (!inst) return;
        const c = inst.el.cloneNode(true) as HTMLElement;
        c.style.left = `${r2(s.left + s.gutter)}px`;
        if (kind === "header") c.style.top = `${r2(s.header)}px`;
        else c.style.bottom = `${r2(s.footer)}px`;
        page.append(c);
        for (const fl of inst.floats) page.append(fl.el.cloneNode(true));
      };
      addHf(p.header, "header");
      addHf(p.footer, "footer");
      // Page borders
      const pb = pageBorder(s, doc, p.indexInSection === 0);
      if (pb) page.append(pb);
      // Page number fields
      for (const f of Array.from(page.querySelectorAll<HTMLElement>(".tf-docx-fld"))) {
        const type = f.dataset.f;
        const fmt = f.dataset.fmt;
        const n = type === "NUMPAGES" ? total : type === "SECTIONPAGES" ? (perSection.get(p.sec) ?? 1) : p.number;
        f.textContent = formatNumber(n, fmt ?? (type === "PAGE" ? s.pgNumFmt : null) ?? "decimal");
      }
      out.push(page);
    }

    // Page-relative floating objects: place them on the page containing their anchor paragraph
    for (const fl of this.o.bodyFloats) {
      const pg = fl.anchor.closest(".tf-docx-page") ?? out[0];
      pg?.append(fl.el);
    }
    return out;
  }
}

interface LineBox {
  top: number;
  bottom: number;
}

const skipInLine = (el: Element) => el.classList.contains("tf-docx-obj") || el.classList.contains("tf-docx-marker");

/** Lines of a paragraph (grouped by the positions of text and inline objects; relative to offset) */
function lineBoxes(p: HTMLElement, offset: number): LineBox[] {
  const rects: LineBox[] = [];
  const range = document.createRange();
  const walk = (el: Element) => {
    for (let c = el.firstChild; c; c = c.nextSibling) {
      if (c.nodeType === Node.TEXT_NODE) {
        if (!(c as Text).length) continue;
        range.selectNodeContents(c);
        for (const r of Array.from(range.getClientRects())) if (r.height > 0) rects.push({ top: r.top - offset, bottom: r.bottom - offset });
      } else if (c instanceof HTMLElement) {
        if (skipInLine(c)) continue;
        if (c.classList.contains("tf-docx-inl")) {
          const r = c.getBoundingClientRect();
          rects.push({ top: r.top - offset, bottom: r.bottom - offset });
        } else walk(c);
      }
    }
  };
  walk(p);
  rects.sort((a, b) => a.top - b.top);
  const lines: LineBox[] = [];
  for (const r of rects) {
    const cur = lines[lines.length - 1];
    if (cur && r.top < cur.bottom - 2) cur.bottom = Math.max(cur.bottom, r.bottom);
    else lines.push({ ...r });
  }
  return lines;
}

/** How many lines fit before limit: returns the split position; with widow/orphan control, at least two lines on each side */
function splitAt(all: LineBox[], from: number, limit: number, widow: boolean): number | null {
  const lines = all.filter((l) => l.top >= from - 1);
  const n = lines.length;
  if (n < 2) return null;
  const bnd = (k: number) => (lines[k - 1].bottom + lines[k].top) / 2;
  let k = 0;
  while (k + 1 < n && bnd(k + 1) <= limit) k++;
  if (widow) {
    if (n - k < 2) k = n - 2;
    if (k < 2) return null;
  }
  if (k < 1) return null;
  return bnd(k);
}

function textNodes(p: HTMLElement): Text[] {
  const out: Text[] = [];
  const walk = (el: Element) => {
    for (let c = el.firstChild; c; c = c.nextSibling) {
      if (c.nodeType === Node.TEXT_NODE) {
        if ((c as Text).length) out.push(c as Text);
      } else if (c instanceof HTMLElement && !skipInLine(c) && !c.classList.contains("tf-docx-inl")) walk(c);
    }
  };
  walk(p);
  return out;
}

/** Find the first character after the split (viewport coordinates) */
function findSplit(p: HTMLElement, y: number): { node: Text; offset: number } | null {
  const range = document.createRange();
  const below = (node: Text, i: number) => {
    range.setStart(node, i);
    range.setEnd(node, i + 1);
    const r = range.getBoundingClientRect();
    return r.height > 0 && (r.top + r.bottom) / 2 > y;
  };
  const nodes = textNodes(p);
  for (let n = 0; n < nodes.length; n++) {
    const node = nodes[n];
    if (!below(node, node.length - 1)) continue;
    let lo = 0;
    let hi = node.length - 1;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (below(node, mid)) hi = mid;
      else lo = mid + 1;
    }
    if (n === 0 && lo === 0) return null;
    return { node, offset: lo };
  }
  return null;
}

/** Cut a paragraph into multiple <p> at the split positions (later segments don't repeat the first-line indent and space before) */
function splitParagraph(p: HTMLElement, list: { node: Text; offset: number }[]): HTMLElement[] {
  const parts: HTMLElement[] = [];
  const range = document.createRange();
  for (let i = list.length - 1; i >= 0; i--) {
    const { node, offset } = list[i];
    if (!p.contains(node)) continue;
    range.setStart(node, offset);
    range.setEnd(p, p.childNodes.length);
    const frag = range.extractContents();
    const np = p.cloneNode(false) as HTMLElement;
    np.append(frag);
    np.style.textIndent = "0";
    np.style.marginTop = "0";
    np.style.borderTop = "";
    if (parts.length) np.style.marginBottom = "0";
    parts.unshift(np);
  }
  if (parts.length) {
    p.style.marginBottom = "0";
    // The last line of an earlier segment is not the paragraph end, so it must be stretched when justified
    for (const x of [p, ...parts.slice(0, -1)]) if (x.style.textAlign === "justify") x.style.textAlignLast = "justify";
  }
  return [p, ...parts];
}

/** Split a table at row `at`: for vertically merged cells crossing the split, add continuation cells in the new part */
function splitRowspans(rows: HTMLTableRowElement[], at: number) {
  const target = rows[at];
  if (!target) return;
  for (let r = 0; r < at; r++) {
    for (const td of Array.from(rows[r].cells)) {
      const span = td.rowSpan || 1;
      if (r + span <= at) continue;
      td.rowSpan = at - r;
      const filler = td.cloneNode(false) as HTMLTableCellElement;
      filler.rowSpan = r + span - at;
      filler.append(h("p", { class: "tf-docx-p" }, h("br")));
      const col = Number(td.dataset.col ?? 0);
      const before = Array.from(target.cells).find((c) => Number(c.dataset.col ?? -1) > col);
      target.insertBefore(filler, before ?? null);
    }
  }
}

/** Page borders (w:pgBorders) */
function pageBorder(s: Section, doc: DocCtx, firstOfSection: boolean): HTMLElement | null {
  const pb = s.pgBorders;
  if (!pb) return null;
  const display = attr(pb, "display") ?? "allPages";
  if ((display === "firstPage" && !firstOfSection) || (display === "notFirstPage" && firstOfSection)) return null;
  const fromText = attr(pb, "offsetFrom") === "text";
  const st: Record<string, string> = {};
  let any = false;
  for (const side of ["top", "left", "bottom", "right"] as const) {
    const el = kids(pb).find((c) => c.localName === side || (side === "left" && c.localName === "start") || (side === "right" && c.localName === "end")) ?? null;
    const b = borderCss(el, doc.theme);
    if (!b || b === "none") continue;
    any = true;
    st[`border-${side}`] = b;
    const space = borderSpace(el) * PT;
    const margin = side === "top" ? s.top : side === "bottom" ? s.bottom : side === "left" ? s.left : s.right;
    st[side] = `${r2(fromText ? Math.max(0, margin - space - 4) : space)}px`;
  }
  if (!any) return null;
  const el = h("div", { class: "tf-docx-pgborder", style: css(st) });
  if (attr(pb, "zOrder") === "back") el.style.zIndex = "-1";
  return el;
}
