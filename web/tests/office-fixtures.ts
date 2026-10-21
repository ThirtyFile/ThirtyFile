import JSZip from "jszip";

/** A minimal one-slide PPTX, created in source so browser acceptance has no external fixture dependency. */
export async function buildPresentation(slideCount = 1): Promise<ArrayBuffer> {
  const zip = new JSZip();
  const rel = "http://schemas.openxmlformats.org/package/2006/relationships";
  const office = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
  const p = "http://schemas.openxmlformats.org/presentationml/2006/main";
  const a = "http://schemas.openxmlformats.org/drawingml/2006/main";
  const indices = Array.from({ length: slideCount }, (_, index) => index + 1);
  zip.file(
    "[Content_Types].xml",
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/></Types>',
  );
  zip.file("_rels/.rels", `<Relationships xmlns="${rel}"><Relationship Id="rId1" Type="${office}/officeDocument" Target="ppt/presentation.xml"/></Relationships>`);
  zip.file(
    "ppt/presentation.xml",
    `<p:presentation xmlns:p="${p}" xmlns:r="${office}"><p:sldIdLst>${indices.map((index) => `<p:sldId id="${255 + index}" r:id="rId${index}"/>`).join("")}</p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>`,
  );
  zip.file(
    "ppt/_rels/presentation.xml.rels",
    `<Relationships xmlns="${rel}">${indices.map((index) => `<Relationship Id="rId${index}" Type="${office}/slide" Target="slides/slide${index}.xml"/>`).join("")}</Relationships>`,
  );
  for (const index of indices) {
    zip.file(
      `ppt/slides/slide${index}.xml`,
      `<p:sld xmlns:p="${p}" xmlns:a="${a}" xmlns:r="${office}"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>`,
    );
  }
  return zip.generateAsync({ type: "arraybuffer" });
}
