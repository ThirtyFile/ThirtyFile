/**
 * Editing a Word document's text in the browser (the preview frame runs it; components/DocxEditor.tsx is the host).
 *
 * The body is shown as one page-wide sheet, drawn by the same renderer as the preview so text keeps its fonts, sizes,
 * spacing and list numbers; pages, headers and footers aren't shown. Paragraphs that only hold text are edited in place
 * (one contenteditable for the whole body, so selecting, typing, Enter, Backspace and undo are the browser's own); every
 * other block is shown as it is and can't be changed (edit.ts says which, and how the edits are written back).
 *
 * The browser's editing is limited to what can be written back: plain text only (formatting commands, pasting rich
 * content and dragging are refused), and Backspace or Delete never joins a paragraph with a block that is kept as it is.
 */

import { h, kid, type OoxmlPackage } from "../core/package";
import { breathe } from "../core/yield";
import { BlockWriter, type BlockItem } from "./blocks";
import type { Flow, PageFloat } from "./context";
import { DOCX_CSS } from "./css";
import { originalPieces, planEdits, readEdits, readParagraph, samePieces, saveEdits, type EditPlan } from "./edit";
import { loadDocx, normalStyle } from "./index";
import { contentWidth, parseSection, type Section } from "./section";
import { fixScaled, resolveTabs } from "./tabs";
import { alternate } from "./xml";

/** Texts of the editing view, in the person's language (the frame has no dictionary of its own) */
export interface EditorTexts {
  /** Name of the editable text, for screen readers */
  label: string;
  /** Shown over a block that can't be changed here */
  locked: string;
}

/** Where the caret was: the n-th block of the view, and how many characters into its text */
export interface Caret {
  block: number;
  offset: number;
}

export interface EditorOptions {
  /** document.xml with edits not saved yet, to open instead of the archive's */
  xml?: string | null;
  texts: EditorTexts;
  caret?: Caret | null;
  scroll?: number;
  /** The text changed (typing, deleting, undo…) */
  onInput(): void;
  /** Ctrl+S */
  onSave(): void;
}

export interface DocxEditor {
  /** document.xml as edited (null when the view holds the text it was opened with), where the caret is, and the scroll position */
  collect(): { path: string; xml: string | null; caret: Caret | null; scroll: number };
  focus(): void;
  dispose(): void;
}

const EDIT_CSS = `
.tf-docx-edit{min-height:100%;padding:16px 0 48px;box-sizing:border-box}
.tf-docx-sheet{background:#fff;margin:0 auto;box-shadow:0 1px 3px rgba(0,0,0,.25);box-sizing:border-box}
.tf-docx-editable{outline:none;min-height:4em;caret-color:#000}
.tf-docx-ro{position:relative;cursor:default}
.tf-docx-ro:hover{outline:1px dashed #94a3b8;outline-offset:2px}
.tf-docx-editable [data-ro]{user-select:none}
.tf-docx-editable .tf-docx-p{tab-size:48px}
`;

/** Input the editor can't write back: formatting, lists, links, rich drops */
const REFUSED = new Set(["insertFromDrop", "deleteByDrag", "insertOrderedList", "insertUnorderedList", "insertHorizontalRule", "insertLink", "insertFromPasteAsQuotation"]);

/** Characters XML can't hold (a pasted control character would make the file unreadable) */
// oxlint-disable-next-line no-control-regex -- control characters are removed on purpose
const CONTROL = /[\u0000-\u0008\u000b\u000c\u000e-\u001f￾￿]/g;

export async function openDocxEditor(pkg: OoxmlPackage, root: HTMLElement, opts: EditorOptions): Promise<DocxEditor> {
  const rootRels = await pkg.rels("");
  const docPath = rootRels.find((r) => r.type.endsWith("/officeDocument"))?.target ?? "word/document.xml";
  if (opts.xml) pkg.replaceXml(docPath, opts.xml);
  const loaded = await loadDocx(pkg);
  const { doc, docXml, docRels } = loaded;
  const plan = planEdits(docXml);
  if (!plan) throw new Error("The document has no body");

  // ── Sections: each block's section, as the preview groups them ──
  const sectPrs: (Element | null)[] = [];
  const secOf: number[] = [];
  for (const b of plan.blocks) {
    secOf.push(sectPrs.length);
    const sp = b.localName === "p" ? kid(kid(b, "pPr"), "sectPr") : null;
    if (sp) sectPrs.push(sp);
  }
  sectPrs.push(kid(plan.body, "sectPr"));
  const sections: Section[] = [];
  sectPrs.forEach((sp, i) => sections.push(parseSection(sp, i ? sections[i - 1] : null)));

  // ── The view ──
  const normal = normalStyle(doc);
  const first = sections[0];
  const editable = h("div", {
    class: "tf-docx-editable",
    contenteditable: "true",
    role: "textbox",
    "aria-multiline": "true",
    "aria-label": opts.texts.label,
  });
  const sheet = h(
    "div",
    {
      class: "tf-docx-sheet",
      style: `width:${first.width}px;padding:${Math.max(24, Math.min(first.top, 96))}px ${first.right}px ${Math.max(24, Math.min(first.bottom, 96))}px ${first.left}px`,
    },
    editable,
  );
  const container = h(
    "div",
    {
      class: "tf-docx tf-docx-edit",
      lang: /^[a-zA-Z-]{2,12}$/.test(doc.settings.lang) ? doc.settings.lang : undefined,
      style: `font-family:${normal.family};font-size:${normal.css["font-size"] ?? "11pt"}`,
    },
    sheet,
  );
  root.replaceChildren(h("style", null, DOCX_CSS + EDIT_CSS), container);

  const shown = new Set<number>();
  const floats: PageFloat[] = [];
  const shared = { fields: [] as Flow["fields"], comments: new Set<string>() };
  const edit = { runId: (r: Element) => plan.runIds.get(r) };
  let items: BlockItem[] = [];
  let writer: BlockWriter | null = null;
  let writerSec = -1;
  for (const [i, block] of plan.blocks.entries()) {
    const sec = Math.min(secOf[i], sections.length - 1);
    if (!writer || sec !== writerSec) {
      const s = sections[sec];
      const cols = s.cols.num;
      const width = cols > 1 ? (contentWidth(s) - s.cols.space * (cols - 1)) / cols : contentWidth(s);
      const flow: Flow = { doc, part: loaded.docPath, rels: docRels, section: s, width, story: "body", depth: 0, fields: shared.fields, comments: shared.comments, floats, edit };
      writer = new BlockWriter(flow, (item) => items.push(item));
      writerSec = sec;
    }
    items = [];
    if (block.localName === "AlternateContent") writer.blocks(alternate(block));
    else writer.block(block);
    const el = viewOf(plan, i, items, opts.texts);
    if (el) {
      editable.append(el);
      shown.add(i);
    }
    await breathe();
  }

  // The renderer's finishing touches: pictures loaded, scaled text and list-number tabs measured
  await Promise.allSettled(doc.pending);
  fixScaled(doc.scaled);
  resolveTabs(doc.tabs);

  // A view narrower than the page shows the page smaller
  const fit = () => {
    const avail = root.clientWidth - 16;
    sheet.style.zoom = avail > 0 && avail < first.width ? String(Math.round((avail / first.width) * 1000) / 1000) : "";
  };
  fit();
  const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(fit);
  ro?.observe(root);

  // ── Editing limited to what can be written back ──
  const onBeforeInput = (e: InputEvent) => {
    if (e.inputType.startsWith("format") || REFUSED.has(e.inputType)) e.preventDefault();
  };
  const onKeyDown = (e: KeyboardEvent) => {
    // Keys an input method uses while composing (choosing a candidate, say) are its own
    if (e.isComposing || e.keyCode === 229) return;
    const mod = e.ctrlKey || e.metaKey;
    if (mod && !e.altKey && e.key.toLowerCase() === "s") {
      e.preventDefault();
      opts.onSave();
      return;
    }
    if (e.key === "Tab" && !mod && !e.altKey && !e.shiftKey) {
      e.preventDefault();
      document.execCommand("insertText", false, "\t");
      return;
    }
    if ((e.key === "Backspace" || e.key === "Delete") && !e.altKey && !e.shiftKey) {
      // Joining a paragraph with a kept block would put the paragraph's text inside it: refused, like Word refuses
      // to join a paragraph with a table
      const sel = getSelection();
      if (!sel?.rangeCount || !sel.isCollapsed) return;
      const p = blockOf(editable, sel.anchorNode);
      if (!p || p.dataset.block !== undefined) return;
      const back = e.key === "Backspace";
      const neighbour = back ? p.previousElementSibling : p.nextElementSibling;
      if (neighbour instanceof HTMLElement && neighbour.dataset.block !== undefined && atEdge(p, sel.getRangeAt(0), back)) e.preventDefault();
    }
  };
  const onPaste = (e: ClipboardEvent) => {
    e.preventDefault();
    const text = (e.clipboardData?.getData("text/plain") ?? "").replace(/\r\n?/g, "\n").replace(CONTROL, "");
    text.split("\n").forEach((line, i) => {
      if (i) document.execCommand("insertParagraph");
      if (line) document.execCommand("insertText", false, line);
    });
  };
  const onDrag = (e: DragEvent) => e.preventDefault();
  const onInput = () => opts.onInput();
  editable.addEventListener("beforeinput", onBeforeInput);
  editable.addEventListener("keydown", onKeyDown);
  editable.addEventListener("paste", onPaste);
  editable.addEventListener("dragstart", onDrag);
  editable.addEventListener("drop", onDrag);
  editable.addEventListener("input", onInput);

  if (opts.scroll) root.scrollTop = opts.scroll;
  const focus = () => {
    editable.focus({ preventScroll: true });
    if (opts.caret) placeCaret(editable, opts.caret);
  };
  focus();

  return {
    collect() {
      const xml = saveEdits(plan, readEdits(editable), shown);
      return { path: loaded.docPath, xml, caret: caretOf(editable), scroll: root.scrollTop };
    },
    focus: () => editable.focus({ preventScroll: true }),
    dispose() {
      ro?.disconnect();
      editable.removeEventListener("beforeinput", onBeforeInput);
      editable.removeEventListener("keydown", onKeyDown);
      editable.removeEventListener("paste", onPaste);
      editable.removeEventListener("dragstart", onDrag);
      editable.removeEventListener("drop", onDrag);
      editable.removeEventListener("input", onInput);
    },
  };
}

/**
 * The view of one body block: an editable paragraph (`data-para`), a block shown as it is (`data-block`), or null when
 * the renderer showed nothing for it (a paragraph that only ends a section).
 */
function viewOf(plan: EditPlan, i: number, items: BlockItem[], texts: EditorTexts): HTMLElement | null {
  if (!items.length) return null;
  const only = items.length === 1 ? items[0].el : null;
  if (plan.editable[i] && only && only.tagName === "P") {
    only.dataset.para = String(i);
    markLeading(only);
    // What the view shows must read back as the paragraph's text exactly, or editing it could change text nobody touched
    if (samePieces(readParagraph(only), originalPieces(plan, i))) return only;
    delete only.dataset.para;
  }
  const kept = h("div", { class: "tf-docx-ro", contenteditable: "false", "data-block": i, title: texts.locked });
  for (const it of items) kept.append(it.el);
  return kept;
}

/** List numbers and bookmark anchors before a paragraph's text: shown, but not part of the text */
function markLeading(p: HTMLElement) {
  for (const c of Array.from(p.childNodes)) {
    if (c.nodeType === 3) {
      if (c.nodeValue) break;
      continue;
    }
    if (!(c instanceof HTMLElement) || c.tagName === "BR") break;
    if (c.dataset.r !== undefined || c.querySelector("[data-r]")) break;
    c.dataset.ro = "";
    c.contentEditable = "false";
  }
}

/** The block of the view (a child of `editable`) holding a node */
function blockOf(editable: HTMLElement, node: Node | null): HTMLElement | null {
  for (let n: Node | null = node; n && n !== editable; n = n.parentNode) {
    if (n.parentNode === editable) return n instanceof HTMLElement ? n : null;
  }
  return null;
}

/** Text of a range, without the parts of the view that aren't text (list numbers) */
function rangeText(range: Range): { text: string; breaks: number } {
  const frag = range.cloneContents();
  for (const el of Array.from(frag.querySelectorAll("[data-ro]"))) el.remove();
  return { text: frag.textContent ?? "", breaks: frag.querySelectorAll("br:not(.tf-docx-eol)").length };
}

/** Whether the caret is at the start (or end) of a paragraph's text */
function atEdge(p: HTMLElement, caret: Range, start: boolean): boolean {
  const r = document.createRange();
  r.selectNodeContents(p);
  if (start) r.setEnd(caret.startContainer, caret.startOffset);
  else r.setStart(caret.endContainer, caret.endOffset);
  const { text, breaks } = rangeText(r);
  // After the caret, the paragraph's last line break is the browser's empty line
  return !text && breaks <= (start ? 0 : 1);
}

function caretOf(editable: HTMLElement): Caret | null {
  const sel = getSelection();
  if (!sel?.rangeCount) return null;
  const p = blockOf(editable, sel.anchorNode);
  if (!p) return null;
  const r = document.createRange();
  r.selectNodeContents(p);
  r.setEnd(sel.anchorNode!, sel.anchorOffset);
  return { block: Array.prototype.indexOf.call(editable.children, p), offset: rangeText(r).text.length };
}

function placeCaret(editable: HTMLElement, caret: Caret) {
  const p = editable.children[caret.block] as HTMLElement | undefined;
  if (!p || p.dataset.block !== undefined) return;
  const walker = document.createTreeWalker(p, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (n.parentElement?.closest("[data-ro]") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT) as number,
  });
  let left = caret.offset;
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const len = n.nodeValue?.length ?? 0;
    if (left <= len) {
      getSelection()?.collapse(n, left);
      return;
    }
    left -= len;
  }
  getSelection()?.collapse(p, p.childNodes.length);
}
