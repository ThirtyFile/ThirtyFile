// Editing the text of a Word document: which paragraphs can be edited, reading the edited view back, and writing only
// what changed into document.xml (everything else stays as it was)
import { describe, expect, test } from "vitest";
import { attr } from "@/ooxml/core/package";
import { isEditableParagraph, originalPieces, planEdits, readAllEdits, readEdits, saveEdits, type EditedBlock, type EditPlan, type Edits } from "@/ooxml/docx/edit";

const W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const W14 = "http://schemas.microsoft.com/office/word/2010/wordml";
const R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

const docXml = (body: string, ns = W) =>
  `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="${ns}" xmlns:w14="${W14}" xmlns:r="${R}"><w:body>${body}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr></w:body></w:document>`;

const parse = (xml: string) => new DOMParser().parseFromString(xml, "application/xml");
const plan = (body: string, ns?: string) => planEdits(parse(docXml(body, ns)))!;
const serialize = (el: Element) => new XMLSerializer().serializeToString(el);

/** A container's edited view as it was opened (the body's by default): its blocks, editable paragraphs with their own text */
function asOpened(p: EditPlan, container = 0): EditedBlock[] {
  return p.containers[container].blocks.map((i) => (p.editable[i] ? { kind: "para", source: i, pieces: originalPieces(p, i) } : { kind: "keep", block: i }));
}
const all = (p: EditPlan) => new Set(p.blocks.map((_, i) => i));

/** The body blocks of the saved document.xml */
function saved(p: EditPlan, edited: EditedBlock[] | Edits, shown = all(p)) {
  const xml = saveEdits(p, edited, shown);
  expect(xml).not.toBeNull();
  const out = parse(xml!);
  const body = find(out, "body")[0];
  return { xml: xml!, blocks: Array.from(body.children).filter((c) => c.localName !== "sectPr"), body };
}

/** Descendants by local name (happy-dom's getElementsByTagNameNS doesn't look below the first level) */
const find = (el: Element | Document, name: string) => Array.from(el.querySelectorAll("*")).filter((e) => e.localName === name);

const text = (el: Element) =>
  find(el, "t")
    .map((t) => t.textContent)
    .join("");

describe("which paragraphs can be edited", () => {
  const p = (inner: string) => find(parse(docXml(`<w:p>${inner}</w:p>`)), "p")[0];
  test("text, tabs, line breaks, links and bookmarks can", () => {
    expect(isEditableParagraph(p('<w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>Title</w:t><w:tab/><w:br/></w:r>'))).toBe(true);
    expect(
      isEditableParagraph(
        p('<w:bookmarkStart w:id="0" w:name="a"/><w:hyperlink r:id="rId5"><w:r><w:t>link</w:t></w:r></w:hyperlink><w:bookmarkEnd w:id="0"/><w:proofErr w:type="spellStart"/>'),
      ),
    ).toBe(true);
  });
  test("fields, pictures, tracked changes, hidden text, page breaks and comments can't", () => {
    for (const inner of [
      '<w:r><w:fldChar w:fldCharType="begin"/></w:r>',
      "<w:r><w:drawing/></w:r>",
      "<w:ins><w:r><w:t>new</w:t></w:r></w:ins>",
      "<w:r><w:rPr><w:vanish/></w:rPr><w:t>secret</w:t></w:r>",
      '<w:r><w:br w:type="page"/></w:r>',
      '<w:commentRangeStart w:id="1"/><w:r><w:t>x</w:t></w:r>',
      '<w:r><w:footnoteReference w:id="2"/></w:r>',
      "<w:sdt><w:sdtContent><w:r><w:t>x</w:t></w:r></w:sdtContent></w:sdt>",
      '<w:pPr><w:rPr><w:del w:id="3"/></w:rPr></w:pPr><w:r><w:t>x</w:t></w:r>',
    ])
      expect({ inner, editable: isEditableParagraph(p(inner)) }).toEqual({ inner, editable: false });
  });
});

describe("writing the edits back", () => {
  test("nothing changed: nothing to save", () => {
    const pl = plan("<w:p><w:r><w:t>One</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl>");
    expect(saveEdits(pl, asOpened(pl), all(pl))).toBeNull();
  });

  test("a changed paragraph keeps its settings and runs' formatting; the others stay exactly as they were", () => {
    const pl = plan(
      '<w:p w14:paraId="1A2B3C4D"><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> world</w:t></w:r></w:p>' +
        "<w:p><w:r><w:rPr><w:i/></w:rPr><w:t>Untouched</w:t></w:r><w:r><w:t/></w:r></w:p>",
    );
    const before = serialize(pl.blocks[1]);
    const edited = asOpened(pl);
    edited[0] = {
      kind: "para",
      source: 0,
      pieces: [
        { run: 0, text: "Hi there" },
        { run: 1, text: " everyone" },
      ],
    };
    const { blocks } = saved(pl, edited);
    expect(blocks).toHaveLength(2);
    const [first, second] = blocks;
    expect(first.getAttribute("w14:paraId")).toBe("1A2B3C4D");
    expect(attr(find(first, "jc")[0], "val")).toBe("center");
    const runs = find(first, "r");
    expect(find(runs[0], "b")).toHaveLength(1);
    expect(text(runs[0])).toBe("Hi there");
    expect(text(runs[1])).toBe(" everyone");
    expect(find(runs[1], "t")[0].getAttribute("xml:space")).toBe("preserve");
    expect(serialize(second)).toBe(before);
    // The plan still describes the document as opened
    expect(text(pl.blocks[0])).toBe("Hello world");
  });

  test("tabs, line breaks and hyphens become Word's elements; characters XML can't hold are dropped", () => {
    const pl = plan("<w:p><w:r><w:t>x</w:t></w:r></w:p>");
    const { blocks } = saved(pl, [{ kind: "para", source: 0, pieces: [{ run: 0, text: "a\tb\nc­‑d\u0001" }] }]);
    const kids = Array.from(find(blocks[0], "r")[0].children).map((c) => c.localName + (c.localName === "t" ? `:${c.textContent}` : ""));
    expect(kids).toEqual(["t:a", "tab", "t:b", "br", "t:c", "softHyphen", "noBreakHyphen", "t:d"]);
  });

  test("text typed into an empty paragraph takes the paragraph mark's formatting, not its tracked changes", () => {
    const pl = plan('<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="a"/><w:color w:val="FF0000"/></w:rPr></w:pPr></w:p>');
    expect(originalPieces(pl, 0)).toEqual([]);
    const { blocks } = saved(pl, [{ kind: "para", source: 0, pieces: [{ run: null, text: "New" }] }]);
    const run = find(blocks[0], "r")[0];
    expect(find(run, "color")).toHaveLength(1);
    expect(find(run, "ins")).toHaveLength(0);
    expect(text(run)).toBe("New");
  });

  test("a split paragraph gives both parts its settings; the last part keeps the section break, the first its ids", () => {
    const pl = plan(
      '<w:p w14:paraId="00000001" w14:textId="00000002"><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="3"/></w:numPr><w:sectPr><w:pgSz w:w="12240"/></w:sectPr></w:pPr><w:r><w:t>First second</w:t></w:r></w:p><w:p><w:r><w:t>Next</w:t></w:r></w:p>',
    );
    const { blocks } = saved(pl, [
      { kind: "para", source: 0, pieces: [{ run: 0, text: "First" }] },
      { kind: "para", source: 0, pieces: [{ run: 0, text: " second" }] },
      { kind: "para", source: 1, pieces: originalPieces(pl, 1) },
    ]);
    expect(blocks.map(text)).toEqual(["First", " second", "Next"]);
    const [a, b] = blocks;
    expect(a.getAttribute("w14:paraId")).toBe("00000001");
    expect(b.getAttribute("w14:paraId")).toBeNull();
    expect(b.getAttribute("w14:textId")).toBeNull();
    for (const p of [a, b]) expect(find(p, "numId")).toHaveLength(1);
    expect(find(a, "sectPr")).toHaveLength(0);
    expect(find(b, "sectPr")).toHaveLength(1);
  });

  test("pressing Enter at the end of a paragraph leaves it as it was and adds an empty one like it", () => {
    const pl = plan('<w:p><w:pPr><w:pStyle w:val="Body"/></w:pPr><w:r><w:t>Line</w:t></w:r></w:p>');
    const before = serialize(pl.blocks[0]);
    const { blocks } = saved(pl, [
      { kind: "para", source: 0, pieces: originalPieces(pl, 0) },
      { kind: "para", source: 0, pieces: [] },
    ]);
    expect(serialize(blocks[0])).toBe(before);
    expect(find(blocks[1], "pStyle")).toHaveLength(1);
    expect(find(blocks[1], "r")).toHaveLength(0);
  });

  test("joined paragraphs keep each run's formatting", () => {
    const pl = plan('<w:p><w:r><w:t>One </w:t></w:r></w:p><w:p><w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>two</w:t></w:r></w:p>');
    const { blocks } = saved(pl, [{ kind: "para", source: 0, pieces: [...originalPieces(pl, 0), ...originalPieces(pl, 1)] }]);
    expect(blocks).toHaveLength(1);
    const runs = find(blocks[0], "r");
    expect(text(blocks[0])).toBe("One two");
    expect(find(runs[1], "u")).toHaveLength(1);
  });

  test("deleting a kept block removes it, a deleted paragraph that ended a section leaves the section break, and blocks not shown stay", () => {
    const pl = plan(
      "<w:p><w:r><w:t>Keep</w:t></w:r></w:p>" +
        "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>" +
        '<w:p><w:pPr><w:sectPr><w:type w:val="continuous"/></w:sectPr></w:pPr></w:p>' +
        '<w:p><w:pPr><w:sectPr><w:type w:val="nextPage"/></w:sectPr></w:pPr><w:r><w:t>Gone</w:t></w:r></w:p>' +
        "<w:p><w:r><w:t>Last</w:t></w:r></w:p>",
    );
    // The view showed every block but the section-only paragraph (2); the table (1) and the paragraph "Gone" (3) were deleted
    const shown = new Set([0, 1, 3, 4]);
    const { blocks } = saved(
      pl,
      [
        { kind: "para", source: 0, pieces: originalPieces(pl, 0) },
        { kind: "para", source: 4, pieces: [{ run: 2, text: "Last!" }] },
      ],
      shown,
    );
    expect(blocks.map((b) => b.localName)).toEqual(["p", "p", "p", "p"]);
    expect(serialize(blocks[1])).toBe(serialize(pl.blocks[2]));
    expect(attr(find(blocks[2], "type")[0], "val")).toBe("nextPage");
    expect(text(blocks[2])).toBe("");
    expect(text(blocks[3])).toBe("Last!");
    expect(blocks.some((b) => b.localName === "tbl")).toBe(false);
  });

  test("links and bookmarks stay around the text they were around", () => {
    const pl = plan(
      '<w:p><w:bookmarkStart w:id="0" w:name="_Toc1"/><w:r><w:t>See </w:t></w:r><w:hyperlink r:id="rId9" w:history="1"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>the site</w:t></w:r></w:hyperlink><w:bookmarkEnd w:id="0"/><w:proofErr w:type="spellEnd"/></w:p>',
    );
    const { blocks } = saved(pl, [
      {
        kind: "para",
        source: 0,
        pieces: [
          { run: 0, text: "Visit " },
          { run: 1, text: "our site" },
        ],
      },
    ]);
    const kids = Array.from(blocks[0].children).map((c) => c.localName);
    expect(kids).toEqual(["bookmarkStart", "r", "hyperlink", "bookmarkEnd"]);
    const link = find(blocks[0], "hyperlink")[0];
    expect(attr(link, "id")).toBe("rId9");
    expect(text(link)).toBe("our site");
  });

  test("a body left without paragraphs, or ending with a table, gets an empty paragraph", () => {
    const pl = plan('<w:tbl><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl><w:p w14:paraId="0000000A"><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t>end</w:t></w:r></w:p>');
    const { blocks } = saved(pl, [{ kind: "keep", block: 0 }]);
    expect(blocks.map((b) => b.localName)).toEqual(["tbl", "p"]);
    expect(blocks[1].getAttribute("w14:paraId")).toBeNull();
    expect(find(blocks[1], "jc")).toHaveLength(1);
    expect(saved(pl, []).blocks.map((b) => b.localName)).toEqual(["p"]);
  });

  test("a strict document gets elements in its own namespace", () => {
    const STRICT = "http://purl.oclc.org/ooxml/wordprocessingml/main";
    const pl = plan("<w:p><w:r><w:t>x</w:t></w:r></w:p>", STRICT);
    const { xml } = saved(pl, [{ kind: "para", source: 0, pieces: [{ run: 0, text: "a\tb" }] }]);
    expect(xml).toContain("<w:tab/>");
    expect(xml).not.toContain(W);
  });
});

describe("reading the edited view back", () => {
  const view = (html: string) => {
    const root = document.createElement("div");
    root.innerHTML = html;
    return readEdits(root);
  };

  test("paragraphs by run, kept blocks, list numbers left out, and the browser's empty line dropped", () => {
    const edits = view(
      '<p data-para="0"><span data-ro="" contenteditable="false">1.</span><span data-r="0">Hello</span><span data-r="1"> you</span><br data-r="1"><br></p>' +
        '<div data-block="1" contenteditable="false"><table><tr><td>cell</td></tr></table></div>' +
        '<p data-para="2"><br class="tf-docx-eol"></p>',
    );
    expect(edits).toEqual([
      {
        kind: "para",
        source: 0,
        pieces: [
          { run: 0, text: "Hello" },
          { run: 1, text: " you\n" },
        ],
      },
      { kind: "keep", block: 1 },
      { kind: "para", source: 2, pieces: [] },
    ]);
  });

  test("a split paragraph is two copies of it; typed text without a run takes the run next to it; loose text becomes a paragraph", () => {
    const edits = view('<p data-para="3">New <span data-r="4">start</span></p><p data-para="3"><span data-r="4">end</span> more</p>tail');
    expect(edits).toEqual([
      { kind: "para", source: 3, pieces: [{ run: 4, text: "New start" }] },
      { kind: "para", source: 3, pieces: [{ run: 4, text: "end more" }] },
      { kind: "para", source: 3, pieces: [{ run: null, text: "tail" }] },
    ]);
  });

  test("a paragraph element nested by the browser is a paragraph of its own", () => {
    // Built by hand: an HTML parser wouldn't put a div inside a p, a browser editing it may
    const root = document.createElement("div");
    const p = document.createElement("p");
    p.dataset.para = "0";
    p.innerHTML = '<span data-r="0">a</span>';
    const inner = document.createElement("div");
    inner.innerHTML = '<span data-r="0">b</span>';
    p.append(inner);
    root.append(p);
    expect(readEdits(root)).toEqual([
      { kind: "para", source: 0, pieces: [{ run: 0, text: "a" }] },
      { kind: "para", source: 0, pieces: [{ run: 0, text: "b" }] },
    ]);
  });
});

describe("table cells", () => {
  const TABLE =
    '<w:tbl><w:tblPr><w:tblW w:w="5000"/></w:tblPr><w:tblGrid><w:gridCol w:w="2500"/><w:gridCol w:w="2500"/></w:tblGrid>' +
    '<w:tr><w:tc><w:tcPr><w:tcW w:w="2500"/><w:shd w:fill="FFFF00"/></w:tcPr><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Region</w:t></w:r></w:p></w:tc>' +
    '<w:tc><w:tcPr><w:tcW w:w="2500"/></w:tcPr><w:p><w:r><w:t>Sales</w:t></w:r></w:p></w:tc></w:tr></w:tbl>';

  test("each cell is a container of its own, after the body", () => {
    const pl = plan(`<w:p><w:r><w:t>Before</w:t></w:r></w:p>${TABLE}`);
    expect(pl.containers.map((c) => c.el.localName)).toEqual(["body", "tc", "tc"]);
    expect(pl.containers[0].blocks.map((i) => pl.blocks[i].localName)).toEqual(["p", "tbl"]);
    expect(pl.containers.slice(1).map((c) => c.blocks.map((i) => text(pl.blocks[i])))).toEqual([["Region"], ["Sales"]]);
    expect(pl.containers.slice(1).every((c) => c.blocks.every((i) => pl.editable[i]))).toBe(true);
  });

  test("a changed cell keeps its settings and formatting; the table and the other cells stay as they were", () => {
    const pl = plan(`<w:p><w:r><w:t>Before</w:t></w:r></w:p>${TABLE}`);
    const [, , region] = pl.blocks;
    const other = serialize(pl.containers[2].el);
    const edits = new Map<number, EditedBlock[]>([
      [0, asOpened(pl)],
      [
        1,
        [
          { kind: "para", source: pl.blocks.indexOf(region), pieces: [{ run: originalPieces(pl, 2)[0].run, text: "Area" }] },
          { kind: "para", source: 2, pieces: [{ run: null, text: "North" }] },
        ],
      ],
      [2, asOpened(pl, 2)],
    ]);
    const { body } = saved(pl, edits);
    const cells = find(body, "tc");
    expect(Array.from(cells[0].children).map((c) => c.localName)).toEqual(["tcPr", "p", "p"]);
    expect(find(cells[0], "shd")).toHaveLength(1);
    // The edited run keeps its bold; the paragraph typed without a run has the paragraph mark's formatting (none)
    expect(find(cells[0], "b")).toHaveLength(1);
    expect(find(cells[0], "p").map(text)).toEqual(["Area", "North"]);
    expect(serialize(cells[1])).toBe(other);
    expect(find(body, "tblW")).toHaveLength(1);
    // The plan still describes the document as opened
    expect(text(pl.containers[1].el)).toBe("Region");
  });

  test("a cell left without paragraphs gets an empty one, as Word needs", () => {
    const pl = plan(TABLE);
    const { body } = saved(pl, new Map([[1, []]]));
    const cell = find(body, "tc")[0];
    expect(Array.from(cell.children).map((c) => c.localName)).toEqual(["tcPr", "p"]);
    expect(text(cell)).toBe("");
  });

  test("a table inside a cell has cells of its own", () => {
    const pl = plan(`<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Outer</w:t></w:r></w:p>${TABLE}<w:p/></w:tc></w:tr></w:tbl>`);
    expect(pl.containers).toHaveLength(4);
    const inner = 2;
    const i = pl.containers[inner].blocks[0];
    const { body } = saved(pl, new Map([[inner, [{ kind: "para", source: i, pieces: [{ run: originalPieces(pl, i)[0].run, text: "Zone" }] }]]]));
    expect(find(body, "t").map((t) => t.textContent)).toEqual(["Outer", "Zone", "Sales"]);
  });

  test("the view's cells are read as their own containers, not as the body's text", () => {
    const root = document.createElement("div");
    root.innerHTML =
      '<p data-para="0"><span data-r="0">Before</span></p>' +
      '<div data-block="1" contenteditable="false"><table><tr><td data-cell="1" data-host=""><p data-para="2"><span data-r="1">Area</span></p><p data-para="2"><span data-r="1">North</span></p></td>' +
      '<td data-cell="2" data-host=""><p data-para="3"><span data-r="2">Sales</span></p></td></tr></table></div>';
    expect(readAllEdits(root)).toEqual(
      new Map([
        [
          0,
          [
            { kind: "para", source: 0, pieces: [{ run: 0, text: "Before" }] },
            { kind: "keep", block: 1 },
          ],
        ],
        [
          1,
          [
            { kind: "para", source: 2, pieces: [{ run: 1, text: "Area" }] },
            { kind: "para", source: 2, pieces: [{ run: 1, text: "North" }] },
          ],
        ],
        [2, [{ kind: "para", source: 3, pieces: [{ run: 2, text: "Sales" }] }]],
      ]),
    );
  });
});
