import JSZip from "jszip";
import { describe, expect, test } from "vitest";
import { partAllowed } from "@/components/docx/session";

function opened() {
  const zip = new JSZip();
  zip.file("[Content_Types].xml", "<Types/>");
  zip.file("_rels/.rels", "<Relationships/>");
  zip.file("word/document.xml", "<w:document/>");
  zip.file("word/_rels/document.xml.rels", "<Relationships/>");
  zip.file("word/styles.xml", "<w:styles/>");
  zip.file("docProps/core.xml", "<cp:coreProperties/>");
  return zip;
}

describe("the parts the Word editor's frame may write into a file", () => {
  test("the file's own parts, its content types, and new XML parts beside them", () => {
    const zip = opened();
    const own = ["word/document.xml", "word/_rels/document.xml.rels", "[Content_Types].xml", "docProps/core.xml"];
    expect(own.filter((path) => !partAllowed(zip, path))).toEqual([]);
    // A list makes the numbering part next to document.xml, and headers their own parts
    const made = ["word/numbering.xml", "word/header1.xml", "word/_rels/header1.xml.rels"];
    expect(made.filter((path) => !partAllowed(zip, path))).toEqual([]);
  });

  test("never a path outside the package, a folder the file doesn't have, or anything but XML", () => {
    const zip = opened();
    const refused = [
      "/word/document.xml",
      "word\\document.xml",
      "word/../evil.xml",
      "./word/document.xml",
      "word//document.xml",
      "evil.xml",
      "elsewhere/new.xml",
      "word/media/new.xml",
      "word/script.js",
      "word/new.bin",
    ];
    expect(refused.filter((path) => partAllowed(zip, path))).toEqual([]);
  });
});
