/**
 * Word (.docx) preview: lays content out page by page using each section's page size and margins.
 *
 * Pipeline: read the package (styles, numbering, theme, settings, headers/footers, footnotes) → render blocks per section →
 * measure in a galley to resolve tabs and heights → paginate → build pages → scale to the container width.
 */

import { attr, css, descendants, h, kid, OoxmlPackage, type Rel } from "../core/package";
import { parseTheme } from "../core/theme";
import { BlockWriter } from "./blocks";
import { breathe } from "../core/yield";
import { DocCtx, type Flow, type PageFloat, type Settings } from "./context";
import { DOCX_CSS } from "./css";
import { HeaderFooters, Paginator, type BodyBlock } from "./layout";
import { renderNotes, endnoteBlock } from "./notes";
import { Numbering } from "./numbering";
import { runStyle, textColor } from "./props";
import { contentWidth, parseSection, type Section } from "./section";
import { cascadeRun } from "./styles";
import { Styles } from "./styles";
import { fixScaled, resolveTabs } from "./tabs";
import { flatKids, twAttr, val } from "./xml";

export interface RenderResult {
  /** Release image URLs and event listeners */
  dispose(): void;
}

const R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/** East Asian language → script for the theme font */
function scriptOf(lang: string) {
  const l = lang.toLowerCase();
  if (/^zh-(tw|hk|mo|hant)/.test(l)) return "Hant";
  if (l.startsWith("zh")) return "Hans";
  if (l.startsWith("ja")) return "Jpan";
  if (l.startsWith("ko")) return "Hang";
  return "Hant";
}

function readSettings(doc: Document | null, styles: Styles): Settings {
  const root = doc?.documentElement;
  const tfl = kid(root, "themeFontLang");
  const docLang = attr(kid(styles.docRPr, "lang"), "eastAsia");
  const lang = attr(tfl, "eastAsia") ?? docLang ?? attr(tfl, "val") ?? "en";
  let compat = 12;
  for (const c of descendants(kid(root, "compat"), "compatSetting")) if (attr(c, "name") === "compatibilityMode") compat = Number(attr(c, "val")) || compat;
  return {
    defaultTab: twAttr(kid(root, "defaultTabStop"), "val") || 720,
    evenAndOdd: !!kid(root, "evenAndOddHeaders") && attr(kid(root, "evenAndOddHeaders"), "val") !== "0",
    script: scriptOf(attr(tfl, "eastAsia") ?? docLang ?? "zh-TW"),
    lang,
    compat,
    footnoteFmt: val(kid(root, "footnotePr"), "numFmt"),
    endnoteFmt: val(kid(root, "endnotePr"), "numFmt"),
  };
}

/** Group body blocks by section: a paragraph's pPr/sectPr ends a section; the sectPr at the end of body is the last section */
function splitSections(body: Element | null): { sectPr: Element | null; children: Element[] }[] {
  const out: { sectPr: Element | null; children: Element[] }[] = [];
  let cur: Element[] = [];
  for (const c of flatKids(body)) {
    if (c.localName === "sectPr") continue;
    cur.push(c);
    const sp = c.localName === "p" ? kid(kid(c, "pPr"), "sectPr") : null;
    if (sp) {
      out.push({ sectPr: sp, children: cur });
      cur = [];
    }
  }
  out.push({ sectPr: kid(body, "sectPr"), children: cur });
  return out;
}

interface RenderState {
  disposed: boolean;
  ro: ResizeObserver | null;
}

export async function renderDocx(buf: ArrayBuffer, root: HTMLElement): Promise<RenderResult> {
  root.replaceChildren();
  const pkg = await OoxmlPackage.open(buf);
  const state: RenderState = { disposed: false, ro: null };
  const result: RenderResult = {
    dispose() {
      state.disposed = true;
      state.ro?.disconnect();
      state.ro = null;
      pkg.dispose();
    },
  };
  try {
    await build(pkg, root, state);
  } catch (e) {
    // Release already-created image URLs on failure too
    result.dispose();
    throw e;
  }
  return result;
}

async function build(pkg: OoxmlPackage, root: HTMLElement, state: RenderState) {
  // ── Read parts ──
  const rootRels = await pkg.rels("");
  const docPath = rootRels.find((r) => r.type.endsWith("/officeDocument"))?.target ?? "word/document.xml";
  const [docXml, docRels] = await Promise.all([pkg.xml(docPath), pkg.rels(docPath)]);
  const target = (suffix: string) => docRels.find((r) => r.type.endsWith(suffix) && !r.external)?.target;
  const load = (p: string | undefined) => (p ? pkg.xml(p) : Promise.resolve(null));
  const [stylesXml, numberingXml, themeXml, settingsXml, footXml, endXml] = await Promise.all([
    load(target("/styles")),
    load(target("/numbering")),
    load(target("/theme")),
    load(target("/settings")),
    load(target("/footnotes")),
    load(target("/endnotes")),
  ]);
  const hfTargets = docRels.filter((r) => /\/(header|footer)$/.test(r.type) && !r.external).map((r) => r.target);
  const noteParts = [target("/footnotes"), target("/endnotes"), target("/numbering")].filter((x): x is string => !!x);
  const parts = new Map<string, Document | null>();
  const rels = new Map<string, Rel[]>([
    [docPath, docRels],
    ["word/document.xml", docRels],
  ]);
  await Promise.all([
    ...hfTargets.map(async (p) => {
      const [x, r] = await Promise.all([pkg.xml(p), pkg.rels(p)]);
      parts.set(p, x);
      rels.set(p, r);
    }),
    ...noteParts.map(async (p) => rels.set(p, await pkg.rels(p))),
  ]);

  const theme = parseTheme(themeXml);
  const styles = new Styles(stylesXml);
  const referenced = new Set<string>();
  for (const x of [docXml, footXml, endXml, ...parts.values()]) for (const n of ["pStyle", "rStyle", "tblStyle"]) for (const e of descendants(x, n)) referenced.add(attr(e, "val") ?? "");
  referenced.add("FootnoteText");
  referenced.add("FootnoteReference");
  referenced.add("EndnoteReference");
  styles.addBuiltins(referenced);
  const numbering = new Numbering(numberingXml, styles);
  const settings = readSettings(settingsXml, styles);
  const notesOf = (x: Document | null, name: string) => {
    const m = new Map<string, Element>();
    for (const n of Array.from(x?.documentElement?.children ?? [])) {
      if (n.localName !== name) continue;
      const t = attr(n, "type");
      if (t === "separator" || t === "continuationSeparator" || t === "continuationNotice") continue;
      const id = attr(n, "id");
      if (id !== null) m.set(id, n);
    }
    return m;
  };
  const doc = new DocCtx(pkg, theme, styles, numbering, settings, rels, notesOf(footXml, "footnote"), notesOf(endXml, "endnote"));
  doc.numberingPart = target("/numbering") ?? null;

  // SmartArt: preload the drawings pre-rendered by Word
  const diagramParts: [string, Document | null][] = [[docPath, docXml], ...hfTargets.map((p) => [p, parts.get(p) ?? null] as [string, Document | null])];
  await Promise.all(
    diagramParts.flatMap(([part, xml]) =>
      descendants(xml, "relIds").map(async (ri) => {
        const dm = ri.getAttributeNS(R, "dm") ?? attr(ri, "dm");
        const partRels = rels.get(part) ?? [];
        const dmRel = partRels.find((r) => r.id === dm);
        if (!dmRel) return;
        const data = await pkg.xml(dmRel.target);
        const ext = descendants(data, "dataModelExt")[0];
        const drawingRel =
          partRels.find((r) => r.id === attr(ext, "relId")) ?? partRels.find((r) => r.type.endsWith("/diagramDrawing") && r.target.replace(/\D/g, "") === dmRel.target.replace(/\D/g, ""));
        if (drawingRel) doc.diagrams.set(`${part}|${dm}`, await pkg.xml(drawingRel.target));
      }),
    ),
  );

  // ── Sections and blocks ──
  const body = kid(docXml?.documentElement, "body");
  const groups = splitSections(body);
  const sections: Section[] = [];
  groups.forEach((g, i) => sections.push(parseSection(g.sectPr, i ? sections[i - 1] : null)));
  const bodyFloats: PageFloat[] = [];
  const blocks: BodyBlock[] = [];
  const shared = { fields: [] as Flow["fields"], comments: new Set<string>() };
  for (const [i, g] of groups.entries()) {
    const s = sections[i];
    const cols = s.cols.num;
    const width = cols > 1 ? (contentWidth(s) - s.cols.space * (cols - 1)) / cols : contentWidth(s);
    const flow: Flow = { doc, part: docPath, rels: docRels, section: s, width, story: "body", depth: 0, fields: shared.fields, comments: shared.comments, floats: bodyFloats };
    const writer = new BlockWriter(flow, (item) => blocks.push({ ...item, sec: i }));
    // Long documents: build blocks in slices so the page keeps responding (the writer keeps its state between calls)
    for (let at = 0; at < g.children.length; at += 50) {
      writer.blocks(g.children.slice(at, at + 50));
      await breathe();
      if (state.disposed) return;
    }
  }
  const baseFlow: Flow = { doc, part: docPath, rels: docRels, section: sections[0], width: contentWidth(sections[0]), story: "note", depth: 0, fields: [], comments: new Set(), floats: [] };
  const footnotes = renderNotes(doc.noteRefs, "foot", baseFlow, target("/footnotes") ?? null);
  const endnotes = endnoteBlock(renderNotes(doc.noteRefs, "end", baseFlow, target("/endnotes") ?? null));
  if (endnotes) blocks.push({ el: endnotes, pageBreak: false, columnBreak: false, sec: sections.length - 1 });
  if (!blocks.length) blocks.push({ el: h("p", { class: "tf-docx-p" }, h("br")), pageBreak: false, columnBreak: false, sec: 0 });

  // ── Layout ──
  const bg = textColor(kid(docXml?.documentElement, "background"), theme);
  const normal = runStyle(
    cascadeRun(
      styles.baseRunProps(),
      [styles.styleRun(styles.defaults.paragraph)].filter((x) => !!x),
      null,
    ),
    theme,
    settings.script,
  );
  const style = h("style", null, DOCX_CSS);
  const container = h("div", {
    class: "tf-docx",
    lang: /^[a-zA-Z-]{2,12}$/.test(settings.lang) ? settings.lang : undefined,
    style: css({ "font-family": normal.family, "font-size": normal.css["font-size"] }),
  });
  const host = h("div", { class: "tf-docx-host" });
  container.append(host);
  root.append(style, container);

  const hf = new HeaderFooters(doc, sections, host, parts);
  hf.prepare();
  const pager = new Paginator({ doc, sections, blocks, bodyFloats, footnotes, hf, host, background: bg, evenAndOdd: settings.evenAndOdd });
  pager.buildGalleys();
  try {
    await (document as Document & { fonts?: FontFaceSet }).fonts?.ready;
  } catch {
    // Measure right away when font loading status is unavailable
  }
  // Finish loading images and charts first (headers/footers carry the image URLs when cloned for each page)
  for (let n = 0; n < doc.pending.length;) {
    const batch = doc.pending.slice(n);
    n = doc.pending.length;
    await Promise.allSettled(batch);
  }
  if (state.disposed) return;
  fixScaled(doc.scaled);
  resolveTabs(doc.tabs);
  const pages = await pager.paginate();
  if (state.disposed) return;
  const pageEls = pager.build(pages);
  host.remove();

  const wraps = pageEls.map((page) => {
    const wrap = h("div", { class: "tf-docx-wrap" }, page);
    container.append(wrap);
    return wrap;
  });
  // Actual page size (the page grows taller when content overflows)
  const sizes = pageEls.map((p) => ({ w: p.offsetWidth, h: p.offsetHeight }));
  let lastScale = -1;
  const fit = () => {
    const avail = root.clientWidth - 32;
    const maxW = Math.max(...sizes.map((s) => s.w), 1);
    const scale = avail > 0 && avail < maxW ? avail / maxW : 1;
    if (Math.abs(scale - lastScale) < 0.001) return;
    lastScale = scale;
    wraps.forEach((wrap, i) => {
      const s = sizes[i];
      wrap.style.width = `${Math.round(s.w * scale * 100) / 100}px`;
      wrap.style.height = `${Math.round(s.h * scale * 100) / 100}px`;
      pageEls[i].style.transform = scale === 1 ? "" : `scale(${scale})`;
    });
  };
  fit();
  if (typeof ResizeObserver !== "undefined" && !state.disposed) {
    state.ro = new ResizeObserver(() => fit());
    state.ro.observe(root);
  }
}
