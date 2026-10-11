// Headers and footers of a Word document: listing those the sections show, adding one to a document without, and
// editing one's text and formatting the way the body's is edited
import { describe, expect, test } from "vitest";
import { attr } from "@/ooxml/core/package";
import { originalPieces, planEdits, saveEdits } from "@/ooxml/docx/edit";
import { formatRange, MARK_END, MARK_START } from "@/ooxml/docx/formatOps";
import { addStory, listStories, type StoryParts } from "@/ooxml/docx/hfOps";

const W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const parse = (xml: string) => new DOMParser().parseFromString(xml, "application/xml");
/** Descendants by local name (happy-dom's getElementsByTagNameNS doesn't look below the first level) */
const find = (el: Element | Document, name: string) => Array.from(el.querySelectorAll("*")).filter((e) => e.localName === name);
const kidsOf = (el: Element) => Array.from(el.children).map((c) => c.localName);

function parts(body: string, styles = "", taken: string[] = []): StoryParts {
  return {
    doc: parse(`<w:document xmlns:w="${W}" xmlns:r="${R}"><w:body>${body}</w:body></w:document>`),
    rels: parse('<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="x" Target="styles.xml"/></Relationships>'),
    types: parse('<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/></Types>'),
    styles: styles ? parse(`<w:styles xmlns:w="${W}">${styles}</w:styles>`) : null,
    docPath: "word/document.xml",
    taken: (path) => taken.includes(path),
  };
}

describe("adding a header or footer", () => {
  test("a header is a new part, named by the first section with its relationship and content type", () => {
    const ps = parts(
      '<w:p><w:pPr><w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr></w:pPr></w:p><w:p><w:r><w:t>Two</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>',
      '<w:style w:type="paragraph" w:styleId="Header"><w:name w:val="header"/></w:style>',
      ["word/header1.xml"],
    );
    const added = addStory(ps, "header")!;
    expect(added.path).toBe("word/header2.xml");
    expect(added.typesChanged).toBe(true);
    const rel = find(ps.rels, "Relationship").pop()!;
    expect([rel.getAttribute("Id"), rel.getAttribute("Type"), rel.getAttribute("Target")]).toEqual(["rId2", `${R}/header`, "header2.xml"]);
    expect(find(ps.types!, "Override").map((o) => [o.getAttribute("PartName"), o.getAttribute("ContentType")])).toEqual([
      ["/word/header2.xml", "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"],
    ]);
    // The first section (the first paragraph's) names it, before its other properties; the last section doesn't
    const [first, last] = find(ps.doc, "sectPr");
    expect(kidsOf(first)).toEqual(["headerReference", "pgSz"]);
    const ref = first.children[0];
    expect([attr(ref, "type"), ref.getAttributeNS(R, "id")]).toEqual(["default", "rId2"]);
    expect(kidsOf(last)).toEqual(["pgSz"]);
    // The part: one empty paragraph in the Header style
    const root = added.part.documentElement;
    expect([root.localName, root.namespaceURI]).toEqual(["hdr", W]);
    expect(find(root, "pStyle").map((s) => attr(s, "val"))).toEqual(["Header"]);
    expect(find(root, "p")).toHaveLength(1);
  });

  test("a footer goes after the section's headers; without a Footer style its paragraph has none; one there already is refused", () => {
    const ps = parts(`<w:sectPr><w:headerReference w:type="default" r:id="rId9"/><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>`);
    const added = addStory(ps, "footer")!;
    expect(added.path).toBe("word/footer1.xml");
    expect(kidsOf(find(ps.doc, "sectPr")[0])).toEqual(["headerReference", "footerReference", "pgSz"]);
    expect(find(added.part, "pStyle")).toHaveLength(0);
    expect(addStory(ps, "footer")).toBeUndefined();
    // A first-page header isn't one for every page: a header can still be added
    const qs = parts(`<w:sectPr><w:headerReference w:type="first" r:id="rId9"/></w:sectPr>`);
    expect(addStory(qs, "header")?.path).toBe("word/header1.xml");
  });
});

describe("the headers and footers the sections show", () => {
  test("each part once, in the order of the sections, those a section takes from the one before not again", () => {
    const rels = [
      { id: "rId1", target: "word/header1.xml" },
      { id: "rId2", target: "word/footer1.xml" },
      { id: "rId3", target: "word/header2.xml" },
      { id: "rId4", target: "word/missing.xml" },
      { id: "rId5", target: "https://example.com/x.xml", external: true },
    ];
    const sections = [
      { headers: { default: "rId1", first: "rId4" }, footers: { default: "rId2" } },
      { headers: { default: "rId1", even: "rId3" }, footers: { default: "rId5" } },
    ];
    const stories = listStories(sections, rels, (path) => path !== "word/missing.xml");
    expect(stories).toEqual([
      { path: "word/header1.xml", kind: "header", type: "default", section: 0 },
      { path: "word/footer1.xml", kind: "footer", type: "default", section: 0 },
      { path: "word/header2.xml", kind: "header", type: "even", section: 1 },
    ]);
  });
});

describe("editing a header", () => {
  const header = (body: string) => parse(`<w:hdr xmlns:w="${W}">${body}</w:hdr>`);

  test("its paragraphs are edited like the body's, and only the changed one is written", () => {
    const doc = header('<w:p><w:pPr><w:pStyle w:val="Header"/></w:pPr><w:r><w:t>Report</w:t></w:r></w:p><w:p><w:r><w:t>Draft</w:t></w:r></w:p>');
    const plan = planEdits(doc)!;
    expect(plan.body).toBe(doc.documentElement);
    expect(plan.editable).toEqual([true, true]);
    const [report] = originalPieces(plan, 0);
    const xml = saveEdits(
      plan,
      [
        { kind: "para", source: 0, pieces: [{ run: report.run, text: "Annual report" }] },
        { kind: "para", source: 1, pieces: originalPieces(plan, 1) },
      ],
      new Set([0, 1]),
    )!;
    // (happy-dom drops prefixes from copies, so the result is read back rather than compared as text)
    const saved = parse(xml).documentElement;
    expect([saved.localName, saved.namespaceURI]).toEqual(["hdr", W]);
    expect(
      find(saved, "p").map((q) => [
        attr(find(q, "pStyle")[0], "val"),
        find(q, "t")
          .map((t) => t.textContent)
          .join(""),
      ]),
    ).toEqual([
      ["Header", "Annual report"],
      [null, "Draft"],
    ]);
    // The paragraph not changed is written as it was
    expect(xml).toContain("<w:p><w:r><w:t>Draft</w:t></w:r></w:p>");
    // The plan still describes the header as it was opened
    expect(find(doc, "t").map((t) => t.textContent)).toEqual(["Report", "Draft"]);
  });

  test("formatting between the marks works in a header too", () => {
    const doc = header(`<w:p><w:r><w:t>a${MARK_START}b${MARK_END}c</w:t></w:r></w:p>`);
    expect(formatRange(doc, { size: 9 })).toBe(true);
    expect(find(doc, "r").map((r) => [r.textContent, attr(find(r, "sz")[0], "val")])).toEqual([
      ["a", null],
      ["b", "18"],
      ["c", null],
    ]);
    const e = header(`<w:p><w:r><w:t>${MARK_START}x${MARK_END}</w:t></w:r></w:p>`);
    expect(formatRange(e, { align: "right" })).toBe(true);
    expect(attr(find(e, "jc")[0], "val")).toBe("right");
  });
});
