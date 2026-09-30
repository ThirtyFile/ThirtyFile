// @vitest-environment jsdom
// The Office renderers (src/ooxml) on small sample files built here: a Word document, a PowerPoint slide and a chart.
// jsdom reads prefixed attributes (w:val) the way browsers do; it has no ResizeObserver or IntersectionObserver, which
// the slides use to scale and to render as they scroll into view, so they are stood in for
import JSZip from "jszip";
import { afterAll, afterEach, beforeAll, describe, expect, test, vi } from "vitest";
import { renderDocx } from "@/ooxml/docx";
import { renderPptx } from "@/ooxml/pptx";
import { renderChart } from "@/ooxml/chart";
import { OoxmlPackage } from "@/ooxml/core/package";

const W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const A = "http://schemas.openxmlformats.org/drawingml/2006/main";
const P = "http://schemas.openxmlformats.org/presentationml/2006/main";
const R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const C = "http://schemas.openxmlformats.org/drawingml/2006/chart";
const REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG_REL = "http://schemas.openxmlformats.org/package/2006/relationships";

const rels = (list: [id: string, type: string, target: string][]) =>
  `<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="${PKG_REL}">${list
    .map(([id, type, target]) => `<Relationship Id="${id}" Type="${REL}/${type}" Target="${target}"/>`)
    .join("")}</Relationships>`;

async function pack(parts: Record<string, string>) {
  const zip = new JSZip();
  for (const [path, text] of Object.entries(parts)) zip.file(path, text);
  return zip.generateAsync({ type: "arraybuffer" });
}

const roots: HTMLElement[] = [];
function mount() {
  const root = document.createElement("div");
  document.body.append(root);
  roots.push(root);
  return root;
}
afterEach(() => roots.splice(0).forEach((r) => r.remove()));

describe("Word", () => {
  const docx = () =>
    pack({
      "[Content_Types].xml": `<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>`,
      "_rels/.rels": rels([["rId1", "officeDocument", "word/document.xml"]]),
      "word/_rels/document.xml.rels": rels([]),
      "word/document.xml": `<?xml version="1.0" encoding="UTF-8"?>
        <w:document xmlns:w="${W}" xmlns:r="${R}"><w:body>
          <w:p><w:r><w:rPr><w:b/><w:sz w:val="48"/></w:rPr><w:t>Quarterly report</w:t></w:r></w:p>
          <w:p><w:r><w:t xml:space="preserve">Sales grew by </w:t></w:r><w:r><w:rPr><w:i/><w:color w:val="C00000"/></w:rPr><w:t>12%</w:t></w:r></w:p>
          <w:tbl>
            <w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>
            <w:tr><w:tc><w:p><w:r><w:t>Region</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Sales</w:t></w:r></w:p></w:tc></w:tr>
            <w:tr><w:tc><w:p><w:r><w:t>North</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>1,200</w:t></w:r></w:p></w:tc></w:tr>
          </w:tbl>
          <w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr>
        </w:body></w:document>`,
    });

  test("renders paragraphs with their run formatting, and tables", async () => {
    const root = mount();
    const result = await renderDocx(await docx(), root);
    expect(root.textContent).toContain("Quarterly report");
    expect(root.textContent).toContain("Sales grew by 12%");

    const title = [...root.querySelectorAll("span")].find((s) => s.textContent === "Quarterly report")!;
    expect(title.style.fontWeight).toBe("bold");
    expect(title.style.fontSize).toBe("24pt");
    const emphasis = [...root.querySelectorAll("span")].find((s) => s.textContent === "12%")!;
    expect(emphasis.style.fontStyle).toBe("italic");
    expect(emphasis.style.color).toMatch(/#c00000|rgb\(192, 0, 0\)/i);

    const cells = [...root.querySelectorAll("td")].map((td) => td.textContent);
    expect(cells).toEqual(["Region", "Sales", "North", "1,200"]);
    result.dispose();
  });

  test("builds the page from its DOM only: document text never becomes markup", async () => {
    const zip = await JSZip.loadAsync(await docx());
    const xml = (await zip.file("word/document.xml")!.async("string")).replace("Quarterly report", "&lt;img src=x onerror=alert(1)&gt;");
    zip.file("word/document.xml", xml);
    const root = mount();
    (await renderDocx(await zip.generateAsync({ type: "arraybuffer" }), root)).dispose();
    expect(root.textContent).toContain("<img src=x onerror=alert(1)>");
    expect(root.querySelector("img")).toBeNull();
  });
});

describe("PowerPoint", () => {
  beforeAll(() => {
    class Observer {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    vi.stubGlobal("ResizeObserver", Observer);
    vi.stubGlobal("IntersectionObserver", Observer);
  });
  afterAll(() => vi.unstubAllGlobals());

  const pptx = () =>
    pack({
      "[Content_Types].xml": `<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>`,
      "_rels/.rels": rels([["rId1", "officeDocument", "ppt/presentation.xml"]]),
      "ppt/presentation.xml": `<?xml version="1.0" encoding="UTF-8"?>
        <p:presentation xmlns:p="${P}" xmlns:r="${R}" xmlns:a="${A}">
          <p:sldIdLst><p:sldId id="256" r:id="rId2"/></p:sldIdLst>
          <p:sldSz cx="9144000" cy="5143500"/>
        </p:presentation>`,
      "ppt/_rels/presentation.xml.rels": rels([["rId2", "slide", "slides/slide1.xml"]]),
      "ppt/slides/slide1.xml": `<?xml version="1.0" encoding="UTF-8"?>
        <p:sld xmlns:p="${P}" xmlns:r="${R}" xmlns:a="${A}"><p:cSld><p:spTree>
          <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>
          <p:sp>
            <p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
            <p:spPr>
              <a:xfrm><a:off x="457200" y="457200"/><a:ext cx="4572000" cy="914400"/></a:xfrm>
              <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
              <a:solidFill><a:srgbClr val="1F4E79"/></a:solidFill>
            </p:spPr>
            <p:txBody><a:bodyPr/><a:p><a:r><a:rPr lang="en-US" sz="3200" b="1"><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></a:rPr><a:t>Roadmap 2026</a:t></a:r></a:p></p:txBody>
          </p:sp>
        </p:spTree></p:cSld></p:sld>`,
    });

  test("renders a slide at its size, with a shape's fill, place and text", async () => {
    const root = mount();
    const result = await renderPptx(await pptx(), root);
    const slides = root.querySelectorAll("[data-slide]");
    expect(slides).toHaveLength(1);
    expect(root.textContent).toContain("Roadmap 2026");
    // 9144000 × 5143500 EMU is 960 × 540 px
    const slide = slides[0].firstElementChild as HTMLElement;
    expect([slide.style.width, slide.style.height]).toEqual(["960px", "540px"]);
    // The shape sits 48 px from the top left, 480 × 96 px, filled dark blue
    const shape = [...slide.querySelectorAll<HTMLElement>("*")].find((e) => e.style.left === "48px" && e.style.top === "48px")!;
    expect(shape).toBeDefined();
    expect([shape.style.width, shape.style.height]).toEqual(["480px", "96px"]);
    expect(slide.innerHTML).toMatch(/#1f4e79|rgb\(31, 78, 121\)/i);
    const text = [...slide.querySelectorAll("span")].find((s) => s.textContent === "Roadmap 2026")!;
    expect(text.style.fontWeight).toBe("bold");
    result.dispose();
  });
});

describe("charts", () => {
  const series = (name: string, values: number[]) => `
    <c:ser><c:idx val="${name === "2025" ? 0 : 1}"/><c:order val="${name === "2025" ? 0 : 1}"/>
      <c:tx><c:strRef><c:f>Sheet1!$B$1</c:f><c:strCache><c:ptCount val="1"/><c:pt idx="0"><c:v>${name}</c:v></c:pt></c:strCache></c:strRef></c:tx>
      <c:cat><c:strRef><c:f>Sheet1!$A$2:$A$4</c:f><c:strCache><c:ptCount val="3"/>
        <c:pt idx="0"><c:v>North</c:v></c:pt><c:pt idx="1"><c:v>South</c:v></c:pt><c:pt idx="2"><c:v>West</c:v></c:pt>
      </c:strCache></c:strRef></c:cat>
      <c:val><c:numRef><c:f>Sheet1!$B$2:$B$4</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val="3"/>
        ${values.map((v, i) => `<c:pt idx="${i}"><c:v>${v}</c:v></c:pt>`).join("")}
      </c:numCache></c:numRef></c:val>
    </c:ser>`;
  const chart = `<?xml version="1.0" encoding="UTF-8"?>
    <c:chartSpace xmlns:c="${C}" xmlns:a="${A}" xmlns:r="${R}"><c:chart>
      <c:title><c:tx><c:rich><a:bodyPr/><a:p><a:r><a:t>Sales by region</a:t></a:r></a:p></c:rich></c:tx><c:overlay val="0"/></c:title>
      <c:plotArea>
        <c:barChart><c:barDir val="col"/><c:grouping val="clustered"/>${series("2025", [120, 80, 45])}${series("2026", [150, 90, 60])}
          <c:axId val="1"/><c:axId val="2"/></c:barChart>
        <c:catAx><c:axId val="1"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:axPos val="b"/><c:crossAx val="2"/></c:catAx>
        <c:valAx><c:axId val="2"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:axPos val="l"/><c:crossAx val="1"/></c:valAx>
      </c:plotArea>
      <c:legend><c:legendPos val="b"/></c:legend>
    </c:chart></c:chartSpace>`;

  test("renders a column chart as SVG: a bar per value, the title, categories and legend", async () => {
    const pkg = await OoxmlPackage.open(await pack({ "word/charts/chart1.xml": chart }));
    const svg = await renderChart(pkg, "word/charts/chart1.xml", { width: 480, height: 320, colors: {} });
    expect(svg.tagName.toLowerCase()).toBe("svg");
    const texts = [...svg.querySelectorAll("text")].map((t) => t.textContent);
    expect(texts).toContain("Sales by region");
    for (const label of ["North", "South", "West", "2025", "2026"]) expect(texts).toContain(label);
    // A bar per value, in the plot area: the first series' three, then the second's, each as tall as its value
    const bars = [...svg.querySelectorAll("g[clip-path] > rect")];
    expect(bars.map((r) => r.getAttribute("fill"))).toEqual([...Array(3).fill("#4472c4"), ...Array(3).fill("#ed7d31")]);
    const heights = bars.map((r) => Number(r.getAttribute("height")));
    const perUnit = heights[0] / 120;
    [120, 80, 45, 150, 90, 60].forEach((v, i) => expect(heights[i]).toBeCloseTo(v * perUnit, 0));
    pkg.dispose();
  });

  test("a part that isn't a chart gives an empty frame instead of failing", async () => {
    const pkg = await OoxmlPackage.open(await pack({ "word/charts/chart1.xml": "<nothing/>" }));
    const el = await renderChart(pkg, "word/charts/chart1.xml", { width: 200, height: 100, colors: {} });
    expect(el.tagName.toLowerCase()).toBe("div");
    expect([el.style.width, el.style.height]).toEqual(["200px", "100px"]);
    pkg.dispose();
  });
});
