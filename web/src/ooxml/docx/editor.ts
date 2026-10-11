/**
 * Editing a Word document's text in the browser (the preview frame runs it; components/DocxEditor.tsx is the host).
 *
 * The body is shown as one page-wide sheet, drawn by the same renderer as the preview so text keeps its fonts, sizes,
 * spacing and list numbers; pages aren't shown. Headers and footers are edited one at a time, each opened in the view
 * instead of the body (as wide as its section's text), and edited like it; one can be added to a document without
 * (hfOps.ts). Paragraphs that only hold text are edited in place
 * (one contenteditable for the whole body, so selecting, typing, Enter, Backspace and undo are the browser's own); every
 * other block is shown as it is and can't be changed (edit.ts says which, and how the edits are written back).
 *
 * Table cells are edited the same way, each as an editable place of its own inside the table (which itself stays as it
 * is): Enter and Backspace stay in the cell, and Tab moves to the next cell (Shift+Tab to the one before), as in Word.
 * Rows and columns are added and deleted by changing the document (tableOps.ts) and opening the view again on it: the
 * host keeps the change before it, so Ctrl+Z right after undoes it. Cells are selected together for merging by
 * dragging across them or Shift+clicking another one; any key lets go of them. A column's right border is dragged to
 * make it wider or narrower (a guide line shows where it goes).
 *
 * The browser's editing is limited to what can be written back: plain text with bold, italic, underline, strikethrough,
 * color and highlight (other formatting commands, pasting rich content and dragging are refused), and Backspace or
 * Delete never joins a paragraph with a block that is kept as it is. That formatting is read back from what the view
 * shows, against what each run showed when the view was opened (formatOps.ts writes the difference). Font size,
 * paragraph style and alignment change the document between two marks put at the selection's ends, and the view is
 * opened again on it, as for tables.
 */

import { h, kid, type OoxmlPackage } from "../core/package";
import { breathe } from "../core/yield";
import { BlockWriter, type BlockItem } from "./blocks";
import type { Flow, PageFloat } from "./context";
import { DOCX_CSS } from "./css";
import { originalPieces, planEdits, readAllEdits, readParagraph, samePieces, saveEdits, writeXml, type EditPlan, type FmtOf } from "./edit";
import { formatRange, insertField, MARK_END, MARK_START, type FieldKind, type Fmt, type RangeFormat } from "./formatOps";
import { formatList, listKind, type ListFormat, type ListKind } from "./listOps";
import { hasCjk } from "./fonts";
import { addStory, listStories, setEvenOdd, setFirstPage, type StoryParts, type Story, type StoryKind } from "./hfOps";
import { cellRange, changeTable, type BorderKind, type TableOp } from "./tableOps";
import { loadDocx, normalStyle } from "./index";
import { contentWidth, parseSection, type Section } from "./section";
import { fixScaled, resolveTabs } from "./tabs";
import { alternate } from "./xml";
import { storyRoot } from "./xmlEdit";

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
  /** Cells selected together (their corners' containers), selected again once the view is opened */
  pick?: [number, number];
  /** Just after the field there (fields aren't counted in `offset`) */
  atom?: boolean;
}

/** What a change of a table needs besides the caret's cell: the split's numbers, the borders, the shading, a column's move */
export interface TableChange {
  cols?: number;
  rows?: number;
  border?: BorderKind;
  color?: string | null;
  /** Resizing: the cell (container) whose right edge moves, and by how many twips */
  cell?: number;
  delta?: number;
}

export interface EditorOptions {
  /** Parts with edits not saved yet (by path: document.xml, a numbering part a list made…), to open instead of the archive's */
  parts?: Record<string, string>;
  texts: EditorTexts;
  /** Path of the header or footer to open instead of the body */
  story?: string | null;
  caret?: Caret | null;
  scroll?: number;
  /** The text changed (typing, deleting, undo…) */
  onInput(): void;
  /** Ctrl+S */
  onSave(): void;
  /** Ctrl+Z: true when the host undid something itself (a change of a table's shape), so the browser doesn't */
  onUndo?(): boolean;
  /** The caret went into a table cell or out of one (`cell`), or cells were selected together for merging (`cells`) */
  onPlace?(place: Place): void;
  /** Tab or Shift+Tab at the start of a list item: the host moves it a level in or out, as the toolbar's buttons do */
  onList?(f: ListFormat): void;
  /** A change of a table made in the view itself (dragging a column's border), for the host to make as it makes the menus' */
  onTable?(op: TableOp, change: TableChange): void;
}

/** The formatting where the caret is, for the host's toolbar */
export interface CaretFormat {
  b: boolean;
  i: boolean;
  u: boolean;
  s: boolean;
  /** Font size (points), paragraph style id, alignment */
  size: number;
  style: string | null;
  align: "left" | "center" | "right" | "both";
  /** The kind of list the paragraph is in, if any */
  list: ListKind | null;
  /** The font shown at the caret */
  font: string;
  /** The paragraph's line spacing in 240ths of a line (null: not a multiple of lines), and its space before and after (twips) */
  line: number | null;
  before: number;
  after: number;
}

/** Where the caret is: in a table cell, how many cells are selected together, and the formatting there */
export interface Place {
  cell: boolean;
  cells: number;
  fmt: CaretFormat | null;
}

/** A font name the editor shows and writes: letters, digits, spaces and a few marks */
export const FONT_NAME = /^[\p{L}\p{N} .\-'&()]{1,64}$/u;

/** A font for the host's font list: East Asian fonts are set for East Asian text too */
export interface FontChoice {
  name: string;
  eastAsia: boolean;
}

/** A paragraph style the document defines, for the host's style list */
export interface ParagraphStyle {
  id: string;
  name: string;
  isDefault: boolean;
}

/** The view's own formatting commands (the browser's, so Ctrl+Z undoes them step by step) */
export type FormatCommand = "bold" | "italic" | "underline" | "strikeThrough" | "foreColor" | "hiliteColor";

export interface DocxEditor {
  /** document.xml as edited (null when the view holds the text it was opened with), where the caret is, and the scroll position */
  collect(): { path: string; xml: string | null; caret: Caret | null; scroll: number };
  /**
   * The document with the table of the caret's cell changed as `op` says, and the cell (container) to put the caret in
   * once it is opened again; undefined when the caret isn't in a cell or that table can't be changed this way
   */
  tableOp(op: TableOp, change?: TableChange): { xml: string; cell: number | null; pick?: [number, number] } | undefined;
  /** A formatting command on the selection, in the view */
  format(cmd: FormatCommand, value?: string): void;
  /**
   * The document with the selection's font, font size, paragraph style, alignment or spacing changed, and where to put the caret once
   * it is opened again; undefined when there is no selection to change
   */
  formatRange(f: RangeFormat): { xml: string; caret: Caret | null } | undefined;
  /** The part open with a field inserted at the caret (after the selection), and the caret just after it; undefined when it can't go there */
  insertField(kind: FieldKind): { xml: string; caret: Caret | null } | undefined;
  /** The paragraph styles the document defines (shown in its own style list) */
  styles: ParagraphStyle[];
  /** The fonts the document names in its font table */
  fonts: FontChoice[];
  /** Path of the part open in the view: document.xml, or the header or footer open */
  path: string;
  /** The headers and footers the document has, the one open (null: the body), and those the first section has none of */
  stories: Story[];
  story: string | null;
  canAdd: Record<StoryKind, boolean>;
  /**
   * The parts adding a header or footer to the first section makes (document.xml, its relationships, the new part and
   * the content types), and the new part's path; undefined when the first section has one
   */
  addStory(kind: StoryKind): Promise<{ parts: Record<string, string>; path: string } | undefined>;
  /** Whether the section of the part open (the first, for the body) has a different first page, and the document different odd and even pages */
  layout: { firstPage: boolean; evenOdd: boolean };
  /**
   * The parts turning a different first page (for that section) or different odd and even pages on or off makes, and
   * the header or footer to open (one made of the open one's kind, or null to keep the one open); undefined when it can't
   */
  setLayout(change: { firstPage: boolean } | { evenOdd: boolean }): Promise<{ parts: Record<string, string>; open: string | null } | undefined>;
  /**
   * The parts a list change (bulleted, numbered, a level in or out) makes of the paragraphs the selection touches:
   * document.xml, and the numbering part with its relationship and content type when they change; and where to put
   * the caret once the view is opened again. Undefined when there is no selection
   */
  formatList(f: ListFormat): Promise<{ parts: Record<string, string>; caret: Caret | null } | undefined>;
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
.tf-docx-editable [data-atom]{background:#e5e7eb;border-radius:2px;cursor:default}
.tf-docx-editable .tf-docx-p{tab-size:48px}
.tf-docx-ro:has([data-cell]):hover{outline:none}
.tf-docx-editable [data-cell]{outline:none;cursor:text}
.tf-docx-editable [data-cell]:focus-within{box-shadow:inset 0 0 0 2px rgba(37,99,235,.55)}
.tf-docx-editable [data-cell][data-picked]{background-image:linear-gradient(rgba(37,99,235,.22),rgba(37,99,235,.22))}
.tf-docx-editable [data-picked] ::selection{background:transparent}
.tf-docx-guide{position:fixed;width:0;border-left:1px dashed #2563eb;pointer-events:none;z-index:10}
`;

/** Input the editor can't write back: formatting, lists, links, rich drops */
const REFUSED = new Set(["insertFromDrop", "deleteByDrag", "insertOrderedList", "insertUnorderedList", "insertHorizontalRule", "insertLink", "insertFromPasteAsQuotation"]);
/** Formatting the view can write back (formatOps.ts); other formatting commands are refused */
const FORMATS = new Set(["formatBold", "formatItalic", "formatUnderline", "formatStrikeThrough", "formatFontColor", "formatBackColor"]);

/** Characters XML can't hold (a pasted control character would make the file unreadable) */
// oxlint-disable-next-line no-control-regex -- control characters are removed on purpose
const CONTROL = /[\u{0}-\u{8}\u{b}\u{c}\u{e}-\u{1f}\u{fffe}\u{ffff}]/gu;

export async function openDocxEditor(pkg: OoxmlPackage, root: HTMLElement, opts: EditorOptions): Promise<DocxEditor> {
  pkg.useParts(opts.parts ?? {});
  const loaded = await loadDocx(pkg);
  const { doc, docXml, docRels } = loaded;
  const docBody = storyRoot(docXml);
  if (!docXml || !docBody) throw new Error("The document has no body");

  // ── Sections: each body block's section, as the preview groups them ──
  const sectPrs: (Element | null)[] = [];
  const secAt = new Map<Element, number>();
  for (const b of Array.from(docBody.children)) {
    if (b.localName === "sectPr") continue;
    secAt.set(b, sectPrs.length);
    const sp = b.localName === "p" ? kid(kid(b, "pPr"), "sectPr") : null;
    if (sp) sectPrs.push(sp);
  }
  sectPrs.push(kid(docBody, "sectPr"));
  const sections: Section[] = [];
  sectPrs.forEach((sp, i) => sections.push(parseSection(sp, i ? sections[i - 1] : null)));

  // ── The part edited: the body, or a header or footer the document has ──
  const stories = listStories(sections, docRels, (path) => !!loaded.parts.get(path), doc.settings.evenAndOdd);
  const story = stories.find((x) => x.path === opts.story) ?? null;
  const storyPath = story?.path ?? loaded.docPath;
  const plan = planEdits(story ? (loaded.parts.get(story.path) ?? null) : docXml);
  if (!plan) throw new Error("The document has no body");
  const bodyBlocks = plan.containers[0].blocks;

  // ── The view ──
  const normal = normalStyle(doc);
  // The page the view is as wide as: the first section's, or the header's or footer's section's
  const first = sections[story?.section ?? 0];
  const margin = (m: number) => (story ? 24 : Math.max(24, Math.min(m, 96)));
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
      style: `width:${first.width}px;padding:${margin(first.top)}px ${first.right}px ${margin(first.bottom)}px ${first.left}px`,
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
    atom: (el) => {
      const id = plan.atomIds.get(el);
      return id === undefined ? undefined : { id, last: plan.atoms[id][plan.atoms[id].length - 1] };
    },
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
    const sec = story ? story.section : Math.min(secAt.get(block) ?? 0, sections.length - 1);
    if (!writer || sec !== writerSec) {
      const s = sections[sec];
      // A header or footer is as wide as its section's text; the body's text as wide as a column
      const cols = story ? 1 : s.cols.num;
      const width = cols > 1 ? (contentWidth(s) - s.cols.space * (cols - 1)) / cols : contentWidth(s);
      const flow: Flow = {
        doc,
        part: storyPath,
        rels: story ? (doc.rels.get(storyPath) ?? []) : docRels,
        section: s,
        width,
        story: story?.kind ?? "body",
        depth: 0,
        fields: shared.fields,
        comments: shared.comments,
        floats,
        edit,
      };
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

  // ── Character formatting: what each run shows now, to tell what the view changes later ──
  const shownFmt = new Map<number, Shown>();
  for (const span of Array.from(editable.querySelectorAll<HTMLElement>("[data-r]"))) {
    const run = Number(span.dataset.r);
    if (shownFmt.has(run) || span.tagName === "BR") continue;
    const text = span.firstChild;
    const f = text ? shownAt(text) : null;
    if (f) shownFmt.set(run, f);
  }
  // A text node's formatting differing from what its run showed (text typed without a run: from its paragraph's)
  const fmtOf: FmtOf = (text, run) => {
    const now = shownAt(text);
    const block = text.parentElement?.closest<HTMLElement>("[data-para], p, div, td, li");
    const before = run !== null ? shownFmt.get(run) : block ? shownOfBlock(block) : null;
    if (!now || !before) return undefined;
    const out: Fmt = {};
    if (now.b !== before.b) out.b = now.b;
    if (now.i !== before.i) out.i = now.i;
    if (now.u !== before.u) out.u = now.u;
    if (now.s !== before.s) out.s = now.s;
    if (now.color !== before.color) out.color = now.color;
    if (now.hl !== before.hl) out.hl = now.hl;
    return Object.keys(out).length ? out : undefined;
  };
  const styles = await paragraphStyles(pkg, docRels, plan.doc);
  const fontTable = docRels.find((r) => r.type.endsWith("/fontTable") && !r.external)?.target;
  const fonts = fontNames(fontTable ? await pkg.xml(fontTable) : null);
  // The parts lists use: the numbering part (where it is, or where it goes), the document's relationships, content types
  const numberingPath = docRels.find((r) => r.type.endsWith("/numbering") && !r.external)?.target ?? loaded.docPath.replace(/[^/]*$/, "numbering.xml");
  const relsPath = loaded.docPath.replace(/([^/]*)$/, "_rels/$1.rels");
  const stylesPath = docRels.find((r) => r.type.endsWith("/styles") && !r.external)?.target ?? null;
  const settingsPath = docRels.find((r) => r.type.endsWith("/settings") && !r.external)?.target ?? null;
  const numbering = await pkg.xml(numberingPath);

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
    if ((e.inputType.startsWith("format") && !FORMATS.has(e.inputType)) || REFUSED.has(e.inputType)) e.preventDefault();
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
      // At the start of a list item (or with several paragraphs selected in a list), Tab and Shift+Tab change its level
      const sel = getSelection();
      const p = sel?.rangeCount ? blockOf(editable, sel.anchorNode) : null;
      const listed = !!p?.querySelector(".tf-docx-marker");
      if (listed && p && sel && (!sel.isCollapsed ? blockOf(editable, sel.focusNode) !== p : atEdge(p, sel.getRangeAt(0), true))) {
        e.preventDefault();
        opts.onList?.({ level: e.shiftKey ? -1 : 1 });
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
  const place = () => opts.onPlace?.({ cell: inCell === true || !!picked, cells: picked ? pickedCells().length : 0, fmt: caretFormat(plan, editable, numbering) });
  let lastFmt = "";
  // The last selection the person made: the host's toolbar takes focus away (a list of sizes, a color menu), and focus
  // coming back may move the selection, so commands from the toolbar act on this one. Once the view loses focus, the
  // selection is only taken again when the person clicks or types in the view
  let lastRange: Range | null = null;
  let frozen = false;
  const onWindowBlur = () => {
    // The selection as it is now: a selection made just before (a double-click) may not have been reported yet
    keepSelection();
    frozen = true;
  };
  const onTouch = () => {
    frozen = false;
  };
  window.addEventListener("blur", onWindowBlur);
  editable.addEventListener("mousedown", onTouch, true);
  editable.addEventListener("keydown", onTouch, true);
  const keepSelection = () => {
    const sel = getSelection();
    if (frozen || !sel?.rangeCount || !editable.contains(sel.anchorNode)) return;
    lastRange = sel.getRangeAt(0).cloneRange();
  };
  const restoreSelection = () => {
    if (!lastRange || !editable.contains(lastRange.startContainer)) return;
    const sel = getSelection();
    sel?.removeAllRanges();
    sel?.addRange(lastRange);
  };
  const onSelection = () => {
    keepSelection();
    const now = !!cellOf(getSelection()?.anchorNode ?? null);
    // Told when the cell or the formatting where the caret is changes
    const fmt = JSON.stringify(caretFormat(plan, editable, numbering));
    if (now === inCell && fmt === lastFmt) return;
    inCell = now;
    lastFmt = fmt;
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
  // A column's right border: grabbed within a few pixels of it, moved with a guide line, applied when let go
  const EDGE = 4;
  const edgeOf = (e: MouseEvent) => {
    const cell = cellOf(e.target as Node);
    if (!cell) return null;
    const box = cell.getBoundingClientRect();
    if (Math.abs(e.clientX - box.right) <= EDGE) return cell;
    // Just right of a border, the pointer is over the next cell: the border is the cell before's
    const prev = cell.previousElementSibling;
    return Math.abs(e.clientX - box.left) <= EDGE && prev instanceof HTMLElement && prev.dataset.cell !== undefined ? prev : null;
  };
  let resizing: { cell: HTMLElement; x: number; guide: HTMLElement } | null = null;
  const onHover = (e: MouseEvent) => {
    if (resizing || e.buttons) return;
    editable.style.cursor = edgeOf(e) ? "col-resize" : "";
  };
  const onResizeMove = (e: MouseEvent) => {
    if (resizing) resizing.guide.style.left = `${e.clientX}px`;
  };
  const onResizeEnd = (e: MouseEvent) => {
    if (!resizing) return;
    const { cell, x, guide } = resizing;
    resizing = null;
    guide.remove();
    document.removeEventListener("mousemove", onResizeMove);
    document.removeEventListener("mouseup", onResizeEnd);
    editable.style.cursor = "";
    // Pixels on screen, at the page's zoom, as twips (15 to a CSS pixel)
    const zoom = Number(sheet.style.zoom) || 1;
    const delta = Math.round(((e.clientX - x) / zoom) * 15);
    if (Math.abs(delta) >= 15) opts.onTable?.("resize", { cell: Number(cell.dataset.cell), delta });
  };
  const onMouseDown = (e: MouseEvent) => {
    if (e.button !== 0) return;
    const edge = edgeOf(e);
    if (edge) {
      e.preventDefault();
      const table = edge.closest("table")!.getBoundingClientRect();
      const guide = h("div", { class: "tf-docx-guide", style: `left:${e.clientX}px;top:${table.top}px;height:${table.height}px` });
      document.body.append(guide);
      resizing = { cell: edge, x: e.clientX, guide };
      document.addEventListener("mousemove", onResizeMove);
      document.addEventListener("mouseup", onResizeEnd);
      return;
    }
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
  editable.addEventListener("mousemove", onHover);
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
    const [pa, pb] = (caret?.pick ?? []).map((c) => editable.querySelector<HTMLElement>(`[data-cell="${c}"]`));
    if (pa && pb) pick(pa, pb);
  };
  focus();

  /**
   * The document as edited, with marks at the selection's ends (`word`: no selection takes the word at the caret), and
   * where the caret was; undefined when there is no selection in the view
   */
  /** A copy of a part, so the view's own stays as it was until it is opened again */
  const copy = async (path: string | null) => {
    const d = path ? await pkg.xml(path) : null;
    return d ? (d.cloneNode(true) as Document) : null;
  };
  const emptyRels = () => new DOMParser().parseFromString('<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>', "application/xml");

  /** Copies of the parts a change of the headers and footers reads and changes, document.xml with the view's edits when it shows the body */
  const storyParts = async (): Promise<StoryParts> => {
    const text = (!story && saveEdits(plan, readAllEdits(editable, fmtOf), shown)) || writeXml(docXml);
    const given = opts.parts ?? {};
    return {
      doc: new DOMParser().parseFromString(text, "application/xml"),
      rels: (await copy(relsPath)) ?? emptyRels(),
      types: await copy("[Content_Types].xml"),
      styles: stylesPath ? await pkg.xml(stylesPath) : null,
      settings: await copy(settingsPath),
      docPath: loaded.docPath,
      taken: (path) => pkg.has(path) || path in given,
    };
  };

  const markSelection = (word: boolean): { doc: Document; caret: Caret | null } | undefined => {
    restoreSelection();
    const sel = getSelection();
    if (!sel?.rangeCount || !editable.contains(sel.anchorNode)) return undefined;
    if (word && sel.isCollapsed) {
      sel.modify("move", "backward", "word");
      sel.modify("extend", "forward", "word");
    }
    const caret = caretOf(editable);
    const range = sel.getRangeAt(0);
    const start = document.createTextNode(MARK_START);
    const end = document.createTextNode(MARK_END);
    const atEnd = range.cloneRange();
    atEnd.collapse(false);
    atEnd.insertNode(end);
    const atStart = range.cloneRange();
    atStart.collapse(true);
    atStart.insertNode(start);
    let text: string;
    try {
      text = saveEdits(plan, readAllEdits(editable, fmtOf), shown) ?? writeXml(plan.doc);
    } finally {
      start.remove();
      end.remove();
    }
    return { doc: new DOMParser().parseFromString(text, "application/xml"), caret };
  };

  return {
    collect() {
      const xml = saveEdits(plan, readAllEdits(editable, fmtOf), shown);
      return { path: storyPath, xml, caret: caretOf(editable), scroll: root.scrollTop };
    },
    tableOp(op, change = {}) {
      restoreSelection();
      // Merging, borders and shading: the cells selected together; resizing: the cell whose border was dragged;
      // anything else: the caret's cell (or the first of those selected)
      const ranged = op === "merge" || op === "border" || op === "shade";
      const host =
        op === "resize"
          ? editable.querySelector<HTMLElement>(`[data-cell="${change.cell}"]`)
          : op === "merge"
            ? picked?.a
            : ranged && picked
              ? picked.a
              : (cellOf(getSelection()?.anchorNode ?? null) ?? picked?.a);
      const tc = host ? plan.containers[Number(host.dataset.cell)]?.el : undefined;
      const otherTc = ranged && picked ? plan.containers[Number(picked.b.dataset.cell)]?.el : undefined;
      if (!tc || tc.localName !== "tc" || (op === "merge" && !otherTc)) return undefined;
      // Borders and shading keep the cells: they stay selected once the view is opened again
      const keep: [number, number] | undefined = (op === "border" || op === "shade") && picked ? [Number(picked.a.dataset.cell), Number(picked.b.dataset.cell)] : undefined;
      // The document as edited, with the cells marked so they can be found in it
      tc.setAttribute("data-tf-cell", "");
      otherTc?.setAttribute("data-tf-other", "");
      let text: string;
      try {
        text = saveEdits(plan, readAllEdits(editable, fmtOf), shown) ?? writeXml(plan.doc);
      } finally {
        tc.removeAttribute("data-tf-cell");
        otherTc?.removeAttribute("data-tf-other");
      }
      const doc = new DOMParser().parseFromString(text, "application/xml");
      const body = storyRoot(doc);
      const cells = Array.from(body?.querySelectorAll("*") ?? []).filter((e) => e.localName === "tc");
      const target = cells.find((e) => e.hasAttribute("data-tf-cell"));
      const other = cells.find((e) => e.hasAttribute("data-tf-other"));
      if (!target) return undefined;
      target.removeAttribute("data-tf-cell");
      other?.removeAttribute("data-tf-other");
      const focusCell = changeTable(target, op, { other, cols: change.cols, rows: change.rows, border: change.border, color: change.color, delta: change.delta });
      if (focusCell === undefined) return undefined;
      // Containers are the body, then the cells in document order (as planEdits makes them)
      const after = Array.from(body?.querySelectorAll("*") ?? []).filter((e) => e.localName === "tc");
      const at = focusCell ? after.indexOf(focusCell) : -1;
      return { xml: writeXml(doc), cell: at >= 0 ? at + 1 : null, pick: keep };
    },
    format(cmd, value) {
      if (!editable.contains(document.activeElement)) editable.focus({ preventScroll: true });
      restoreSelection();
      document.execCommand("styleWithCSS", false, "true");
      document.execCommand(cmd, false, value);
      // The command replaces the text it formats: the selection kept is the one now on it, for the next command
      const sel = getSelection();
      if (sel?.rangeCount && editable.contains(sel.anchorNode)) lastRange = sel.getRangeAt(0).cloneRange();
      onSelection();
    },
    formatRange(f) {
      const marked = markSelection("size" in f || "font" in f);
      if (!marked) return undefined;
      if (!formatRange(marked.doc, f)) return undefined;
      return { xml: writeXml(marked.doc), caret: marked.caret };
    },
    insertField(kind) {
      restoreSelection();
      getSelection()?.collapseToEnd();
      const marked = markSelection(false);
      if (!marked || !insertField(marked.doc, kind)) return undefined;
      return { xml: writeXml(marked.doc), caret: marked.caret && marked.caret.cell === undefined ? { ...marked.caret, atom: true } : marked.caret };
    },
    async formatList(f) {
      const marked = markSelection(false);
      if (!marked) return undefined;
      // Copies of the parts a list may change (the numbering part is the document's, whichever part is open)
      const rels = (await copy(relsPath)) ?? emptyRels();
      const result = formatList(
        { doc: marked.doc, numbering: await copy(numberingPath), styles: stylesPath ? await pkg.xml(stylesPath) : null, rels, types: await copy("[Content_Types].xml") },
        f,
      );
      if (!result) return undefined;
      const parts: Record<string, string> = { [storyPath]: writeXml(marked.doc) };
      if (result.changed.numbering && result.numbering) parts[numberingPath] = writeXml(result.numbering);
      if (result.changed.rels && result.rels) parts[relsPath] = writeXml(result.rels);
      if (result.changed.types && result.types) parts["[Content_Types].xml"] = writeXml(result.types);
      return { parts, caret: marked.caret };
    },
    async addStory(kind) {
      const sp = await storyParts();
      const added = addStory(sp, kind);
      if (!added) return undefined;
      const parts: Record<string, string> = { [loaded.docPath]: writeXml(sp.doc), [relsPath]: writeXml(sp.rels), [added.path]: writeXml(added.part) };
      if (added.typesChanged && sp.types) parts["[Content_Types].xml"] = writeXml(sp.types);
      return { parts, path: added.path };
    },
    layout: { firstPage: !!first.titlePg, evenOdd: doc.settings.evenAndOdd },
    async setLayout(change) {
      const sp = await storyParts();
      const result = "firstPage" in change ? setFirstPage(sp, story?.section ?? 0, change.firstPage) : setEvenOdd(sp, change.evenOdd);
      if (!result) return undefined;
      const parts: Record<string, string> = { [loaded.docPath]: writeXml(sp.doc) };
      for (const m of result.made) parts[m.path] = writeXml(m.part);
      if (result.relsChanged) parts[relsPath] = writeXml(sp.rels);
      if (result.typesChanged && sp.types) parts["[Content_Types].xml"] = writeXml(sp.types);
      const settingsAt = result.settingsPath ?? settingsPath;
      if (result.settings && settingsAt) parts[settingsAt] = writeXml(result.settings);
      // The new first-page or even-page one of the kind open (a header, when the body is)
      const open = result.made.find((m) => m.kind === (story?.kind ?? "header"))?.path ?? null;
      return { parts, open };
    },
    path: storyPath,
    stories,
    story: story?.path ?? null,
    canAdd: { header: !sections[0]?.headers.default, footer: !sections[0]?.footers.default },
    styles,
    fonts,
    focus: () => editable.focus({ preventScroll: true }),
    dispose() {
      ro?.disconnect();
      document.removeEventListener("selectionchange", onSelection);
      window.removeEventListener("blur", onWindowBlur);
      editable.removeEventListener("mousedown", onTouch, true);
      editable.removeEventListener("keydown", onTouch, true);
      document.removeEventListener("mouseup", onMouseUp);
      editable.removeEventListener("mousedown", onMouseDown);
      editable.removeEventListener("mousemove", onMouseMove);
      editable.removeEventListener("mousemove", onHover);
      document.removeEventListener("mousemove", onResizeMove);
      document.removeEventListener("mouseup", onResizeEnd);
      resizing?.guide.remove();
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
      // At the end of a text the field follows: just after the field
      if (caret.atom && left === len && afterAtom(p, n)) return;
      getSelection()?.collapse(n, left);
      return;
    }
    left -= len;
  }
  // No text after the caret: after the paragraph's last field, or at its end
  const atoms = p.querySelectorAll("[data-atom]");
  const last = caret.atom ? atoms[atoms.length - 1] : null;
  if (last?.parentNode) getSelection()?.collapse(last.parentNode, Array.prototype.indexOf.call(last.parentNode.childNodes, last) + 1);
  else getSelection()?.collapse(p, p.childNodes.length);
}

/** Puts the caret just after the field right after `n` (within paragraph `p`), if there is one there */
function afterAtom(p: HTMLElement, n: Node): boolean {
  let e: Node | null = n;
  while (e && e !== p && !e.nextSibling) e = e.parentNode;
  const next = e && e !== p ? e.nextSibling : null;
  if (!(next instanceof HTMLElement) || next.dataset.atom === undefined || !next.parentNode) return false;
  getSelection()?.collapse(next.parentNode, Array.prototype.indexOf.call(next.parentNode.childNodes, next) + 1);
  return true;
}

// ───────────── Formatting shown ─────────────

/** Character formatting as the view shows it (colors as "RRGGBB"; a highlight null when there is none) */
interface Shown {
  b: boolean;
  i: boolean;
  u: boolean;
  s: boolean;
  color: string;
  hl: string | null;
}

/** "rgb(…)" or "rgba(…)" as "RRGGBB", or null when transparent */
function hexOf(css: string): string | null {
  const m = /rgba?\(\s*(\d+)[,\s]+(\d+)[,\s]+(\d+)(?:[,\s/]+([\d.]+))?/.exec(css);
  if (!m || (m[4] !== undefined && Number(m[4]) === 0)) return null;
  return [m[1], m[2], m[3]]
    .map((v) => Number(v).toString(16).padStart(2, "0"))
    .join("")
    .toUpperCase();
}

/** Elements that hold a paragraph's text, whose own looks (paragraph or cell shading) aren't the text's */
const isHolder = (e: Element) => e instanceof HTMLElement && (e.dataset.para !== undefined || e.dataset.host !== undefined || ["P", "DIV", "TD", "LI"].includes(e.tagName));

/** What a text node shows: weight, style and color from its own element; lines and highlight from those up to its paragraph */
function shownAt(text: Node): Shown | null {
  const el = text.parentElement;
  if (!el || !el.isConnected) return null;
  const cs = getComputedStyle(el);
  let u = false;
  let s = false;
  let hl: string | null = null;
  for (let e: Element | null = el; e && !isHolder(e); e = e.parentElement) {
    const st = getComputedStyle(e);
    const lines = st.textDecorationLine;
    if (lines.includes("underline")) u = true;
    if (lines.includes("line-through")) s = true;
    hl ??= hexOf(st.backgroundColor);
  }
  return { b: Number(cs.fontWeight) >= 600, i: cs.fontStyle !== "normal", u, s, color: hexOf(cs.color) ?? "000000", hl };
}

/** What a paragraph shows for text typed into it without a run (the paragraph mark's looks) */
function shownOfBlock(block: HTMLElement): Shown {
  const cs = getComputedStyle(block);
  return { b: Number(cs.fontWeight) >= 600, i: cs.fontStyle !== "normal", u: false, s: false, color: hexOf(cs.color) ?? "000000", hl: null };
}

/** The formatting where the caret is */
function caretFormat(plan: EditPlan, editable: HTMLElement, numbering: Document | null): CaretFormat | null {
  const sel = getSelection();
  const node = sel?.anchorNode ?? null;
  if (!node || !editable.contains(node)) return null;
  const el = node instanceof Element ? node : node.parentElement;
  if (!el) return null;
  const text = node.nodeType === 3 ? node : (el.firstChild ?? el);
  const shown = text.nodeType === 3 ? shownAt(text) : null;
  const cs = getComputedStyle(el);
  const para = el.closest<HTMLElement>("[data-para]");
  const source = para ? plan.blocks[Number(para.dataset.para)] : null;
  const pStyle = source ? kid(kid(source, "pPr"), "pStyle") : null;
  const align = para ? getComputedStyle(para).textAlign : "left";
  const numPr = source ? kid(kid(source, "pPr"), "numPr") : null;
  const numVal = (name: string) => {
    const e = kid(numPr, name);
    return e ? (e.getAttributeNS(e.namespaceURI, "val") ?? e.getAttribute("w:val")) : null;
  };
  return {
    b: shown?.b ?? Number(cs.fontWeight) >= 600,
    i: shown?.i ?? cs.fontStyle !== "normal",
    u: shown?.u ?? false,
    s: shown?.s ?? false,
    size: Math.round(parseFloat(cs.fontSize) * 0.75 * 2) / 2,
    style: pStyle ? (pStyle.getAttributeNS(pStyle.namespaceURI, "val") ?? pStyle.getAttribute("w:val")) : null,
    align: align === "center" ? "center" : align === "right" || align === "end" ? "right" : align === "justify" ? "both" : "left",
    list: listKind(numbering, numVal("numId"), Number(numVal("ilvl") ?? 0) || 0),
    font: cs.fontFamily.split(",")[0].replace(/["']/g, "").trim(),
    line: para?.dataset.spLine ? Number(para.dataset.spLine) : null,
    before: Number(para?.dataset.spBefore ?? 0) || 0,
    after: Number(para?.dataset.spAfter ?? 0) || 0,
  };
}

/** Character sets of East Asian fonts in a font table: Shift JIS, Hangul, Johab, GB 2312, Big5 */
const EAST_ASIAN_CHARSETS = new Set(["80", "81", "82", "86", "88"]);

/** The fonts a font table names (those safe to show and to write back), East Asian by their character set or name */
function fontNames(table: Document | null): FontChoice[] {
  const out = new Map<string, FontChoice>();
  for (const f of Array.from(table?.documentElement?.children ?? [])) {
    if (f.localName !== "font") continue;
    const name = f.getAttributeNS(f.namespaceURI, "name") ?? f.getAttribute("w:name") ?? "";
    if (!FONT_NAME.test(name)) continue;
    const charset = kid(f, "charset");
    const cs = (charset?.getAttributeNS(charset.namespaceURI, "val") ?? charset?.getAttribute("w:val") ?? "").toUpperCase();
    out.set(name, { name, eastAsia: EAST_ASIAN_CHARSETS.has(cs) || hasCjk(name) });
  }
  return Array.from(out.values()).sort((a, b) => a.name.localeCompare(b.name));
}

/** The paragraph styles the document defines that Word shows in its gallery, and those its paragraphs use */
async function paragraphStyles(pkg: OoxmlPackage, rels: { type: string; target: string; external?: boolean }[], docXml: Document | null): Promise<ParagraphStyle[]> {
  const path = rels.find((r) => r.type.endsWith("/styles") && !r.external)?.target;
  const xml = path ? await pkg.xml(path) : null;
  const used = new Set(
    Array.from(docXml?.querySelectorAll("*") ?? [])
      .filter((e) => e.localName === "pStyle")
      .map((e) => e.getAttributeNS(e.namespaceURI, "val") ?? e.getAttribute("w:val") ?? ""),
  );
  const out: ParagraphStyle[] = [];
  for (const s of Array.from(xml?.documentElement?.children ?? [])) {
    if (s.localName !== "style") continue;
    const val = (name: string) => s.getAttributeNS(s.namespaceURI, name) ?? s.getAttribute(`w:${name}`);
    if (val("type") !== "paragraph") continue;
    const id = val("styleId") ?? "";
    const isDefault = val("default") === "1" || val("default") === "true";
    const shownInGallery = !!kid(s, "qFormat") && !kid(s, "semiHidden");
    if (!id || !(isDefault || shownInGallery || used.has(id))) continue;
    const nameEl = kid(s, "name");
    const name = (nameEl && (nameEl.getAttributeNS(nameEl.namespaceURI, "val") ?? nameEl.getAttribute("w:val"))) || id;
    out.push({ id, name, isDefault });
  }
  // The default first, then headings in order, then the others as Word lists them
  const rank = (s: ParagraphStyle) => (s.isDefault ? 0 : /^heading \d$/i.test(s.name) ? 1 + Number(s.name.slice(-1)) : 20);
  return out.sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
}
