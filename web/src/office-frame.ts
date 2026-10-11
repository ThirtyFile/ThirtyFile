/**
 * Sandbox page for Word / PowerPoint previews.
 *
 * Documents are turned into DOM by our own renderers (ooxml/docx, ooxml/pptx), building the view only with textContent / setAttribute;
 * as an extra layer of protection, rendering still happens in an iframe without same-origin rights: even if a renderer has a bug, it can't get this site's cookies or call the API.
 *
 * This code is embedded by the parent page into the iframe's srcdoc (the iframe neither needs nor is allowed any network access).
 * Messaging: the parent sends { type: "render", kind, buffer }; this replies { type: "done" } or { type: "error", message },
 * and replies { type: "link", href } when a link in the document is clicked; the parent opens it after checking it's http(s) / mailto.
 */

import { renderDocx } from "@/ooxml/docx";
import { renderPptx } from "@/ooxml/pptx";
import { OoxmlPackage, h } from "@/ooxml/core/package";
import { parseTheme, type Theme } from "@/ooxml/core/theme";
import { anchorBox, readDrawings, renderItem } from "@/ooxml/xlsx/drawings";
import { FONT_NAME, openDocxEditor, type Caret, type DocxEditor, type EditorTexts, type FormatCommand, type TableChange } from "@/ooxml/docx/editor";
import type { FieldKind, RangeFormat } from "@/ooxml/docx/formatOps";
import type { ListFormat } from "@/ooxml/docx/listOps";
import type { TableOp } from "@/ooxml/docx/tableOps";

type RenderMessage = { type: "render"; kind: "docx" | "pptx"; buffer: ArrayBuffer };
/** Excel: the workbook is loaded once, then each worksheet's drawing objects are rendered on request at the given column/row positions */
type LoadXlsxMessage = { type: "load"; kind: "xlsx"; buffer: ArrayBuffer };
type DrawingsMessage = { type: "render"; kind: "xlsx-drawings"; sheet: string; cols: Float64Array; rows: Float64Array; frozen?: { rows: number; cols: number } };
type ScrollMessage = { type: "scroll"; x: number; y: number };
/**
 * Word editing: open the document for editing (`parts`: its edited parts not saved yet). The frame then says "input"
 * when the text changes and "save" for Ctrl+S, answers "collect" with the edited parts, and tells the part open and the
 * headers and footers the document has ("stories") each time the view is opened
 */
type EditMessage = { type: "edit"; kind: "docx"; buffer: ArrayBuffer; parts?: Parts; texts: EditorTexts; caret?: Caret | null; scroll?: number };
/** Edited parts by path (document.xml, a numbering part a list made…); the others are the archive's */
type Parts = Record<string, string>;
type CollectMessage = { type: "collect"; id: number };
/**
 * Word editing: change the table the caret is in (answered "table-refused" when it can't be): a row or column added or
 * deleted, the cells selected together merged, or the caret's cell split into `cols` columns and `rows` rows
 */
type TableMessage = { type: "table"; op: TableOp; cols?: number; rows?: number; border?: string; color?: string | null };
/**
 * Word editing: formatting of the selection. `cmd` is one of the view's own commands (bold… with `value` a color);
 * `size`, `font`, `style`, `align`, `line`, `spaceBefore` or `spaceAfter` change the document and open the view again
 * (answered "format-refused" when they can't); `list` and `level` change lists
 */
type FormatMessage = {
  type: "format";
  cmd?: string;
  value?: string;
  size?: number;
  style?: string;
  isDefault?: boolean;
  align?: string;
  list?: string;
  level?: number;
  font?: string;
  eastAsia?: boolean;
  line?: number;
  spaceBefore?: boolean;
  spaceAfter?: boolean;
  /** A field to insert at the caret: the page number, or the number of pages */
  field?: string;
};
/**
 * Word editing: open a header or footer the document has (`path`; null: the body), add one (`add`) and open it, or turn
 * a different first page (for the open one's section) or different odd and even pages on or off
 */
type StoryMessage = { type: "story"; path?: string | null; add?: string; firstPage?: boolean; evenOdd?: boolean };
type Message = RenderMessage | LoadXlsxMessage | DrawingsMessage | ScrollMessage | EditMessage | CollectMessage | TableMessage | FormatMessage | StoryMessage;

const root = document.getElementById("root")!;
/**
 * "*": this frame's origin is opaque, so it can't name the parent's origin either. The message only goes to window.parent,
 * which ignores messages that don't come from this frame's window; nothing in them is secret
 */
const reply = (msg: object) => window.parent.postMessage(msg, "*");
let dispose: (() => void) | null = null;
let editor: DocxEditor | null = null;

// The iframe can't open new windows: external links are handed to the parent; in-page bookmarks scroll directly.
// While editing, a click on a link puts the caret in it; Ctrl+click (⌘ on a Mac) follows it
document.addEventListener("click", (e) => {
  const a = (e.target as Element | null)?.closest?.("a[href]");
  if (!a) return;
  const href = a.getAttribute("href") ?? "";
  e.preventDefault();
  if (editor && !e.ctrlKey && !e.metaKey) return;
  if (href.startsWith("#")) {
    document.getElementById(href.slice(1))?.scrollIntoView({ block: "start" });
    return;
  }
  if (/^(https?:|mailto:)/i.test(href)) reply({ type: "link", href });
});

async function render({ kind, buffer }: RenderMessage) {
  dispose?.();
  dispose = null;
  editor = null;
  root.replaceChildren();
  const result = kind === "docx" ? await renderDocx(buffer, root) : await renderPptx(buffer, root);
  dispose = result.dispose;
}

/**
 * After typing stops, and when the frame loses focus, the edited document goes to the host, which keeps it when the tab
 * is switched (switching takes the focus away first, so a longer pause loses nothing)
 */
const CHANGED_DELAY = 1500;
const TABLE_OPS = new Set<string>(["rowAbove", "rowBelow", "colLeft", "colRight", "deleteRow", "deleteCol", "merge", "split", "border", "shade"]);
const BORDER_KINDS = new Set<string>(["all", "outside", "inside", "thickOutside", "top", "bottom", "left", "right", "none"]);
const FORMAT_COMMANDS = new Set<string>(["bold", "italic", "underline", "strikeThrough", "foreColor", "hiliteColor"]);
const ALIGNMENTS = new Set<string>(["left", "center", "right", "both"]);
const COLOR = /^(#[0-9a-f]{6}|transparent)$/i;

/**
 * The document being edited. `base` are the edited parts the view was last opened on: a change of a table's shape opens
 * the view again on the changed document, keeping the one before it in `undo`, for Ctrl+Z right after (`typed` says
 * whether anything was typed since). `story` is the header or footer open in the view (null: the body)
 */
let editing: {
  pkg: OoxmlPackage;
  texts: EditorTexts;
  base: Parts;
  undo: { parts: Parts; caret: Caret | null; story: string | null }[];
  typed: boolean;
  story: string | null;
  /** The parts the view is being opened on, until it is open (saving meanwhile takes these) */
  opening: Parts | null;
} | null = null;
/** Editing actions waiting in the queue or running (Ctrl+Z waits for them, so it undoes the latest) */
let pendingActions = 0;
/** Text typed while the view opens again: kept, and typed into the new view once it is open */
let held: string | null = null;
let changedTimer = 0;

/** The edited parts (none: the archive as it is), with the caret and scroll */
function edited(): { parts: Parts; caret: Caret | null; scroll: number } {
  if (!editor || !editing) throw new Error("The document isn't open for editing");
  const c = editor.collect();
  return { parts: c.xml ? { ...editing.base, [c.path]: c.xml } : editing.base, caret: c.caret, scroll: c.scroll };
}

/** What the host keeps of the edits: the parts, and the caret and scroll only while the body is open (they are the body's) */
function forHost() {
  const e = edited();
  return editing?.story ? { parts: e.parts, caret: null, scroll: 0 } : e;
}

/** Only parts with text, from a message */
function partsOf(value: unknown): Parts {
  const out: Parts = {};
  if (value && typeof value === "object") for (const [k, v] of Object.entries(value)) if (typeof v === "string") out[k] = v;
  return out;
}

function sendChanged() {
  window.clearTimeout(changedTimer);
  changedTimer = 0;
  if (!editor) return;
  try {
    reply({ type: "changed", ...forHost() });
  } catch (err) {
    console.warn("docx edit", err);
  }
}
window.addEventListener("blur", () => {
  if (changedTimer) sendChanged();
});

// While the view opens again, nothing typed goes into the view being replaced: text is kept for the new one, and
// keys that would change the text (Enter, Backspace…) wait (the person presses them again)
root.addEventListener(
  "beforeinput",
  (e) => {
    if (held === null) return;
    e.preventDefault();
    if (e.inputType === "insertText" && e.data) held += e.data;
  },
  true,
);

async function openEditor(parts: Parts, caret: Caret | null | undefined, scroll: number | undefined): Promise<DocxEditor> {
  const e = editing!;
  editor?.dispose();
  editor = null;
  e.opening = parts;
  held ??= "";
  let opened: DocxEditor;
  try {
    opened = await openView(e, parts, caret, scroll);
  } catch (err) {
    held = null;
    throw err;
  } finally {
    e.opening = null;
  }
  e.base = parts;
  editor = opened;
  // What was typed meanwhile goes where the caret is now
  const typed = held;
  held = null;
  if (typed) {
    opened.focus();
    document.execCommand("insertText", false, typed);
  }
  // The host's lists follow the part open: its styles, the headers and footers, and which one is open
  e.story = opened.story;
  reply({ type: "styles", styles: opened.styles, fonts: opened.fonts });
  reply({ type: "stories", stories: opened.stories, story: opened.story, canAdd: opened.canAdd, layout: opened.layout });
  return opened;
}

function openView(e: NonNullable<typeof editing>, parts: Parts, caret: Caret | null | undefined, scroll: number | undefined): Promise<DocxEditor> {
  return openDocxEditor(e.pkg, root, {
    parts,
    texts: e.texts,
    story: e.story,
    caret,
    scroll,
    onInput: () => {
      e.typed = true;
      reply({ type: "input" });
      window.clearTimeout(changedTimer);
      changedTimer = window.setTimeout(sendChanged, CHANGED_DELAY);
    },
    onSave: () => reply({ type: "save" }),
    // Ctrl+Z right after a change of the document's shape undoes it, also one still waiting to be made: the undo waits
    // for it, and takes what is latest then
    onUndo: () => {
      if (pendingActions === 0 && (e.typed || !e.undo.length)) return false;
      act(async () => {
        const last = e.undo.pop();
        if (!last) return;
        e.story = last.story;
        await reopen(last.parts, last.caret);
      });
      return true;
    },
    onPlace: (place) => reply({ type: "place", ...place }),
    // Tab or Shift+Tab in a list item: made like the toolbar's level buttons
    onList: (f) => act(() => changeList(f)),
    // Dragging a column's border: made like the menus' changes, after what may already be waiting
    onTable: (op, change) => act(() => changeTable(op, change)),
  });
}

/**
 * Runs an editing action after what is already waiting. Should it fail, the host is told so (the editor stays), and a
 * view it had closed is opened again on the document as it was before it
 */
function act(job: () => Promise<void>) {
  pendingActions++;
  queue = queue
    .then(job)
    .catch(async (err: unknown) => {
      console.warn("docx edit", err);
      reply({ type: "op-error", message: err instanceof Error ? err.message : String(err) });
      if (editing && !editor) {
        try {
          await openEditor(editing.base, null, root.scrollTop);
        } catch (again) {
          reply({ type: "error", message: again instanceof Error ? again.message : String(again) });
        }
      }
    })
    .finally(() => {
      pendingActions--;
    });
}

/** The view opened again on a changed document, and the host told */
async function reopen(parts: Parts, caret: Caret | null) {
  const scroll = root.scrollTop;
  await openEditor(parts, caret, scroll);
  editing!.typed = false;
  reply({ type: "input" });
  sendChanged();
}

async function edit({ buffer, parts: given, texts, caret, scroll }: EditMessage) {
  const parts = partsOf(given);
  dispose?.();
  dispose = null;
  editor = null;
  root.replaceChildren();
  const pkg = await OoxmlPackage.open(buffer);
  editing = { pkg, texts, base: parts, undo: [], typed: false, story: null, opening: null };
  try {
    await openEditor(parts, caret, scroll);
    dispose = () => {
      editor?.dispose();
      pkg.dispose();
      editor = null;
      editing = null;
    };
  } catch (e) {
    pkg.dispose();
    editing = null;
    throw e;
  }
}

/** A table change asked for by the host's menus: only the values each change takes, checked */
function tableOp({ op, cols, rows, border, color }: TableMessage) {
  const change: TableChange = {};
  if (op === "split") Object.assign(change, { cols: Number(cols) || 1, rows: Number(rows) || 1 });
  if (op === "border") change.border = BORDER_KINDS.has(String(border)) ? (border as TableChange["border"]) : undefined;
  if (op === "shade") change.color = typeof color === "string" ? color : null;
  return changeTable(op, change);
}

async function changeTable(op: TableOp, change: TableChange) {
  if (!editor || !editing) return;
  const before = edited();
  let changed: ReturnType<DocxEditor["tableOp"]>;
  try {
    changed = editor.tableOp(op, change);
  } catch (err) {
    console.warn("docx table", err);
    changed = undefined;
  }
  if (!changed) {
    reply({ type: "table-refused" });
    return;
  }
  editing.undo.push({ parts: before.parts, caret: before.caret, story: editing.story });
  await reopen({ ...before.parts, [editor.path]: changed.xml }, changed.cell !== null ? { block: -1, offset: 0, cell: changed.cell, pick: changed.pick } : before.caret);
}

/** Formatting from the host's toolbar: the view's own commands right away; size, style and alignment by opening the view again */
function format(msg: FormatMessage) {
  if (!editor || !editing) return;
  if (msg.cmd !== undefined) {
    if (!FORMAT_COMMANDS.has(msg.cmd)) return;
    const value = msg.value !== undefined && COLOR.test(msg.value) ? msg.value : undefined;
    if ((msg.cmd === "foreColor" || msg.cmd === "hiliteColor") && !value) return;
    editor.format(msg.cmd as FormatCommand, value);
    return;
  }
  if (msg.list === "bullet" || msg.list === "number" || msg.list === "none") {
    const list = msg.list;
    act(() => changeList({ list }));
    return;
  }
  if (msg.field === "PAGE" || msg.field === "NUMPAGES") {
    const kind = msg.field;
    act(() => addField(kind));
    return;
  }
  if (msg.level === 1 || msg.level === -1) {
    const level = msg.level;
    act(() => changeList({ level }));
    return;
  }
  let f: RangeFormat | null = null;
  if (typeof msg.size === "number" && msg.size >= 1 && msg.size <= 1638) f = { size: msg.size };
  else if (typeof msg.style === "string" && editor.styles.some((s) => s.id === msg.style)) f = { style: msg.style, isDefault: !!msg.isDefault };
  else if (typeof msg.align === "string" && ALIGNMENTS.has(msg.align)) f = { align: msg.align as "left" | "center" | "right" | "both" };
  else if (typeof msg.font === "string" && FONT_NAME.test(msg.font)) f = { font: msg.font, eastAsia: !!msg.eastAsia };
  else if (typeof msg.line === "number" && Number.isInteger(msg.line) && msg.line >= 60 && msg.line <= 2400) f = { line: msg.line };
  else if (typeof msg.spaceBefore === "boolean") f = { spaceBefore: msg.spaceBefore };
  else if (typeof msg.spaceAfter === "boolean") f = { spaceAfter: msg.spaceAfter };
  if (!f) return;
  const range = f;
  act(async () => {
    if (!editor || !editing) return;
    const before = edited();
    const changed = editor.formatRange(range);
    if (!changed) {
      reply({ type: "format-refused" });
      return;
    }
    editing.undo.push({ parts: before.parts, caret: before.caret, story: editing.story });
    await reopen({ ...before.parts, [editor.path]: changed.xml }, changed.caret);
  });
}

/** A list change (bulleted, numbered, a level in or out): the parts it changes, and the view opened again on them */
async function changeList(f: ListFormat) {
  if (!editor || !editing) return;
  const before = edited();
  const changed = await editor.formatList(f);
  if (!changed) {
    reply({ type: "format-refused" });
    return;
  }
  editing.undo.push({ parts: before.parts, caret: before.caret, story: editing.story });
  await reopen({ ...before.parts, ...changed.parts }, changed.caret);
}

/** A field inserted at the caret, and the view opened again with the caret just after it */
async function addField(kind: FieldKind) {
  if (!editor || !editing) return;
  const before = edited();
  const changed = editor.insertField(kind);
  if (!changed) {
    reply({ type: "field-refused" });
    return;
  }
  editing.undo.push({ parts: before.parts, caret: before.caret, story: editing.story });
  await reopen({ ...before.parts, [editor.path]: changed.xml }, changed.caret);
}

/**
 * Another part in the view: a header or footer the document has, or the body (the edits so far are kept, and Ctrl+Z
 * right after no longer undoes a change made in the other one); or a header or footer added to the first section
 */
async function changeStory(msg: StoryMessage) {
  if (!editor || !editing) return;
  const before = edited();
  if (msg.add === "header" || msg.add === "footer") {
    const added = await editor.addStory(msg.add);
    if (!added) {
      reply({ type: "story-refused" });
      return;
    }
    editing.undo.push({ parts: before.parts, caret: before.caret, story: editing.story });
    editing.story = added.path;
    await reopen({ ...before.parts, ...added.parts }, null);
    return;
  }
  if (typeof msg.firstPage === "boolean" || typeof msg.evenOdd === "boolean") {
    const changed = await editor.setLayout(typeof msg.firstPage === "boolean" ? { firstPage: msg.firstPage } : { evenOdd: !!msg.evenOdd });
    if (!changed) {
      reply({ type: "story-refused" });
      return;
    }
    editing.undo.push({ parts: before.parts, caret: before.caret, story: editing.story });
    // A first-page or even-page one made opens; one turned off isn't listed any more, and the body opens instead
    if (changed.open) editing.story = changed.open;
    await reopen({ ...before.parts, ...changed.parts }, null);
    return;
  }
  const path = msg.path ?? null;
  if (path !== null && !editor.stories.some((x) => x.path === path)) return;
  if (path === editing.story) return;
  editing.undo = [];
  editing.story = path;
  const scroll = root.scrollTop;
  await openEditor(before.parts, null, path ? 0 : scroll);
  editing.typed = false;
}

function collect({ id }: CollectMessage) {
  if (editing && !editor && editing.opening) {
    // The view is being opened again (after a change of the document's shape): the parts it opens on are the edits
    reply({ type: "collected", id, parts: editing.opening, caret: null, scroll: 0 });
    return;
  }
  try {
    reply({ type: "collected", id, ...forHost() });
  } catch (err) {
    reply({ type: "collect-error", id, message: err instanceof Error ? err.message : String(err) });
  }
}

// ── Excel drawing layer ──
let xlsx: { pkg: OoxmlPackage; theme: Theme | null } | null = null;
let layer: HTMLElement | null = null;
/** Bumped for every drawings request: a request that was overtaken by a newer one is skipped or cut short */
let generation = 0;
/**
 * The sheet is split like the grid: frozen rows/columns don't scroll along that axis, and each pane clips its objects
 * at the freeze line. `inner` keeps sheet coordinates (offset back by the pane's origin) and carries the scroll transform.
 */
type Pane = { inner: HTMLElement; fixX: boolean; fixY: boolean };
let panes: Pane[] = [];
let scrollAt = { x: 0, y: 0 };

function applyScroll() {
  for (const p of panes) p.inner.style.transform = `translate(${p.fixX ? 0 : -scrollAt.x}px,${p.fixY ? 0 : -scrollAt.y}px)`;
}

/** The four panes (or one, without frozen rows/columns), given the frozen area's size in px */
function makePanes(layer: HTMLElement, fw: number, fh: number): Pane[] {
  const out: Pane[] = [];
  const add = (fixX: boolean, fixY: boolean) => {
    const left = fixX ? 0 : fw;
    const top = fixY ? 0 : fh;
    const outer = h("div", {
      style: `position:absolute;overflow:hidden;left:${left}px;top:${top}px;${fixX ? `width:${fw}px` : "right:0"};${fixY ? `height:${fh}px` : "bottom:0"}`,
    });
    const inner = h("div", { style: `position:absolute;left:${-left}px;top:${-top}px;width:0;height:0;overflow:visible` });
    outer.append(inner);
    layer.append(outer);
    out.push({ inner, fixX, fixY });
  };
  if (fw > 0 && fh > 0) add(true, true);
  if (fh > 0) add(false, true);
  if (fw > 0) add(true, false);
  add(false, false);
  return out;
}

async function loadXlsx({ buffer }: LoadXlsxMessage) {
  dispose?.();
  dispose = null;
  root.replaceChildren();
  const pkg = await OoxmlPackage.open(buffer);
  const themeRel = await pkg.relOfType("xl/workbook.xml", "/theme");
  const theme = parseTheme(themeRel ? await pkg.xml(themeRel.target) : null);
  xlsx = { pkg, theme };
  dispose = () => {
    pkg.dispose();
    xlsx = null;
  };
}

/** Objects of one worksheet at the given column/row start positions (px); charts are slow, so they're added one by one */
async function renderDrawings({ sheet, cols, rows, frozen }: DrawingsMessage, gen: number) {
  // Overtaken while waiting in the queue (the person switched sheets again): nothing to do
  if (gen !== generation) return;
  if (!xlsx) throw new Error("workbook not loaded");
  const { pkg, theme } = xlsx;
  const at = (list: Float64Array, i: number) => list[Math.max(0, Math.min(i, list.length - 1))];
  const frozenCols = frozen?.cols ?? 0;
  const frozenRows = frozen?.rows ?? 0;
  layer = h("div", { style: "position:absolute;inset:0;overflow:hidden" });
  panes = makePanes(layer, frozenCols > 0 ? at(cols, frozenCols) : 0, frozenRows > 0 ? at(rows, frozenRows) : 0);
  applyScroll();
  root.replaceChildren(layer);
  for (const item of await readDrawings(pkg, sheet)) {
    if (gen !== generation || xlsx?.pkg !== pkg) return;
    const box = anchorBox(
      item.anchor,
      (c) => at(cols, c),
      (r) => at(rows, r),
    );
    const el = await renderItem(item, box, { pkg, theme, colors: { theme } }).catch(() => null);
    // A newer render or a new workbook replaced this one meanwhile
    if (gen !== generation || xlsx?.pkg !== pkg) return;
    if (!el) continue;
    const from = item.anchor.from;
    const fixX = !!from && from.col < frozenCols;
    const fixY = !!from && from.row < frozenRows;
    // Panes already carry the scroll transform, so an object is in the right place the moment it's added
    panes.find((p) => p.fixX === fixX && p.fixY === fixY)?.inner.append(el);
  }
}

/** Messages are handled one after another: a render for a sheet always runs after the workbook it refers to has loaded */
let queue: Promise<void> = Promise.resolve();

window.addEventListener("message", (e: MessageEvent) => {
  // Only accept the parent window that embeds this page
  if (e.source !== window.parent) return;
  const msg = e.data as Message;
  if (msg?.type === "scroll") {
    if (typeof msg.x === "number" && typeof msg.y === "number") {
      scrollAt = { x: msg.x, y: msg.y };
      applyScroll();
    }
    return;
  }
  if (msg?.type === "format") {
    format(msg);
    return;
  }
  if (msg?.type === "collect" && typeof msg.id === "number") {
    // Answered right away (not queued behind a render): the host waits for it to save
    collect(msg);
    return;
  }
  let job: () => Promise<void>;
  if (msg?.type === "edit" && msg.kind === "docx" && msg.buffer instanceof ArrayBuffer && msg.texts && typeof msg.texts === "object") job = () => edit(msg);
  else if (msg?.type === "table" && TABLE_OPS.has(msg.op)) {
    act(() => tableOp(msg));
    return;
  } else if (msg?.type === "story") {
    act(() => changeStory(msg));
    return;
  } else if (msg?.type === "render" && (msg.kind === "docx" || msg.kind === "pptx") && msg.buffer instanceof ArrayBuffer) job = () => render(msg);
  else if (msg?.type === "load" && msg.kind === "xlsx" && msg.buffer instanceof ArrayBuffer) job = () => loadXlsx(msg);
  else if (msg?.type === "render" && msg.kind === "xlsx-drawings" && typeof msg.sheet === "string" && msg.cols instanceof Float64Array && msg.rows instanceof Float64Array) {
    const gen = ++generation;
    job = () => renderDrawings(msg, gen);
  } else return;
  queue = queue.then(job).then(
    () => reply({ type: "done" }),
    (err: unknown) => reply({ type: "error", message: err instanceof Error ? err.message : String(err) }),
  );
});

reply({ type: "ready" });
