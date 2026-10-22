// What the page takes from the Word editor's frame: parts by path as text only, and the toolbar's formatting only in
// the shape it expects (the frame shows the document's content, so its messages aren't trusted as they come)
import { describe, expect, test } from "vite-plus/test";
import { caretFormatFrom, partsFrom } from "@/components/docx/frameMessages";

describe("messages from the editing frame", () => {
  test("parts: XML parts of the package, as text", () => {
    expect(
      partsFrom({
        "word/document.xml": "<w:document/>",
        "[Content_Types].xml": "<Types/>",
        "word/_rels/document.xml.rels": "<Relationships/>",
        "../outside.xml": "x",
        "word/script.js": "x",
        "word/header1.xml": 42,
      }),
    ).toEqual({ "word/document.xml": "<w:document/>", "[Content_Types].xml": "<Types/>", "word/_rels/document.xml.rels": "<Relationships/>" });
    expect(partsFrom(null)).toEqual({});
    expect(partsFrom("word/document.xml")).toEqual({});
  });

  test("the formatting at the caret, in the toolbar's shape or not at all", () => {
    const fmt = { b: true, i: false, u: false, s: false, size: 11, style: null, align: "left", list: null, font: "Calibri", line: 240, before: 0, after: 160 };
    expect(caretFormatFrom(fmt)).toEqual(fmt);
    expect(caretFormatFrom({ ...fmt, size: "11" })).toBeNull();
    expect(caretFormatFrom({ ...fmt, align: "middle" })).toBeNull();
    expect(caretFormatFrom({ ...fmt, list: "other" })).toBeNull();
    expect(caretFormatFrom({ ...fmt, font: undefined })).toBeNull();
    expect(caretFormatFrom(null)).toBeNull();
  });
});
