/**
 * Styles (styles.xml) and property cascading:
 * document defaults → table style (with conditional formatting) → paragraph style chain → character style chain → direct formatting.
 * rFonts merge per slot; toggle properties (bold, italic…) toggle (XOR) across style levels, while direct formatting is absolute.
 */

import { attr, kid, kids, numAttr, parseXml, toggle } from "@/lib/office/ooxml";
import type { FontSlots } from "@/lib/office/docx/fonts";
import { flatKids, twAttr } from "@/lib/office/docx/xml";

// ───────────── Run properties ─────────────

const TOGGLES = ["b", "bCs", "i", "iCs", "caps", "smallCaps", "strike", "dstrike", "outline", "shadow", "emboss", "imprint", "vanish", "specVanish", "webHidden"] as const;
type ToggleKey = (typeof TOGGLES)[number];

export interface RunProps extends Partial<Record<ToggleKey, boolean>> {
  fonts: FontSlots;
  /** Half-points */
  sz?: number;
  szCs?: number;
  u?: Element;
  color?: Element;
  highlight?: string;
  shd?: Element;
  bdr?: Element;
  vertAlign?: string;
  /** Half-points; positive raises */
  position?: number;
  /** twips */
  spacing?: number;
  /** Horizontal scale (percent) */
  w?: number;
  em?: string;
  rtl?: boolean;
  cs?: boolean;
  rStyle?: string;
  /** Word 2010 text effect fill (w14:textFill) */
  textFill?: Element;
}

const SLOTS: [keyof FontSlots, keyof FontSlots][] = [
  ["ascii", "asciiTheme"],
  ["hAnsi", "hAnsiTheme"],
  ["eastAsia", "eastAsiaTheme"],
  ["cs", "cstheme"],
];

function mergeFonts(target: FontSlots, el: Element) {
  for (const [lit, th] of SLOTS) {
    const t = attr(el, th);
    const l = attr(el, lit);
    if (t) target[th] = t;
    if (l) {
      target[lit] = l;
      // When a higher level gives only a font name, it replaces the lower level's theme font
      if (!t) delete target[th];
    }
  }
  const hint = attr(el, "hint");
  if (hint) target.hint = hint;
}

/** Apply one rPr onto target (within the same level: later values replace earlier ones) */
export function applyRPr(target: RunProps, rPr: Element | null | undefined) {
  if (!rPr) return target;
  for (const c of flatKids(rPr)) {
    const n = c.localName;
    switch (n) {
      case "rFonts":
        mergeFonts(target.fonts, c);
        break;
      case "sz":
      case "szCs": {
        const v = numAttr(c, "val");
        if (v !== null && v > 0) target[n] = v;
        break;
      }
      case "u":
      case "color":
      case "shd":
      case "bdr":
        target[n] = c;
        break;
      case "highlight":
      case "vertAlign":
      case "em":
      case "rStyle":
        target[n] = attr(c, "val") ?? undefined;
        break;
      case "position": {
        const v = numAttr(c, "val");
        if (v !== null) target.position = v;
        break;
      }
      case "spacing": {
        const v = twAttr(c, "val");
        if (v !== null) target.spacing = v;
        break;
      }
      case "w": {
        const v = numAttr(c, "val");
        if (v !== null && v > 0) target.w = v;
        break;
      }
      case "rtl":
      case "cs":
        target[n] = toggle(c);
        break;
      case "textFill":
        target.textFill = c;
        break;
      default:
        if ((TOGGLES as readonly string[]).includes(n)) target[n as ToggleKey] = toggle(c);
    }
  }
  return target;
}

export const emptyRun = (): RunProps => ({ fonts: {} });

export function cloneRun(r: RunProps): RunProps {
  return { ...r, fonts: { ...r.fonts } };
}

/** Layer another level on top (non-toggle properties: replace when defined) */
function overlayRun(target: RunProps, layer: RunProps) {
  for (const [lit, th] of SLOTS) {
    if (layer.fonts[th]) target.fonts[th] = layer.fonts[th];
    if (layer.fonts[lit]) {
      target.fonts[lit] = layer.fonts[lit];
      if (!layer.fonts[th]) delete target.fonts[th];
    }
  }
  if (layer.fonts.hint) target.fonts.hint = layer.fonts.hint;
  for (const [k, v] of Object.entries(layer)) {
    if (k === "fonts" || v === undefined || (TOGGLES as readonly string[]).includes(k)) continue;
    (target as unknown as Record<string, unknown>)[k] = v;
  }
}

/**
 * Cascade: base (document defaults) → each style level (toggle properties XOR) → direct formatting.
 */
export function cascadeRun(base: RunProps, levels: (RunProps | null | undefined)[], direct?: RunProps | null): RunProps {
  const out = cloneRun(base);
  const ls = levels.filter((l): l is RunProps => !!l);
  for (const l of ls) overlayRun(out, l);
  for (const k of TOGGLES) {
    let mentioned = false;
    let v = false;
    for (const l of ls) {
      if (l[k] === undefined) continue;
      mentioned = true;
      if (l[k]) v = !v;
    }
    if (mentioned) out[k] = v;
  }
  if (direct) {
    overlayRun(out, direct);
    for (const k of TOGGLES) if (direct[k] !== undefined) out[k] = direct[k];
  }
  return out;
}

// ───────────── Paragraph properties ─────────────

export interface Ind {
  left?: number;
  right?: number;
  /** Positive: first-line indent; negative: hanging indent */
  first?: number;
  /** In units of 1/100 character (takes precedence over twips) */
  leftChars?: number;
  rightChars?: number;
  firstChars?: number;
}

export interface Spacing {
  before?: number;
  after?: number;
  /** 1/100 line */
  beforeLines?: number;
  afterLines?: number;
  beforeAuto?: boolean;
  afterAuto?: boolean;
  line?: number;
  lineRule?: string;
}

export interface TabStop {
  /** twips, relative to the left margin */
  pos: number;
  val: string;
  leader?: string;
}

export interface ParaProps {
  jc?: string;
  ind: Ind;
  spacing: Spacing;
  borders: Partial<Record<"top" | "bottom" | "left" | "right" | "between" | "bar", Element>>;
  shd?: Element;
  tabs: TabStop[];
  keepNext?: boolean;
  keepLines?: boolean;
  widowControl?: boolean;
  pageBreakBefore?: boolean;
  contextualSpacing?: boolean;
  bidi?: boolean;
  snapToGrid?: boolean;
  autoSpaceDE?: boolean;
  autoSpaceDN?: boolean;
  wordWrap?: boolean;
  numId?: string;
  ilvl?: number;
  /** Numbering comes from the style (the style's indent overrides the numbering level's indent) */
  numFromStyle?: boolean;
  outlineLvl?: number;
  framePr?: Element;
  textAlignment?: string;
  /** Run properties of the paragraph mark (height of an empty paragraph) */
  markRPr?: Element;
}

export const emptyPara = (): ParaProps => ({ ind: {}, spacing: {}, borders: {}, tabs: [] });

const SIDE_ALIAS: Record<string, "top" | "bottom" | "left" | "right" | "between" | "bar"> = {
  top: "top", bottom: "bottom", left: "left", start: "left", right: "right", end: "right", between: "between", bar: "bar",
};

export function applyPPr(target: ParaProps, pPr: Element | null | undefined, fromStyle: boolean) {
  if (!pPr) return target;
  for (const c of flatKids(pPr)) {
    switch (c.localName) {
      case "jc":
        target.jc = attr(c, "val") ?? undefined;
        break;
      case "ind": {
        const ind = target.ind;
        const left = twAttr(c, "left") ?? twAttr(c, "start");
        const right = twAttr(c, "right") ?? twAttr(c, "end");
        const leftCh = numAttr(c, "leftChars") ?? numAttr(c, "startChars");
        const rightCh = numAttr(c, "rightChars") ?? numAttr(c, "endChars");
        if (left !== null) ind.left = left;
        if (leftCh !== null) ind.leftChars = leftCh;
        else if (left !== null) ind.leftChars = undefined;
        if (right !== null) ind.right = right;
        if (rightCh !== null) ind.rightChars = rightCh;
        else if (right !== null) ind.rightChars = undefined;
        const hanging = twAttr(c, "hanging");
        const firstLine = twAttr(c, "firstLine");
        const hangCh = numAttr(c, "hangingChars");
        const firstCh = numAttr(c, "firstLineChars");
        if (hanging !== null) ind.first = -hanging;
        else if (firstLine !== null) ind.first = firstLine;
        if (hangCh !== null) ind.firstChars = -hangCh;
        else if (firstCh !== null) ind.firstChars = firstCh;
        else if (hanging !== null || firstLine !== null) ind.firstChars = undefined;
        break;
      }
      case "spacing": {
        const sp = target.spacing;
        const before = twAttr(c, "before");
        const after = twAttr(c, "after");
        const bl = numAttr(c, "beforeLines");
        const al = numAttr(c, "afterLines");
        if (before !== null) sp.before = before;
        if (bl !== null) sp.beforeLines = bl;
        else if (before !== null) sp.beforeLines = undefined;
        if (after !== null) sp.after = after;
        if (al !== null) sp.afterLines = al;
        else if (after !== null) sp.afterLines = undefined;
        const ba = attr(c, "beforeAutospacing");
        const aa = attr(c, "afterAutospacing");
        if (ba !== null) sp.beforeAuto = !/^(0|false|off)$/i.test(ba);
        if (aa !== null) sp.afterAuto = !/^(0|false|off)$/i.test(aa);
        const line = numAttr(c, "line");
        if (line !== null) {
          sp.line = line;
          sp.lineRule = attr(c, "lineRule") ?? "auto";
        }
        break;
      }
      case "pBdr":
        for (const b of kids(c)) {
          const side = SIDE_ALIAS[b.localName];
          if (side) target.borders[side] = b;
        }
        break;
      case "shd":
        target.shd = c;
        break;
      case "tabs":
        for (const t of kids(c, "tab")) {
          const pos = twAttr(t, "pos");
          const v = attr(t, "val") ?? "left";
          if (pos === null) continue;
          target.tabs = target.tabs.filter((x) => x.pos !== pos);
          if (v !== "clear") target.tabs.push({ pos, val: v, leader: attr(t, "leader") ?? undefined });
        }
        target.tabs.sort((a, b) => a.pos - b.pos);
        break;
      case "keepNext":
      case "keepLines":
      case "widowControl":
      case "pageBreakBefore":
      case "contextualSpacing":
      case "bidi":
      case "snapToGrid":
      case "autoSpaceDE":
      case "autoSpaceDN":
      case "wordWrap":
        target[c.localName] = toggle(c);
        break;
      case "numPr": {
        const id = attr(kid(c, "numId"), "val");
        const lvl = numAttr(kid(c, "ilvl"), "val");
        if (id !== null) {
          target.numId = id;
          target.numFromStyle = fromStyle;
        }
        if (lvl !== null) target.ilvl = lvl;
        break;
      }
      case "outlineLvl": {
        const v = numAttr(c, "val");
        if (v !== null) target.outlineLvl = v;
        break;
      }
      case "framePr":
        target.framePr = c;
        break;
      case "textAlignment":
        target.textAlignment = attr(c, "val") ?? undefined;
        break;
      case "rPr":
        if (!fromStyle) target.markRPr = c;
        break;
    }
  }
  return target;
}

const W_NS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const HEAD = (lvl: number, before: number, sz: number, color: string, extra = "") =>
  `<w:style w:type="paragraph" w:styleId="Heading${lvl}"><w:basedOn w:val="Normal"/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before="${before}" w:after="0"/><w:outlineLvl w:val="${lvl - 1}"/></w:pPr><w:rPr><w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi" w:eastAsiaTheme="majorEastAsia"/><w:color w:val="${color}"/><w:sz w:val="${sz}"/>${extra}</w:rPr></w:style>`;

/** Default formatting for Word's built-in styles (used when the document doesn't define them) */
const BUILTIN: Record<string, string> = {
  Heading1: HEAD(1, 240, 32, "2F5496"),
  Heading2: HEAD(2, 40, 26, "2F5496"),
  Heading3: HEAD(3, 40, 24, "1F3763"),
  Heading4: HEAD(4, 40, 22, "2F5496", "<w:i/>"),
  Heading5: HEAD(5, 40, 22, "2F5496"),
  Heading6: HEAD(6, 40, 22, "1F3763"),
  Heading7: HEAD(7, 40, 22, "1F3763", "<w:i/>"),
  Heading8: HEAD(8, 40, 21, "272727"),
  Heading9: HEAD(9, 40, 21, "272727", "<w:i/>"),
  Title: `<w:style w:type="paragraph" w:styleId="Title"><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/><w:contextualSpacing/></w:pPr><w:rPr><w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi" w:eastAsiaTheme="majorEastAsia"/><w:spacing w:val="-10"/><w:sz w:val="56"/></w:rPr></w:style>`,
  Subtitle: `<w:style w:type="paragraph" w:styleId="Subtitle"><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:after="160"/></w:pPr><w:rPr><w:color w:val="5A5A5A"/><w:spacing w:val="15"/><w:sz w:val="22"/></w:rPr></w:style>`,
  TOCHeading: `<w:style w:type="paragraph" w:styleId="TOCHeading"><w:basedOn w:val="Heading1"/><w:pPr><w:outlineLvl w:val="9"/></w:pPr></w:style>`,
  ListParagraph: `<w:style w:type="paragraph" w:styleId="ListParagraph"><w:basedOn w:val="Normal"/><w:pPr><w:ind w:left="720"/><w:contextualSpacing/></w:pPr></w:style>`,
  Quote: `<w:style w:type="paragraph" w:styleId="Quote"><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:before="200" w:after="160"/><w:ind w:left="864" w:right="864"/><w:jc w:val="center"/></w:pPr><w:rPr><w:i/><w:color w:val="404040"/></w:rPr></w:style>`,
  Caption: `<w:style w:type="paragraph" w:styleId="Caption"><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:after="200" w:line="240" w:lineRule="auto"/></w:pPr><w:rPr><w:i/><w:color w:val="44546A"/><w:sz w:val="18"/></w:rPr></w:style>`,
  Hyperlink: `<w:style w:type="character" w:styleId="Hyperlink"><w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr></w:style>`,
  FootnoteText: `<w:style w:type="paragraph" w:styleId="FootnoteText"><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:rPr><w:sz w:val="20"/></w:rPr></w:style>`,
  FootnoteReference: `<w:style w:type="character" w:styleId="FootnoteReference"><w:rPr><w:vertAlign w:val="superscript"/></w:rPr></w:style>`,
  EndnoteReference: `<w:style w:type="character" w:styleId="EndnoteReference"><w:rPr><w:vertAlign w:val="superscript"/></w:rPr></w:style>`,
  TableGrid: `<w:style w:type="table" w:styleId="TableGrid"><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideH w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideV w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders></w:tblPr></w:style>`,
};

// ───────────── Style sheet ─────────────

export interface CondFormat {
  pPr: Element | null;
  rPr: Element | null;
  tblPr: Element | null;
  trPr: Element | null;
  tcPr: Element | null;
}

export interface StyleDef extends CondFormat {
  id: string;
  type: string;
  name: string;
  basedOn: string | null;
  isDefault: boolean;
  /** Table style conditional formatting (firstRow, band1Horz…) */
  cond: Map<string, CondFormat>;
}

function condOf(el: Element): CondFormat {
  return { pPr: kid(el, "pPr"), rPr: kid(el, "rPr"), tblPr: kid(el, "tblPr"), trPr: kid(el, "trPr"), tcPr: kid(el, "tcPr") };
}

export class Styles {
  readonly byId = new Map<string, StyleDef>();
  readonly defaults: Record<string, string> = {};
  readonly docRPr: Element | null;
  readonly docPPr: Element | null;
  private runLayer = new Map<string, RunProps>();
  private baseRun: RunProps | null = null;

  constructor(doc: Document | null) {
    const root = doc?.documentElement;
    const dd = kid(root, "docDefaults");
    this.docRPr = kid(kid(dd, "rPrDefault"), "rPr");
    this.docPPr = kid(kid(dd, "pPrDefault"), "pPr");
    for (const s of kids(root, "style")) {
      const id = attr(s, "styleId");
      if (!id) continue;
      const type = attr(s, "type") ?? "paragraph";
      const def: StyleDef = {
        id,
        type,
        name: attr(kid(s, "name"), "val") ?? id,
        basedOn: attr(kid(s, "basedOn"), "val"),
        isDefault: /^(1|true|on)$/i.test(attr(s, "default") ?? ""),
        ...condOf(s),
        cond: new Map(),
      };
      for (const c of kids(s, "tblStylePr")) {
        const t = attr(c, "type");
        if (t) def.cond.set(t, condOf(c));
      }
      this.byId.set(id, def);
      if (def.isDefault && !this.defaults[type]) this.defaults[type] = id;
    }
  }

  /**
   * Built-in styles referenced but not defined by the document (common in generated documents): filled in with Word's built-in defaults.
   */
  addBuiltins(referenced: Set<string>) {
    const want = [...referenced].filter((id) => BUILTIN[id] && !this.byId.has(id));
    while (want.length) {
      const id = want.pop()!;
      const xml = BUILTIN[id];
      if (this.byId.has(id)) continue;
      const doc = parseXml(`<w:styles xmlns:w="${W_NS}">${xml}</w:styles>`);
      const s = doc?.documentElement.firstElementChild;
      if (!s) continue;
      const cond = new Map<string, CondFormat>();
      this.byId.set(id, {
        id,
        type: attr(s, "type") ?? "paragraph",
        name: id,
        basedOn: attr(kid(s, "basedOn"), "val"),
        isDefault: false,
        ...condOf(s),
        cond,
      });
      const base = attr(kid(s, "basedOn"), "val");
      if (base && BUILTIN[base] && !this.byId.has(base)) want.push(base);
    }
  }

  /** Style chain (base first); guards against cycles */
  chain(id: string | null | undefined): StyleDef[] {
    const out: StyleDef[] = [];
    const seen = new Set<string>();
    let cur = id ? this.byId.get(id) : undefined;
    while (cur && !seen.has(cur.id) && out.length < 20) {
      seen.add(cur.id);
      out.unshift(cur);
      cur = cur.basedOn ? this.byId.get(cur.basedOn) : undefined;
    }
    return out;
  }

  /** Paragraph style id: falls back to the default paragraph style when unspecified or missing (Word's behavior) */
  paraStyleId(id: string | null | undefined): string | undefined {
    if (id && this.byId.has(id)) return id;
    return this.defaults.paragraph;
  }

  baseRunProps(): RunProps {
    if (!this.baseRun) this.baseRun = applyRPr(emptyRun(), this.docRPr);
    return this.baseRun;
  }

  /** Run properties of a style chain (paragraph or character style; results cached) */
  styleRun(id: string | null | undefined): RunProps | null {
    if (!id) return null;
    let v = this.runLayer.get(id);
    if (!v) {
      v = emptyRun();
      for (const s of this.chain(id)) applyRPr(v, s.rPr);
      this.runLayer.set(id, v);
    }
    return v;
  }

  /** Run properties of a table style: whole table → conditional formats applied in priority order */
  tableRun(id: string | null | undefined, conds: string[]): RunProps | null {
    if (!id) return null;
    const key = `${id}\u0000${conds.join(",")}`;
    let v = this.runLayer.get(key);
    if (!v) {
      v = emptyRun();
      const chain = this.chain(id);
      for (const s of chain) applyRPr(v, s.rPr);
      for (const c of conds) for (const s of chain) applyRPr(v, s.cond.get(c)?.rPr);
      this.runLayer.set(key, v);
    }
    return v;
  }

  /** pPr of the paragraph style chain (base first) */
  stylePPrs(id: string | null | undefined): Element[] {
    return this.chain(id)
      .map((s) => s.pPr)
      .filter((x): x is Element => !!x);
  }

  /** pPr of a table style (whole table + conditional formats) */
  tablePPrs(id: string | null | undefined, conds: string[]): Element[] {
    if (!id) return [];
    const chain = this.chain(id);
    const out: Element[] = [];
    for (const s of chain) if (s.pPr) out.push(s.pPr);
    for (const c of conds) for (const s of chain) {
      const p = s.cond.get(c)?.pPr;
      if (p) out.push(p);
    }
    return out;
  }
}

/** Font size when unset (half-points): Word's default is 10pt */
export const DEFAULT_SZ = 20;

