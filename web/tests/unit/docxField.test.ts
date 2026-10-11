// Fields in a Word document being edited: a paragraph holding fields (page numbers…) can be edited around them, each
// field kept as it is as one piece, and a page number or the number of pages is inserted at the caret
import { describe, expect, test } from "vitest";
import { attr } from "@/ooxml/core/package";
import { fieldsOf, isEditableParagraph, originalPieces, planEdits, readParagraph, saveEdits } from "@/ooxml/docx/edit";
import { insertField, MARK_END, MARK_START } from "@/ooxml/docx/formatOps";

const W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const parse = (xml: string) => new DOMParser().parseFromString(xml, "application/xml");
const doc = (body: string) => parse(`<w:document xmlns:w="${W}"><w:body>${body}<w:sectPr/></w:body></w:document>`);
/** Descendants by local name (happy-dom's getElementsByTagNameNS doesn't look below the first level) */
const find = (el: Element | Document, name: string) => Array.from(el.querySelectorAll("*")).filter((e) => e.localName === name);
const firstP = (d: Document) => find(d, "p")[0];

/** A page number as Word writes it: begin, code, separate, result, end */
const PAGE =
  '<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>3</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>';
const NUMPAGES = '<w:fldSimple w:instr=" NUMPAGES "><w:r><w:t>9</w:t></w:r></w:fldSimple>';

describe("paragraphs with fields", () => {
  test("a field whole in the paragraph is one piece; the text around it is the paragraph's", () => {
    const d = doc(`<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r>${PAGE}<w:r><w:t xml:space="preserve"> of </w:t></w:r>${NUMPAGES}</w:p>`);
    expect(fieldsOf(firstP(d))?.map((f) => f.length)).toEqual([5, 1]);
    expect(isEditableParagraph(firstP(d))).toBe(true);
    const plan = planEdits(d)!;
    expect(originalPieces(plan, 0)).toEqual([
      { run: 0, text: "Page " },
      { run: null, text: "", atom: 0 },
      { run: 1, text: " of " },
      { run: null, text: "", atom: 1 },
    ]);
  });

  test("a field not whole in the paragraph, or starting in a run with text, keeps the paragraph as it is", () => {
    const begin = doc('<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> TOC </w:instrText></w:r></w:p>');
    expect(isEditableParagraph(firstP(begin))).toBe(false);
    const mixed = doc('<w:p><w:r><w:t>x</w:t><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>');
    expect(isEditableParagraph(firstP(mixed))).toBe(false);
    const stray = doc('<w:p><w:r><w:fldChar w:fldCharType="separate"/></w:r></w:p>');
    expect(isEditableParagraph(firstP(stray))).toBe(false);
  });

  test("text typed around a field keeps the field exactly; a field deleted in the view goes", () => {
    const d = doc(`<w:p><w:bookmarkStart w:id="1" w:name="here"/>${NUMPAGES}<w:r><w:t>pages</w:t></w:r></w:p>`);
    const plan = planEdits(d)!;
    const [field, pages] = originalPieces(plan, 0);
    const typed = saveEdits(plan, [{ kind: "para", source: 0, pieces: [{ run: pages.run, text: "Total " }, field, { run: pages.run, text: " pages" }] }], new Set([0]))!;
    const p = firstP(parse(typed));
    expect(Array.from(p.children).map((c) => c.localName)).toEqual(["r", "bookmarkStart", "fldSimple", "r"]);
    expect(find(p, "t").map((t) => t.textContent)).toEqual(["Total ", "9", " pages"]);
    expect(attr(find(p, "fldSimple")[0], "instr")).toBe(" NUMPAGES ");
    const gone = saveEdits(plan, [{ kind: "para", source: 0, pieces: [pages] }], new Set([0]))!;
    expect(find(parse(gone), "fldSimple")).toHaveLength(0);
  });

  test("the view's field is read back as its piece, not as text", () => {
    const p = document.createElement("p");
    p.innerHTML = '<span data-r="0">Page </span><span data-atom="0" data-ro="" contenteditable="false">3</span><span data-r="1"> of </span>';
    expect(readParagraph(p)).toEqual([
      { run: 0, text: "Page " },
      { run: null, text: "", atom: 0 },
      { run: 1, text: " of " },
    ]);
  });
});

describe("inserting a field", () => {
  test("a page number goes at the mark, between the halves of its run, formatted as the run", () => {
    // As the view writes it: the space before the marks isn't at the text's edge, so nothing says to keep it yet
    const d = doc(`<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Page ${MARK_START}${MARK_END}here</w:t></w:r></w:p>`);
    expect(insertField(d, "PAGE")).toBe(true);
    const p = firstP(d);
    expect(Array.from(p.children).map((c) => c.localName)).toEqual(["r", "fldSimple", "r"]);
    const field = find(p, "fldSimple")[0];
    expect(attr(field, "instr")).toBe(" PAGE ");
    expect(find(field, "b")).toHaveLength(1);
    expect(find(p, "t").map((t) => t.textContent)).toEqual(["Page ", "1", "here"]);
    expect(find(p, "t")[0].getAttribute("xml:space")).toBe("preserve");
    // The paragraph can still be edited, the field kept as one piece
    expect(isEditableParagraph(p)).toBe(true);
  });

  test("at the start or end of a run no empty run is left; in a link or without the mark nothing is inserted", () => {
    const d = doc(`<w:p><w:r><w:t>${MARK_START}${MARK_END}</w:t></w:r></w:p>`);
    expect(insertField(d, "NUMPAGES")).toBe(true);
    expect(Array.from(firstP(d).children).map((c) => c.localName)).toEqual(["fldSimple"]);
    const end = doc(`<w:p><w:r><w:t>of ${MARK_START}${MARK_END}</w:t></w:r></w:p>`);
    insertField(end, "NUMPAGES");
    expect(Array.from(firstP(end).children).map((c) => c.localName)).toEqual(["r", "fldSimple"]);
    const link = doc(`<w:p><w:hyperlink><w:r><w:t>a${MARK_START}${MARK_END}b</w:t></w:r></w:hyperlink></w:p>`);
    expect(insertField(link, "PAGE")).toBe(false);
    expect(find(link, "t")[0].textContent).toBe("ab");
    expect(insertField(doc("<w:p><w:r><w:t>x</w:t></w:r></w:p>"), "PAGE")).toBe(false);
  });

  test("in a footer too", () => {
    const d = parse(`<w:ftr xmlns:w="${W}"><w:p><w:r><w:t>${MARK_START}${MARK_END}</w:t></w:r></w:p></w:ftr>`);
    expect(insertField(d, "PAGE")).toBe(true);
    expect(find(d, "fldSimple")).toHaveLength(1);
  });
});
