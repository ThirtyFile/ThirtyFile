/** pdf.js, shared by PDF thumbnails and the PDF viewer (loaded only when one of them is needed) */
// The legacy build: it carries the newest JavaScript features pdf.js uses, for browsers a year or two old
import * as pdfjs from "pdfjs-dist/legacy/build/pdf.mjs";
import workerUrl from "pdfjs-dist/legacy/build/pdf.worker.min.mjs?url";

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

export { pdfjs };

/**
 * Data pdf.js fetches only for the files that need it, served with the interface: character maps for Chinese, Japanese
 * and Korean text in fonts the file doesn't embed, and the decoders of scanned pages (JBIG2) and JPEG 2000 pictures
 */
// Always files of their own (never written into this script), so only those a document asks for are downloaded
const assets: Record<string, string> = {
  ...import.meta.glob<string>("../../node_modules/pdfjs-dist/cmaps/*.bcmap", { query: "?url&no-inline", import: "default", eager: true }),
  ...import.meta.glob<string>("../../node_modules/pdfjs-dist/wasm/{jbig2,openjpeg,qcms_bg}.wasm", { query: "?url&no-inline", import: "default", eager: true }),
};
const byName = new Map(Object.entries(assets).map(([path, url]) => [path.slice(path.lastIndexOf("/") + 1), url]));

/** Hands pdf.js the files above by name (their addresses carry a content hash, so no folder address fits them all) */
class AssetFactory {
  async fetch({ filename }: { kind: string; filename: string }) {
    const url = byName.get(filename);
    if (!url) throw new Error(`pdf.js asked for ${filename}, which isn't served`);
    const res = await fetch(url);
    if (!res.ok) throw new Error(`${filename}: ${res.status}`);
    return new Uint8Array(await res.arrayBuffer());
  }
}

/** One worker for every document (each would otherwise start and stop a worker of its own) */
let worker: pdfjs.PDFWorker | undefined;

/** Options every document is opened with */
export function documentOptions() {
  if (!worker || worker.destroyed) worker = new pdfjs.PDFWorker();
  return {
    worker,
    BinaryDataFactory: AssetFactory,
    // Any value: pdf.js only asks for these when they are set, and AssetFactory finds the files by name
    cMapUrl: "cmaps/",
    wasmUrl: "wasm/",
    // Fonts the file doesn't embed are drawn with the browser's fonts
    useSystemFonts: true,
    // Fonts are drawn without compiling code from the file (the page's Content-Security-Policy allows none)
    isEvalSupported: false,
  } as const;
}

/** Errors that say the file itself has no page to show (damaged, not a PDF, locked with a password) */
export const UNREADABLE = ["InvalidPDFException", "PasswordException", "FormatError"];

/** pdf.js's page viewer components. They read the library from `globalThis.pdfjsLib`, so it is set before they load */
export async function loadViewerComponents() {
  (globalThis as { pdfjsLib?: unknown }).pdfjsLib = pdfjs;
  return import("pdfjs-dist/legacy/web/pdf_viewer.mjs");
}
