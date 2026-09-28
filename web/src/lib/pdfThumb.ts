/** The first page of a PDF drawn with pdf.js (loaded only when a PDF thumbnail is needed) */
// The legacy build: it carries the newest JavaScript features pdf.js uses, for browsers a year or two old
import { GlobalWorkerOptions, getDocument } from "pdfjs-dist/legacy/build/pdf.mjs";
import workerUrl from "pdfjs-dist/legacy/build/pdf.worker.min.mjs?url";

GlobalWorkerOptions.workerSrc = workerUrl;

export async function pdfFirstPage(url: string, side: number): Promise<HTMLCanvasElement | null> {
  const task = getDocument({
    url,
    // Only the parts of the file the first page needs, with range requests, rather than the whole file
    disableAutoFetch: true,
    disableStream: true,
    // Fonts the file doesn't embed are drawn with the browser's fonts
    useSystemFonts: true,
  });
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
  } finally {
    void task.destroy();
  }
}
