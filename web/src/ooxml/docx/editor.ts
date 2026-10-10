/**
 * Editing a Word document's text in the browser (the preview frame runs it; components/DocxEditor.tsx is the host).
 *
 * The body is shown as one page-wide sheet, drawn by the same renderer as the preview so text keeps its fonts, sizes,
 * spacing and list numbers; pages, headers and footers aren't shown. Paragraphs that only hold text are edited in place
 * (one contenteditable for the whole body, so selecting, typing, Enter, Backspace and undo are the browser's own); every
 * other block is shown as it is and can't be changed (edit.ts says which, and how the edits are written back).
 *
 * Table cells are edited the same way, each as an editable place of its own inside the table (which itself stays as it
 * is): Enter and Backspace stay in the cell, and Tab moves to the next cell (Shift+Tab to the one before), as in Word.
 * Rows and columns are added and deleted by changing the document (tableOps.ts) and opening the view again on it: the
 * host keeps the change before it, so Ctrl+Z right after undoes it. Cells are selected together for merging by
 * dragging across them or Shift+clicking another one; any key lets go of them.
 *
 * The browser's editing is limited to what can be written back: plain text only (formatting commands, pasting rich
 * content and dragging are refused), and Backspace or Delete never joins a paragraph with a block that is kept as it is.
 */

import { h, kid, type OoxmlPackage } from "../core/package";
import { breathe } from "../core/yield";
import { BlockWriter, type BlockItem } from "./blocks";
import type { Flow, PageFloat } from "./context";
import { DOCX_CSS } from "./css";
import { originalPieces, planEdits, readAllEdits, readParagraph, samePieces, saveEdits, writeXml, type EditPlan } from "./edit";
import { cellRange, changeTable, type TableOp } from "./tableOps";
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
  /** Shown over a table, whose cells' text can be changed */
  table: string;
}

/** Where the caret was: the n-th block of the view, and how many characters into its text; or at the end of a table cell (its container) */
export interface Caret {
  block: number;
  offset: number;
  cell?: number;
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
  /** Ctrl+Z: true when the host undid something itself (a change of a table's shape), so the browser doesn't */
  onUndo?(): boolean;
  /** The caret went into a table cell or out of one (`cell`), or cells were selected together for merging (`cells`) */
  onPlace?(place: { cell: boolean; cells: number }): void;
}

export interface DocxEditor {
  /** document.xml as edited (null when the view holds the text it was opened with), where the caret is, and the scroll position */
  collect(): { path: string; xml: string | null; caret: Caret | null; scroll: number };
  /**
   * The document with the table of the caret's cell changed as `op` says, and the cell (container) to put the caret in
   * once it is opened again; undefined when the caret isn't in a cell or that table can't be changed this way
   */
  tableOp(op: TableOp, split?: { cols: number; rows: number }): { xml: string; cell: number | null } | undefined;
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
.tf-docx-ro:has([data-cell]):hover{outline:none}
.tf-docx-editable [data-cell]{outline:none;cursor:text}
.tf-docx-editable [data-cell]:focus-within{box-shadow:inset 0 0 0 2px rgba(37,99,235,.55)}
.tf-docx-editable [data-cell][data-picked]{background-image:linear-gradient(rgba(37,99,235,.22),rgba(37,99,235,.22))}
.tf-docx-editable [data-picked] ::selection{background:transparent}
`;

/** Input the editor can't write back: formatting, lists, links, rich drops */
const REFUSED = new Set(["insertFromDrop", "deleteByDrag", "insertOrderedList", "insertUnorderedList", "insertHorizontalRule", "insertLink", "insertFromPasteAsQuotation"]);

/** Characters XML can't hold (a pasted control character would make the file unreadable) */
// oxlint-disable-next-line no-control-regex -- control characters are removed on purpose
const CONTROL = /[\u{0}-\u{8}\u{b}\u{c}\u{e}-\u{1f}\u{fffe}\u{ffff}]/gu;

export async function openDocxEditor(pkg: OoxmlPackage, root: HTMLElement, opts: EditorOptions): Promise<DocxEditor> {
  const rootRels = await pkg.rels("");
  const docPath = rootRels.find((r) => r.type.endsWith("/officeDocument"))?.target ?? "word/document.xml";
  if (opts.xml) pkg.replaceXml(docPath, opts.xml);
  else pkg.forgetXml(docPath);
  const loaded = await loadDocx(pkg);
  const { doc, docXml, docRels } = loaded;
  const plan = planEdits(docXml);
  if (!plan) throw new Error("The document has no body");

  // ── Sections: each block's section, as the preview groups them ──
  const bodyBlocks = plan.containers[0].blocks;
  const sectPrs: (Element | null)[] = [];
  const secOf = new Map<number, number>();
  for (const i of bodyBlocks) {
    const b = plan.blocks[i];
    secOf.set(i, sectPrs.length);
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
    "data-host": "",
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
  const edit: NonNullable<Flow["edit"]> = {
    runId: (r) => plan.runIds.get(r),
    // A table cell: its blocks, each an editable paragraph or kept as it is, in an editable place of its own
    cell(tc, host, _content, f) {
      const c = plan.containerOf.get(tc);
      if (c === undefined) return false;
      let cellItems: BlockItem[] = [];
      const w = new BlockWriter(f, (item) => cellItems.push(item));
      for (const i of plan.containers[c].blocks) {
        cellItems = [];
        const block = plan.blocks[i];
        if (block.localName === "AlternateContent") w.blocks(alternate(block));
        else w.block(block);
        const el = viewOf(plan, i, cellItems, opts.texts);
        if (el) {
          host.append(el);
          shown.add(i);
        }
      }
      host.contentEditable = "true";
      host.dataset.cell = String(c);
      host.dataset.host = "";
      return true;
    },
  };
  let items: BlockItem[] = [];
  let writer: BlockWriter | null = null;
  let writerSec = -1;
  for (const i of bodyBlocks) {
    const block = plan.blocks[i];
    const sec = Math.min(secOf.get(i) ?? 0, sections.length - 1);
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
    // Typing, moving or Escape lets go of cells selected together (shortcuts such as Ctrl+S don't)
    if (picked && !mod && !["Shift", "Control", "Alt", "Meta"].includes(e.key)) unpick();
    if (mod && !e.altKey && e.key.toLowerCase() === "s") {
      e.preventDefault();
      opts.onSave();
      return;
    }
    if (mod && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "z" && opts.onUndo?.()) {
      e.preventDefault();
      return;
    }
    if (e.key === "Tab" && !mod && !e.altKey) {
      const cell = hostOf(getSelection()?.anchorNode ?? null, editable);
      // In a table, Tab goes to the next cell and Shift+Tab to the one before; elsewhere Tab types a tab
      if (cell && cell !== editable) {
        e.preventDefault();
        moveToCell(cell, e.shiftKey ? -1 : 1);
        return;
      }
      if (e.shiftKey) return;
      e.preventDefault();
      document.execCommand("insertText", false, "\t");
      return;
    }
    if ((e.key === "Backspace" || e.key === "Delete") && !e.altKey && !e.shiftKey) {
      // Joining a paragraph with a kept block would put the paragraph's text inside it: refused, like Word refuses
      // to join a paragraph with a table
      const sel = getSelection();
      if (!sel?.rangeCount || !sel.isCollapsed) return;
      const host = hostOf(sel.anchorNode, editable);
      if (!host) return;
      const p = blockOf(host, sel.anchorNode);
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
  // Whether the caret is in a table cell, and how many cells are selected together, told when it changes (the host's
  // table menus follow it)
  let inCell: boolean | null = null;
  let picked: { a: HTMLElement; b: HTMLElement } | null = null;
  const cellOf = (n: Node | null) => {
    const host = hostOf(n, editable);
    return host && host !== editable ? host : null;
  };
  const pickedCells = () => Array.from(editable.querySelectorAll<HTMLElement>("[data-cell][data-picked]"));
  const place = () => opts.onPlace?.({ cell: inCell === true || !!picked, cells: picked ? pickedCells().length : 0 });
  const onSelection = () => {
    const now = !!cellOf(getSelection()?.anchorNode ?? null);
    if (now === inCell) return;
    inCell = now;
    place();
  };
  const unpick = () => {
    if (!picked) return;
    for (const el of pickedCells()) delete el.dataset.picked;
    picked = null;
    place();
  };
  /** The cells from one to the other, grown as merging would (never through a merged cell) */
  const pick = (a: HTMLElement, b: HTMLElement) => {
    for (const el of pickedCells()) delete el.dataset.picked;
    const ta = plan.containers[Number(a.dataset.cell)]?.el;
    const tb = plan.containers[Number(b.dataset.cell)]?.el;
    const range = new Set(ta && tb ? (cellRange(ta, tb) ?? []) : []);
    picked = range.size > 1 ? { a, b } : null;
    if (picked) for (const host of Array.from(editable.querySelectorAll<HTMLElement>("[data-cell]"))) if (range.has(plan.containers[Number(host.dataset.cell)]?.el)) host.dataset.picked = "";
    place();
  };
  let dragFrom: HTMLElement | null = null;
  const sameTable = (a: HTMLElement, b: HTMLElement) => a.closest("table") === b.closest("table");
  const onMouseDown = (e: MouseEvent) => {
    if (e.button !== 0) return;
    const cell = cellOf(e.target as Node);
    if (e.shiftKey && cell) {
      const from = picked?.a ?? cellOf(getSelection()?.anchorNode ?? null);
      if (from && from !== cell && sameTable(from, cell)) {
        e.preventDefault();
        pick(from, cell);
        return;
      }
    }
    unpick();
    dragFrom = cell;
  };
  const onMouseMove = (e: MouseEvent) => {
    if (!dragFrom || !(e.buttons & 1)) return;
    const over = cellOf(e.target as Node);
    if (!over || !sameTable(over, dragFrom)) return;
    if (over === dragFrom) {
      // Back in the cell the drag started in: selecting its text again
      unpick();
      return;
    }
    if (picked?.b !== over) pick(dragFrom, over);
    getSelection()?.removeAllRanges();
    e.preventDefault();
  };
  const onMouseUp = () => {
    dragFrom = null;
  };
  document.addEventListener("selectionchange", onSelection);
  editable.addEventListener("mousedown", onMouseDown);
  editable.addEventListener("mousemove", onMouseMove);
  document.addEventListener("mouseup", onMouseUp);
  editable.addEventListener("beforeinput", onBeforeInput);
  editable.addEventListener("keydown", onKeyDown);
  editable.addEventListener("paste", onPaste);
  editable.addEventListener("dragstart", onDrag);
  editable.addEventListener("drop", onDrag);
  editable.addEventListener("input", onInput);

  if (opts.scroll) root.scrollTop = opts.scroll;
  const focus = () => {
    const caret = opts.caret;
    const cell = caret?.cell !== undefined ? editable.querySelector<HTMLElement>(`[data-cell="${caret.cell}"]`) : null;
    if (cell) caretAtEnd(cell);
    else {
      editable.focus({ preventScroll: true });
      if (caret) placeCaret(editable, caret);
    }
    onSelection();
  };
  focus();

  return {
    collect() {
      const xml = saveEdits(plan, readAllEdits(editable), shown);
      return { path: loaded.docPath, xml, caret: caretOf(editable), scroll: root.scrollTop };
    },
    tableOp(op, split) {
      // Merging: the cells selected together; anything else: the caret's cell (or the first of those selected)
      const host = op === "merge" ? picked?.a : (cellOf(getSelection()?.anchorNode ?? null) ?? picked?.a);
      const tc = host ? plan.containers[Number(host.dataset.cell)]?.el : undefined;
      const otherTc = op === "merge" && picked ? plan.containers[Number(picked.b.dataset.cell)]?.el : undefined;
      if (!tc || tc.localName !== "tc" || (op === "merge" && !otherTc)) return undefined;
      // The document as edited, with the cells marked so they can be found in it
      tc.setAttribute("data-tf-cell", "");
      otherTc?.setAttribute("data-tf-other", "");
      let text: string;
      try {
        text = saveEdits(plan, readAllEdits(editable), shown) ?? writeXml(plan.doc);
      } finally {
        tc.removeAttribute("data-tf-cell");
        otherTc?.removeAttribute("data-tf-other");
      }
      const doc = new DOMParser().parseFromString(text, "application/xml");
      const body = Array.from(doc.documentElement.children).find((e) => e.localName === "body");
      const cells = Array.from(body?.querySelectorAll("*") ?? []).filter((e) => e.localName === "tc");
      const target = cells.find((e) => e.hasAttribute("data-tf-cell"));
      const other = cells.find((e) => e.hasAttribute("data-tf-other"));
      if (!target) return undefined;
      target.removeAttribute("data-tf-cell");
      other?.removeAttribute("data-tf-other");
      const focusCell = changeTable(target, op, { other, cols: split?.cols, rows: split?.rows });
      if (focusCell === undefined) return undefined;
      // Containers are the body, then the cells in document order (as planEdits makes them)
      const after = Array.from(body?.querySelectorAll("*") ?? []).filter((e) => e.localName === "tc");
      const at = focusCell ? after.indexOf(focusCell) : -1;
      return { xml: writeXml(doc), cell: at >= 0 ? at + 1 : null };
    },
    focus: () => editable.focus({ preventScroll: true }),
    dispose() {
      ro?.disconnect();
      document.removeEventListener("selectionchange", onSelection);
      document.removeEventListener("mouseup", onMouseUp);
      editable.removeEventListener("mousedown", onMouseDown);
      editable.removeEventListener("mousemove", onMouseMove);
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
 * The view of one block (of the body or a cell): an editable paragraph (`data-para`), a block shown as it is (`data-block`), or null when
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
  const kept = h("div", { class: "tf-docx-ro", contenteditable: "false", "data-block": i });
  for (const it of items) kept.append(it.el);
  kept.title = kept.querySelector("[data-cell]") ? texts.table : texts.locked;
  return kept;
}

/** The editable place holding a node: the view, or a table cell */
function hostOf(node: Node | null, editable: HTMLElement): HTMLElement | null {
  const el = node instanceof Element ? node : (node?.parentElement ?? null);
  const host = el?.closest<HTMLElement>("[data-host]") ?? null;
  return host && editable.contains(host) ? host : null;
}

/** The caret at the end of a cell's text */
function caretAtEnd(cell: HTMLElement) {
  cell.focus({ preventScroll: true });
  const r = document.createRange();
  r.selectNodeContents(cell);
  r.collapse(false);
  const sel = getSelection();
  sel?.removeAllRanges();
  sel?.addRange(r);
  cell.scrollIntoView({ block: "nearest" });
}

/** Tab in a table: the caret goes to the end of the next (or previous) cell of the same table */
function moveToCell(cell: HTMLElement, step: number) {
  const table = cell.closest("table");
  if (!table) return;
  const cells = Array.from(table.querySelectorAll<HTMLElement>("[data-cell]")).filter((c) => c.closest("table") === table);
  const next = cells[cells.indexOf(cell) + step];
  if (next) caretAtEnd(next);
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
  // In a table cell: that cell (the caret goes back to its end)
  const host = hostOf(sel.anchorNode, editable);
  if (host && host !== editable) return { block: -1, offset: 0, cell: Number(host.dataset.cell) };
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
