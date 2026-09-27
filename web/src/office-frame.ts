/**
 * Sandbox page for Word / PowerPoint previews.
 *
 * Documents are turned into DOM by our own renderers (lib/office/docx, lib/office/pptx), building the view only with textContent / setAttribute;
 * as an extra layer of protection, rendering still happens in an iframe without same-origin rights: even if a renderer has a bug, it can't get this site's cookies or call the API.
 *
 * This code is embedded by the parent page into the iframe's srcdoc (the iframe neither needs nor is allowed any network access).
 * Messaging: the parent sends { type: "render", kind, buffer }; this replies { type: "done" } or { type: "error", message },
 * and replies { type: "link", href } when a link in the document is clicked; the parent opens it after checking it's http(s) / mailto.
 */

import { renderDocx } from "@/lib/office/docx/index";
import { renderPptx } from "@/lib/office/pptx/index";
import { OoxmlPackage, h } from "@/lib/office/ooxml";
import { parseTheme, type Theme } from "@/lib/office/theme";
import { anchorBox, readDrawings, renderItem } from "@/lib/office/xlsx/drawings";

type RenderMessage = { type: "render"; kind: "docx" | "pptx"; buffer: ArrayBuffer };
/** Excel: the workbook is loaded once, then each worksheet's drawing objects are rendered on request at the given column/row positions */
type LoadXlsxMessage = { type: "load"; kind: "xlsx"; buffer: ArrayBuffer };
type DrawingsMessage = { type: "render"; kind: "xlsx-drawings"; sheet: string; cols: Float64Array; rows: Float64Array; frozen?: { rows: number; cols: number } };
type ScrollMessage = { type: "scroll"; x: number; y: number };
type Message = RenderMessage | LoadXlsxMessage | DrawingsMessage | ScrollMessage;

const root = document.getElementById("root")!;
/**
 * "*": this frame's origin is opaque, so it can't name the parent's origin either. The message only goes to window.parent,
 * which ignores messages that don't come from this frame's window; nothing in them is secret
 */
const reply = (msg: object) => window.parent.postMessage(msg, "*");
let dispose: (() => void) | null = null;

// The iframe can't open new windows: external links are handed to the parent; in-page bookmarks scroll directly
document.addEventListener("click", (e) => {
  const a = (e.target as Element | null)?.closest?.("a[href]");
  if (!a) return;
  const href = a.getAttribute("href") ?? "";
  e.preventDefault();
  if (href.startsWith("#")) {
    document.getElementById(href.slice(1))?.scrollIntoView({ block: "start" });
    return;
  }
  if (/^(https?:|mailto:)/i.test(href)) reply({ type: "link", href });
});

async function render({ kind, buffer }: RenderMessage) {
  dispose?.();
  dispose = null;
  root.replaceChildren();
  const result = kind === "docx" ? await renderDocx(buffer, root) : await renderPptx(buffer, root);
  dispose = result.dispose;
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
    const box = anchorBox(item.anchor, (c) => at(cols, c), (r) => at(rows, r));
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
  let job: () => Promise<void>;
  if (msg?.type === "render" && (msg.kind === "docx" || msg.kind === "pptx") && msg.buffer instanceof ArrayBuffer) job = () => render(msg);
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
