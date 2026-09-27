/**
 * Paragraphs and their content: text, tabs, line breaks, symbols, fields, hyperlinks, bookmarks, revisions, comment ranges, images.
 *
 * A page break inside a paragraph cuts it into segments (each a <p>); the block renderer decides where pages break.
 */

import { attr, css, h, kid, kids, numAttr, safeHref } from "@/lib/office/ooxml";
import type { Flow, TabRef } from "@/lib/office/docx/context";
import { renderDrawing, renderPictureBullet } from "@/lib/office/docx/drawing";
import { bulletText, cleanFace, mapSymbol } from "@/lib/office/docx/fonts";
import { renderMath } from "@/lib/office/docx/math";
import { estimateWidthEm, formatNumber } from "@/lib/office/docx/numfmt";
import { borderCss, borderSig, borderSpace, borderWidthPt, ratioOf, runStyle, shadeColor, type RunStyle } from "@/lib/office/docx/props";
import { gridPitchPt } from "@/lib/office/docx/section";
import { applyPPr, applyRPr, cascadeRun, emptyPara, emptyRun, type ParaProps, type RunProps } from "@/lib/office/docx/styles";
import { renderVml } from "@/lib/office/docx/vml";
import { flatKids, tw, val } from "@/lib/office/docx/xml";

export interface Segment {
  el: HTMLElement;
  pageBreak: boolean;
  columnBreak: boolean;
}

export interface ParaResult {
  segments: Segment[];
  props: ParaProps;
  styleId?: string;
  /** px */
  before: number;
  after: number;
  /** Signature for border merging (empty string means no border) */
  borderKey: string;
  /** Paragraph ends with a page break: the next block starts on a new page */
  breakAfter: boolean;
  /** Empty paragraph carrying only section properties (not displayed) */
  sectionOnly: boolean;
  /** Border info (used for merging) */
  box: { top: string | null; bottom: string | null; between: string | null; topSpace: number; bottomSpace: number };
}

const PT = 96 / 72;

/** Resolve paragraph properties: document defaults → table style → (numbering from style) → paragraph style chain → (direct numbering) → direct formatting */
export function paraProps(pPr: Element | null, f: Flow, styleId: string | undefined): { props: ParaProps; lvlPPr: Element | null } {
  const st = f.doc.styles;
  const layers: [Element | null, boolean][] = [[st.docPPr, true]];
  for (const x of st.tablePPrs(f.tableStyle?.id, f.tableStyle?.conds ?? [])) layers.push([x, true]);
  const styleLayers: [Element | null, boolean][] = st.stylePPrs(styleId).map((x) => [x, true]);
  const build = (lvl: Element | null, fromStyle: boolean) => {
    const out = emptyPara();
    for (const [x, s] of layers) applyPPr(out, x, s);
    if (lvl && fromStyle) applyPPr(out, lvl, true);
    for (const [x, s] of styleLayers) applyPPr(out, x, s);
    if (lvl && !fromStyle) applyPPr(out, lvl, true);
    applyPPr(out, pPr, false);
    return out;
  };
  const first = build(null, false);
  if (first.numId && first.numId !== "0") {
    const lvl = f.doc.numbering.level(first.numId, first.ilvl ?? 0);
    if (lvl?.pPr) return { props: build(lvl.pPr, !!first.numFromStyle), lvlPPr: lvl.pPr };
  }
  return { props: first, lvlPPr: null };
}

/** Run property layers for a paragraph (table style, paragraph style) */
function paraRunLayers(f: Flow, styleId: string | undefined): RunProps[] {
  const st = f.doc.styles;
  const out: RunProps[] = [];
  const t = st.tableRun(f.tableStyle?.id, f.tableStyle?.conds ?? []);
  if (t) out.push(t);
  const p = st.styleRun(styleId);
  if (p) out.push(p);
  return out;
}

interface FieldInfo {
  type: string;
  args: string;
}

function parseInstr(instr: string): FieldInfo {
  const s = instr.trim();
  const m = /^(\S+)\s*([\s\S]*)$/.exec(s);
  return { type: (m?.[1] ?? "").toUpperCase(), args: m?.[2] ?? "" };
}

/** The \* format switch in a field (for page numbers) */
function fieldFormat(args: string): string | null {
  const m = /\\\*\s*(\w+)/.exec(args);
  if (!m) return null;
  const map: Record<string, string> = { roman: "lowerRoman", ROMAN: "upperRoman", alphabetic: "lowerLetter", ALPHABETIC: "upperLetter", Arabic: "decimal", ArabicDash: "numberInDash", CHINESENUM1: "taiwaneseCounting", CHINESENUM2: "ideographLegalTraditional", CHINESENUM3: "taiwaneseCountingThousand", CardText: "cardinalText", OrdText: "ordinalText", Ordinal: "ordinal" };
  return map[m[1]] ?? null;
}

const bookmarkId = (name: string) => `tf-bm-${name.replace(/[\s"'<>#%]/g, "_")}`;

/** How line height is computed */
interface LineMode {
  kind: "auto" | "exact" | "atLeast" | "grid";
  mult: number;
  /** pt: the exact/atLeast value, or the grid pitch for grid */
  value: number;
}

class InlineRenderer {
  readonly segments: Segment[] = [];
  private p: HTMLElement;
  private sinks: HTMLElement[];
  private rev: "ins" | "del" | null = null;
  private lastSpan: HTMLElement | null = null;
  private lastKey = "";
  /** Style of the first text run (the paragraph's strut font) */
  strut: RunStyle | null = null;
  strutRatio = 0;
  maxSize = 0;
  maxRatio = 0;
  hasContent = false;
  endsWithBr = false;
  readonly tabs: TabRef[] = [];
  private noteStyle = false;

  constructor(
    private f: Flow,
    private base: RunProps,
    private layers: RunProps[],
    private mode: LineMode,
    private pLeft: number,
    private indent: number,
    private stops: TabRef["stops"],
  ) {
    this.p = h("p", { class: "tf-docx-p" });
    this.segments.push({ el: this.p, pageBreak: false, columnBreak: false });
    this.sinks = [this.p];
    // Hyperlink field spanning paragraphs: recreate the link in the new paragraph
    for (const fs of f.fields) if (fs.link && fs.phase === "result") this.push(fs.link());
  }

  private get sink() {
    return this.sinks[this.sinks.length - 1];
  }

  private push(el: HTMLElement) {
    this.sink.append(el);
    this.sinks.push(el);
    this.lastSpan = null;
  }

  private pop(el: HTMLElement) {
    const i = this.sinks.lastIndexOf(el);
    if (i > 0) this.sinks.length = i;
    this.lastSpan = null;
  }

  /** Inside field code or a hidden field result: emit no content */
  private get muted() {
    return this.f.fields.some((x) => x.phase === "code" || x.suppress);
  }

  private newSegment(page: boolean, column: boolean) {
    const p = h("p", { class: "tf-docx-p" });
    this.segments.push({ el: p, pageBreak: page, columnBreak: column });
    // Recreate nested containers such as links in the new paragraph
    const nested = this.sinks.slice(1).map((s) => s.cloneNode(false) as HTMLElement);
    this.p = p;
    this.sinks = [p];
    for (const n of nested) this.push(n);
    this.lastSpan = null;
    this.endsWithBr = false;
  }

  /** Decide from the paragraph's line-height rule whether an individual run needs its own line height */
  private lineHeightFor(rs: RunStyle, text: string): string | undefined {
    const ratio = ratioOf(rs.fonts, text);
    this.maxSize = Math.max(this.maxSize, rs.size);
    this.maxRatio = Math.max(this.maxRatio, ratio);
    if (!this.strut) {
      this.strut = rs;
      this.strutRatio = ratio;
      return undefined;
    }
    if (this.mode.kind === "auto" && Math.abs(ratio - this.strutRatio) > 0.02) return String(Math.round(ratio * this.mode.mult * 1000) / 1000);
    if (this.mode.kind === "atLeast") {
      const natural = rs.size * ratio;
      if (natural > this.mode.value) return `${Math.round(natural * 100) / 100}pt`;
    }
    return undefined;
  }

  private classes(extra?: string) {
    let c = extra ?? "";
    if (this.rev) c += ` tf-docx-${this.rev}`;
    if (this.f.comments.size) c += " tf-docx-cmt";
    return c.trim();
  }

  text(text: string, rs: RunStyle) {
    if (!text || this.muted || rs.hidden) return;
    const lh = this.lineHeightFor(rs, text);
    const st$: Record<string, string | undefined> = { ...rs.css, "line-height": lh };
    // Footnote reference: shrink and raise automatically when the style doesn't set superscript
    if (this.noteStyle) st$["font-size"] = `${Math.round(rs.size * 0.65 * 100) / 100}pt`;
    const style = css(st$);
    const cls = this.classes(this.noteStyle ? "tf-docx-sup" : rs.scale ? "tf-docx-sx" : undefined);
    const key = `${style}|${cls}`;
    if (this.lastSpan && key === this.lastKey && this.lastSpan.parentElement === this.sink && this.sink.lastChild === this.lastSpan && !rs.scale) {
      this.lastSpan.append(text);
    } else {
      const span = h("span", { style, class: cls || undefined }, text);
      if (rs.scale) {
        // Horizontal scaling: fix up spacing from the actual width after layout
        span.dataset.sx = String(rs.scale);
        this.f.doc.scaled.push(span);
      }
      this.sink.append(span);
      this.lastSpan = span;
      this.lastKey = key;
    }
    this.hasContent = true;
    this.endsWithBr = false;
  }

  /** Non-text inline elements (tabs, images) */
  inlineEl(el: Node, rs?: RunStyle) {
    if (this.muted || rs?.hidden) return;
    this.sink.append(el);
    this.lastSpan = null;
    this.hasContent = true;
    this.endsWithBr = false;
  }

  tab(rs: RunStyle, ptab?: TabRef["ptab"]) {
    if (this.muted || rs.hidden) return;
    const el = h("span", { class: "tf-docx-tab", style: css({ "font-size": rs.css["font-size"], "font-family": rs.family, color: rs.css.color }) });
    this.lineHeightFor(rs, " ");
    this.inlineEl(el);
    this.tabs.push({ el, p: this.p, pLeft: this.pLeft, indent: this.indent, stops: this.stops, defaultTab: this.f.doc.settings.defaultTab, ptab });
  }

  br(type: string | null, clear: string | null, rs: RunStyle) {
    if (this.muted) return;
    const f = this.f;
    const breakable = f.story === "body" && !f.inCell && f.depth === 0;
    if (type === "page" || type === "column") {
      if (!breakable) return;
      const column = type === "column" && f.section.cols.num > 1;
      this.newSegment(!column, column);
      return;
    }
    this.lineHeightFor(rs, " ");
    this.sink.append(h("br", clear && clear !== "none" ? { style: "clear:both" } : null));
    this.lastSpan = null;
    this.hasContent = true;
    this.endsWithBr = true;
  }

  // ───────────── Traversal ─────────────

  content(parent: Element) {
    for (const c of flatKids(parent)) this.node(c);
  }

  node(c: Element) {
    const f = this.f;
    switch (c.localName) {
      case "r":
        this.run(c);
        break;
      case "hyperlink":
        this.hyperlink(c);
        break;
      case "fldSimple":
        this.fldSimple(c);
        break;
      case "ins":
      case "moveTo": {
        const prev = this.rev;
        this.rev = "ins";
        this.content(c);
        this.rev = prev;
        break;
      }
      case "del":
      case "moveFrom": {
        const prev = this.rev;
        this.rev = "del";
        this.content(c);
        this.rev = prev;
        break;
      }
      case "sdt":
        this.content(kid(c, "sdtContent") ?? c);
        break;
      case "smartTag":
      case "customXml":
      case "dir":
      case "bdo":
        this.content(c);
        break;
      case "bookmarkStart": {
        const name = attr(c, "name");
        if (name && name !== "_GoBack" && !this.muted) {
          this.sink.append(h("a", { id: bookmarkId(name), class: "tf-docx-bm" }));
          this.lastSpan = null;
        }
        break;
      }
      case "commentRangeStart":
        f.comments.add(attr(c, "id") ?? "");
        this.lastSpan = null;
        break;
      case "commentRangeEnd":
        f.comments.delete(attr(c, "id") ?? "");
        this.lastSpan = null;
        break;
      case "oMath":
      case "oMathPara":
        if (!this.muted) {
          const base = runStyle(cascadeRun(this.base, this.layers, null), f.doc.theme, f.doc.settings.script);
          this.lineHeightFor(base, "x");
          this.inlineEl(renderMath(c, base.size));
        }
        break;
    }
  }

  private hyperlink(c: Element) {
    const f = this.f;
    const id = attr(c, "id");
    const anchor = attr(c, "anchor");
    let a: HTMLElement | null = null;
    if (id) {
      const rel = f.rels.find((r) => r.id === id);
      const href = safeHref(rel?.target);
      if (href) a = h("a", { href: anchor ? `${href}#${encodeURIComponent(anchor)}` : href, target: "_blank", rel: "noopener noreferrer" });
    } else if (anchor) a = h("a", { href: `#${bookmarkId(anchor)}` });
    if (a && !this.muted) {
      const tip = attr(c, "tooltip");
      if (tip) a.title = tip;
      a.className = "tf-docx-link";
      this.push(a);
      this.content(c);
      this.pop(a);
    } else this.content(c);
  }

  private fldSimple(c: Element) {
    const info = parseInstr(attr(c, "instr") ?? "");
    const rPr = kid(kids(c, "r")[0], "rPr");
    if (["PAGE", "NUMPAGES", "SECTIONPAGES"].includes(info.type)) {
      this.pageField(info, this.styleOf(rPr));
      return;
    }
    if (info.type === "HYPERLINK") {
      const a = this.fieldLink(info.args);
      if (a && !this.muted) {
        this.push(a);
        this.content(c);
        this.pop(a);
        return;
      }
    }
    this.content(c);
  }

  private fieldLink(args: string): HTMLElement | null {
    const local = /\\l\s+"([^"]*)"/.exec(args);
    const url = /^\s*"([^"]*)"/.exec(args) ?? /^\s*([^\s\\]+)/.exec(args);
    const href = safeHref(url?.[1]);
    if (href) return h("a", { href, target: "_blank", rel: "noopener noreferrer", class: "tf-docx-link" });
    if (local) return h("a", { href: `#${bookmarkId(local[1])}`, class: "tf-docx-link" });
    return null;
  }

  private pageField(info: FieldInfo, rs: RunStyle) {
    if (this.muted || rs.hidden) return;
    const fmt = fieldFormat(info.args);
    const el = h("span", { class: "tf-docx-fld", "data-f": info.type, "data-fmt": fmt ?? undefined, style: css(rs.css) }, "1");
    this.lineHeightFor(rs, "1");
    this.inlineEl(el);
  }

  private styleOf(rPr: Element | null): RunStyle {
    const direct = rPr ? applyRPr(emptyRun(), rPr) : null;
    const st = this.f.doc.styles;
    const props = cascadeRun(this.base, [...this.layers, ...(direct?.rStyle ? [st.styleRun(direct.rStyle)] : [])], direct);
    return runStyle(props, this.f.doc.theme, this.f.doc.settings.script);
  }

  private fldChar(c: Element, rs: RunStyle) {
    const f = this.f;
    const t = attr(c, "fldCharType");
    if (t === "begin") {
      f.fields.push({ instr: "", phase: "code", type: "", suppress: false, emitted: false, ffData: kid(c, "ffData") });
    } else if (t === "separate") {
      const top = f.fields[f.fields.length - 1];
      if (!top || top.phase !== "code") return;
      const info = parseInstr(top.instr);
      top.type = info.type;
      // Switch to the result section first; emit output only when the enclosing field isn't in its code section
      top.phase = "result";
      if (["PAGE", "NUMPAGES", "SECTIONPAGES"].includes(info.type)) {
        this.pageField(info, rs);
        top.emitted = true;
        top.suppress = true;
      } else if (info.type === "HYPERLINK" && !this.muted) {
        const args = info.args;
        const make = () => this.fieldLink(args) ?? h("span");
        const a = make();
        top.link = make;
        this.push(a);
      }
    } else if (t === "end") {
      const top = f.fields.pop();
      if (!top) return;
      if (top.phase === "code") {
        // Field without a result section
        top.phase = "result";
        const info = parseInstr(top.instr);
        if (!this.muted) {
          if (["PAGE", "NUMPAGES", "SECTIONPAGES"].includes(info.type)) this.pageField(info, rs);
          else if (info.type === "FORMCHECKBOX") {
            const cb = kid(top.ffData, "checkBox");
            const checked = attr(kid(cb, "checked"), "val") ?? attr(kid(cb, "default"), "val");
            this.text(checked === "1" || checked === "true" ? "☒" : "☐", rs);
          }
        }
      } else if (top.link) {
        // Close link: find the last link on the current stack
        for (let i = this.sinks.length - 1; i > 0; i--) {
          if (this.sinks[i].tagName === "A" || this.sinks[i].tagName === "SPAN") {
            this.pop(this.sinks[i]);
            break;
          }
        }
      }
    }
  }

  private run(r: Element) {
    const f = this.f;
    const doc = f.doc;
    const rPr = kid(r, "rPr");
    const rs = this.styleOf(rPr);
    const prevNote = this.noteStyle;
    for (const c of flatKids(r)) {
      switch (c.localName) {
        case "t":
          this.text((c.textContent ?? "").replace(/[\r\n]/g, ""), rs);
          break;
        case "delText":
          if (this.rev === "del") this.text(c.textContent ?? "", rs);
          break;
        case "instrText": {
          const top = f.fields[f.fields.length - 1];
          if (top && top.phase === "code") top.instr += c.textContent ?? "";
          break;
        }
        case "fldChar":
          this.fldChar(c, rs);
          break;
        case "tab":
          this.tab(rs);
          break;
        case "ptab":
          this.tab(rs, { align: attr(c, "alignment") ?? "left", width: f.width });
          break;
        case "br":
          this.br(attr(c, "type"), attr(c, "clear"), rs);
          break;
        case "cr":
          this.br(null, null, rs);
          break;
        case "sym": {
          const font = cleanFace(attr(c, "font"));
          const code = parseInt(attr(c, "char") ?? "", 16);
          if (!Number.isFinite(code)) break;
          const mapped = mapSymbol(font, code);
          if (mapped) this.text(mapped, rs);
          else this.text(String.fromCodePoint(code), { ...rs, css: { ...rs.css, "font-family": `"${font ?? "Symbol"}"` } });
          break;
        }
        case "noBreakHyphen":
          this.text("\u2011", rs);
          break;
        case "softHyphen":
          this.text("\u00ad", rs);
          break;
        case "pgNum":
          this.pageField({ type: "PAGE", args: "" }, rs);
          break;
        case "drawing":
          this.drawing(c, rs);
          break;
        case "pict":
        case "object": {
          if (this.muted || rs.hidden) break;
          const inner = c.localName === "object" ? kid(c, "drawing") : null;
          if (inner) {
            this.drawing(inner, rs);
            break;
          }
          const out = renderVml(c, f, this.p);
          if (out) this.placeObject(out.el, out.mode, rs);
          break;
        }
        case "footnoteReference":
        case "endnoteReference": {
          const kind = c.localName === "footnoteReference" ? "foot" : "end";
          const id = attr(c, "id") ?? "";
          const custom = attr(c, "customMarkFollows");
          let mark = "";
          if (!custom || /^(0|false)$/i.test(custom)) {
            const n = kind === "foot" ? ++doc.footCount : ++doc.endCount;
            const fmt = kind === "foot" ? f.section.footnoteFmt ?? doc.settings.footnoteFmt ?? "decimal" : f.section.endnoteFmt ?? doc.settings.endnoteFmt ?? "lowerRoman";
            mark = formatNumber(n, fmt);
          }
          doc.noteRefs.push({ kind, id, mark });
          if (kind === "foot") (f.notes ??= []).push(id);
          if (mark) {
            this.noteStyle = !rs.css.top;
            this.text(mark, rs);
            this.noteStyle = prevNote;
          }
          break;
        }
        case "footnoteRef":
        case "endnoteRef":
          if (f.noteMark) {
            this.noteStyle = !rs.css.top;
            this.text(f.noteMark, rs);
            this.noteStyle = prevNote;
          }
          break;
        case "ruby":
          this.ruby(c, rs);
          break;
        case "separator":
          if (!this.muted) this.inlineEl(h("span", { class: "tf-docx-fnsep" }));
          break;
        case "continuationSeparator":
          if (!this.muted) this.inlineEl(h("span", { class: "tf-docx-fnsep tf-docx-fnsep-wide" }));
          break;
      }
    }
  }

  private ruby(c: Element, rs: RunStyle) {
    if (this.muted) return;
    const textOf = (el: Element | null) =>
      kids(el, "r")
        .flatMap((r) => kids(r, "t"))
        .map((t) => t.textContent ?? "")
        .join("");
    const base = textOf(kid(c, "rubyBase"));
    const top = textOf(kid(c, "rt"));
    this.lineHeightFor(rs, base);
    this.inlineEl(h("ruby", { style: css(rs.css) }, base, h("rt", null, top)));
  }

  private drawing(c: Element, rs: RunStyle) {
    if (this.muted || rs.hidden) return;
    const out = renderDrawing(c, this.f, this.p, this.pLeft);
    if (out) this.placeObject(out.el, out.mode, rs);
  }

  /** Place pictures and shapes into the paragraph according to their layout */
  private placeObject(el: HTMLElement, mode: string, rs: RunStyle) {
    switch (mode) {
      case "inline":
        this.lineHeightFor(rs, "x");
        this.inlineEl(el);
        break;
      case "page":
        this.f.floats.push({ el, anchor: this.p });
        break;
      case "para":
        this.p.classList.add("tf-docx-rel");
        this.p.append(el);
        break;
      default:
        // float/block: put at the start of the paragraph so text wraps from the paragraph's top
        this.p.insertBefore(el, this.p.firstChild);
        break;
    }
  }
}

/** Paragraph line-height settings */
function lineMode(props: ParaProps, f: Flow): LineMode {
  const sp = props.spacing;
  const rule = sp.lineRule ?? "auto";
  const line = sp.line ?? 240;
  if (rule === "exact") return { kind: "exact", mult: 1, value: Math.abs(line) / 20 };
  if (rule === "atLeast") return { kind: "atLeast", mult: 1, value: line / 20 };
  const mult = line > 0 ? line / 240 : 1;
  const pitch = gridPitchPt(f.section);
  if (pitch && props.snapToGrid !== false) return { kind: "grid", mult, value: pitch };
  return { kind: "auto", mult, value: 0 };
}

const pxOf = (pt: number) => Math.round(pt * PT * 100) / 100;

export function renderParagraph(p: Element, f: Flow): ParaResult {
  const doc = f.doc;
  const st = doc.styles;
  const pPr = kid(p, "pPr");
  const styleId = st.paraStyleId(val(pPr, "pStyle"));
  const { props } = paraProps(pPr, f, styleId);
  const layers = paraRunLayers(f, styleId);
  const base = st.baseRunProps();
  const markDirect = props.markRPr ? applyRPr(emptyRun(), props.markRPr) : null;
  const markProps = cascadeRun(base, layers, markDirect);
  const markStyle = runStyle(markProps, doc.theme, doc.settings.script);
  const mode = lineMode(props, f);
  const bidi = !!props.bidi;

  // Character-unit indents are computed from the paragraph font size
  const fontPt = markStyle.size;
  const ind = props.ind;
  const chars = (v: number | undefined) => (v ? (v / 100) * fontPt * PT : null);
  const indLeft = chars(ind.leftChars) ?? tw(ind.left ?? 0);
  const indRight = chars(ind.rightChars) ?? tw(ind.right ?? 0);
  const first = chars(ind.firstChars) ?? tw(ind.first ?? 0);

  // Borders
  const theme = doc.theme;
  const bTop = borderCss(props.borders.top, theme);
  const bBottom = borderCss(props.borders.bottom, theme);
  const bLeft = borderCss(props.borders.left, theme);
  const bRight = borderCss(props.borders.right, theme);
  const bBetween = borderCss(props.borders.between, theme);
  const has = (b: string | null) => !!b && b !== "none";
  const sLeft = has(bLeft) ? borderSpace(props.borders.left) * PT : 0;
  const sRight = has(bRight) ? borderSpace(props.borders.right) * PT : 0;
  const pLeft = indLeft - (has(bLeft) ? sLeft + borderWidthPt(bLeft) * PT : 0);
  const pRight = indRight - (has(bRight) ? sRight + borderWidthPt(bRight) * PT : 0);
  const stopsPx = props.tabs.map((t) => ({ pos: tw(t.pos), val: t.val, leader: t.leader }));

  const inl = new InlineRenderer(f, base, layers, mode, bidi ? pRight : pLeft, bidi ? indRight : indLeft, stopsPx);
  inl.content(p);

  // Empty paragraph: section properties only
  const sectionOnly = !!kid(pPr, "sectPr") && !inl.hasContent && inl.segments.length === 1 && !p.getElementsByTagNameNS("*", "drawing").length && !p.getElementsByTagNameNS("*", "pict").length;

  // Page break at paragraph end: since Word 2013 the paragraph mark stays on the previous page and the next paragraph starts a new page
  let breakAfter = false;
  const lastSeg = inl.segments[inl.segments.length - 1];
  if (inl.segments.length > 1 && lastSeg.pageBreak && !lastSeg.el.childNodes.length) {
    inl.segments.pop();
    breakAfter = true;
  }

  // Numbering
  let markerEl: HTMLElement | null = null;
  if (props.numId && props.numId !== "0") {
    const m = doc.numbering.next(props.numId, props.ilvl ?? 0);
    if (m) {
      const lp = cascadeRun(markProps, [], m.level.rPr ? applyRPr(emptyRun(), m.level.rPr) : null);
      // Numbering does not take the paragraph mark's underline, highlight, or revision formatting
      const ms = runStyle(lp, theme, doc.settings.script);
      if (!ms.hidden) {
        markerEl = h("span", { class: "tf-docx-marker" });
        let body: Node | null = null;
        if (m.level.picBullet) body = renderPictureBullet(m.level.picBullet, f, ms.size);
        let txt = m.text;
        let style = { ...ms.css };
        if (!body && m.level.fmt === "bullet") {
          const bt = bulletText(txt, ms.fonts.latin);
          txt = bt.text;
          if (!bt.keepFont) style = { ...style, "font-family": markStyle.family };
          // Bullets are not underlined
          delete style["text-decoration-line"];
        }
        markerEl.setAttribute("style", css(style));
        markerEl.append(body ?? txt);
        const widthEm = body ? 1 : estimateWidthEm(txt);
        if (m.level.jc === "right") markerEl.style.marginLeft = `${-widthEm}em`;
        else if (m.level.jc === "center") markerEl.style.marginLeft = `${-widthEm / 2}em`;
        const suffix = m.level.suff;
        const frag: Node[] = [markerEl];
        if (suffix === "space") frag.push(h("span", { style: css({ "font-size": ms.css["font-size"] }) }, "\u00a0"));
        else if (suffix !== "nothing") {
          const tab = h("span", { class: "tf-docx-tab", style: css({ "font-size": ms.css["font-size"] }) });
          frag.push(tab);
          const firstP = inl.segments[0].el;
          inl.tabs.unshift({ el: tab, p: firstP, pLeft: bidi ? pRight : pLeft, indent: bidi ? indRight : indLeft, stops: stopsPx, defaultTab: doc.settings.defaultTab });
        }
        inl.segments[0].el.prepend(...frag);
        if (!inl.strut) {
          inl.strut = ms;
          inl.strutRatio = ratioOf(ms.fonts, txt);
        }
        inl.maxSize = Math.max(inl.maxSize, ms.size);
      }
    }
  }

  // Strut font: the first text run, or the paragraph mark when there is no text
  const strut = inl.strut ?? markStyle;
  const strutRatio = inl.strut ? inl.strutRatio : ratioOf(markStyle.fonts, "");
  const maxSize = Math.max(inl.maxSize, strut.size);
  const maxRatio = Math.max(inl.maxRatio, strutRatio);

  let lineHeight: string;
  switch (mode.kind) {
    case "exact":
      lineHeight = `${mode.value}pt`;
      break;
    case "atLeast":
      lineHeight = `${Math.round(Math.max(mode.value, strut.size * strutRatio) * 100) / 100}pt`;
      break;
    case "grid": {
      const natural = maxSize * maxRatio;
      const lines = Math.max(1, Math.ceil(natural / mode.value - 0.05));
      lineHeight = `${Math.round(lines * mode.value * mode.mult * 100) / 100}pt`;
      break;
    }
    default:
      lineHeight = String(Math.round(strutRatio * mode.mult * 1000) / 1000);
  }

  // Space before/after
  const sp = props.spacing;
  const lineUnit = 12;
  const before = sp.beforeAuto ? (f.inCell ? 0 : pxOf(14)) : sp.beforeLines ? pxOf((sp.beforeLines / 100) * lineUnit) : tw(sp.before ?? 0);
  const after = sp.afterAuto ? (f.inCell ? 0 : pxOf(14)) : sp.afterLines ? pxOf((sp.afterLines / 100) * lineUnit) : tw(sp.after ?? 0);

  // Alignment
  const jc = props.jc ?? (bidi ? "right" : "left");
  const align: Record<string, string> = { left: "left", start: bidi ? "right" : "left", center: "center", right: "right", end: bidi ? "left" : "right", both: "justify", distribute: "justify", thaiDistribute: "justify", highKashida: "justify", mediumKashida: "justify", lowKashida: "justify" };
  const shade = shadeColor(props.shd, theme);

  const base$: Record<string, string | undefined> = {
    "font-family": strut.family,
    "font-size": strut.css["font-size"],
    "line-height": lineHeight,
    "text-align": align[jc] ?? (bidi ? "right" : "left"),
    "text-align-last": jc === "distribute" ? "justify" : undefined,
    direction: bidi ? "rtl" : undefined,
    "margin-left": pLeft ? `${Math.round((bidi ? pRight : pLeft) * 100) / 100}px` : undefined,
    "margin-right": pRight ? `${Math.round((bidi ? pLeft : pRight) * 100) / 100}px` : undefined,
    "background-color": shade ?? undefined,
    "border-left": has(bLeft) ? bLeft! : undefined,
    "border-right": has(bRight) ? bRight! : undefined,
    "padding-left": sLeft ? `${Math.round(sLeft * 100) / 100}px` : undefined,
    "padding-right": sRight ? `${Math.round(sRight * 100) / 100}px` : undefined,
  };
  if (shade && !strut.css.color) {
    // "Auto" text color on dark shading is white
    if (isDarkHex(shade)) base$.color = "#fff";
  }
  if (props.autoSpaceDE === false && props.autoSpaceDN === false) base$["text-autospace"] = "no-autospace";
  if (props.wordWrap === false) base$["word-break"] = "break-all";
  if (props.framePr && attr(props.framePr, "dropCap") && attr(props.framePr, "dropCap") !== "none") {
    base$.float = "left";
    base$["margin-right"] = `${Math.round(tw(numAttr(props.framePr, "hSpace") ?? 0) * 100) / 100 + 2}px`;
  }

  const segs = inl.segments;
  segs.forEach((s, i) => {
    const el = s.el;
    const st$: Record<string, string | undefined> = { ...base$, "text-indent": i === 0 && first ? `${Math.round(first * 100) / 100}px` : undefined };
    el.setAttribute("style", css(st$));
    // A paragraph without text (or ending with a line break) needs an empty line
    const last = i === segs.length - 1;
    if (!el.childNodes.length || (last && inl.endsWithBr) || !hasFlowContent(el)) el.append(h("br", { class: "tf-docx-eol" }));
  });

  // Tabs: those in the same paragraph are processed in order
  if (inl.tabs.length) {
    const byP = new Map<HTMLElement, TabRef[]>();
    for (const t of inl.tabs) {
      const list = byP.get(t.p) ?? [];
      list.push(t);
      byP.set(t.p, list);
    }
    for (const list of byP.values()) doc.tabs.push(list);
  }

  const topSpace = has(bTop) ? borderSpace(props.borders.top) * PT : 0;
  const bottomSpace = has(bBottom) ? borderSpace(props.borders.bottom) * PT : 0;
  const anyBorder = has(bTop) || has(bBottom) || has(bLeft) || has(bRight) || has(bBetween);
  const borderKey = anyBorder
    ? [props.borders.top, props.borders.bottom, props.borders.left, props.borders.right, props.borders.between].map(borderSig).join("|") + `|${pLeft}|${pRight}`
    : "";

  return {
    segments: segs,
    props,
    styleId,
    before,
    after,
    borderKey,
    breakAfter,
    sectionOnly,
    box: { top: has(bTop) ? bTop : null, bottom: has(bBottom) ? bBottom : null, between: has(bBetween) ? bBetween : null, topSpace, bottomSpace },
  };
}

function isDarkHex(hex: string) {
  const n = parseInt(hex.slice(1, 7), 16);
  const r = (n >> 16) & 255;
  const g = (n >> 8) & 255;
  const b = n & 255;
  return 0.299 * r + 0.587 * g + 0.114 * b < 110;
}

/** Whether the paragraph has content that produces a line box (not when it has only floating or absolutely positioned objects) */
function hasFlowContent(el: HTMLElement): boolean {
  for (let c = el.firstChild; c; c = c.nextSibling) {
    if (c.nodeType === Node.TEXT_NODE) {
      if (c.textContent) return true;
      continue;
    }
    if (!(c instanceof HTMLElement)) continue;
    if (c.classList.contains("tf-docx-float") || c.classList.contains("tf-docx-abs") || c.classList.contains("tf-docx-bm")) continue;
    if (c.tagName === "A" || (c.tagName === "SPAN" && !c.classList.contains("tf-docx-tab") && !c.classList.contains("tf-docx-obj") && !c.classList.contains("tf-docx-inl"))) {
      if (hasFlowContent(c) || c.classList.contains("tf-docx-fld")) return true;
      continue;
    }
    return true;
  }
  return false;
}
