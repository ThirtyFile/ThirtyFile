/** The first page of a PDF drawn with pdf.js (loaded only when a PDF thumbnail is needed) */
import { UNREADABLE, documentOptions, pdfjs } from "@/lib/pdfjs";

/** The first page, or null when the file has none to show; fails when it couldn't be read or `signal` stopped it */
export async function pdfFirstPage(url: string, side: number, signal: AbortSignal): Promise<HTMLCanvasElement | null> {
  const task = pdfjs.getDocument({
    ...documentOptions(),
    url,
    // Only the parts of the file the first page needs, with range requests, rather than the whole file
    disableAutoFetch: true,
    disableStream: true,
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
