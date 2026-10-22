// Keeping a Word document's content when it is edited: characters people make themselves (private use, U+E000…) are
// never taken for the editor's selection marks, bookmarks stay when the text after them goes, text typed into an
// empty paragraph gets only formatting the view shows, and lists in Strict documents are linked as Strict ones
import { describe, expect, test } from "vite-plus/test";
import { attr } from "@/ooxml/core/package";
import { originalPieces, planEdits, readEdits, saveEdits } from "@/ooxml/docx/edit";
import { applyFmt, AUTO_COLOR, formatRange, hasMarks, MARK_END, MARK_START } from "@/ooxml/docx/formatOps";
import { formatList, type ListParts } from "@/ooxml/docx/listOps";
import { WordXml } from "@/ooxml/docx/xmlEdit";

const W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const STRICT = "http://purl.oclc.org/ooxml/wordprocessingml/main";
const parse = (xml: string) => new DOMParser().parseFromString(xml, "application/xml");
const doc = (body: string, ns = W) => parse(`<w:document xmlns:w="${ns}"><w:body>${body}<w:sectPr/></w:body></w:document>`);
/** Descendants by local name (happy-dom's getElementsByTagNameNS doesn't look below the first level) */
const find = (el: Element | Document, name: string) => Array.from(el.querySelectorAll("*")).filter((e) => e.localName === name);
/** The local names of a paragraph's children, bookmarks with their name or id */
const shape = (p: Element) =>
  Array.from(p.children).map((c) => (c.localName.startsWith("bookmark") ? `${c.localName}:${attr(c, "name") ?? attr(c, "id")}` : c.localName === "r" ? `r:${c.textContent}` : c.localName));
const CUSTOM = String.fromCharCode(0xe000);

describe("the editor's selection marks", () => {
  test("a custom character (U+E000) is kept, and only the marked paragraph changes", () => {
    const d = doc(`<w:p><w:r><w:t>${CUSTOM} name</w:t></w:r></w:p><w:p><w:r><w:t>${MARK_START}text${MARK_END}</w:t></w:r></w:p>`);
    expect(formatRange(d, { align: "center" })).toBe(true);
    expect(find(d, "t").map((t) => t.textContent)).toEqual([`${CUSTOM} name`, "text"]);
    expect(find(d, "p").map((p) => attr(find(p, "jc")[0], "val"))).toEqual([null, "center"]);
    expect([hasMarks(CUSTOM), hasMarks(`a${MARK_START}`)]).toEqual([false, true]);
  });
});

describe("bookmarks", () => {
  test("a bookmark stays when the text after it is deleted, in its place among what is left", () => {
    const d = doc('<w:p><w:bookmarkStart w:id="1" w:name="X"/><w:r><w:t>Hello</w:t></w:r><w:bookmarkEnd w:id="1"/><w:r><w:t xml:space="preserve"> world</w:t></w:r></w:p>');
    const plan = planEdits(d)!;
    const [, world] = originalPieces(plan, 0);
    const p = find(parse(saveEdits(plan, [{ kind: "para", source: 0, pieces: [world] }], new Set([0]))!), "p")[0];
    expect(shape(p)).toEqual(["bookmarkStart:X", "bookmarkEnd:1", "r: world"]);
  });

  test("a bookmark before a run without text, or before the last run, stays at the paragraph's end", () => {
    const d = doc(
      '<w:p><w:bookmarkStart w:id="2" w:name="_Toc1"/><w:r><w:lastRenderedPageBreak/></w:r><w:r><w:t>Title</w:t></w:r><w:bookmarkStart w:id="3" w:name="Y"/><w:r><w:t>!</w:t></w:r></w:p>',
    );
    const plan = planEdits(d)!;
    const [title] = originalPieces(plan, 0);
    const p = find(parse(saveEdits(plan, [{ kind: "para", source: 0, pieces: [{ run: title.run, text: "New title" }] }], new Set([0]))!), "p")[0];
    expect(shape(p)).toEqual(["bookmarkStart:_Toc1", "r:New title", "bookmarkStart:Y"]);
  });

  test("a bookmark before a field that is deleted stays", () => {
    const d = doc('<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:bookmarkStart w:id="4" w:name="Z"/><w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p>');
    const plan = planEdits(d)!;
    const [page] = originalPieces(plan, 0);
    const p = find(parse(saveEdits(plan, [{ kind: "para", source: 0, pieces: [page] }], new Set([0]))!), "p")[0];
    expect(shape(p)).toEqual(["r:Page ", "bookmarkStart:Z"]);
  });
});

describe("text typed into an empty paragraph", () => {
  test("gets the paragraph mark's formatting, but not hidden text, highlight or shading", () => {
    const d = doc('<w:p><w:pPr><w:rPr><w:b/><w:vanish/><w:highlight w:val="yellow"/><w:shd w:val="clear" w:fill="FF0000"/></w:rPr></w:pPr></w:p>');
    const plan = planEdits(d)!;
    const p = find(parse(saveEdits(plan, [{ kind: "para", source: 0, pieces: [{ run: null, text: "typed" }] }], new Set([0]))!), "p")[0];
    const rPr = find(find(p, "r")[0], "rPr")[0];
    expect(Array.from(rPr.children).map((c) => c.localName)).toEqual(["b"]);
  });

  test("text straight in the view is read with the formatting shown there", () => {
    const root = document.createElement("div");
    root.innerHTML = "plain<b>bold</b>";
    const edits = readEdits(root, (text) => (text.parentElement?.tagName === "B" ? { b: true } : undefined));
    expect(edits).toEqual([
      {
        kind: "para",
        source: null,
        pieces: [
          { run: null, text: "plain" },
          { run: null, text: "bold", fmt: { b: true } },
        ],
      },
    ]);
  });
});

describe("colors and lists", () => {
  test("automatic font color is written as auto", () => {
    const d = doc('<w:p><w:r><w:rPr><w:color w:val="FF0000" w:themeColor="accent2"/></w:rPr><w:t>x</w:t></w:r></w:p>');
    const rPr = find(d, "rPr")[0];
    applyFmt(new WordXml(d, rPr), rPr, { color: AUTO_COLOR });
    const color = find(rPr, "color")[0];
    expect([attr(color, "val"), attr(color, "themeColor")]).toEqual(["auto", null]);
  });

  test("a list in a Strict document, whose main part isn't in word/, gets a Strict relationship to a part next to it", () => {
    const ps: ListParts = {
      doc: doc(`<w:p><w:r><w:t>${MARK_START}a${MARK_END}</w:t></w:r></w:p>`, STRICT),
      numbering: null,
      styles: null,
      rels: parse('<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>'),
      types: parse('<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>'),
      numberingPath: "doc/numbering.xml",
    };
    const r = formatList(ps, { list: "bullet" })!;
    expect(r.numbering!.documentElement.namespaceURI).toBe(STRICT);
    const rel = find(ps.rels!, "Relationship")[0];
    expect([rel.getAttribute("Type"), rel.getAttribute("Target")]).toEqual(["http://purl.oclc.org/ooxml/officeDocument/relationships/numbering", "numbering.xml"]);
    expect(find(ps.types!, "Override")[0].getAttribute("PartName")).toBe("/doc/numbering.xml");
  });
});
