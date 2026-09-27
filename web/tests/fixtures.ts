/**
 * Sample Office files built in the tests with JSZip, so no binary fixtures live in the repository.
 */
import JSZip from "jszip";

const XML = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n';
const MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG = "http://schemas.openxmlformats.org/package/2006/relationships";
const DOC_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/** A part the editor doesn't know about: it must come back byte for byte */
export const UNKNOWN_PART = "customXml/item1.xml";
export const UNKNOWN_CONTENT = `${XML}<root xmlns="urn:example:keep"><value a="1">keep me &amp; my entities</value></root>`;

export interface SheetSpec {
  name: string;
  /** Contents of <sheetData> */
  rows: string;
  /** Anything after <sheetData> (mergeCells, conditionalFormatting...) */
  after?: string;
}

/**
 * A workbook with shared strings, a style sheet (0: default, 1: bold, 2: 0.00), a calculation chain,
 * a defined name and an unknown part.
 */
export async function buildWorkbook(sheets: SheetSpec[], strings: string[] = [], definedNames = ""): Promise<ArrayBuffer> {
  const zip = new JSZip();
  zip.file(
    "[Content_Types].xml",
    `${XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">` +
      '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>' +
      '<Default Extension="xml" ContentType="application/xml"/>' +
      '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>' +
      sheets
        .map(
          (_, i) =>
            `<Override PartName="/xl/worksheets/sheet${i + 1}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>`,
        )
        .join("") +
      '<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>' +
      '<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>' +
      '<Override PartName="/xl/calcChain.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.calcChain+xml"/>' +
      "</Types>",
  );
  zip.file(
    "_rels/.rels",
    `${XML}<Relationships xmlns="${PKG}"><Relationship Id="rId1" Type="${DOC_REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>`,
  );
  zip.file(
    "xl/workbook.xml",
    `${XML}<workbook xmlns="${MAIN}" xmlns:r="${R}"><sheets>` +
      sheets.map((s, i) => `<sheet name="${s.name}" sheetId="${i + 1}" r:id="rId${i + 1}"/>`).join("") +
      `</sheets>${definedNames ? `<definedNames>${definedNames}</definedNames>` : ""}</workbook>`,
  );
  const n = sheets.length;
  zip.file(
    "xl/_rels/workbook.xml.rels",
    `${XML}<Relationships xmlns="${PKG}">` +
      sheets.map((_, i) => `<Relationship Id="rId${i + 1}" Type="${DOC_REL}/worksheet" Target="worksheets/sheet${i + 1}.xml"/>`).join("") +
      `<Relationship Id="rId${n + 1}" Type="${DOC_REL}/styles" Target="styles.xml"/>` +
      `<Relationship Id="rId${n + 2}" Type="${DOC_REL}/sharedStrings" Target="sharedStrings.xml"/>` +
      `<Relationship Id="rId${n + 3}" Type="${DOC_REL}/calcChain" Target="calcChain.xml"/>` +
      "</Relationships>",
  );
  sheets.forEach((s, i) =>
    zip.file(
      `xl/worksheets/sheet${i + 1}.xml`,
      `${XML}<worksheet xmlns="${MAIN}" xmlns:r="${R}"><dimension ref="A1"/><sheetFormatPr defaultRowHeight="15"/>` +
        `<sheetData>${s.rows}</sheetData>${s.after ?? ""}</worksheet>`,
    ),
  );
  zip.file(
    "xl/sharedStrings.xml",
    `${XML}<sst xmlns="${MAIN}" count="${strings.length}" uniqueCount="${strings.length}">` +
      strings.map((s) => `<si><t>${s}</t></si>`).join("") +
      "</sst>",
  );
  zip.file(
    "xl/styles.xml",
    `${XML}<styleSheet xmlns="${MAIN}">` +
      '<numFmts count="1"><numFmt numFmtId="164" formatCode="0.000"/></numFmts>' +
      '<fonts count="2"><font><sz val="11"/><name val="Calibri"/></font><font><b/><sz val="11"/><name val="Calibri"/></font></fonts>' +
      '<fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills>' +
      '<borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>' +
      '<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>' +
      '<cellXfs count="3"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>' +
      '<xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/>' +
      '<xf numFmtId="2" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/></cellXfs>' +
      "</styleSheet>",
  );
  zip.file("xl/calcChain.xml", `${XML}<calcChain xmlns="${MAIN}"><c r="A1" i="1"/></calcChain>`);
  zip.file(UNKNOWN_PART, UNKNOWN_CONTENT);
  return zip.generateAsync({ type: "arraybuffer" });
}

/** A minimal Word document with the given paragraphs */
export async function buildDocx(paragraphs: string[]): Promise<ArrayBuffer> {
  const zip = new JSZip();
  zip.file(
    "[Content_Types].xml",
    `${XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">` +
      '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>' +
      '<Default Extension="xml" ContentType="application/xml"/>' +
      '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>' +
      "</Types>",
  );
  zip.file(
    "_rels/.rels",
    `${XML}<Relationships xmlns="${PKG}"><Relationship Id="rId1" Type="${DOC_REL}/officeDocument" Target="word/document.xml"/></Relationships>`,
  );
  zip.file(
    "word/document.xml",
    `${XML}<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>` +
      paragraphs.map((p) => `<w:p><w:r><w:t xml:space="preserve">${p}</w:t></w:r></w:p>`).join("") +
      "</w:body></w:document>",
  );
  return zip.generateAsync({ type: "arraybuffer" });
}
