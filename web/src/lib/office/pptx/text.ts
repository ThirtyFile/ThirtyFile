/**
 * Text: a:txBody → paragraphs and runs.
 * Paragraph/character properties are inherited item by item: "paragraph itself → shape list style → layout → master → master text styles → presentation defaults".
 */

import { attr, css, h, kid, kids, numAttr, safeHref, type Rel } from "@/lib/office/ooxml";
import { colorIn, fontStack, rgbaCss, schemeColor, themeFont, type ColorContext, type Rgba, type Theme } from "@/lib/office/theme";
import { fillElIn, fillColor, readFill, cssGradient } from "./paint";
import { C } from "./styles";

export interface TextCtx {
  cc: ColorContext;
  theme: Theme | null;
  /** List styles (elements containing lvlNpPr), in priority order */
  lists: (Element | null | undefined)[];
  /** The first few entries in lists belong to the shape itself (p:style's fontRef ranks after them) */
  own: number;
  fontRefColor?: Rgba | null;
  fontRefFace?: string | null;
  /** Text properties from the table style (above list styles, below the paragraph itself) */
  cellBold?: boolean;
  cellItalic?: boolean;
  cellColor?: Rgba | null;
  cellFace?: string | null;
  slideNum: number;
  rels: Map<string, Rel>;
  /** Picture bullets (buBlip): relationship id → blob: URL */
  media?: (rid: string) => Promise<string | null>;
}

export interface BodyProps {
  /** Insets (px) */
  l: number;
  t: number;
  r: number;
  b: number;
  anchor: string;
  anchorCtr: boolean;
  wrap: boolean;
  vert: string;
  numCol: number;
  spcCol: number;
  autofit: "norm" | "sp" | "none" | null;
  fontScale: number;
  lnReduce: number;
}

/** Look up bodyPr in order (shape → layout → master) */
export function readBodyPr(chain: (Element | null | undefined)[], defaultAnchor = "t"): BodyProps {
  const list = chain.filter((e): e is Element => !!e);
  const a = (n: string) => {
    for (const e of list) {
      const v = attr(e, n);
      if (v !== null) return v;
    }
    return null;
  };
  const num = (n: string, d: number) => {
    const v = a(n);
    return v === null || v === "" || !Number.isFinite(Number(v)) ? d : Number(v);
  };
  let autofit: BodyProps["autofit"] = null;
  let fit: Element | null = null;
  for (const e of list) {
    const f = kids(e).find((c) => c.localName === "normAutofit" || c.localName === "spAutoFit" || c.localName === "noAutofit");
    if (f) {
      fit = f;
      autofit = f.localName === "normAutofit" ? "norm" : f.localName === "spAutoFit" ? "sp" : "none";
      break;
    }
  }
  return {
    l: num("lIns", 91440) / 9525,
    t: num("tIns", 45720) / 9525,
    r: num("rIns", 91440) / 9525,
    b: num("bIns", 45720) / 9525,
    anchor: a("anchor") ?? defaultAnchor,
    anchorCtr: a("anchorCtr") === "1",
    wrap: a("wrap") !== "none",
    vert: a("vert") ?? "horz",
    numCol: num("numCol", 1),
    spcCol: num("spcCol", 0) / 9525,
    autofit,
    fontScale: autofit === "norm" ? (numAttr(fit, "fontScale") ?? 100000) / 100000 : 1,
    lnReduce: autofit === "norm" ? (numAttr(fit, "lnSpcReduction") ?? 0) / 100000 : 0,
  };
}

// ───────────── Property chain ─────────────

interface Link {
  el: Element;
  own: boolean;
}

function first(chain: Link[], pick: (e: Element) => string | null) {
  for (const l of chain) {
    const v = pick(l.el);
    if (v !== null) return v;
  }
  return null;
}

function firstKid(chain: Link[], names: string[]) {
  for (const l of chain) for (const c of kids(l.el)) if (names.includes(c.localName)) return c;
  return null;
}

const FILLS = ["solidFill", "gradFill", "noFill", "pattFill", "blipFill"];

/** pPr chain for a paragraph level */
function paraChain(tc: TextCtx, pPr: Element | null, lvl: number): Link[] {
  const key = `lvl${lvl + 1}pPr`;
  const out: Link[] = [];
  if (pPr) out.push({ el: pPr, own: true });
  tc.lists.forEach((l, i) => {
    const e = kid(l, key);
    if (e) out.push({ el: e, own: i < tc.own });
  });
  for (const l of tc.lists) {
    const e = kid(l, "defPPr");
    if (e) out.push({ el: e, own: false });
  }
  return out;
}

// ───────────── Characters ─────────────

interface RunStyle {
  size: number; // pt
  css: string;
  color: Rgba | null;
  face: string;
  href: string | null;
}

/** Common Wingdings/Symbol bullets → Unicode */
const SYMBOL_MAP: Record<string, Record<number, string>> = {
  wingdings: {
    0x6c: "●", 0x6e: "■", 0x71: "❑", 0x75: "◆", 0x76: "❖", 0xa7: "▪", 0xa8: "◻",
    0xd8: "➢", 0xfc: "✔", 0xfb: "✘", 0xe0: "➔", 0xe8: "➔", 0x9f: "•", 0xa1: "○",
    0x77: "⬥", 0x6f: "□", 0x70: "□", 0x73: "◆", 0xf0: "⇨", 0xef: "⇦", 0xde: "➤", 0x4a: "☺",
  },
  symbol: { 0xb7: "•", 0xa8: "♦", 0xa7: "♣", 0xde: "⇒", 0xae: "→", 0x2d: "−", 0xd7: "⋅" },
};

function symbolChar(ch: string, font: string | null): { ch: string; font: string | null } {
  const key = (font ?? "").toLowerCase().replace(/\s+\d+$/, "");
  const map = SYMBOL_MAP[key];
  if (!map) return { ch, font };
  const code = ch.codePointAt(0) ?? 0;
  // Symbol fonts sometimes use the Private Use Area (U+F0xx)
  const c = code >= 0xf000 && code <= 0xf0ff ? code - 0xf000 : code;
  const m = map[c];
  return m ? { ch: m, font: null } : { ch, font };
}

const UNDERLINE: Record<string, string> = {
  sng: "solid", dbl: "double", heavy: "solid", dotted: "dotted", dottedHeavy: "dotted", dash: "dashed", dashHeavy: "dashed",
  dashLong: "dashed", dashLongHeavy: "dashed", dotDash: "dashed", dotDashHeavy: "dashed", dotDotDash: "dashed",
  dotDotDashHeavy: "dashed", wavy: "wavy", wavyHeavy: "wavy", wavyDbl: "wavy", words: "solid",
};

/** Language code → theme font script tag */
function scriptOf(lang: string | null): string {
  const l = (lang ?? "").toLowerCase();
  if (/^zh-(cn|sg)|^zh-hans/.test(l)) return "Hans";
  if (l.startsWith("ja")) return "Jpan";
  if (l.startsWith("ko")) return "Hang";
  return "Hant";
}

const cleanFace = (f: string | null | undefined) => (f ? f.replace(/["';{}<>\\]/g, "").trim() || null : null);

function runStyle(tc: TextCtx, rPr: Element | null, pChain: Link[], scale: number): RunStyle {
  const chain: Link[] = [];
  if (rPr) chain.push({ el: rPr, own: true });
  for (const l of pChain) {
    const d = kid(l.el, "defRPr");
    if (d) chain.push({ el: d, own: l.own });
  }
  const own = chain.filter((l) => l.own);
  const inh = chain.filter((l) => !l.own);
  const a = (n: string) => first(chain, (e) => attr(e, n));

  const size = (Number(a("sz") ?? 1800) / 100) * scale;
  const flag = (n: string, cell: boolean | undefined) => {
    const o = first(own, (e) => attr(e, n));
    if (o !== null) return o === "1" || o === "true";
    if (cell !== undefined) return cell;
    const v = first(inh, (e) => attr(e, n));
    return v === "1" || v === "true";
  };
  const bold = flag("b", tc.cellBold);
  const italic = flag("i", tc.cellItalic);

  // Color: shape itself → table style/fontRef → inherited
  let fillEl = firstKid(own, FILLS);
  let color: Rgba | null = null;
  let gradient: string | null = null;
  if (!fillEl) color = tc.cellColor ?? tc.fontRefColor ?? null;
  if (!fillEl && !color) fillEl = firstKid(inh, FILLS);
  if (fillEl) {
    const f = readFill(fillEl, tc.cc, "");
    if (f?.t === "none") color = { r: 0, g: 0, b: 0, a: 0 };
    else if (f?.t === "grad") {
      gradient = cssGradient(f);
      color = fillColor(f);
    } else color = fillColor(f);
  }
  if (!color) color = schemeColor("tx1", tc.cc) ?? { r: 0, g: 0, b: 0, a: 1 };

  // Fonts
  const faceOf = (tag: string) => {
    const pick = (links: Link[]) => first(links, (e) => cleanFace(attr(kid(e, tag), "typeface")));
    return pick(own) ?? (tag === "latin" ? tc.cellFace : null) ?? (tc.fontRefFace ? tc.fontRefFace.replace("-lt", tag === "latin" ? "-lt" : "-ea") : null) ?? pick(inh);
  };
  // East Asian fonts pick the theme's script font by language (Traditional Chinese Hant, Simplified Chinese Hans…)
  const script = scriptOf(a("altLang") ?? a("lang"));
  const latin = cleanFace(themeFont(faceOf("latin") ?? "+mn-lt", tc.theme, script));
  const ea = cleanFace(themeFont(faceOf("ea") ?? "+mn-ea", tc.theme, script));
  const face = fontStack(latin, ea);

  // Hyperlinks
  const hl = kid(rPr, "hlinkClick");
  let href: string | null = null;
  if (hl) {
    const rel = tc.rels.get(attr(hl, "id") ?? "");
    if (rel?.external) href = safeHref(rel.target);
    if (rel || href) color = schemeColor("hlink", tc.cc) ?? { r: 5, g: 99, b: 193, a: 1 };
  }

  const u = a("u");
  const strike = a("strike");
  const deco: string[] = [];
  if ((u && u !== "none") || (hl && (href || tc.rels.get(attr(hl, "id") ?? "")))) deco.push("underline");
  if (strike && strike !== "noStrike") deco.push("line-through");
  const uStyle = u && u !== "none" ? UNDERLINE[u] : undefined;
  const cap = a("cap");
  const baseline = Number(a("baseline") ?? 0);
  const spc = Number(a("spc") ?? 0);
  const hlColor = colorIn(firstKid(chain, ["highlight"]), tc.cc);
  const ln = rPr ? kid(rPr, "ln") : null;
  const lnFill = ln ? readFill(fillElIn(ln), tc.cc, "") : null;
  const lnColor = fillColor(lnFill);
  const shadow = kid(firstKid(own, ["effectLst"]), "outerShdw");
  let textShadow: string | undefined;
  if (shadow) {
    const sc = colorIn(shadow, tc.cc) ?? { r: 0, g: 0, b: 0, a: 0.4 };
    const dist = (numAttr(shadow, "dist") ?? 0) / 9525;
    const dir = ((numAttr(shadow, "dir") ?? 0) / 60000) * (Math.PI / 180);
    const r1 = (n: number) => Math.round(n * 10) / 10;
    textShadow = `${r1(dist * Math.cos(dir))}px ${r1(dist * Math.sin(dir))}px ${r1((numAttr(shadow, "blurRad") ?? 0) / 9525)}px ${rgbaCss(sc)}`;
  }
  const sizePx = baseline ? size * 0.66 : size;
  const style = css({
    "font-family": face,
    "font-size": `${Math.round(((sizePx * 96) / 72) * 100) / 100}px`,
    "font-weight": bold ? "bold" : undefined,
    "font-style": italic ? "italic" : undefined,
    color: gradient ? "transparent" : rgbaCss(color),
    "background-image": gradient ?? undefined,
    "-webkit-background-clip": gradient ? "text" : undefined,
    "background-clip": gradient ? "text" : undefined,
    "text-decoration-line": deco.length ? deco.join(" ") : undefined,
    "text-decoration-style": uStyle && uStyle !== "solid" ? uStyle : undefined,
    "text-decoration-color": rgbaCss(colorIn(kid(kid(rPr, "uFill"), "solidFill"), tc.cc)),
    "text-decoration-thickness": u && /Heavy|heavy/.test(u) ? "0.12em" : undefined,
    "text-transform": cap === "all" ? "uppercase" : undefined,
    "font-variant": cap === "small" ? "small-caps" : undefined,
    // baseline is a percentage of the original font size; shrunk text is converted to em
    "vertical-align": baseline ? `${Math.round((baseline / 100000 / 0.66) * 100) / 100}em` : undefined,
    "letter-spacing": spc ? `${Math.round(((spc / 100) * 96) / 72 * 100) / 100}px` : undefined,
    "background-color": hlColor ? rgbaCss(hlColor) : undefined,
    "-webkit-text-stroke": lnColor && ln && numAttr(ln, "w") ? `${Math.max(0.3, (numAttr(ln, "w") ?? 0) / 9525)}px ${rgbaCss(lnColor)}` : undefined,
    "text-shadow": textShadow,
  });
  return { size, css: style, color, face, href };
}

// ───────────── Bullets ─────────────

const ROMAN: [number, string][] = [[1000, "m"], [900, "cm"], [500, "d"], [400, "cd"], [100, "c"], [90, "xc"], [50, "l"], [40, "xl"], [10, "x"], [9, "ix"], [5, "v"], [4, "iv"], [1, "i"]];

function roman(n: number) {
  if (!Number.isInteger(n) || n <= 0 || n >= 4000) return String(n);
  let out = "";
  for (const [v, s] of ROMAN) while (n >= v) {
    out += s;
    n -= v;
  }
  return out;
}

function alpha(n: number) {
  let out = "";
  let x = n;
  while (x > 0) {
    x--;
    out = String.fromCharCode(97 + (x % 26)) + out;
    x = Math.floor(x / 26);
  }
  return out;
}

const CJK_DIGITS = "〇一二三四五六七八九"; // i18n-ignore: CJK numbering glyphs
const CIRCLED = "①②③④⑤⑥⑦⑧⑨⑩⑪⑫⑬⑭⑮⑯⑰⑱⑲⑳";

function cjkNum(n: number) {
  if (n < 10) return CJK_DIGITS[n];
  if (n < 20) return `十${n % 10 ? CJK_DIGITS[n % 10] : ""}`; // i18n-ignore: CJK numbering glyphs
  if (n < 100) return `${CJK_DIGITS[Math.floor(n / 10)]}十${n % 10 ? CJK_DIGITS[n % 10] : ""}`; // i18n-ignore: CJK numbering glyphs
  return String(n);
}

export function autoNumText(scheme: string, n: number): string {
  const m = /^(arabic|alphaLc|alphaUc|romanLc|romanUc|circleNum|ea1Cht|ea1Chs|ea1Jpn|hebrew2Minus|thaiAlpha)?(.*)$/.exec(scheme) ?? [];
  const kind = m[1] ?? "arabic";
  const tail = m[2] ?? "Period";
  let v: string;
  switch (kind) {
    case "alphaLc":
      v = alpha(n);
      break;
    case "alphaUc":
      v = alpha(n).toUpperCase();
      break;
    case "romanLc":
      v = roman(n);
      break;
    case "romanUc":
      v = roman(n).toUpperCase();
      break;
    case "circleNum":
      return CIRCLED[n - 1] ?? String(n);
    case "ea1Cht":
    case "ea1Chs":
    case "ea1Jpn":
      v = cjkNum(n);
      break;
    default:
      v = String(n);
  }
  if (/ParenBoth/.test(tail)) return `(${v})`;
  if (/ParenR/.test(tail)) return `${v})`;
  if (/Period/.test(tail)) return kind.startsWith("ea1") ? `${v}、` : `${v}.`; // i18n-ignore: ideographic comma after East Asian numbering
  if (/Comma/.test(tail)) return `${v},`;
  return v;
}

// ───────────── Paragraphs ─────────────

/** PowerPoint single line spacing is about 1.2 times the font size */
const LINE = 1.2;

export interface RenderOpts {
  bp: BodyProps;
  /** Extra font size scaling (used by text autofit) */
  scale?: number;
}

/** Text body → paragraph elements */
export function renderParagraphs(txBody: Element, tc: TextCtx, opts: RenderOpts): HTMLElement[] {
  const { bp } = opts;
  const scale = bp.fontScale * (opts.scale ?? 1);
  const lnReduce = bp.lnReduce;
  const ownList = kid(txBody, "lstStyle");
  const ctx: TextCtx = ownList ? { ...tc, lists: [ownList, ...tc.lists], own: tc.own + 1 } : tc;
  const paras = kids(txBody, "p");
  const counters = new Map<number, { scheme: string; n: number }>();
  const out: HTMLElement[] = [];

  paras.forEach((p, pi) => {
    const pPr = kid(p, "pPr");
    const lvl = Math.min(8, Math.max(0, numAttr(pPr, "lvl") ?? 0));
    const chain = paraChain(ctx, pPr, lvl);
    const pa = (n: string) => first(chain, (e) => attr(e, n));
    const pk = (names: string[]) => firstKid(chain, names);

    // Runs
    const items = kids(p).filter((c) => c.localName === "r" || c.localName === "br" || c.localName === "fld");
    const styled = items.map((el) => ({ el, st: runStyle(ctx, kid(el, "rPr"), chain, scale) }));
    const hasText = items.some((el) => el.localName !== "br" && (kid(el, "t")?.textContent ?? "") !== "");
    const endSt = runStyle(ctx, kid(p, "endParaRPr"), chain, scale);
    const firstSt = styled.find((x) => x.el.localName !== "br")?.st ?? endSt;
    const maxSize = styled.reduce((m, x) => Math.max(m, x.st.size), styled.length ? 0 : endSt.size) || endSt.size;

    // Spacing
    const lnSpc = pk(["lnSpc"]);
    const lnPct = numAttr(kid(lnSpc, "spcPct"), "val");
    const lnPts = numAttr(kid(lnSpc, "spcPts"), "val");
    let lineHeight: string;
    if (lnPts !== null) lineHeight = `${Math.round(((lnPts / 100) * 96) / 72 * 100) / 100}px`;
    else {
      const pct = Math.max(0.1, (lnPct ?? 100000) / 100000 - lnReduce);
      lineHeight = String(Math.round(pct * LINE * 1000) / 1000);
    }
    const space = (name: string) => {
      const el = pk([name]);
      const pts = numAttr(kid(el, "spcPts"), "val");
      if (pts !== null) return ((pts / 100) * 96) / 72;
      const pct = numAttr(kid(el, "spcPct"), "val");
      if (pct !== null) return (pct / 100000) * ((maxSize * 96) / 72) * LINE;
      return 0;
    };
    const before = pi === 0 ? 0 : space("spcBef");
    const after = pi === paras.length - 1 ? 0 : space("spcAft");
    const marL = Number(pa("marL") ?? 0) / 9525;
    const marR = Number(pa("marR") ?? 0) / 9525;
    const indent = Number(pa("indent") ?? 0) / 9525;
    const algn = pa("algn") ?? "l";
    const align = { l: "left", ctr: "center", r: "right", just: "justify", dist: "justify", justLow: "justify", thaiDist: "justify" }[algn] ?? "left";
    const rtl = pa("rtl") === "1";
    const tab = Number(pa("defTabSz") ?? 914400) / 9525;

    const div = h("div", {
      class: C.p,
      style: css({
        "font-size": `${Math.round(((maxSize * 96) / 72) * 100) / 100}px`,
        "line-height": lineHeight,
        "text-align": align,
        "text-align-last": algn === "dist" ? "justify" : undefined,
        "padding-left": marL ? `${Math.round(marL * 100) / 100}px` : undefined,
        "padding-right": marR ? `${Math.round(marR * 100) / 100}px` : undefined,
        "text-indent": indent ? `${Math.round(indent * 100) / 100}px` : undefined,
        "margin-top": before ? `${Math.round(before * 100) / 100}px` : undefined,
        "margin-bottom": after ? `${Math.round(after * 100) / 100}px` : undefined,
        direction: rtl ? "rtl" : undefined,
        "tab-size": tab > 0 && Math.abs(tab - 96) > 0.5 ? `${Math.round(tab * 100) / 100}px` : undefined,
        "font-family": firstSt.face,
      }),
    });

    // Bullets
    const bu = pk(["buNone", "buChar", "buAutoNum", "buBlip"]);
    if (!hasText) {
      // Empty paragraphs show no bullet and don't affect numbering
    } else if (bu && bu.localName !== "buNone") {
      let text = "";
      let font: string | null = null;
      if (bu.localName === "buChar") {
        const buFont = pk(["buFont", "buFontTx"]);
        const face = buFont?.localName === "buFont" ? cleanFace(themeFont(attr(buFont, "typeface"), ctx.theme)) : null;
        const sym = symbolChar(attr(bu, "char") ?? "•", face);
        text = sym.ch;
        font = sym.font;
        for (const k of [...counters.keys()]) if (k >= lvl) counters.delete(k);
      } else if (bu.localName === "buAutoNum") {
        const scheme = attr(bu, "type") ?? "arabicPeriod";
        const prev = counters.get(lvl);
        const n = prev && prev.scheme === scheme ? prev.n + 1 : Math.min(32767, Math.max(1, Math.floor(numAttr(bu, "startAt") ?? 1)));
        counters.set(lvl, { scheme, n });
        for (const k of [...counters.keys()]) if (k > lvl) counters.delete(k);
        text = autoNumText(scheme, n);
      }
      const clr = pk(["buClr", "buClrTx"]);
      const color = clr?.localName === "buClr" ? colorIn(clr, ctx.cc) : firstSt.color;
      const sz = pk(["buSzPct", "buSzPts", "buSzTx"]);
      let size = firstSt.size;
      if (sz?.localName === "buSzPct") size = (firstSt.size * (numAttr(sz, "val") ?? 100000)) / 100000;
      else if (sz?.localName === "buSzPts") size = ((numAttr(sz, "val") ?? 1800) / 100) * scale;
      const hang = indent < 0 ? -indent : 0;
      const px = Math.round(((size * 96) / 72) * 100) / 100;
      let content: Node | string = text;
      if (bu.localName === "buBlip") {
        // Picture bullets: image added asynchronously
        const img = h("img", { alt: "", style: `height:${px}px;width:auto;vertical-align:middle` });
        const rid = attr(kid(bu, "blip"), "embed");
        if (rid && ctx.media) void ctx.media(rid).then((u) => u && img.setAttribute("src", u));
        content = img;
      }
      const span = h(
        "span",
        {
          class: C.bullet,
          style: css({
            "font-family": font ? fontStack(font) : firstSt.face,
            "font-size": `${px}px`,
            color: rgbaCss(color),
            "min-width": hang ? `${Math.round(hang * 100) / 100}px` : undefined,
            "padding-right": hang ? undefined : "0.4em",
          }),
        },
        content,
      );
      div.append(span);
    } else {
      for (const k of [...counters.keys()]) if (k >= lvl) counters.delete(k);
    }

    // Text
    for (const { el, st } of styled) {
      if (el.localName === "br") {
        div.append(h("br"));
        continue;
      }
      let text = kid(el, "t")?.textContent ?? "";
      if (el.localName === "fld" && /^slidenum$/i.test(attr(el, "type") ?? "")) text = String(ctx.slideNum);
      if (!text) continue;
      const span = h("span", { style: st.css }, text);
      if (st.href) div.append(h("a", { href: st.href, target: "_blank", rel: "noopener noreferrer", class: C.link }, span));
      else div.append(span);
    }
    if (!hasText || items[items.length - 1]?.localName === "br") div.append("​");
    out.push(div);
  });
  return out;
}

const WRITING: Record<string, string> = {
  vert: "vertical-rl",
  eaVert: "vertical-rl",
  wordArtVert: "vertical-rl",
  wordArtVertRtl: "vertical-rl",
  mongolianVert: "vertical-lr",
  vert270: "vertical-rl",
};

/**
 * Text box: box is the text area (px, relative to the shape); applies insets, vertical alignment, vertical text and columns from bodyPr.
 */
export function textBox(txBody: Element, tc: TextCtx, bp: BodyProps, box: { x: number; y: number; w: number; h: number }, scale = 1): HTMLElement {
  const justify = { t: "flex-start", ctr: "center", b: "flex-end", just: "center", dist: "space-between" }[bp.anchor] ?? "flex-start";
  const writing = WRITING[bp.vert];
  const paras = renderParagraphs(txBody, tc, { bp, scale });
  // No wrap: the text block sizes to its content and is placed left/center/right per the first paragraph's alignment (extending both ways on overflow)
  let self: string | undefined = bp.anchorCtr ? "center" : undefined;
  if (!bp.wrap && !writing) {
    const a = paras[0]?.style.textAlign;
    self = a === "center" ? "center" : a === "right" ? "flex-end" : "flex-start";
  }
  const inner = h(
    "div",
    {
      class: bp.wrap ? C.tb : `${C.tb} ${C.nowrap}`,
      style: css({
        "column-count": bp.numCol > 1 ? bp.numCol : undefined,
        "column-gap": bp.numCol > 1 ? `${bp.spcCol}px` : undefined,
        "align-self": self,
      }),
    },
    ...paras,
  );
  const outer = h(
    "div",
    {
      class: C.tx,
      style: css({
        left: `${Math.round(box.x * 100) / 100}px`,
        top: `${Math.round(box.y * 100) / 100}px`,
        width: `${Math.round(box.w * 100) / 100}px`,
        height: `${Math.round(box.h * 100) / 100}px`,
        padding: `${Math.round(bp.t * 100) / 100}px ${Math.round(bp.r * 100) / 100}px ${Math.round(bp.b * 100) / 100}px ${Math.round(bp.l * 100) / 100}px`,
        "justify-content": justify,
        "writing-mode": writing,
        "text-orientation": bp.vert === "wordArtVert" || bp.vert === "wordArtVertRtl" ? "upright" : undefined,
        transform: bp.vert === "vert270" ? "rotate(180deg)" : undefined,
      }),
    },
    inner,
  );
  return outer;
}

/** Whether the text body has any visible text */
export function hasVisibleText(txBody: Element | null): boolean {
  if (!txBody) return false;
  for (const p of kids(txBody, "p")) for (const r of kids(p)) if ((r.localName === "r" || r.localName === "fld") && (kid(r, "t")?.textContent ?? "") !== "") return true;
  return false;
}

