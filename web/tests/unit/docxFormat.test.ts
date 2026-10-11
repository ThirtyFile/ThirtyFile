// Formatting text in a Word document: character formatting the view changed written into the runs as a difference,
// and font size, paragraph style and alignment applied between the editor's two marks
import { describe, expect, test } from "vitest";
import { attr } from "@/ooxml/core/package";
import { originalPieces, planEdits, readEdits, saveEdits, type FmtOf } from "@/ooxml/docx/edit";
import { applyFmt, formatRange, MARK_END, MARK_START } from "@/ooxml/docx/formatOps";
import { WordXml } from "@/ooxml/docx/xmlEdit";

const W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const doc = (body: string) => new DOMParser().parseFromString(`<w:document xmlns:w="${W}"><w:body>${body}<w:sectPr/></w:body></w:document>`, "application/xml");
/** Descendants by local name (happy-dom's getElementsByTagNameNS doesn't look below the first level) */
const find = (el: Element | Document, name: string) => Array.from(el.querySelectorAll("*")).filter((e) => e.localName === name);
const kidsOf = (el: Element) => Array.from(el.children).map((c) => c.localName + (attr(c, "val") !== null ? `=${attr(c, "val")}` : ""));
const text = (el: Element) =>
  find(el, "t")
    .map((t) => t.textContent)
    .join("");

describe("character formatting written into a run", () => {
  test("on and off, in Word's order of a run's properties; off writes the value 0, so a style's formatting goes too", () => {
    const d = doc('<w:p><w:r><w:rPr><w:rStyle w:val="Strong"/><w:sz w:val="24"/><w:dstrike/></w:rPr><w:t>x</w:t></w:r></w:p>');
    const rPr = find(d, "rPr")[0];
    applyFmt(new WordXml(d, rPr), rPr, { b: false, i: true, u: true, s: true, color: "c00000", hl: "FFFF00" });
    expect(kidsOf(rPr)).toEqual(["rStyle=Strong", "b=0", "bCs=0", "i", "iCs", "strike", "color=C00000", "sz=24", "highlight=yellow", "u=single"]);
  });

  test("a highlight that isn't one of Word's colors is shading, and none takes both off", () => {
    const d = doc('<w:p><w:r><w:rPr><w:highlight w:val="green"/></w:rPr><w:t>x</w:t></w:r></w:p>');
    const rPr = find(d, "rPr")[0];
    const x = new WordXml(d, rPr);
    applyFmt(x, rPr, { hl: "ABCDEF" });
    expect(kidsOf(rPr)).toEqual(["shd=clear"]);
    expect(attr(find(rPr, "shd")[0], "fill")).toBe("ABCDEF");
    applyFmt(x, rPr, { hl: null });
    expect(kidsOf(rPr)).toEqual(["highlight=none"]);
  });

  test("saving the view writes the formatting it gave a run's text, keeping the rest of the run", () => {
    const plan = planEdits(doc('<w:p><w:r><w:rPr><w:sz w:val="28"/></w:rPr><w:t xml:space="preserve">Hello </w:t><w:lastRenderedPageBreak/></w:r><w:r><w:t>world</w:t></w:r></w:p>'))!;
    const [hello, world] = originalPieces(plan, 0);
    const xml = saveEdits(plan, [{ kind: "para", source: 0, pieces: [hello, { ...world, text: "wo" }, { ...world, text: "rld", fmt: { b: true } }] }], new Set([0]))!;
    const out = new DOMParser().parseFromString(xml, "application/xml");
    const runs = find(out, "r");
    expect(runs.map(text)).toEqual(["Hello ", "wo", "rld"]);
    // The untouched run is as it was; the bold part of the other got bold
    expect(find(runs[0], "lastRenderedPageBreak")).toHaveLength(1);
    expect(find(runs[1], "b")).toHaveLength(0);
    expect(kidsOf(find(runs[2], "rPr")[0])).toEqual(["b", "bCs"]);
  });

  test("the view is read with the formatting of each text, split where it changes", () => {
    const root = document.createElement("div");
    root.innerHTML = '<p data-para="0"><span data-r="0">plain <b>bold</b></span></p>';
    const fmtOf: FmtOf = (node) => (node.parentElement?.tagName === "B" ? { b: true } : undefined);
    expect(readEdits(root, fmtOf)).toEqual([
      {
        kind: "para",
        source: 0,
        pieces: [
          { run: 0, text: "plain " },
          { run: 0, text: "bold", fmt: { b: true } },
        ],
      },
    ]);
  });
});

describe("formatting between the marks", () => {
  test("a font size goes to the text between the marks, the runs split at them, and the marks go", () => {
    const d = doc(`<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>ab${MARK_START}cd</w:t></w:r><w:r><w:t>ef${MARK_END}gh</w:t></w:r></w:p>`);
    expect(formatRange(d, { size: 14 })).toBe(true);
    const runs = find(d, "r");
    expect(runs.map(text)).toEqual(["ab", "cd", "ef", "gh"]);
    expect(runs.map((r) => attr(find(r, "sz")[0], "val"))).toEqual([null, "28", "28", null]);
    expect(kidsOf(find(runs[1], "rPr")[0])).toEqual(["b", "sz=28", "szCs=28"]);
    expect(text(d.documentElement)).not.toMatch(/[]/);
  });

  test("a style or alignment goes to every paragraph the marks touch", () => {
    const d = doc(
      `<w:p><w:r><w:t>one${MARK_START}</w:t></w:r></w:p><w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t>two</w:t></w:r></w:p><w:p><w:r><w:t>${MARK_END}three</w:t></w:r></w:p><w:p><w:r><w:t>four</w:t></w:r></w:p>`,
    );
    expect(formatRange(d, { align: "center" })).toBe(true);
    expect(find(d, "p").map((p) => attr(find(p, "jc")[0], "val"))).toEqual(["center", "center", "center", null]);
    // The runs stay as they were: marks inside a word don't split it
    const m = doc(`<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>wo${MARK_START}${MARK_END}rld</w:t></w:r></w:p>`);
    formatRange(m, { align: "right" });
    expect(find(m, "r").map(text)).toEqual(["world"]);
    const e = doc(`<w:p><w:pPr><w:jc w:val="left"/></w:pPr><w:r><w:t>${MARK_START}${MARK_END}x</w:t></w:r></w:p>`);
    formatRange(e, { style: "Heading1" });
    expect(kidsOf(find(e, "pPr")[0])).toEqual(["pStyle=Heading1", "jc=left"]);
    const n = doc(`<w:p><w:pPr><w:pStyle w:val="Heading1"/><w:jc w:val="left"/></w:pPr><w:r><w:t>${MARK_START}x${MARK_END}</w:t></w:r></w:p>`);
    formatRange(n, { style: "Normal", isDefault: true });
    expect(kidsOf(find(n, "pPr")[0])).toEqual(["jc=left"]);
  });

  test("a size for an empty paragraph goes to its paragraph mark; without both marks nothing changes", () => {
    const d = doc(`<w:p><w:r><w:t>${MARK_START}${MARK_END}</w:t></w:r></w:p>`);
    expect(formatRange(d, { size: 9 })).toBe(true);
    expect(kidsOf(find(find(d, "pPr")[0], "rPr")[0])).toEqual(["sz=18", "szCs=18"]);
    // Both marks in one run, round its whole text: the run is the range
    const w = doc(`<w:p><w:r><w:t>${MARK_START}Second${MARK_END}</w:t></w:r></w:p>`);
    formatRange(w, { size: 14 });
    expect(find(w, "r").map((r) => [text(r), attr(find(r, "sz")[0], "val")])).toEqual([["Second", "28"]]);
    expect(find(w, "pPr")).toHaveLength(0);
    // Both marks before a run's text: no empty run is left behind
    const c = doc(`<w:p><w:r><w:t>${MARK_START}${MARK_END}Second</w:t></w:r></w:p>`);
    formatRange(c, { size: 9 });
    expect(find(c, "r").map(text)).toEqual(["Second"]);
    const e = doc(`<w:p><w:r><w:t>a${MARK_START}b</w:t></w:r></w:p>`);
    expect(formatRange(e, { size: 9 })).toBe(false);
    expect(text(e.documentElement)).toBe("ab");
  });
});
