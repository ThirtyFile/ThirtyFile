/**
 * Headers and footers of a Word document (the editor's Header and footer menu): which ones the document has, and
 * adding a header or footer to a document without one.
 *
 * Done the way Word does it:
 * - A section names its headers and footers (default, first page, even pages) by relationship. A section that doesn't
 *   name one of a kind uses the section before's, so a header added to the first section is every section's that has
 *   none of its own.
 * - A new header or footer is a part of its own (word/header1.xml…), with its relationship and content type, holding
 *   one empty paragraph in the document's Header or Footer style when it defines one.
 */

import { attr, kid } from "../core/package";
import { WordXml } from "./xmlEdit";

export type StoryKind = "header" | "footer";
export type StoryType = "default" | "first" | "even";

/** A header or footer the document has: its part, and the first section it is shown in */
export interface Story {
  path: string;
  kind: StoryKind;
  type: StoryType;
  section: number;
}

/** The parts adding a header or footer reads and changes */
export interface StoryParts {
  doc: Document;
  /** The document's relationships, the package's content types, and the styles (null when the document has none) */
  rels: Document;
  types: Document | null;
  styles: Document | null;
  /** Path of document.xml (the new part goes next to it) */
  docPath: string;
  /** Whether the package already has a part at a path */
  taken(path: string): boolean;
}

export interface StoryAdded {
  /** Path of the new part, and the part */
  path: string;
  part: Document;
  /** Whether the content types changed (the document and its relationships always do) */
  typesChanged: boolean;
}

const RELS = "http://schemas.openxmlformats.org/package/2006/relationships";
const STRICT = "http://purl.oclc.org/ooxml/wordprocessingml/main";
const kids = (el: Element | null | undefined, name: string) => Array.from(el?.children ?? []).filter((c) => c.localName === name);

/**
 * The headers and footers the sections show (each section's own, and those it takes from the section before), each
 * part once, in the order of the sections: headers, then footers, each default, first page, even pages
 */
export function listStories(
  sections: { headers: Partial<Record<StoryType, string>>; footers: Partial<Record<StoryType, string>> }[],
  rels: { id: string; target: string; external?: boolean }[],
  exists: (path: string) => boolean,
): Story[] {
  const out: Story[] = [];
  const seen = new Set<string>();
  sections.forEach((s, section) => {
    for (const kind of ["header", "footer"] as const)
      for (const type of ["default", "first", "even"] as const) {
        const id = (kind === "header" ? s.headers : s.footers)[type];
        const path = id ? rels.find((r) => r.id === id && !r.external)?.target : undefined;
        if (!path || seen.has(path) || !exists(path)) continue;
        seen.add(path);
        out.push({ path, kind, type, section });
      }
  });
  return out;
}

/** The section properties of the document's first section: the first paragraph's that ends a section, or the body's */
function firstSection(x: WordXml, body: Element): Element {
  for (const b of Array.from(body.children)) {
    const sp = b.localName === "p" ? kid(kid(b, "pPr"), "sectPr") : null;
    if (sp) return sp;
  }
  const last = kids(body, "sectPr").pop();
  if (last) return last;
  const made = x.el("sectPr");
  body.append(made);
  return made;
}

/**
 * Adds a header or footer for every page (default) to the document's first section, as a new part; undefined when the
 * first section already has one of that kind
 */
export function addStory(parts: StoryParts, kind: StoryKind): StoryAdded | undefined {
  const root = parts.doc.documentElement;
  const body = kids(root, "body")[0];
  if (!body) return undefined;
  const x = new WordXml(parts.doc, body);
  const sectPr = firstSection(x, body);
  const refName = `${kind}Reference`;
  if (kids(sectPr, refName).some((r) => (attr(r, "type") ?? "default") === "default")) return undefined;

  const strict = x.ns === STRICT;
  const relNs = strict ? "http://purl.oclc.org/ooxml/officeDocument/relationships" : "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
  const dir = parts.docPath.replace(/[^/]*$/, "");
  let n = 1;
  while (parts.taken(`${dir}${kind}${n}.xml`)) n++;
  const name = `${kind}${n}.xml`;
  const path = `${dir}${name}`;

  // The relationship, with an id the document doesn't use
  const rels = parts.rels.documentElement;
  const ids = new Set(kids(rels, "Relationship").map((r) => r.getAttribute("Id")));
  let id = 1;
  while (ids.has(`rId${id}`)) id++;
  const rel = parts.rels.createElementNS(RELS, "Relationship");
  rel.setAttribute("Id", `rId${id}`);
  rel.setAttribute("Type", `${relNs}/${kind}`);
  rel.setAttribute("Target", name);
  rels.append(rel);

  // The content type
  const types = parts.types?.documentElement;
  let typesChanged = false;
  if (types && !kids(types, "Override").some((o) => o.getAttribute("PartName") === `/${path}`)) {
    const over = parts.types!.createElementNS(types.namespaceURI, "Override");
    over.setAttribute("PartName", `/${path}`);
    over.setAttribute("ContentType", `application/vnd.openxmlformats-officedocument.wordprocessingml.${kind}+xml`);
    types.append(over);
    typesChanged = true;
  }

  // The section's reference to it: references come first in a section's properties
  const ref = x.el(refName);
  x.set(ref, "type", "default");
  ref.setAttributeNS(relNs, "r:id", `rId${id}`);
  const refs = Array.from(sectPr.children).filter((c) => c.localName === "headerReference" || c.localName === "footerReference");
  if (refs.length) refs[refs.length - 1].after(ref);
  else sectPr.prepend(ref);

  // The part: one empty paragraph, in the Header or Footer style when the document defines it
  const tag = (local: string) => (x.prefix ? `${x.prefix}:${local}` : local);
  const part = new DOMParser().parseFromString(`<${tag(kind === "header" ? "hdr" : "ftr")} ${x.prefix ? `xmlns:${x.prefix}` : "xmlns"}="${x.ns}" xmlns:r="${relNs}"/>`, "application/xml");
  const px = new WordXml(part, part.documentElement);
  const p = px.el("p");
  const styleId = kind === "header" ? "Header" : "Footer";
  const defined = kids(parts.styles?.documentElement, "style").some((s) => attr(s, "type") === "paragraph" && attr(s, "styleId") === styleId);
  if (defined) {
    const pPr = px.el("pPr");
    pPr.append(px.el("pStyle", styleId));
    p.append(pPr);
  }
  part.documentElement.append(p);
  return { path, part, typesChanged };
}
