/**
 * Printing a PDF from the viewer: each page drawn as a picture into a part of the page only the printer sees
 * (style.css, `.pdf-print`), then the browser's own print dialog. Works the same in every browser, phones included,
 * where printing the file through a frame doesn't
 */
import type { PDFDocumentProxy } from "pdfjs-dist/legacy/build/pdf.mjs";

/** Pages are drawn at this many dots per inch: sharp on paper without holding too much in memory */
const PRINT_DPI = 150;

/** Draw every page, then open the print dialog; `onProgress` follows the pages drawn. Stops when `signal` does */
export async function printPdf(doc: PDFDocumentProxy, onProgress: (done: number) => void, signal: AbortSignal): Promise<void> {
  const holder = document.createElement("div");
  holder.className = "pdf-print";
  holder.setAttribute("aria-hidden", "true");
  const urls: string[] = [];
  const cleanUp = () => {
    holder.remove();
    for (const u of urls) URL.revokeObjectURL(u);
  };
  try {
    for (let i = 1; i <= doc.numPages; i++) {
      if (signal.aborted) throw signal.reason;
      const page = await doc.getPage(i);
      const viewport = page.getViewport({ scale: PRINT_DPI / 72 });
      const canvas = document.createElement("canvas");
      canvas.width = Math.round(viewport.width);
      canvas.height = Math.round(viewport.height);
      await page.render({ canvas, viewport, background: "#ffffff", intent: "print" }).promise;
      const blob = await new Promise<Blob | null>((done) => canvas.toBlob(done, "image/jpeg", 0.92));
      canvas.width = canvas.height = 0;
      page.cleanup();
      if (!blob) throw new Error("A page couldn't be prepared for printing");
      const url = URL.createObjectURL(blob);
      urls.push(url);
      const img = document.createElement("img");
      img.src = url;
      img.alt = "";
      // Landscape pages stay landscape on paper as wide as the page allows
      img.style.aspectRatio = `${viewport.width} / ${viewport.height}`;
      holder.append(img);
      onProgress(i);
    }
    if (signal.aborted) throw signal.reason;
    document.body.append(holder);
    await Promise.all([...holder.querySelectorAll("img")].map((img) => img.decode().catch(() => undefined)));
  } catch (e) {
    cleanUp();
    throw e;
  }
  // Some browsers return from print() before the dialog closes: the pages go once it says printing is over
  window.addEventListener("afterprint", cleanUp, { once: true });
  window.print();
  // Browsers that never say so (some phones) keep them until the next print or a minute later, whichever comes first
  setTimeout(cleanUp, 60_000);
}
