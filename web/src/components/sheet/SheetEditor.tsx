import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useReducer, useRef, useState } from "react";
import { Loader2Icon, Redo2Icon, SaveIcon, Undo2Icon, XIcon } from "lucide-react";
import { toast } from "sonner";
import { ApiError, type FileSource, type Node } from "@/api";
import { Button } from "@/components/ui/button";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { ConfirmDialog } from "@/components/dialogs";
import { getDraft, setDraft } from "@/lib/drafts";
import { t, tc } from "@/lib/i18n";
import { shortcut } from "@/lib/keys";
import { cn } from "@/lib/utils";
import { Axis, MAX_COLS, MAX_ROWS, cellName, key, mergeAt, normRange, parseCellName, rangeName, type Range, type Scalar, type Sheet } from "@/ooxml/xlsx/model";
import { checkFormula } from "@/ooxml/xlsx/formula";
import { formatGeneral } from "@/ooxml/core/numfmt";
import { deriveStyle } from "@/ooxml/xlsx/ops";
import { HEADER_H, HEADER_W, cellRect, cellText, draw, fontOf, type View } from "./renderer";
import { SheetToolbar } from "./SheetToolbar";
import { SheetMenu, type MenuTarget } from "./SheetMenu";
import * as history from "./history";
import { clipStore, createClipboard } from "./clipboard";
import { createFormatting } from "./format";
import { createKeyboard } from "./keyboard";
import { useGridMouse } from "./mouse";
import type { Drag, Editing, WorkspaceCtx } from "./workspace";
import { saveSession } from "./save";
import { dropSession, editText, keepSession, openSession, parseInput, releaseIfClean, reusableSession, type Change, type Entry, type Layout, type Sel, type Session } from "./session";
import { cancellable } from "@/lib/cancellable";
import { officeErrorMessage } from "@/lib/officeErrors";

// ───────────── Component ─────────────

export default function SheetEditor(props: { node: Node; source: FileSource; onSaved?(n: Node): void; onExit(): void }) {
  const { node } = props;
  const [session, setSession] = useState<Session | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    const existing = reusableSession(node);
    if (existing) {
      setSession(existing);
      return;
    }
    setSession(null);
    setError(null);
    // Closing the editor (or moving to another file) while the workbook downloads stops the download, and a workbook
    // that arrives after that isn't kept in memory
    return cancellable(
      (signal) => openSession(node, props.source, signal),
      (s) => {
        keepSession(node.id, s);
        setSession(s);
      },
      (e) => setError(officeErrorMessage(e, t("Couldn't open this spreadsheet"))),
    );
    // Load only when the file changes or a reload is requested; updated_at changing after a save doesn't require reloading
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the node object is new after every save
  }, [node.id, reload]);

  // The file got other content while it is open (an earlier version restored, say), and there are no changes here to
  // keep (none, or they were discarded): what it has now is loaded, so the next save doesn't take it for someone else's
  useEffect(() => {
    if (!session || session.base === node.updated_at) return;
    if (session.version !== session.saved && getDraft(node.id, "sheet")) return;
    dropSession(node.id);
    setReload((x) => x + 1);
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- when the file's version changes
  }, [node.updated_at]);

  if (error)
    return (
      <div className="flex size-full flex-col items-center justify-center gap-3 p-6 text-center text-sm text-destructive">
        {error}
        <Button variant="outline" size="sm" onClick={props.onExit}>
          {t("Back to preview")}
        </Button>
      </div>
    );
  if (!session)
    return (
      <div className="flex size-full items-center justify-center text-muted-foreground">
        <Loader2Icon className="size-6 animate-spin" />
      </div>
    );
  return (
    <Workspace
      key={`${node.id}-${reload}`}
      node={node}
      session={session}
      onSaved={props.onSaved}
      onExit={props.onExit}
      onReload={() => {
        dropSession(node.id);
        setDraft(node.id, null);
        setReload((x) => x + 1);
      }}
    />
  );
}

function Workspace({ node, session, onSaved, onExit, onReload }: { node: Node; session: Session; onSaved?(n: Node): void; onExit(): void; onReload(): void }) {
  const { book, calc } = session;
  const [, tick] = useReducer((x: number) => x + 1, 0);
  const [sheetIdx, setSheetIdx] = useState(session.sheet);
  const [sel, setSelState] = useState<Sel>(session.sel);
  const [editing, setEditing] = useState<Editing | null>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [saving, setSaving] = useState(false);
  /** Saving in progress (live value; Ctrl+S may be pressed again before the UI updates) */
  const savingRef = useRef(false);
  const [confirmExit, setConfirmExit] = useState(false);
  const [clip, setClip] = useState<Range | null>(clipStore.current?.sheet === session.sheet ? clipStore.current.range : null);
  const [menuTarget, setMenuTarget] = useState<MenuTarget>("cell");
  const [cursor, setCursor] = useState<string>("cell");
  // Update the layout live while dragging column widths/row heights
  const [layoutVer, bumpLayout] = useReducer((x: number) => x + 1, 0);

  const sheet = book.sheets[sheetIdx];
  const wrapRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const barRef = useRef<HTMLInputElement>(null);
  const scroll = useRef({ x: 0, y: 0 });
  const drag = useRef<Drag | null>(null);
  const refAnchor = useRef<[number, number]>([0, 0]);

  const dirty = session.version !== session.saved;
  const range = normRange({ r1: sel.anchor[0], c1: sel.anchor[1], r2: sel.focus[0], c2: sel.focus[1] });
  const active = sel.anchor;
  const styleAt = (r: number, c: number, s: Sheet = sheet) => {
    const idx = s.cells.get(key(r, c))?.s;
    return idx !== undefined ? book.styles[idx] : undefined;
  };
  const wholeRows = range.c1 === 0 && range.c2 >= MAX_COLS - 1;
  const wholeCols = range.r1 === 0 && range.r2 >= MAX_ROWS - 1;

  useEffect(() => {
    session.sel = sel;
    session.sheet = sheetIdx;
  }, [sel, sheetIdx, session]);

  // On unmount, release the session if it has no unsaved changes (keep it otherwise, to continue editing when switching back to the tab)
  useEffect(() => () => releaseIfClean(node.id, session), [node.id, session]);

  // Mark the tab "unsaved" and warn before closing the browser
  useEffect(() => {
    setDraft(node.id, dirty ? { kind: "sheet", base: session.base } : null);
  }, [dirty, node.id, session.base]);

  // ───── Layout ─────

  const rowsCount = Math.min(MAX_ROWS, Math.max(sheet.maxRow + 80, 200, (wholeCols ? range.r1 : range.r2) + 40));
  const colsCount = Math.min(MAX_COLS, Math.max(sheet.maxCol + 10, 26, (wholeRows ? range.c1 : range.c2) + 8));
  const rows = useMemo(
    () => new Axis(rowsCount, (i) => sheet.rowHeights.get(i) ?? sheet.defaultRowHeight),
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the sheet is changed in place: session.version and layoutVer say when its row heights changed
    [sheet, rowsCount, session.version, layoutVer],
  );
  const cols = useMemo(
    () => new Axis(colsCount, (i) => sheet.colWidths.get(i) ?? sheet.defaultColWidth),
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- as above, for column widths
    [sheet, colsCount, session.version, layoutVer],
  );
  const view = (): View => ({ width: size.w, height: size.h, scrollX: scroll.current.x, scrollY: scroll.current.y, rows, cols });

  useLayoutEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // ───── Drawing ─────

  const frame = useRef(0);
  const stateRef = useRef({ range, active, editing, clip, sheetIdx });
  stateRef.current = { range, active, editing, clip, sheetIdx };
  const redraw = useCallback(() => {
    cancelAnimationFrame(frame.current);
    frame.current = requestAnimationFrame(() => {
      const canvas = canvasRef.current;
      if (!canvas || !size.w) return;
      const dpr = window.devicePixelRatio || 1;
      if (canvas.width !== Math.round(size.w * dpr) || canvas.height !== Math.round(size.h * dpr)) {
        canvas.width = Math.round(size.w * dpr);
        canvas.height = Math.round(size.h * dpr);
      }
      const ctx = canvas.getContext("2d")!;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      const st = stateRef.current;
      draw(ctx, view(), {
        sheet: book.sheets[st.sheetIdx],
        sheetIndex: st.sheetIdx,
        styles: book.styles,
        calc,
        selection: st.range,
        active: st.active,
        editing: !!st.editing,
        clip: st.clip,
      });
    });
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- draws from stateRef and the workbook, which change in place; these say when the geometry changed
  }, [size, rows, cols, sheet]);
  useEffect(() => redraw());

  // ───── Scrolling and selection ─────

  const ensureVisible = (r: number, c: number) => {
    const el = scrollRef.current;
    if (!el) return;
    const m = mergeAt(sheet, r, c);
    const x1 = cols.offset(m ? m.c1 : c);
    const x2 = cols.offset((m ? m.c2 : c) + 1);
    const y1 = rows.offset(m ? m.r1 : r);
    const y2 = rows.offset((m ? m.r2 : r) + 1);
    const vw = el.clientWidth - HEADER_W;
    const vh = el.clientHeight - HEADER_H;
    if (c < MAX_COLS - 1) {
      if (x1 < el.scrollLeft) el.scrollLeft = x1;
      else if (x2 > el.scrollLeft + vw) el.scrollLeft = Math.min(x1, x2 - vw);
    }
    if (r < MAX_ROWS - 1) {
      if (y1 < el.scrollTop) el.scrollTop = y1;
      else if (y2 > el.scrollTop + vh) el.scrollTop = Math.min(y1, y2 - vh);
    }
  };

  const setSel = (next: Sel, reveal = true) => {
    const clamp = ([r, c]: [number, number]): [number, number] => [Math.max(0, Math.min(MAX_ROWS - 1, r)), Math.max(0, Math.min(MAX_COLS - 1, c))];
    const s = { anchor: clamp(next.anchor), focus: clamp(next.focus) };
    setSelState(s);
    if (reveal) requestAnimationFrame(() => ensureVisible(s.focus[0], s.focus[1]));
  };

  const focusGrid = () => inputRef.current?.focus({ preventScroll: true });
  useEffect(() => {
    focusGrid();
  }, []);

  // ───── Changes and undo ─────

  const commitChanges = (changes: Change[], layout?: Entry["layout"]) => {
    if (history.commit(session, changes, sheetIdx, sel, layout)) tick();
  };

  const changeAt = (s: number, r: number, c: number, content: { v: Scalar; f?: string } | null, style?: number | null) => history.changeAt(book, s, r, c, content, style);

  const applyLayout = (sheetI: number, l: Layout) => history.applyLayout(book, sheetI, l);

  const pushEntry = (e: Entry) => {
    history.pushEntry(session, e);
    tick();
  };

  const showEntry = (e: Entry | undefined) => {
    if (!e) return;
    setSheetIdx(e.sheet);
    setSel(e.sel);
    tick();
  };
  const undo = () => showEntry(history.undo(session));
  const redo = () => showEntry(history.redo(session));

  // ───── Editing ─────

  const startEdit = (text: string, mode: Editing["mode"], from: Editing["from"] = "cell") => {
    setEditing({ text, mode, from });
    const el = inputRef.current;
    if (el) {
      el.value = text;
      if (from === "cell") {
        el.focus({ preventScroll: true });
        el.setSelectionRange(text.length, text.length);
      }
    }
    ensureVisible(active[0], active[1]);
  };

  const endEdit = () => {
    setEditing(null);
    if (inputRef.current) inputRef.current.value = "";
    focusGrid();
  };

  function commitEdit(move: [number, number] | null) {
    if (!editing) return;
    const text = inputRef.current?.value ?? editing.text;
    const [r, c] = active;
    const style = styleAt(r, c);
    const parsed = parseInput(text, style);
    if (parsed.f) {
      const err = checkFormula(parsed.f);
      if (err) {
        toast.error(t("There's a problem with this formula ({error}). Check the parentheses, quotes, and references.", { error: err }));
        return;
      }
    }
    const before = sheet.cells.get(key(r, c));
    if (text !== editText(before, style)) {
      // Automatically apply the matching number format when typing dates or percentages (same as Excel)
      const s = parsed.fmt ? deriveStyle(book, before?.s, (st) => ({ ...st, numFmt: parsed.fmt })) : undefined;
      commitChanges([changeAt(sheetIdx, r, c, parsed, s === undefined ? undefined : s || null)]);
    }
    endEdit();
    if (move) {
      const m = mergeAt(sheet, r, c);
      const nr = move[0] > 0 && m ? m.r2 + 1 : r + move[0];
      const nc = move[1] > 0 && m ? m.c2 + 1 : c + move[1];
      setSel({ anchor: [nr, nc], focus: [nr, nc] });
    }
  }

  /** While typing a formula with an operator or left parenthesis before the caret, clicking a cell inserts a reference (Excel's "Point" mode) */
  const expectsRef = () => {
    const el = inputRef.current;
    if (!editing || !el) return false;
    const text = el.value;
    if (!text.startsWith("=")) return false;
    const before = text.slice(0, el.selectionStart ?? text.length).trimEnd();
    return /[=(,+\-*/^&<>:]$/.test(before);
  };

  const insertRef = (ref: string, replaceFrom?: number) => {
    const el = inputRef.current!;
    const pos = replaceFrom ?? el.selectionStart ?? el.value.length;
    const end = replaceFrom !== undefined ? (el.selectionEnd ?? el.value.length) : pos;
    el.value = el.value.slice(0, pos) + ref + el.value.slice(end);
    el.setSelectionRange(pos + ref.length, pos + ref.length);
    setEditing((e) => (e ? { ...e, text: el.value } : e));
    return pos;
  };

  // ───── Feature modules ─────

  const ctx: WorkspaceCtx = {
    node,
    session,
    book,
    calc,
    sheet,
    sheetIdx,
    sel,
    range,
    active,
    wholeRows,
    wholeCols,
    editing,
    setEditing,
    size,
    rows,
    cols,
    view,
    inputRef,
    scrollRef,
    canvasRef,
    drag,
    refAnchor,
    setSel,
    setSelState,
    setClip,
    setMenuTarget,
    cursor,
    setCursor,
    bumpLayout,
    styleAt,
    ensureVisible,
    focusGrid,
    commitChanges,
    changeAt,
    applyLayout,
    pushEntry,
    undo,
    redo,
    startEdit,
    endEdit,
    commitEdit,
    expectsRef,
    insertRef,
    save: () => save(),
  };
  const { styleRange, restyle, toggleMerge, actions, structureBlocked } = createFormatting(ctx);
  const { copyRange, pasteText, clearRange, onCopy, onCut, onPaste } = createClipboard(ctx, { styleRange });
  const { autofit, onMouseDown, onHover, onDoubleClick } = useGridMouse(ctx);
  const { onKeyDown, onInput, onCompositionStart } = createKeyboard(ctx, { restyle, clearRange });

  // ───── Saving ─────

  const save = async () => {
    // Compare versions live: Ctrl+S first commits the cell being edited, so the dirty value computed at render time is still pre-change
    if (savingRef.current || session.version === session.saved) return;
    savingRef.current = true;
    setSaving(true);
    try {
      const { node: n, cells } = await saveSession(session, node.id);
      tick();
      toast.success(cells ? t("Saved ({n} cell)|Saved ({n} cells)", { n: cells }) : t("Saved"));
      onSaved?.(n);
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        toast.error(e.message, { duration: 10000, action: { label: t("Reload (discard changes)"), onClick: onReload } });
      } else toast.error(officeErrorMessage(e, t("Couldn't save")));
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  };

  const exit = () => {
    if (editing) commitEdit(null);
    if (session.version !== session.saved) setConfirmExit(true);
    else {
      dropSession(node.id);
      onExit();
    }
  };

  // ───── View ─────

  const activeCell = sheet.cells.get(key(active[0], active[1]));
  const barText = editing ? editing.text : editText(activeCell, styleAt(active[0], active[1]));
  const editorRect = cellRect(view(), sheet, active[0], active[1]);
  const activeStyle = styleAt(active[0], active[1]);
  const activeMerged = sheet.merges.some((m) => m.r1 <= range.r2 && m.r2 >= range.r1 && m.c1 <= range.c2 && m.c2 >= range.c1);

  // Status bar: sum, average and count of the selection
  const stats = useMemo(() => {
    if (range.r1 === range.r2 && range.c1 === range.c2) return null;
    let count = 0;
    let nums = 0;
    let sum = 0;
    const r2 = Math.min(range.r2, sheet.maxRow);
    const c2 = Math.min(range.c2, sheet.maxCol);
    if ((r2 - range.r1 + 1) * (c2 - range.c1 + 1) > 200000) return null;
    for (let r = range.r1; r <= r2; r++)
      for (let c = range.c1; c <= c2; c++) {
        if (!sheet.cells.has(key(r, c))) continue;
        const v = calc.value(sheetIdx, r, c);
        if (v === null || v === "") continue;
        count++;
        if (typeof v === "number") {
          nums++;
          sum += v;
        }
      }
    return { count, nums, sum };
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the cells are changed in place: session.version says when
  }, [range.r1, range.r2, range.c1, range.c2, sheetIdx, session.version]);

  const nameBox = wholeRows && wholeCols ? t("All") : range.r1 === range.r2 && range.c1 === range.c2 ? cellName(active[0], active[1]) : rangeName(range);
  const rowCount = wholeCols ? 1 : range.r2 - range.r1 + 1;
  const colCount = wholeRows ? 1 : range.c2 - range.c1 + 1;

  // Screen readers can't read the canvas: the active cell and its value (or the selected range) are announced when they change.
  // Nothing while editing: the cell's text box is read out itself
  const descId = useId();
  const activeText = cellText({ sheet, sheetIndex: sheetIdx, styles: book.styles, calc }, active[0], active[1]);
  const announcement = editing
    ? ""
    : range.r1 !== range.r2 || range.c1 !== range.c2
      ? tc("sheet", "{range} selected", { range: nameBox })
      : activeCell?.f
        ? tc("sheet", "{cell}, {value}, formula {formula}", { cell: nameBox, value: activeText, formula: barText })
        : activeText
          ? tc("sheet", "{cell}, {value}", { cell: nameBox, value: activeText })
          : tc("sheet", "{cell}, empty", { cell: nameBox });

  return (
    <div className="flex size-full flex-col bg-background text-foreground">
      {/* Format toolbar */}
      <SheetToolbar style={activeStyle ?? {}} merged={activeMerged} actions={actions} disabledStructure={structureBlocked} />

      {/* Formula bar */}
      <div className="flex h-10 shrink-0 items-center gap-2 border-b px-2">
        <input
          aria-label={t("Name box")}
          className="h-7 w-24 shrink-0 rounded border bg-background px-2 text-xs tabular-nums outline-none focus-visible:ring-2 focus-visible:ring-ring"
          defaultValue={nameBox}
          key={nameBox}
          onKeyDown={(e) => {
            if (e.key !== "Enter") return;
            // Don't let this Enter carry over into the cell (focus moves there during keydown)
            e.preventDefault();
            const [a, b] = e.currentTarget.value.trim().split(":");
            const p1 = parseCellName(a ?? "");
            const p2 = b ? parseCellName(b) : p1;
            if (p1 && p2) {
              setSel({ anchor: p1, focus: p2 });
              focusGrid();
            } else toast.error(t("Enter a cell reference, such as B3 or A1:C5"));
          }}
        />
        <span className="text-xs font-semibold text-muted-foreground italic">fx</span>
        <input
          ref={barRef}
          aria-label={t("Formula bar")}
          className="h-7 min-w-0 flex-1 rounded border bg-background px-2 font-mono text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring"
          value={barText}
          onFocus={() => {
            if (!editing) startEdit(barText, "edit", "bar");
          }}
          onChange={(e) => {
            if (inputRef.current) inputRef.current.value = e.target.value;
            setEditing({ text: e.target.value, mode: "edit", from: "bar" });
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              commitEdit([1, 0]);
            } else if (e.key === "Escape") {
              e.preventDefault();
              endEdit();
            } else if (e.key === "Tab") {
              e.preventDefault();
              commitEdit([0, 1]);
            }
          }}
        />
        <div className="flex shrink-0 items-center gap-1">
          <Button
            variant="ghost"
            size="icon-sm"
            title={t("{action} ({keys})", { action: t("Undo"), keys: shortcut("Ctrl+Z") })}
            aria-label={t("Undo")}
            disabled={!session.undo.length}
            onClick={undo}
          >
            <Undo2Icon />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            title={t("{action} ({keys})", { action: t("Redo"), keys: shortcut("Ctrl+Y") })}
            aria-label={t("Redo")}
            disabled={!session.redo.length}
            onClick={redo}
          >
            <Redo2Icon />
          </Button>
          <Button size="sm" disabled={!dirty || saving} onClick={() => void save()}>
            {saving ? <Loader2Icon className="animate-spin" /> : <SaveIcon />}
            {t("Save")}
            <kbd className="ml-1 text-[10px] opacity-60 max-md:hidden">{shortcut("Ctrl+S")}</kbd>
          </Button>
          <Button variant="outline" size="sm" onClick={exit}>
            <XIcon /> {t("Done editing")}
          </Button>
        </div>
      </div>

      {/* Cell area */}
      <ContextMenu>
        <ContextMenuTrigger className="relative min-h-0 flex-1 overflow-hidden">
          <div ref={wrapRef} role="group" aria-label={tc("sheet", "Sheet {name}", { name: sheet.name })} className="absolute inset-0">
            <canvas ref={canvasRef} aria-hidden className="absolute inset-0" style={{ width: size.w, height: size.h }} />
            {/* Not a Tab stop of its own: the keyboard moves through the cells from the cell's text box, which scrolls this along */}
            <div
              ref={scrollRef}
              tabIndex={-1}
              className="absolute inset-0 overflow-auto outline-none"
              style={{ cursor }}
              onScroll={(e) => {
                scroll.current = { x: e.currentTarget.scrollLeft, y: e.currentTarget.scrollTop };
                redraw();
                if (editing) tick();
              }}
              onMouseDown={onMouseDown}
              onMouseMove={onHover}
              onDoubleClick={onDoubleClick}
            >
              <div style={{ width: cols.total + HEADER_W + 40, height: rows.total + HEADER_H + 40 }} />
            </div>
            <textarea
              ref={inputRef}
              aria-label={t("Cell contents")}
              aria-describedby={descId}
              spellCheck={false}
              className={cn(
                "absolute z-10 resize-none overflow-hidden border-2 border-[#2563eb] bg-white px-[2px] leading-tight text-[#1f2328] outline-none",
                !editing && "pointer-events-none opacity-0",
              )}
              style={{
                left: Math.max(HEADER_W, editorRect.x) - 1,
                top: Math.max(HEADER_H, editorRect.y) - 1,
                minWidth: editorRect.w + 1,
                width: editing
                  ? Math.min(Math.max(editorRect.w + 1, (editing.text.split("\n").reduce((m, l) => Math.max(m, l.length), 0) + 2) * 9), size.w - editorRect.x - 4)
                  : editorRect.w + 1,
                height: editing ? Math.max(editorRect.h + 1, editing.text.split("\n").length * 18 + 6) : editorRect.h + 1,
                font: fontOf(activeStyle),
                fontWeight: activeStyle?.bold ? "bold" : undefined,
                color: activeStyle?.color,
                background: activeStyle?.bg ?? "#ffffff",
                textAlign: activeStyle?.hAlign === "center" ? "center" : activeStyle?.hAlign === "right" && editing?.mode === "edit" ? "right" : "left",
              }}
              onKeyDown={onKeyDown}
              onInput={onInput}
              onCompositionStart={onCompositionStart}
              onCopy={onCopy}
              onCut={onCut}
              onPaste={onPaste}
              onBlur={(e) => {
                // Keep the editing state when moving to the formula bar; commit input when clicking elsewhere on the page
                if (editing && e.relatedTarget && e.relatedTarget !== barRef.current && !scrollRef.current?.contains(e.relatedTarget as Element)) commitEdit(null);
              }}
            />
            <p id={descId} className="sr-only">
              {t("Arrow keys move between cells and Shift with the arrow keys selects. Type to replace the cell's contents, or press F2 to edit them; Enter confirms and Esc cancels.")}
            </p>
            <div role="status" className="sr-only">
              {announcement}
            </div>
          </div>
        </ContextMenuTrigger>
        <ContextMenuContent>
          <SheetMenu
            target={menuTarget}
            rowCount={rowCount}
            colCount={colCount}
            structureBlocked={structureBlocked}
            merged={activeMerged}
            canUndo={session.undo.length > 0}
            canRedo={session.redo.length > 0}
            onCut={() => void navigator.clipboard?.writeText(copyRange(true))}
            onCopy={() => void navigator.clipboard?.writeText(copyRange(false))}
            onPaste={async () => {
              try {
                pasteText(await navigator.clipboard.readText());
              } catch {
                toast.info(t("Your browser doesn't allow reading the clipboard from the menu. Use {keys} instead.", { keys: shortcut("Ctrl+V") }));
              }
            }}
            onInsertRows={actions.insertRows}
            onInsertCols={actions.insertCols}
            onDeleteRows={actions.deleteRows}
            onDeleteCols={actions.deleteCols}
            onClear={clearRange}
            onClearFormat={actions.clearFormat}
            onMerge={toggleMerge}
            onAutofit={() => autofit(range.c1)}
            onUndo={undo}
            onRedo={redo}
          />
        </ContextMenuContent>
      </ContextMenu>

      {/* Sheet tabs and status bar */}
      <div className="flex h-8 shrink-0 items-center gap-2 border-t bg-muted/40 pr-3 text-xs">
        <div className="flex min-w-0 flex-1 overflow-x-auto">
          {book.sheets.map((s, i) => (
            <button
              key={s.id}
              type="button"
              aria-current={i === sheetIdx ? "true" : undefined}
              className={cn(
                "shrink-0 border-r px-3 py-1.5 whitespace-nowrap hover:bg-muted",
                i === sheetIdx ? "border-b-2 border-b-brand bg-background font-medium text-brand" : "text-muted-foreground",
              )}
              onClick={() => {
                if (editing) commitEdit(null);
                setSheetIdx(i);
                setSelState({ anchor: [0, 0], focus: [0, 0] });
                scrollRef.current?.scrollTo({ left: 0, top: 0 });
                setClip(clipStore.current?.sheet === i ? clipStore.current.range : null);
                focusGrid();
              }}
            >
              {s.name}
            </button>
          ))}
        </div>
        {stats && (
          <span className="flex shrink-0 gap-4 text-muted-foreground tabular-nums max-sm:hidden">
            {stats.nums > 0 && <span>{t("Average: {value}", { value: formatGeneral(stats.sum / stats.nums) })}</span>}
            <span>{t("Count: {n}", { n: stats.count })}</span>
            {stats.nums > 0 && <span>{t("Sum: {value}", { value: formatGeneral(stats.sum) })}</span>}
          </span>
        )}
        {dirty && <span className="shrink-0 text-amber-600 dark:text-amber-400">{t("Unsaved changes")}</span>}
      </div>

      {confirmExit && (
        <ConfirmDialog
          title={tc("sheet", "Discard unsaved changes?")}
          description={t("This workbook has unsaved changes. They'll be lost if you stop editing.")}
          confirmText={t("Discard and exit")}
          destructive
          onClose={() => setConfirmExit(false)}
          onConfirm={async () => {
            dropSession(node.id);
            setDraft(node.id, null);
            onExit();
          }}
        />
      )}
    </div>
  );
}
