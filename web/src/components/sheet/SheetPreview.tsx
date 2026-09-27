import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import JSZip from "jszip";
import { Loader2Icon } from "lucide-react";
import { readXlsx } from "@/lib/sheet/xlsx";
import { Axis, MAX_COLS, MAX_ROWS, colName, key, type Workbook } from "@/lib/sheet/model";
import { Calculator, type Value } from "@/lib/sheet/formula";
import { OoxmlPackage } from "@/lib/office/ooxml";
import { parseTheme, type Theme } from "@/lib/office/theme";
import { readDrawings, type DrawingItem } from "@/lib/office/xlsx/anchors";
import { frameDocument, loadFrameScript } from "@/components/officeFrame";
import { computeConditional, readDxfs, type CellDecoration, type Dxf } from "@/lib/sheet/conditional";
import { t, tc } from "@/lib/i18n";
import { cn } from "@/lib/utils";
import { HEADER_H, HEADER_W, cellText, draw, visibleCells, type View } from "./renderer";

interface Loaded {
  book: Workbook;
  pkg: OoxmlPackage;
  theme: Theme | null;
  /** Differential formats for conditional formatting, and custom indexed colors */
  dxfs: Dxf[];
  palette?: string[];
  /** What the drawing frame needs, as a small package; null when no sheet has pictures, charts or shapes */
  drawingParts: ArrayBuffer | null;
}

/** Cell data, formulas, macros and the like: large, and not needed to draw pictures, charts and shapes */
const CELL_DATA = /^xl\/(worksheets\/[^/]+\.xml|sharedStrings\.xml|calcChain\.xml|vbaProject\.bin|pivotCache\/|externalLinks\/)/;

/**
 * The workbook without its cell data, for the drawing frame. Entries are copied as they are, still compressed, so this
 * neither unzips nor compresses them again; null when there is nothing to draw, so the frame isn't loaded at all.
 */
async function drawingParts(zip: JSZip): Promise<ArrayBuffer | null> {
  const paths = Object.keys(zip.files);
  if (!paths.some((p) => /^xl\/drawings\/[^/]+\.xml$/.test(p))) return null;
  const parts = new JSZip();
  for (const p of paths) if (!zip.files[p].dir && !CELL_DATA.test(p)) parts.files[p] = zip.files[p];
  return parts.generateAsync({ type: "arraybuffer", compression: "DEFLATE" });
}

/**
 * Read-only Excel preview: same canvas rendering as the online editor (formats, borders, merged cells, frozen panes),
 * showing cached values from the file (formulas aren't recalculated), with images, charts and shapes layered above the cells.
 */
export default function SheetPreview({ buffer, onError }: { buffer: ArrayBuffer; onError(message: string): void }) {
  const [data, setData] = useState<Loaded | null>(null);
  const [sheetIdx, setSheetIdx] = useState(0);
  const drawingFrame = useDrawingFrame(data?.drawingParts ?? null);

  useEffect(() => {
    let cancelled = false;
    let pkg: OoxmlPackage | null = null;
    setData(null);
    (async () => {
      const { zip, book } = await readXlsx(buffer);
      pkg = OoxmlPackage.fromZip(zip);
      if (cancelled) return pkg.dispose();
      const themeRel = await pkg.relOfType("xl/workbook.xml", "/theme");
      const theme = parseTheme(themeRel ? await pkg.xml(themeRel.target) : null);
      const styles = await pkg.xml("xl/styles.xml");
      const custom = Array.from(styles?.getElementsByTagNameNS("*", "rgbColor") ?? []).map((c) => (c.getAttribute("rgb") ?? "").slice(-6));
      const palette = custom.length ? custom : undefined;
      const parts = await drawingParts(zip);
      if (cancelled) return pkg.dispose();
      setSheetIdx(book.active ?? 0);
      setData({ book, pkg, theme, dxfs: readDxfs(styles, theme, palette), palette, drawingParts: parts });
    })().catch((e) => !cancelled && onError(e instanceof Error ? e.message : t("Couldn't open this spreadsheet")));
    return () => {
      cancelled = true;
      pkg?.dispose();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [buffer]);

  if (!data)
    return (
      <div className="flex size-full items-center justify-center text-muted-foreground">
        <Loader2Icon className="size-6 animate-spin" />
      </div>
    );
  // Hidden sheets aren't shown (unless all are hidden)
  const visible = data.book.sheets.map((s, i) => ({ s, i })).filter(({ s }) => !s.hidden);
  const tabs = visible.length ? visible : data.book.sheets.map((s, i) => ({ s, i }));
  return (
    <div className="flex size-full flex-col bg-white">
      <div className="relative flex min-h-0 flex-1 flex-col">
        <Grid key={sheetIdx} data={data} sheetIdx={sheetIdx} drawingFrame={drawingFrame} />
        {/* One sandboxed frame for the whole workbook: switching sheets only re-renders the drawing layer, not the frame */}
        {drawingFrame.element}
      </div>
      {tabs.length > 1 && (
        <div className="flex h-8 shrink-0 overflow-x-auto border-t bg-muted text-xs">
          {tabs.map(({ s, i }) => (
            <button
              key={s.id}
              type="button"
              aria-current={i === sheetIdx ? "true" : undefined}
              onClick={() => setSheetIdx(i)}
              className={cn(
                "relative shrink-0 border-r px-3 py-1.5 whitespace-nowrap hover:bg-muted",
                i === sheetIdx ? "border-b-2 border-b-[#2563eb] bg-background font-medium text-[#2563eb]" : "text-muted-foreground",
              )}
            >
              {s.tabColor && <span className="absolute inset-x-1 top-0 h-0.5 rounded-full" style={{ background: s.tabColor }} />}
              {s.name}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * Pictures, charts and shapes are rendered by the same sandboxed frame as Word / PowerPoint previews (no same-origin rights,
 * no network), laid over the canvas, and only loaded for workbooks that have them. The host sends the drawing parts once, then the sheet and the column/row positions to draw at,
 * and the scroll offset as the user scrolls.
 */
interface DrawingFrame {
  /** Mount the frame (transparent overlay) in the given container */
  element: React.ReactElement | null;
  render(sheetPath: string, cols: Float64Array, rows: Float64Array, frozen: { rows: number; cols: number }): void;
  scroll(x: number, y: number): void;
}

function useDrawingFrame(parts: ArrayBuffer | null): DrawingFrame {
  const iframe = useRef<HTMLIFrameElement>(null);
  const [srcDoc, setSrcDoc] = useState<string | null>(null);
  const state = useRef({
    ready: false,
    loaded: false,
    pending: null as { sheet: string; cols: Float64Array; rows: Float64Array; frozen: { rows: number; cols: number } } | null,
    scroll: { x: 0, y: 0 },
  });
  const wanted = parts !== null;
  useEffect(() => {
    if (!wanted) return;
    loadFrameScript().then(
      (js) => setSrcDoc(frameDocument(js, true)),
      () => {},
    );
  }, [wanted]);
  // "*": the sandboxed frame has an opaque origin, which no target origin can name. Only that frame receives it
  // (its contentWindow), and it only accepts messages from this page (window.parent)
  const post = useCallback((msg: object, transfer?: Transferable[]) => {
    iframe.current?.contentWindow?.postMessage(msg, "*", transfer ?? []);
  }, []);
  useEffect(() => {
    const st = state.current;
    st.ready = false;
    st.loaded = false;
    const onMessage = (e: MessageEvent) => {
      if (e.source !== iframe.current?.contentWindow) return;
      const msg = e.data as { type?: string };
      if (msg.type === "ready") {
        st.ready = true;
        // Transferred, so it doesn't stay in this page's memory
        if (parts?.byteLength) post({ type: "load", kind: "xlsx", buffer: parts }, [parts]);
      } else if (msg.type === "done" && !st.loaded) {
        st.loaded = true;
        if (st.pending) {
          post({ type: "render", kind: "xlsx-drawings", ...st.pending });
          post({ type: "scroll", ...st.scroll });
        }
      } else if (msg.type === "error") {
        // Cells still show; only the shapes/charts of this sheet are missing
        console.warn("Drawing layer:", (msg as { message?: string }).message);
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [parts, srcDoc, post]);
  return useMemo(
    () => ({
      element: srcDoc && wanted ? (
        <iframe
          ref={iframe}
          srcDoc={srcDoc}
          title=""
          aria-hidden
          // Scripts only: no same-origin rights, can't navigate this page or open windows
          sandbox="allow-scripts"
          className="pointer-events-none absolute border-0 bg-transparent"
          style={{ left: HEADER_W, top: HEADER_H, width: `calc(100% - ${HEADER_W}px)`, height: `calc(100% - ${HEADER_H}px)` }}
        />
      ) : null,
      render(sheetPath, cols, rows, frozen) {
        state.current.pending = { sheet: sheetPath, cols, rows, frozen };
        if (state.current.loaded) {
          post({ type: "render", kind: "xlsx-drawings", sheet: sheetPath, cols, rows, frozen });
          post({ type: "scroll", ...state.current.scroll });
        }
      },
      scroll(x, y) {
        state.current.scroll = { x, y };
        if (state.current.loaded) post({ type: "scroll", x, y });
      },
    }),
    [srcDoc, wanted, post],
  );
}

/** Most rows and columns copied into the screen reader table (a full screen of a typical sheet fits) */
const MIRROR = { rows: 100, cols: 40 };

function Grid({ data, sheetIdx, drawingFrame }: { data: Loaded; sheetIdx: number; drawingFrame: DrawingFrame }) {
  const { book, pkg, theme, dxfs, palette } = data;
  const sheet = book.sheets[sheetIdx];
  const wrapRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const scroll = useRef({ x: 0, y: 0 });
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [drawings, setDrawings] = useState<DrawingItem[]>([]);
  const [conditional, setConditional] = useState<Map<number, CellDecoration> | null>(null);

  useEffect(() => {
    let cancelled = false;
    readDrawings(pkg, sheet.path).then(
      (items) => !cancelled && setDrawings(items),
      () => {},
    );
    // Conditional formatting: computed once from cached values (preview doesn't change data)
    pkg.xml(sheet.path).then((doc) => {
      if (cancelled || !doc) return;
      try {
        const map = computeConditional({
          book,
          sheetIndex: sheetIdx,
          sheetDoc: doc,
          dxfs,
          theme,
          palette,
          value: (r, c) => sheet.cells.get(key(r, c))?.v ?? null,
        });
        if (map.size) setConditional(map);
      } catch (e) {
        console.warn("conditional formatting failed", e);
      }
    }, (e) => console.warn("conditional formatting failed", e));
    return () => {
      cancelled = true;
    };
  }, [pkg, sheet, sheetIdx, book, dxfs, theme, palette]);

  // Extent: data and drawing objects plus some extra blank space (like Excel, you can scroll past the data)
  let lastRow = sheet.maxRow;
  let lastCol = sheet.maxCol;
  for (const d of drawings) {
    lastRow = Math.max(lastRow, d.anchor.to?.row ?? d.anchor.from?.row ?? 0);
    lastCol = Math.max(lastCol, d.anchor.to?.col ?? d.anchor.from?.col ?? 0);
  }
  const rows = useMemo(
    () => new Axis(Math.min(MAX_ROWS, Math.max(lastRow + 30, 60)), (i) => sheet.rowHeights.get(i) ?? sheet.defaultRowHeight),
    [sheet, lastRow],
  );
  const cols = useMemo(
    () => new Axis(Math.min(MAX_COLS, Math.max(lastCol + 6, 20)), (i) => sheet.colWidths.get(i) ?? sheet.defaultColWidth),
    [sheet, lastCol],
  );
  // The preview shows cached values from the file (same as Excel's last calculation);
  // only formulas without a cached value (e.g. files generated by code and never opened in Excel) are computed here
  const values = useMemo(() => {
    let calc: Calculator | null = null;
    return {
      value: (si: number, r: number, c: number): Value => {
        const cell = book.sheets[si]?.cells.get(key(r, c));
        if (!cell) return null;
        if (cell.f && (cell.v === null || cell.v === "")) return (calc ??= new Calculator(book)).value(si, r, c);
        return cell.v;
      },
    };
  }, [book]);

  useLayoutEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const frame = useRef(0);
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
      const v: View = { width: size.w, height: size.h, scrollX: scroll.current.x, scrollY: scroll.current.y, rows, cols, frozen: sheet.frozen };
      draw(ctx, v, {
        sheet,
        sheetIndex: sheetIdx,
        styles: book.styles,
        calc: values,
        selection: null,
        active: [0, 0],
        editing: false,
        clip: null,
        decorate: conditional ? (r, c) => conditional.get(key(r, c)) : undefined,
      });
      // Drawing objects scroll along
      drawingFrame.scroll(scroll.current.x, scroll.current.y);
    });
  }, [size, rows, cols, sheet, sheetIdx, book, values, conditional, drawingFrame]);
  useEffect(() => redraw());
  useEffect(() => () => cancelAnimationFrame(frame.current), []);

  // Screen readers can't read the canvas: the cells on screen are copied into an off-screen table, updated once scrolling stops
  const descId = useId();
  const [mirrorAt, setMirrorAt] = useState({ x: 0, y: 0 });
  const mirrorTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(mirrorTimer.current), []);
  const mirror = useMemo(() => {
    if (!size.w) return null;
    const shown = visibleCells({ width: size.w, height: size.h, scrollX: mirrorAt.x, scrollY: mirrorAt.y, rows, cols, frozen: sheet.frozen }, MIRROR);
    const text = { sheet, sheetIndex: sheetIdx, styles: book.styles, calc: values };
    // Empty rows, and empty columns after the last filled one, are left out; the headers keep the row numbers and column letters
    const lines = shown.rows.map((r) => ({ r, cells: shown.cols.map((c) => cellText(text, r, c)) })).filter((l) => l.cells.some(Boolean));
    const width = Math.max(0, ...lines.map((l) => l.cells.findLastIndex(Boolean) + 1));
    return { cols: shown.cols.slice(0, width), lines: lines.map((l) => ({ r: l.r, cells: l.cells.slice(0, width) })) };
  }, [size, mirrorAt, rows, cols, sheet, sheetIdx, book, values]);

  // Drawing objects are rendered by the sandboxed frame at these column/row positions
  useEffect(() => {
    const frozen = { rows: sheet.frozen?.rows ?? 0, cols: sheet.frozen?.cols ?? 0 };
    if (drawings.length) drawingFrame.render(sheet.path, cols.offsets(), rows.offsets(), frozen);
    else drawingFrame.render(sheet.path, new Float64Array(0), new Float64Array(0), frozen);
  }, [drawings, rows, cols, sheet.path, sheet.frozen, drawingFrame]);

  return (
    <div ref={wrapRef} className="relative min-h-0 flex-1 overflow-hidden">
      <canvas ref={canvasRef} aria-hidden className="absolute inset-0" style={{ width: size.w, height: size.h }} />
      <div
        role="region"
        aria-label={tc("sheet", "Sheet {name}", { name: sheet.name })}
        aria-describedby={descId}
        tabIndex={0}
        className="absolute inset-0 overflow-auto outline-none focus-visible:ring-2 focus-visible:ring-ring/60 focus-visible:ring-inset"
        onScroll={(e) => {
          scroll.current = { x: e.currentTarget.scrollLeft, y: e.currentTarget.scrollTop };
          redraw();
          clearTimeout(mirrorTimer.current);
          mirrorTimer.current = setTimeout(() => setMirrorAt({ ...scroll.current }), 200);
        }}
      >
        <div style={{ width: cols.total + HEADER_W + 40, height: rows.total + HEADER_H + 40 }} />
      </div>
      <p id={descId} className="sr-only">
        {t("The cells on screen are listed in the table that follows. Scroll with the arrow keys to show other cells.")}
      </p>
      {mirror && (
        <table className="sr-only">
          <caption>{tc("sheet", "Cells on screen in {name}", { name: sheet.name })}</caption>
          {mirror.lines.length > 0 ? (
            <>
              <thead>
                <tr>
                  <td />
                  {mirror.cols.map((c) => (
                    <th key={c} scope="col">
                      {colName(c)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {mirror.lines.map(({ r, cells }) => (
                  <tr key={r}>
                    <th scope="row">{r + 1}</th>
                    {cells.map((text, i) => (
                      <td key={mirror.cols[i]}>{text}</td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </>
          ) : (
            <tbody>
              <tr>
                <td>{t("The cells on screen are empty.")}</td>
              </tr>
            </tbody>
          )}
        </table>
      )}
    </div>
  );
}
