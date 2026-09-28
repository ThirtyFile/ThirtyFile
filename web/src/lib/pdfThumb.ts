/** The first page of a PDF drawn with pdf.js (loaded only when a PDF thumbnail is needed) */
// The legacy build: it carries the newest JavaScript features pdf.js uses, for browsers a year or two old
import { GlobalWorkerOptions, PDFWorker, getDocument } from "pdfjs-dist/legacy/build/pdf.mjs";
import workerUrl from "pdfjs-dist/legacy/build/pdf.worker.min.mjs?url";

GlobalWorkerOptions.workerSrc = workerUrl;

/** One worker for every thumbnail (each document would otherwise start and stop a worker of its own) */
let worker: PDFWorker | undefined;

/** Errors that say the file itself has no page to show (damaged, not a PDF, locked with a password) */
const UNREADABLE = ["InvalidPDFException", "PasswordException", "FormatError"];

/** The first page, or null when the file has none to show; fails when it couldn't be read or `signal` stopped it */
export async function pdfFirstPage(url: string, side: number, signal: AbortSignal): Promise<HTMLCanvasElement | null> {
  if (!worker || worker.destroyed) worker = new PDFWorker();
  const task = getDocument({
    url,
    worker,
    // Only the parts of the file the first page needs, with range requests, rather than the whole file
    disableAutoFetch: true,
    disableStream: true,
    // Fonts the file doesn't embed are drawn with the browser's fonts
    useSystemFonts: true,
  });
  const stop = () => void task.destroy();
  signal.addEventListener("abort", stop);
  try {
    const doc = await task.promise;
    const page = await doc.getPage(1);
    const base = page.getViewport({ scale: 1 });
    const viewport = page.getViewport({ scale: side / Math.max(base.width, base.height) });
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(viewport.width));
    canvas.height = Math.max(1, Math.round(viewport.height));
    await page.render({ canvas, viewport, background: "#ffffff" }).promise;
    return canvas;
  } catch (e) {
    if (!signal.aborted && e instanceof Error && UNREADABLE.includes(e.name)) return null;
    throw e;
  } finally {
    signal.removeEventListener("abort", stop);
    void task.destroy();
  }
}
