/** The PDF viewer's side panel: a small picture of each page, drawn once it scrolls into view; a click goes to the page */
import { useEffect, useRef, useState } from "react";
import type { PDFDocumentProxy, RenderTask } from "pdfjs-dist/legacy/build/pdf.mjs";
import { t } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/** Width of a page picture, in CSS pixels */
const WIDTH = 96;

/** Pictures are drawn one after another, so scrolling through a long document doesn't start hundreds at once */
let queue: Promise<unknown> = Promise.resolve();
function inTurn<T>(work: () => Promise<T>): Promise<T> {
  const next = queue.then(work, work);
  queue = next.catch(() => undefined);
  return next;
}

export function Thumbnails({ doc, current, onGo }: { doc: PDFDocumentProxy; current: number; onGo(page: number): void }) {
  const list = useRef<HTMLElement>(null);
  // The page shown stays in sight in the panel
  useEffect(() => {
    list.current?.querySelector(`[data-page="${current}"]`)?.scrollIntoView({ block: "nearest" });
  }, [current]);
  return (
    <nav ref={list} aria-label={t("Pages")} className="flex h-full flex-col items-center gap-3 overflow-x-hidden overflow-y-auto border-r bg-muted/40 px-3 py-3">
      {Array.from({ length: doc.numPages }, (_, i) => (
        <Thumb key={i + 1} doc={doc} page={i + 1} current={current === i + 1} onGo={onGo} />
      ))}
    </nav>
  );
}

function Thumb({ doc, page, current, onGo }: { doc: PDFDocumentProxy; page: number; current: boolean; onGo(page: number): void }) {
  const box = useRef<HTMLButtonElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [visible, setVisible] = useState(false);
  const [ratio, setRatio] = useState(1.294);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const seen = new IntersectionObserver(([e]) => e.isIntersecting && setVisible(true), { rootMargin: "300px 0px" });
    seen.observe(el);
    return () => seen.disconnect();
  }, []);

  useEffect(() => {
    if (!visible) return;
    let stopped = false;
    let task: RenderTask | undefined;
    void inTurn(async () => {
      if (stopped || !canvas.current) return;
      const p = await doc.getPage(page);
      const base = p.getViewport({ scale: 1 });
      const scale = (WIDTH * (window.devicePixelRatio || 1)) / base.width;
      const viewport = p.getViewport({ scale });
      if (stopped || !canvas.current) return;
      setRatio(base.height / base.width);
      canvas.current.width = Math.round(viewport.width);
      canvas.current.height = Math.round(viewport.height);
      task = p.render({ canvas: canvas.current, viewport, background: "#ffffff" });
      await task.promise;
    }).catch(() => undefined);
    return () => {
      stopped = true;
      task?.cancel();
    };
  }, [visible, doc, page]);

  return (
    <button
      ref={box}
      type="button"
      data-page={page}
      aria-current={current ? "page" : undefined}
      aria-label={t("Page {n}", { n: page })}
      onClick={() => onGo(page)}
      className="group flex shrink-0 flex-col items-center gap-1 rounded-md p-1 text-xs text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      <canvas
        ref={canvas}
        style={{ width: WIDTH, height: Math.round(WIDTH * ratio) }}
        className={cn("bg-white shadow-sm ring-1 ring-border", current ? "ring-2 ring-brand" : "group-hover:ring-foreground/30")}
      />
      <span className={cn(current && "font-medium text-foreground")}>{page}</span>
    </button>
  );
}
