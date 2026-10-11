/**
 * Headers and footers of a Word document (the editor's Header and footer menu): which ones the document has, adding
 * a header or footer to a document without one, and a different first page or different odd and even pages.
 *
 * Done the way Word does it:
 * - A section names its headers and footers (default, first page, even pages) by relationship. A section that doesn't
 *   name one of a kind uses the section before's, so a header added to the first section is every section's that has
 *   none of its own.
 * - A new header or footer is a part of its own (word/header1.xml…), with its relationship and content type, holding
 *   one empty paragraph in the document's Header or Footer style when it defines one.
 * - A different first page is a section's own setting (w:titlePg); different odd and even pages are the whole
 *   document's (w:evenAndOddHeaders, in its settings part, which is made when the document has none). Turning either on
 *   adds the first-page or even-page header and footer when there are none yet, empty, as Word shows them; turning it
 *   off keeps them, unused, as Word does.
 */

import { attr, kid } from "../core/package";
import { placeIn, WordXml } from "./xmlEdit";

export type StoryKind = "header" | "footer";
export type StoryType = "default" | "first" | "even";

/** A header or footer the document has: its part, and the first section it is shown in */
export interface Story {
  path: string;
  kind: StoryKind;
  type: StoryType;
  section: number;
}

/** The parts changing headers and footers reads and changes */
export interface StoryParts {
  doc: Document;
  /** The document's relationships, the package's content types, and the styles (null when the document has none) */
  rels: Document;
  types: Document | null;
  styles: Document | null;
  /** The document's settings (null when it has none: one is made when needed) */
  settings?: Document | null;
  /** Path of document.xml (new parts go next to it) */
  docPath: string;
  /** Whether the package already has a part at a path */
  taken(path: string): boolean;
}

/** A new header or footer: its path and part */
export interface MadePart {
  path: string;
  part: Document;
  kind: StoryKind;
  type: StoryType;
}

export interface StoryAdded {
  /** Path of the new part, and the part */
  path: string;
  part: Document;
  /** Whether the content types changed (the document and its relationships always do) */
  typesChanged: boolean;
}

/** What a change of the headers' layout made, besides document.xml */
export interface LayoutChange {
  /** New headers and footers */
  made: MadePart[];
  /** The settings part, when it changed or was made (`settingsPath` set when made) */
  settings: Document | null;
  settingsPath: string | null;
  relsChanged: boolean;
  typesChanged: boolean;
}

const RELS = "http://schemas.openxmlformats.org/package/2006/relationships";
const STRICT = "http://purl.oclc.org/ooxml/wordprocessingml/main";
const kids = (el: Element | null | undefined, name: string) => Array.from(el?.children ?? []).filter((c) => c.localName === name);

/** The order Word needs a section's properties in (CT_SectPr), after its headers and footers */
const SECTPR = [
  "headerReference",
  "footerReference",
  "footnotePr",
  "endnotePr",
  "type",
  "pgSz",
  "pgMar",
  "paperSrc",
  "pgBorders",
  "lnNumType",
  "pgNumType",
  "cols",
  "formProt",
  "vAlign",
  "noEndnote",
  "titlePg",
  "textDirection",
  "bidi",
  "rtlGutter",
  "docGrid",
  "printerSettings",
  "sectPrChange",
];
/** The order Word needs the document's settings in (CT_Settings), as far as w:evenAndOddHeaders and the next few */
const SETTINGS = [
  "writeProtection",
  "view",
  "zoom",
  "removePersonalInformation",
  "removeDateAndTime",
  "doNotDisplayPageBoundaries",
  "displayBackgroundShape",
  "printPostScriptOverText",
  "printFractionalCharacterWidth",
  "printFormsData",
  "embedTrueTypeFonts",
  "embedSystemFonts",
  "saveSubsetFonts",
  "saveFormsData",
  "mirrorMargins",
  "alignBordersAndEdges",
  "bordersDoNotSurroundHeader",
  "bordersDoNotSurroundFooter",
  "gutterAtTop",
  "hideSpellingErrors",
  "hideGrammaticalErrors",
  "activeWritingStyle",
  "proofState",
  "formsDesign",
  "attachedTemplate",
  "linkStyles",
  "stylePaneFormatFilter",
  "stylePaneSortMethod",
  "documentType",
  "mailMerge",
  "revisionView",
  "trackRevisions",
  "doNotTrackMoves",
  "doNotTrackFormatting",
  "documentProtection",
  "autoFormatOverride",
  "styleLockTheme",
  "styleLockQFSet",
  "defaultTabStop",
  "autoHyphenation",
  "consecutiveHyphenLimit",
  "hyphenationZone",
  "doNotHyphenateCaps",
  "showEnvelope",
  "summaryLength",
  "clickAndTypeStyle",
  "defaultTableStyle",
  "evenAndOddHeaders",
  "bookFoldRevPrinting",
  "bookFoldPrinting",
  "bookFoldPrintingSheets",
  "drawingGridHorizontalSpacing",
  "drawingGridVerticalSpacing",
  "displayHorizontalDrawingGridEvery",
  "displayVerticalDrawingGridEvery",
  "doNotUseMarginsForDrawingGridOrigin",
  "drawingGridHorizontalOrigin",
  "drawingGridVerticalOrigin",
  "doNotShadeFormData",
  "noPunctuationKerning",
  "characterSpacingControl",
  "printTwoOnOne",
  "strictFirstAndLastChars",
  "noLineBreaksAfter",
  "noLineBreaksBefore",
  "savePreviewPicture",
  "doNotValidateAgainstSchema",
  "saveInvalidXml",
  "ignoreMixedContent",
  "alwaysShowPlaceholderText",
  "doNotDemarcateInvalidXml",
  "saveXmlDataOnly",
  "useXSLTWhenSaving",
  "saveThroughXslt",
  "showXMLTags",
  "alwaysMergeEmptyNamespace",
  "updateFields",
  "hdrShapeDefaults",
  "footnotePr",
  "endnotePr",
  "compat",
  "docVars",
  "rsids",
  "mathPr",
  "attachedSchema",
  "themeFontLang",
  "clrSchemeMapping",
  "doNotIncludeSubdocsInStats",
  "doNotAutoCompressPictures",
  "forceUpgrade",
  "captions",
  "readModeInkLockDown",
  "smartTagType",
  "schemaLibrary",
  "shapeDefaults",
  "doNotEmbedSmartTags",
  "decimalSymbol",
  "listSeparator",
];

/**
 * The headers and footers the sections show (each section's own, and those it takes from the section before), each
 * part once, in the order of the sections: headers, then footers, each default, first page, even pages. First-page
 * ones only where the section has a different first page, even-page ones only with different odd and even pages.
 */
export function listStories(
  sections: { headers: Partial<Record<StoryType, string>>; footers: Partial<Record<StoryType, string>>; titlePg?: boolean }[],
  rels: { id: string; target: string; external?: boolean }[],
  exists: (path: string) => boolean,
  evenOdd: boolean,
): Story[] {
  const out: Story[] = [];
  const seen = new Set<string>();
  sections.forEach((s, section) => {
    for (const kind of ["header", "footer"] as const)
      for (const type of ["default", "first", "even"] as const) {
        if ((type === "first" && !s.titlePg) || (type === "even" && !evenOdd)) continue;
        const id = (kind === "header" ? s.headers : s.footers)[type];
        const path = id ? rels.find((r) => r.id === id && !r.external)?.target : undefined;
        if (!path || seen.has(path) || !exists(path)) continue;
        seen.add(path);
        out.push({ path, kind, type, section });
      }
  });
  return out;
}

/** Making headers and footers in a document: new parts, their relationships and content types */
class Stories {
  readonly x: WordXml;
  readonly body: Element;
  readonly relNs: string;
  readonly made: MadePart[] = [];
  relsChanged = false;
  typesChanged = false;
  private readonly used = new Set<string>();

  constructor(private parts: StoryParts) {
    this.body = kids(parts.doc.documentElement, "body")[0];
    this.x = new WordXml(parts.doc, this.body);
    this.relNs = this.x.ns === STRICT ? "http://purl.oclc.org/ooxml/officeDocument/relationships" : "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
  }

  /** Every section's properties in order: the paragraphs' that end a section, then the body's (made when missing) */
  sections(): Element[] {
    const out: Element[] = [];
    for (const b of Array.from(this.body.children)) {
      const sp = b.localName === "p" ? kid(kid(b, "pPr"), "sectPr") : null;
      if (sp) out.push(sp);
    }
    let last = kids(this.body, "sectPr").pop();
    if (!last) {
      last = this.x.el("sectPr");
      this.body.append(last);
    }
    out.push(last);
    return out;
  }

  /** Whether a section shows a header or footer of a type: its own, or one a section before it names */
  has(sections: Element[], index: number, kind: StoryKind, type: StoryType): boolean {
    return sections.slice(0, index + 1).some((sp) => kids(sp, `${kind}Reference`).some((r) => (attr(r, "type") ?? "default") === type));
  }

  /** A free path next to document.xml: name1.xml, name2.xml… */
  private freePath(name: string): string {
    const dir = this.parts.docPath.replace(/[^/]*$/, "");
    let n = 1;
    while (this.parts.taken(`${dir}${name}${n}.xml`) || this.used.has(`${dir}${name}${n}.xml`)) n++;
    const path = `${dir}${name}${n}.xml`;
    this.used.add(path);
    return path;
  }

  /** A relationship of the document to a new part (an id it doesn't use), and the part's content type */
  private relate(path: string, type: string, contentType: string): string {
    const rels = this.parts.rels.documentElement;
    const ids = new Set(kids(rels, "Relationship").map((r) => r.getAttribute("Id")));
    let id = 1;
    while (ids.has(`rId${id}`)) id++;
    const rel = this.parts.rels.createElementNS(RELS, "Relationship");
    rel.setAttribute("Id", `rId${id}`);
    rel.setAttribute("Type", `${this.relNs}/${type}`);
    rel.setAttribute("Target", path.slice(path.lastIndexOf("/") + 1));
    rels.append(rel);
    this.relsChanged = true;
    const types = this.parts.types?.documentElement;
    if (types && !kids(types, "Override").some((o) => o.getAttribute("PartName") === `/${path}`)) {
      const over = this.parts.types!.createElementNS(types.namespaceURI, "Override");
      over.setAttribute("PartName", `/${path}`);
      over.setAttribute("ContentType", contentType);
      types.append(over);
      this.typesChanged = true;
    }
    return `rId${id}`;
  }

  /** A new header or footer of a type, named by a section (references come first in a section's properties) */
  make(sectPr: Element, kind: StoryKind, type: StoryType): MadePart {
    const { x } = this;
    const path = this.freePath(kind);
    const id = this.relate(path, kind, `application/vnd.openxmlformats-officedocument.wordprocessingml.${kind}+xml`);
    const ref = x.el(`${kind}Reference`);
    x.set(ref, "type", type);
    ref.setAttributeNS(this.relNs, "r:id", id);
    const refs = Array.from(sectPr.children).filter((c) => c.localName === "headerReference" || c.localName === "footerReference");
    if (refs.length) refs[refs.length - 1].after(ref);
    else sectPr.prepend(ref);

    // The part: one empty paragraph, in the Header or Footer style when the document defines it
    const tag = (local: string) => (x.prefix ? `${x.prefix}:${local}` : local);
    const part = new DOMParser().parseFromString(
      `<${tag(kind === "header" ? "hdr" : "ftr")} ${x.prefix ? `xmlns:${x.prefix}` : "xmlns"}="${x.ns}" xmlns:r="${this.relNs}"/>`,
      "application/xml",
    );
    const px = new WordXml(part, part.documentElement);
    const p = px.el("p");
    const styleId = kind === "header" ? "Header" : "Footer";
    const defined = kids(this.parts.styles?.documentElement, "style").some((s) => attr(s, "type") === "paragraph" && attr(s, "styleId") === styleId);
    if (defined) {
      const pPr = px.el("pPr");
      pPr.append(px.el("pStyle", styleId));
      p.append(pPr);
    }
    part.documentElement.append(p);
    const made = { path, part, kind, type };
    this.made.push(made);
    return made;
  }

  /** The settings part, made (with its relationship and content type) when the document has none */
  settings(): { doc: Document; path: string | null } {
    if (this.parts.settings?.documentElement) return { doc: this.parts.settings, path: null };
    const { x } = this;
    const doc = new DOMParser().parseFromString(`<${x.prefix ? `${x.prefix}:settings xmlns:${x.prefix}` : "settings xmlns"}="${x.ns}"/>`, "application/xml");
    const usual = this.parts.docPath.replace(/[^/]*$/, "settings.xml");
    const path = this.parts.taken(usual) ? this.freePath("settings") : usual;
    this.relate(path, "settings", "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml");
    return { doc, path };
  }

  result(settings: Document | null = null, settingsPath: string | null = null): LayoutChange {
    return { made: this.made, settings, settingsPath, relsChanged: this.relsChanged, typesChanged: this.typesChanged };
  }
}

/**
 * Adds a header or footer for every page (default) to the document's first section, as a new part; undefined when the
 * first section already has one of that kind
 */
export function addStory(parts: StoryParts, kind: StoryKind): StoryAdded | undefined {
  const s = new Stories(parts);
  if (!s.body) return undefined;
  const sections = s.sections();
  if (s.has(sections, 0, kind, "default")) return undefined;
  const made = s.make(sections[0], kind, "default");
  return { path: made.path, part: made.part, typesChanged: s.typesChanged };
}

/**
 * A different first page for a section, or not: turned on, the section gets a first-page header and footer when it
 * shows none yet
 */
export function setFirstPage(parts: StoryParts, section: number, on: boolean): LayoutChange | undefined {
  const s = new Stories(parts);
  if (!s.body) return undefined;
  const sections = s.sections();
  const index = Math.min(section, sections.length - 1);
  const sectPr = sections[index];
  const titlePg = kid(sectPr, "titlePg");
  if (on) {
    s.x.unset(titlePg ?? placeIn(sectPr, s.x.el("titlePg"), SECTPR), "val");
    for (const kind of ["header", "footer"] as const) if (!s.has(sections, index, kind, "first")) s.make(sectPr, kind, "first");
  } else titlePg?.remove();
  return s.result();
}

/**
 * Different odd and even pages for the whole document, or not: turned on, the first section gets an even-page header
 * and footer when it has none yet (later sections use them, as they use the first section's others)
 */
export function setEvenOdd(parts: StoryParts, on: boolean): LayoutChange | undefined {
  const s = new Stories(parts);
  if (!s.body) return undefined;
  const sections = s.sections();
  if (!on) {
    const el = kid(parts.settings?.documentElement, "evenAndOddHeaders");
    if (!el) return s.result();
    el.remove();
    return s.result(parts.settings!, null);
  }
  const { doc, path } = s.settings();
  const root = doc.documentElement;
  const sx = new WordXml(doc, root);
  sx.unset(kid(root, "evenAndOddHeaders") ?? placeIn(root, sx.el("evenAndOddHeaders"), SETTINGS), "val");
  for (const kind of ["header", "footer"] as const) if (!s.has(sections, 0, kind, "even")) s.make(sections[0], kind, "even");
  return s.result(doc, path);
}
