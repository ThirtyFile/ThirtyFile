/**
 * A PDF shown with pdf.js rather than the browser's own reader, so it looks and works the same everywhere, phones
 * included: pages drawn as they scroll into view, a panel of page pictures, going to a page, zoom, finding text, and
 * Print and Download that follow what the person may do with the file. Opened from search results that looked inside
 * files, it goes to the first match and marks the words searched for
 */
import { useCallback, useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import {
  ChevronDownIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  ChevronUpIcon,
  DownloadIcon,
  Loader2Icon,
  LockIcon,
  PanelLeftIcon,
  PrinterIcon,
  SearchIcon,
  ZoomInIcon,
  ZoomOutIcon,
} from "lucide-react";
import type { PDFDocumentProxy } from "pdfjs-dist/legacy/build/pdf.mjs";
import type { EventBus, PDFViewer } from "pdfjs-dist/legacy/web/pdf_viewer.mjs";
import "pdfjs-dist/legacy/web/pdf_viewer.css";
import type { FileSource, Node } from "@/api";
import { triggerDownload } from "@/downloads";
import { Button } from "@/components/ui/button";
import { Thumbnails } from "@/components/pdf/Thumbnails";
import { printPdf } from "@/components/pdf/print";
import { UNREADABLE, documentOptions, loadViewerComponents, pdfjs } from "@/lib/pdfjs";
import { ZOOM_STEPS, pageFromInput, percent, scaleValue, searchWords, stepZoom, type Zoom } from "@/lib/pdfView";
import { useMediaQuery } from "@/lib/focus";
import { reportShown } from "@/lib/errorReport";
import { t } from "@/lib/i18n";
import { cn, errorMessage } from "@/lib/utils";

/** pdf.js's find states (web/pdf_find_controller.js) */
const FIND_NOT_FOUND = 1;
const FIND_WRAPPED = 2;

/**
 * pdf.js's own text for page landmarks and form fields is left out: the viewer labels what it shows itself, in the
 * interface's language, and pdf.js would otherwise fetch translation files of its own
 */
const NO_L10N = {
  getLanguage: () => document.documentElement.lang || "en",
  getDirection: () => "ltr",
  get: async (_ids: unknown, _args?: unknown, fallback?: string) => fallback ?? "",
  translate: async () => undefined,
  translateOnce: async () => undefined,
  pause: () => undefined,
  resume: () => undefined,
  destroy: async () => undefined,
};

type Status = { kind: "loading" } | { kind: "password"; wrong: boolean } | { kind: "error"; message: string } | { kind: "ready"; doc: PDFDocumentProxy };

/** What the find box looks for: words typed, or the words a search inside files found (any of them) */
type Query = string | string[];

export default function PdfViewer(props: { node: Node; source: FileSource; allowDownload?: boolean; embedded?: boolean; find?: string }) {
  const { node, source } = props;
  const url = source.contentUrl(node);
  const [status, setStatus] = useState<Status>({ kind: "loading" });
  const givePassword = useRef<((password: string) => void) | null>(null);

  useEffect(() => {
    let alive = true;
    setStatus({ kind: "loading" });
    const task = pdfjs.getDocument({
      ...documentOptions(),
      url,
      // Pages are fetched as they are needed (range requests), so a large file opens without being read whole
      disableAutoFetch: true,
      disableStream: true,
    });
    task.onPassword = (update: (password: string) => void, reason: number) => {
      if (!alive) return;
      givePassword.current = update;
      setStatus({ kind: "password", wrong: reason === pdfjs.PasswordResponses.INCORRECT_PASSWORD });
    };
    task.promise.then(
      (doc) => alive && setStatus({ kind: "ready", doc }),
      (e: unknown) => {
        if (!alive) return;
        const unreadable = e instanceof Error && UNREADABLE.includes(e.name);
        if (!unreadable) reportShown("preview", e, node.id);
        setStatus({
          kind: "error",
          message: unreadable ? t("This file is damaged or isn't a PDF, so it can't be shown.") : errorMessage(e, t("The PDF couldn't be loaded.")),
        });
      },
    );
    return () => {
      alive = false;
      givePassword.current = null;
      void task.destroy();
    };
  }, [url, node.id]);

  const frame = cn("flex size-full min-h-0 flex-col overflow-hidden bg-background text-foreground", !props.embedded && "max-w-5xl rounded-lg");
  if (status.kind === "ready")
    return (
      <div className={frame}>
        <Reader key={url} doc={status.doc} {...props} />
      </div>
    );
  return (
    <div className={cn(frame, "items-center justify-center p-6")}>
      {status.kind === "loading" && <Loader2Icon className="size-6 animate-spin text-muted-foreground" aria-label={t("Loading…")} />}
      {status.kind === "password" && <PasswordForm wrong={status.wrong} onSubmit={(p) => givePassword.current?.(p)} />}
      {status.kind === "error" && (
        <div role="alert" className="flex max-w-sm flex-col items-center gap-3 text-center text-sm">
          <p>{status.message}</p>
          {props.allowDownload !== false && (
            <Button variant="outline" onClick={() => triggerDownload(source.contentUrl(node, true))}>
              <DownloadIcon /> {t("Download")}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}

function PasswordForm({ wrong, onSubmit }: { wrong: boolean; onSubmit(password: string): void }) {
  const [value, setValue] = useState("");
  const [sent, setSent] = useState(false);
  // A wrong password comes back as a new request: the form can be sent again
  useEffect(() => setSent(false), [wrong]);
  return (
    <form
      className="flex w-full max-w-xs flex-col items-center gap-3 text-center text-sm"
      onSubmit={(e) => {
        e.preventDefault();
        if (!value) return;
        setSent(true);
        onSubmit(value);
        setValue("");
      }}
    >
      <LockIcon className="size-8 text-muted-foreground" />
      <p>{t("This PDF is protected with a password. Enter it to open the file.")}</p>
      <input
        type="password"
        autoFocus
        autoComplete="off"
        aria-label={t("Password")}
        aria-invalid={wrong || undefined}
        aria-describedby={wrong ? "pdf-password-wrong" : undefined}
        value={value}
        onChange={(e) => setValue(e.target.value)}
        className="h-8 w-full rounded-md border bg-background px-2.5 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
      />
      {wrong && (
        <p id="pdf-password-wrong" role="alert" className="text-destructive">
          {t("That password isn't right. Try again.")}
        </p>
      )}
      <Button type="submit" disabled={!value || sent}>
        {t("Open")}
      </Button>
    </form>
  );
}

/** pdf.js's viewer and what it reports, for the toolbar */
interface Engine {
  viewer: PDFViewer;
  bus: EventBus;
}

function Reader({ doc, node, source, allowDownload, find }: { doc: PDFDocumentProxy; node: Node; source: FileSource; allowDownload?: boolean; find?: string }) {
  const container = useRef<HTMLDivElement>(null);
  const pages = useRef<HTMLDivElement>(null);
  const findBox = useRef<HTMLInputElement>(null);
  const [engine, setEngine] = useState<Engine | null>(null);
  const [page, setPage] = useState(1);
  const [pageText, setPageText] = useState("1");
  const [scale, setScale] = useState(1);
  const [zoom, setZoom] = useState<Zoom | "auto">("auto");
  const narrow = useMediaQuery("(max-width: 47.99rem)");
  const [sidebar, setSidebar] = useState(() => !narrow && doc.numPages > 1);
  const initialWords = useRef(searchWords(find));
  const [findText, setFindText] = useState(() => initialWords.current.join(" "));
  const query = useRef<Query>(initialWords.current);
  const [matches, setMatches] = useState<{ current: number; total: number } | null>(null);
  const [findState, setFindState] = useState<number | null>(null);
  const [printing, setPrinting] = useState<{ done: number; stop: AbortController } | null>(null);

  // pdf.js's viewer, set up once its code has loaded
  useEffect(() => {
    let alive = true;
    let made: Engine | null = null;
    void loadViewerComponents().then(({ EventBus, PDFLinkService, PDFFindController, PDFViewer, LinkTarget }) => {
      if (!alive || !container.current || !pages.current) return;
      const bus = new EventBus();
      // Links inside the document move within it; links out open in a new tab, without telling the site where from
      const linkService = new PDFLinkService({ eventBus: bus, externalLinkTarget: LinkTarget.BLANK, externalLinkRel: "noopener noreferrer nofollow" });
      const findController = new PDFFindController({ eventBus: bus, linkService });
      const viewer = new PDFViewer({
        container: container.current,
        viewer: pages.current,
        eventBus: bus,
        linkService,
        findController,
        l10n: NO_L10N as unknown as ConstructorParameters<typeof PDFViewer>[0]["l10n"],
        // Form fields are shown as filled in, not as fields to type in (nothing typed could be saved)
        annotationMode: pdfjs.AnnotationMode.ENABLE,
      });
      linkService.setViewer(viewer);
      bus.on("pagesinit", () => {
        viewer.currentScaleValue = "auto";
        if (initialWords.current.length) search(bus, initialWords.current, false, false);
      });
      bus.on("pagechanging", (e: { pageNumber: number }) => {
        setPage(e.pageNumber);
        setPageText(String(e.pageNumber));
      });
      bus.on("scalechanging", (e: { scale: number; presetValue?: string }) => {
        setScale(e.scale);
        const preset = e.presetValue;
        setZoom(preset === "auto" || preset === "page-width" || preset === "page-fit" ? preset : e.scale);
      });
      bus.on("updatefindmatchescount", (e: { matchesCount: { current: number; total: number } }) => setMatches(e.matchesCount));
      bus.on("updatefindcontrolstate", (e: { state: number; matchesCount: { current: number; total: number } }) => {
        setFindState(e.state);
        setMatches(e.matchesCount);
      });
      viewer.setDocument(doc);
      linkService.setDocument(doc, null);
      made = { viewer, bus };
      setEngine(made);
    });
    return () => {
      alive = false;
      made?.viewer.setDocument(null as unknown as PDFDocumentProxy);
      made?.viewer.cleanup();
    };
  }, [doc]);

  // Ctrl + mouse wheel (and pinching a touchpad, which browsers report the same way) zooms around the pointer
  useEffect(() => {
    const el = container.current;
    if (!engine || !el) return;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey && !e.metaKey) return;
      e.preventDefault();
      engine.viewer.updateScale({ drawingDelay: 300, scaleFactor: Math.exp(-e.deltaY / 200), origin: [e.clientX, e.clientY] });
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    // Two fingers on a touch screen
    const stop = new AbortController();
    new pdfjs.TouchManager({
      container: el,
      signal: stop.signal,
      onPinching: (origin: [number, number], before: number, after: number) => {
        if (before > 0) engine.viewer.updateScale({ drawingDelay: 400, scaleFactor: after / before, origin });
      },
    });
    return () => {
      el.removeEventListener("wheel", onWheel);
      stop.abort();
    };
  }, [engine]);

  // Printing stops when the file is left
  useEffect(() => () => printing?.stop.abort(), [printing]);

  const goTo = useCallback(
    (n: number) => {
      if (engine) engine.viewer.currentPageNumber = Math.min(Math.max(n, 1), doc.numPages);
    },
    [engine, doc.numPages],
  );
  const setZoomTo = (z: Zoom | "auto") => {
    if (engine) engine.viewer.currentScaleValue = z === "auto" ? "auto" : scaleValue(z);
  };
  const zoomStep = (dir: 1 | -1) => setZoomTo(stepZoom(scale, dir));

  const runFind = (again: boolean, previous: boolean) => {
    if (!engine) return;
    const q = query.current;
    if (!(typeof q === "string" ? q.trim() : q.length)) return;
    search(engine.bus, q, again, previous);
  };
  const onFindChange = (text: string) => {
    setFindText(text);
    query.current = text;
    if (!engine) return;
    if (text.trim()) search(engine.bus, text, false, false);
    else {
      engine.bus.dispatch("findbarclose", { source: null });
      setMatches(null);
      setFindState(null);
    }
  };

  const print = async () => {
    if (printing) return;
    const stop = new AbortController();
    setPrinting({ done: 0, stop });
    try {
      await printPdf(doc, (done) => setPrinting((p) => (p && p.stop === stop ? { ...p, done } : p)), stop.signal);
    } catch (e) {
      if (!stop.signal.aborted) reportShown("print", e, node.id);
    } finally {
      setPrinting((p) => (p?.stop === stop ? null : p));
    }
  };

  /** Keys while the viewer has the focus: find, zoom, print, and paging through the document */
  const onKeyDown = (e: ReactKeyboardEvent) => {
    const mod = e.ctrlKey || e.metaKey;
    const typing = (e.target as HTMLElement).closest("input, textarea, select");
    if (mod && e.key.toLowerCase() === "f") {
      e.preventDefault();
      findBox.current?.focus();
      findBox.current?.select();
    } else if (mod && (e.key === "+" || e.key === "=")) {
      e.preventDefault();
      zoomStep(1);
    } else if (mod && e.key === "-") {
      e.preventDefault();
      zoomStep(-1);
    } else if (mod && e.key === "0") {
      e.preventDefault();
      setZoomTo("auto");
    } else if (mod && e.key.toLowerCase() === "p" && allowDownload !== false) {
      e.preventDefault();
      void print();
    } else if (!typing && !mod && (e.key === "Home" || e.key === "End")) {
      e.preventDefault();
      goTo(e.key === "Home" ? 1 : doc.numPages);
    }
  };

  const count = doc.numPages;
  let findStatus = "";
  if (findText.trim() && findState === FIND_NOT_FOUND) findStatus = t("Not found");
  else if (matches && matches.total > 0) findStatus = t("Match {current} of {total}", { current: Math.max(matches.current, 1), total: matches.total });
  const zoomOptions: { value: string; label: string }[] = [
    { value: "auto", label: t("Automatic") },
    { value: "page-width", label: t("Fit width") },
    { value: "page-fit", label: t("Fit page") },
    ...ZOOM_STEPS.map((z) => ({ value: String(z), label: percent(z) })),
  ];
  const zoomValue = String(zoom);
  // A size reached by pinching or Ctrl + wheel is shown too
  if (!zoomOptions.some((o) => o.value === zoomValue)) zoomOptions.push({ value: zoomValue, label: percent(scale) });

  return (
    // The viewer's own keys while it has the focus; its right-click menu is the browser's, for copying text
    <div className="flex size-full min-h-0 flex-col" onKeyDown={onKeyDown} onContextMenu={(e) => e.stopPropagation()}>
      <div role="toolbar" aria-label={t("PDF tools")} className="flex shrink-0 flex-wrap items-center gap-1 border-b px-2 py-1 text-sm">
        {count > 1 && (
          <Button variant="ghost" size="icon-sm" aria-pressed={sidebar} aria-label={t("Page thumbnails")} title={t("Page thumbnails")} onClick={() => setSidebar((s) => !s)}>
            <PanelLeftIcon />
          </Button>
        )}
        <Button variant="ghost" size="icon-sm" aria-label={t("Previous page")} title={t("Previous page")} disabled={page <= 1} onClick={() => goTo(page - 1)}>
          <ChevronUpIcon />
        </Button>
        <Button variant="ghost" size="icon-sm" aria-label={t("Next page")} title={t("Next page")} disabled={page >= count} onClick={() => goTo(page + 1)}>
          <ChevronDownIcon />
        </Button>
        <label className="flex items-center gap-1 text-xs text-muted-foreground">
          <input
            aria-label={t("Page number")}
            inputMode="numeric"
            value={pageText}
            onChange={(e) => setPageText(e.target.value)}
            onFocus={(e) => e.target.select()}
            onBlur={() => setPageText(String(page))}
            onKeyDown={(e) => {
              if (e.key !== "Enter") return;
              const n = pageFromInput(pageText, count);
              if (n) goTo(n);
              setPageText(String(n ?? page));
            }}
            className="h-7 w-12 rounded-md border bg-background px-1.5 text-center text-sm text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
          />
          {t("of {count}", { count })}
        </label>
        <span className="mx-1 h-5 w-px bg-border max-sm:hidden" aria-hidden />
        <Button variant="ghost" size="icon-sm" aria-label={t("Zoom out")} title={t("Zoom out (Ctrl+−)")} disabled={scale <= ZOOM_STEPS[0]} onClick={() => zoomStep(-1)}>
          <ZoomOutIcon />
        </Button>
        <select
          aria-label={t("Zoom")}
          value={zoomValue}
          onChange={(e) => setZoomTo(e.target.value === "auto" || e.target.value === "page-width" || e.target.value === "page-fit" ? e.target.value : Number(e.target.value))}
          className="h-7 rounded-md border bg-background px-1.5 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {zoomOptions.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
        <Button variant="ghost" size="icon-sm" aria-label={t("Zoom in")} title={t("Zoom in (Ctrl++)")} disabled={scale >= ZOOM_STEPS[ZOOM_STEPS.length - 1]} onClick={() => zoomStep(1)}>
          <ZoomInIcon />
        </Button>
        <div className="ml-auto flex items-center gap-1" role="search">
          <div className="relative flex items-center">
            <SearchIcon className="pointer-events-none absolute left-2 size-3.5 text-muted-foreground" aria-hidden />
            <input
              ref={findBox}
              type="search"
              aria-label={t("Find in document")}
              placeholder={t("Find in document")}
              value={findText}
              onChange={(e) => onFindChange(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  runFind(true, e.shiftKey);
                } else if (e.key === "Escape" && findText) {
                  // Clears the box first; Escape again leaves as it does elsewhere
                  e.preventDefault();
                  e.stopPropagation();
                  onFindChange("");
                }
              }}
              className="h-7 w-36 rounded-md border bg-background pr-2 pl-7 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring sm:w-48"
            />
          </div>
          <span role="status" className="min-w-12 text-xs whitespace-nowrap text-muted-foreground">
            {findStatus}
            {findState === FIND_WRAPPED && <span className="sr-only">{t("Continued from the other end")}</span>}
          </span>
          <Button variant="ghost" size="icon-sm" aria-label={t("Previous match")} title={t("Previous match (Shift+Enter)")} disabled={!matches?.total} onClick={() => runFind(true, true)}>
            <ChevronLeftIcon />
          </Button>
          <Button variant="ghost" size="icon-sm" aria-label={t("Next match")} title={t("Next match (Enter)")} disabled={!matches?.total} onClick={() => runFind(true, false)}>
            <ChevronRightIcon />
          </Button>
        </div>
        {allowDownload !== false && (
          <>
            <span className="mx-1 h-5 w-px bg-border max-sm:hidden" aria-hidden />
            <Button variant="ghost" size="icon-sm" aria-label={t("Print")} title={t("Print (Ctrl+P)")} disabled={!!printing} onClick={() => void print()}>
              {printing ? <Loader2Icon className="animate-spin" /> : <PrinterIcon />}
            </Button>
            <Button variant="ghost" size="icon-sm" aria-label={t("Download")} title={t("Download")} onClick={() => triggerDownload(source.contentUrl(node, true))}>
              <DownloadIcon />
            </Button>
          </>
        )}
      </div>
      {printing && (
        <div role="status" className="flex shrink-0 items-center gap-2 border-b bg-muted/50 px-3 py-1 text-xs">
          {t("Preparing pages for printing: {done} of {count}", { done: printing.done, count })}
          <Button variant="ghost" size="xs" onClick={() => printing.stop.abort()}>
            {t("Cancel")}
          </Button>
        </div>
      )}
      <div className="relative flex min-h-0 flex-1">
        {sidebar && count > 1 && (
          <div className={cn("w-36 shrink-0", narrow && "absolute inset-y-0 left-0 z-10 shadow-lg")}>
            <Thumbnails
              doc={doc}
              current={page}
              onGo={(n) => {
                goTo(n);
                if (narrow) setSidebar(false);
              }}
            />
          </div>
        )}
        <div className="relative min-w-0 flex-1 bg-muted">
          {/* pdf.js needs its scrolling box positioned absolutely */}
          <div
            ref={container}
            tabIndex={0}
            role="document"
            aria-label={node.name}
            className="tf-pdf absolute inset-0 touch-pan-x touch-pan-y overflow-auto outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
          >
            <div ref={pages} className="pdfViewer" />
          </div>
          {!engine && <Loader2Icon className="absolute inset-0 m-auto size-6 animate-spin text-muted-foreground" aria-label={t("Loading…")} />}
        </div>
      </div>
    </div>
  );
}

function search(bus: EventBus, query: Query, again: boolean, previous: boolean) {
  bus.dispatch("find", {
    source: null,
    type: again ? "again" : "",
    query,
    caseSensitive: false,
    entireWord: false,
    highlightAll: true,
    findPrevious: previous,
    matchDiacritics: false,
  });
}
