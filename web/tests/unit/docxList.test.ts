// Lists in a Word document: bulleted and numbered lists made the way Word makes them (definitions like Word's own,
// added to the numbering part, which is made when missing), turned off, and moved a level in or out
import { describe, expect, test } from "vite-plus/test";
import { attr } from "@/ooxml/core/package";
import { MARK_END, MARK_START } from "@/ooxml/docx/formatOps";
import { formatList, listKind, type ListParts } from "@/ooxml/docx/listOps";

const W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const parse = (xml: string) => new DOMParser().parseFromString(xml, "application/xml");
/** Descendants by local name (happy-dom's getElementsByTagNameNS doesn't look below the first level) */
const find = (el: Element | Document, name: string) => Array.from(el.querySelectorAll("*")).filter((e) => e.localName === name);
const kidsOf = (el: Element) => Array.from(el.children).map((c) => c.localName);

const p = (text: string, pPr = "") => `<w:p>${pPr ? `<w:pPr>${pPr}</w:pPr>` : ""}<w:r><w:t>${text}</w:t></w:r></w:p>`;
const numPr = (numId: number, ilvl = 0) => `<w:numPr><w:ilvl w:val="${ilvl}"/><w:numId w:val="${numId}"/></w:numPr>`;
const BULLETS = `<w:abstractNum w:abstractNumId="3"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="-"/></w:lvl></w:abstractNum>`;
const NUMBERS = `<w:abstractNum w:abstractNumId="4"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>`;

function parts(body: string, numbering?: string, styles?: string): ListParts {
  return {
    doc: parse(`<w:document xmlns:w="${W}"><w:body>${body}<w:sectPr/></w:body></w:document>`),
    numbering: numbering === undefined ? null : parse(`<w:numbering xmlns:w="${W}">${numbering}</w:numbering>`),
    styles: styles ? parse(`<w:styles xmlns:w="${W}">${styles}</w:styles>`) : null,
    rels: parse('<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="x" Target="styles.xml"/></Relationships>'),
    types: parse('<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/></Types>'),
  };
}

/** Each paragraph's list (numId) and level, or "-" */
const lists = (doc: Document) =>
  find(doc, "p").map((q) => {
    const n = find(q, "numPr")[0];
    return n ? `${attr(find(n, "numId")[0], "val")}/${attr(find(n, "ilvl")[0], "val")}` : "-";
  });

describe("making lists", () => {
  test("a bulleted list in a document without a numbering part makes one, with its relationship and content type", () => {
    const ps = parts(p(`${MARK_START}one`) + p(`two${MARK_END}`) + p("three"));
    const r = formatList(ps, { list: "bullet" })!;
    expect(r.changed).toEqual({ numbering: true, rels: true, types: true });
    expect(lists(ps.doc)).toEqual(["1/0", "1/0", "-"]);
    const root = r.numbering!.documentElement;
    expect(kidsOf(root)).toEqual(["abstractNum", "num"]);
    expect(find(root, "lvl")).toHaveLength(9);
    expect(listKind(r.numbering, "1", 0)).toBe("bullet");
    expect(
      find(root, "lvl")
        .map((l) => attr(find(l, "lvlText")[0], "val"))
        .slice(0, 3),
    ).toEqual([String.fromCharCode(0xf0b7), "o", String.fromCharCode(0xf0a7)]);
    const rel = find(ps.rels!, "Relationship").pop()!;
    expect([attr(rel, "Id"), attr(rel, "Target")]).toEqual(["rId2", "numbering.xml"]);
    expect(attr(find(ps.types!, "Override")[0], "PartName")).toBe("/word/numbering.xml");
    // The paragraphs' text is as it was, without the marks
    expect(find(ps.doc, "t").map((t) => t.textContent)).toEqual(["one", "two", "three"]);
  });

  test("a bulleted list uses the document's own; a numbered list is new and starts at 1, its definitions before its lists", () => {
    const ps = parts(p(`${MARK_START}a${MARK_END}`) + p("b"), `${BULLETS}<w:num w:numId="7"><w:abstractNumId w:val="3"/></w:num>`);
    formatList(ps, { list: "bullet" });
    expect(lists(ps.doc)).toEqual(["7/0", "-"]);
    const qs = parts(p(`${MARK_START}a${MARK_END}`), `${BULLETS}<w:num w:numId="7"><w:abstractNumId w:val="3"/></w:num>`);
    const r = formatList(qs, { list: "number" })!;
    expect(r.changed.rels).toBe(false);
    expect(kidsOf(r.numbering!.documentElement)).toEqual(["abstractNum", "abstractNum", "num", "num"]);
    expect(lists(qs.doc)).toEqual(["8/0"]);
    const num = find(r.numbering!, "num").pop()!;
    expect(attr(find(num, "abstractNumId")[0], "val")).toBe("4");
    expect(attr(find(num, "startOverride")[0], "val")).toBe("1");
  });

  test("right after a numbered list, a numbered paragraph joins it (its numbering goes on)", () => {
    const ps = parts(p("one", numPr(5)) + p(`${MARK_START}two${MARK_END}`), `${NUMBERS}<w:num w:numId="5"><w:abstractNumId w:val="4"/></w:num>`);
    const r = formatList(ps, { list: "number" })!;
    expect(lists(ps.doc)).toEqual(["5/0", "5/0"]);
    expect(find(r.numbering!, "num")).toHaveLength(1);
  });
});

describe("turning lists off, and levels", () => {
  test("the same kind again turns it off; numbering from a style is turned off with list 0", () => {
    const ps = parts(
      p(`${MARK_START}a`, numPr(7)) + p(`b${MARK_END}`, `<w:pStyle w:val="ListBullet"/>`),
      `${BULLETS}<w:num w:numId="7"><w:abstractNumId w:val="3"/></w:num>`,
      `<w:style w:type="paragraph" w:styleId="ListBullet"><w:pPr>${numPr(7)}</w:pPr></w:style>`,
    );
    // The second paragraph's list comes from its style: only the first is a list of its own, so this makes both one
    formatList(ps, { list: "bullet" });
    expect(lists(ps.doc)).toEqual(["7/0", "7/0"]);
    const qs = parts(
      p(`${MARK_START}a`, numPr(7)) + p(`b${MARK_END}`, `<w:pStyle w:val="ListBullet"/>${numPr(7)}`),
      `${BULLETS}<w:num w:numId="7"><w:abstractNumId w:val="3"/></w:num>`,
      `<w:style w:type="paragraph" w:styleId="ListBullet"><w:pPr>${numPr(7)}</w:pPr></w:style>`,
    );
    formatList(qs, { list: "bullet" });
    expect(lists(qs.doc)).toEqual(["-", "0/0"]);
  });

  test("a list item moves a level in or out, within Word's nine; another paragraph's indent moves by half an inch", () => {
    const ps = parts(p(`${MARK_START}a`, numPr(7, 8)) + p(`b${MARK_END}`, `<w:ind w:left="360" w:hanging="360"/>`), `${BULLETS}<w:num w:numId="7"><w:abstractNumId w:val="3"/></w:num>`);
    formatList(ps, { level: 1 });
    expect(lists(ps.doc)[0]).toBe("7/8");
    expect(attr(find(ps.doc, "ind")[0], "left")).toBe("1080");
    expect(attr(find(ps.doc, "ind")[0], "hanging")).toBe("360");
    const qs = parts(p(`${MARK_START}a${MARK_END}`, numPr(7, 2)) + p("b"), `${BULLETS}<w:num w:numId="7"><w:abstractNumId w:val="3"/></w:num>`);
    formatList(qs, { level: -1 });
    expect(lists(qs.doc)).toEqual(["7/1", "-"]);
    // No indent below none
    const rs = parts(p(`${MARK_START}a${MARK_END}`));
    formatList(rs, { level: -1 });
    expect(find(rs.doc, "ind")).toHaveLength(0);
  });

  test("without both marks nothing changes", () => {
    const ps = parts(p("a"));
    expect(formatList(ps, { list: "bullet" })).toBeUndefined();
  });
});
