// The PDF viewer's choices that don't need pdf.js (lib/pdfView.ts): zoom steps, typed page numbers, search words
import { describe, expect, test } from "vite-plus/test";
import { pageFromInput, percent, scaleValue, searchWords, stepZoom, viewPath } from "@/lib/pdfView";

describe("zoom", () => {
  test("steps go to the next fixed size, from wherever pinching left the scale", () => {
    expect(stepZoom(1, 1)).toBe(1.25);
    expect(stepZoom(1, -1)).toBe(0.75);
    expect(stepZoom(1.1, 1)).toBe(1.25);
    expect(stepZoom(1.1, -1)).toBe(1);
  });

  test("steps stop at the smallest and largest sizes", () => {
    expect(stepZoom(4, 1)).toBe(4);
    expect(stepZoom(9, 1)).toBe(4);
    expect(stepZoom(0.25, -1)).toBe(0.25);
    expect(stepZoom(0.1, -1)).toBe(0.25);
  });

  test("fits and sizes are what pdf.js takes", () => {
    expect(scaleValue("page-width")).toBe("page-width");
    expect(scaleValue(1.5)).toBe("1.5");
    expect(percent(1.254)).toBe("125%");
  });
});

describe("page numbers typed in", () => {
  test("go to a page within the document", () => {
    expect(pageFromInput("3", 10)).toBe(3);
    expect(pageFromInput(" 7 ", 10)).toBe(7);
    expect(pageFromInput("0", 10)).toBe(1);
    expect(pageFromInput("99", 10)).toBe(10);
  });

  test("that aren't numbers go nowhere", () => {
    expect(pageFromInput("", 10)).toBeNull();
    expect(pageFromInput("abc", 10)).toBeNull();
    expect(pageFromInput("1", 0)).toBeNull();
  });
});

describe("search words", () => {
  test("are the words of the search, each once, without quotation marks", () => {
    expect(searchWords("tender  Budget budget")).toEqual(["tender", "Budget"]);
    expect(searchWords('"annual report"')).toEqual(["annual", "report"]);
    expect(searchWords("「預算」 會議")).toEqual(["預算", "會議"]);
  });

  test("are read in the compatibility form the search uses", () => {
    expect(searchWords("ＡＢＣ")).toEqual(["ABC"]);
  });

  test("are none without a search", () => {
    expect(searchWords(undefined)).toEqual([]);
    expect(searchWords("   ")).toEqual([]);
  });

  test("ride along in the address of a file opened from search results", () => {
    expect(viewPath("abc")).toBe("/view/abc");
    expect(viewPath("abc", "  ")).toBe("/view/abc");
    expect(viewPath("abc", "budget 2026 & more")).toBe("/view/abc?find=budget%202026%20%26%20more");
  });
});
