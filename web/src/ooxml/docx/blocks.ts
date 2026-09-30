/**
 * Block level (paragraphs, tables, content controls): paragraph spacing (previous paragraph's space-after + this one's space-before; Word adds them instead of taking the larger like HTML),
 * spacing between paragraphs of the same style (contextualSpacing), border merging of adjacent paragraphs, and page breaks.
 */

import { kid } from "../core/package";
import type { Flow } from "./context";
import { renderParagraph, type ParaResult } from "./paragraph";
import { renderTable } from "./table";
import { flatKids } from "./xml";

export interface BlockItem {
  el: HTMLElement;
  pageBreak: boolean;
  columnBreak: boolean;
}

const px = (n: number) => `${Math.round(n * 100) / 100}px`;


interface Last {
  res: ParaResult;
  first: HTMLElement;
  last: HTMLElement;
}

export class BlockWriter {
  private prev: Last | null = null;
  private breakNext = false;

  constructor(
    private f: Flow,
    private emit: (item: BlockItem) => void,
  ) {}

  blocks(children: Element[]) {
    for (const c of children) this.block(c);
  }

  block(c: Element) {
    try {
      this.render(c);
    } catch (e) {
      // Skip a single broken block without affecting the rest of the content
      console.warn("docx block", e);
    }
  }

  private render(c: Element) {
    switch (c.localName) {
      case "p":
        this.paragraph(c);
        break;
      case "tbl":
        this.table(c);
        break;
      case "sdt":
        this.blocks(flatKids(kid(c, "sdtContent")));
        break;
      case "customXml":
      case "smartTag":
        this.blocks(flatKids(c));
        break;
      case "ins":
      case "moveTo":
        this.blocks(flatKids(c));
        break;
    }
  }

  private paragraph(p: Element) {
    const f = this.f;
    const outer = f.notes;
    f.notes = [];
    const res = renderParagraph(p, f);
    const notes = f.notes;
    f.notes = outer;
    if (notes.length) outer?.push(...notes);
    if (res.sectionOnly) {
      if (res.breakAfter) this.breakNext = true;
      return;
    }
    const segs = res.segments;
    const first = segs[0].el;
    const last = segs[segs.length - 1].el;
    const meta = f.doc.meta(first);
    const prev = this.prev;

    // Space before: added to the previous paragraph's space after; ignored for the same style with contextualSpacing
    let before = res.before;
    let prevAfter = prev ? prev.res.after : 0;
    if (prev && prev.res.styleId === res.styleId) {
      if (res.props.contextualSpacing) before = 0;
      if (prev.res.props.contextualSpacing) prevAfter = 0;
    }
    meta.before = before;
    const merge = !!prev && !!res.borderKey && res.borderKey === prev.res.borderKey;
    if (merge) {
      // Border merging: inner top/bottom borders become "between", spacing goes inside the border
      prev!.last.style.borderBottom = "none";
      prev!.last.style.paddingBottom = px(prevAfter);
      prev!.last.style.marginBottom = "0";
      if (res.box.between) {
        first.style.borderTop = res.box.between;
        first.style.paddingTop = px(before + res.box.topSpace);
      } else first.style.paddingTop = px(before);
      first.style.marginTop = "0";
    } else {
      if (prev) prev.last.style.marginBottom = "0";
      // Shading excludes space before/after (like Word, consecutive shaded paragraphs have gaps between them)
      const gap = prevAfter + before;
      if (gap) first.style.marginTop = px(gap);
      if (res.box.top) {
        first.style.borderTop = res.box.top;
        first.style.paddingTop = px(res.box.topSpace);
      }
    }
    if (res.box.bottom) {
      last.style.borderBottom = res.box.bottom;
      last.style.paddingBottom = px(res.box.bottomSpace);
    }
    if (res.after) last.style.marginBottom = px(res.after);
    if (notes.length) meta.notes = notes;
    if (res.props.keepNext) f.doc.meta(last).keepNext = true;
    if (res.props.keepLines || res.props.framePr) meta.keepLines = true;
    if (res.props.widowControl) meta.widow = true;

    const breakable = f.story === "body" && !f.inCell && f.depth === 0;
    segs.forEach((s, i) => {
      const pageBreak = (i === 0 && (this.breakNext || (breakable && !!res.props.pageBreakBefore))) || s.pageBreak;
      this.emit({ el: s.el, pageBreak, columnBreak: s.columnBreak });
    });
    this.breakNext = res.breakAfter;
    this.prev = { res, first, last };
  }

  private table(tbl: Element) {
    const f = this.f;
    const outer = f.notes;
    f.notes = [];
    const el = renderTable(tbl, f);
    const notes = f.notes;
    f.notes = outer;
    if (notes.length) outer?.push(...notes);
    if (!el) return;
    if (notes.length) f.doc.meta(el).notes = notes;
    this.emit({ el, pageBreak: this.breakNext, columnBreak: false });
    this.breakNext = false;
    this.prev = null;
  }
}

/** Render blocks into a container (table cell, text box, header/footer, footnote) */
export function fillBlocks(container: HTMLElement, children: Element[], f: Flow) {
  new BlockWriter(f, (item) => container.append(item.el)).blocks(children);
}
