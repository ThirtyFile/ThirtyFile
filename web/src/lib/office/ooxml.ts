/**
 * Shared foundation for Office Open XML (.docx/.pptx/.xlsx) previews:
 * reading the ZIP package, XML node helpers (namespace-prefix agnostic), relationships (_rels), unit conversion, media files.
 *
 * All renderers build output with DOM APIs only (textContent, setAttribute), never innerHTML,
 * so document content is never interpreted as HTML or executed as script.
 */

import JSZip from "jszip";

// ───────────── Units ─────────────

/** 1 px (96 DPI) = 9525 EMU; 1 pt = 12700 EMU; 1 twip = 1/20 pt */
export const EMU_PER_PX = 9525;
export const emuToPx = (emu: number) => emu / EMU_PER_PX;
export const twipToPx = (tw: number) => (tw / 20) * (96 / 72);
export const ptToPx = (pt: number) => (pt * 96) / 72;
/** Round to 0.01 to keep the generated CSS short */
export const round = (n: number) => Math.round(n * 100) / 100;
export const px = (n: number) => `${round(n)}px`;
export const pt = (n: number) => `${round(n)}pt`;

// ───────────── XML ─────────────

const parser = new DOMParser();

export function parseXml(text: string): Document | null {
  const doc = parser.parseFromString(text, "application/xml");
  return doc.getElementsByTagName("parsererror").length ? null : doc;
}

/** Direct child elements (by localName, ignoring namespace prefix) */
export function kids(el: Element | null | undefined, name?: string): Element[] {
  if (!el) return [];
  const out: Element[] = [];
  for (let c = el.firstElementChild; c; c = c.nextElementSibling) if (!name || c.localName === name) out.push(c);
  return out;
}

export function kid(el: Element | null | undefined, name: string): Element | null {
  if (!el) return null;
  for (let c = el.firstElementChild; c; c = c.nextElementSibling) if (c.localName === name) return c;
  return null;
}

/** Walk down a path: kidPath(el, "spPr", "xfrm", "off") */
export function kidPath(el: Element | null | undefined, ...names: string[]): Element | null {
  let cur: Element | null | undefined = el;
  for (const n of names) cur = kid(cur, n);
  return cur ?? null;
}

/** All descendant elements (by localName) */
export function descendants(el: Element | Document | null | undefined, name: string): Element[] {
  if (!el) return [];
  return Array.from(el.getElementsByTagNameNS("*", name));
}

/** Attribute by localName (e.g. r:id and w:val can be read as "id" and "val") */
export function attr(el: Element | null | undefined, name: string): string | null {
  if (!el) return null;
  const direct = el.getAttribute(name);
  if (direct !== null) return direct;
  for (const a of Array.from(el.attributes)) if (a.localName === name) return a.value;
  return null;
}

export function numAttr(el: Element | null | undefined, name: string): number | null {
  const v = attr(el, name);
  if (v === null || v === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

/**
 * Toggle property (<w:b/>, <w:b w:val="0"/>, <a:rPr b="1"/>):
 * true when the element exists and val is not 0/false/off; undefined when absent (meaning inherit from the parent level)
 */
export function toggle(el: Element | null | undefined): boolean | undefined {
  if (!el) return undefined;
  const v = attr(el, "val");
  return v === null || !/^(0|false|off|none)$/i.test(v);
}

export function boolAttr(el: Element | null | undefined, name: string): boolean | undefined {
  const v = attr(el, name);
  if (v === null) return undefined;
  return !/^(0|false|off)$/i.test(v);
}

// ───────────── Package ─────────────

export interface Rel {
  id: string;
  type: string;
  /** Resolved to an absolute path within the package (without leading /); the original URL for external links */
  target: string;
  external: boolean;
}

/** Resolve a relative path against base's folder */
export function resolvePath(base: string, target: string): string {
  if (target.startsWith("/")) return target.slice(1);
  const parts = base.split("/");
  parts.pop();
  for (const seg of target.split("/")) {
    if (seg === "..") parts.pop();
    else if (seg !== "." && seg !== "") parts.push(seg);
  }
  return parts.join("/");
}

const IMAGE_TYPES: [RegExp, string][] = [
  [/\.png$/i, "image/png"],
  [/\.jpe?g$/i, "image/jpeg"],
  [/\.gif$/i, "image/gif"],
  [/\.bmp$/i, "image/bmp"],
  [/\.webp$/i, "image/webp"],
  [/\.svg$/i, "image/svg+xml"],
  [/\.tiff?$/i, "image/tiff"],
];

/** Detect the image format from content (the extension may be wrong) */
function sniffImage(b: Uint8Array): string | null {
  if (b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47) return "image/png";
  if (b[0] === 0xff && b[1] === 0xd8) return "image/jpeg";
  if (b[0] === 0x47 && b[1] === 0x49 && b[2] === 0x46) return "image/gif";
  if (b[0] === 0x42 && b[1] === 0x4d) return "image/bmp";
  if (b[0] === 0x52 && b[1] === 0x49 && b[2] === 0x46 && b[3] === 0x46 && b[8] === 0x57 && b[9] === 0x45) return "image/webp";
  return null;
}

/** Error code thrown when content in the archive is too large (possible zip bomb); the caller turns it into a message */
export const TOO_LARGE = "OOXML_TOO_LARGE";
/** Decompressed size limits: one XML part, one media file (image), and everything read from one package */
export const MAX_XML_PART = 64 * 1024 * 1024;
const MAX_MEDIA_PART = 50 * 1024 * 1024;
const MAX_TOTAL = 300 * 1024 * 1024;
/** A real document has a few hundred entries; a central directory with hundreds of thousands only serves to stall the browser */
const MAX_ENTRIES = 5000;

/**
 * Before decompressing, check the entry count and the sizes declared in the central directory: refuse to open if a single part
 * or the total is too large. The declared sizes can lie, so `readEntry` enforces the same limits again while inflating.
 */
export function checkZipSizes(zip: JSZip) {
  let total = 0;
  let count = 0;
  for (const f of Object.values(zip.files)) {
    if (f.dir) continue;
    if (++count > MAX_ENTRIES) throw new Error(TOO_LARGE);
    const size = (f as unknown as { _data?: { uncompressedSize?: number } })._data?.uncompressedSize ?? 0;
    total += size;
    if (size > (isXmlPart(f.name) ? MAX_XML_PART : MAX_MEDIA_PART) || total > MAX_TOTAL) throw new Error(TOO_LARGE);
  }
}

const isXmlPart = (name: string) => /\.(xml|rels|vml)$/i.test(name);

interface JSZipStream {
  on(event: "data", cb: (chunk: Uint8Array) => void): JSZipStream;
  on(event: "error", cb: (e: Error) => void): JSZipStream;
  on(event: "end", cb: () => void): JSZipStream;
  resume(): JSZipStream;
  pause(): JSZipStream;
}

/** Decompressed bytes read so far per archive, so a package can't exceed MAX_TOTAL even across many small parts.
 * An entry read twice (the spreadsheet model and the preview both read sheet XML) is charged once. */
const budgets = new WeakMap<JSZip, { spent: number; seen: Set<string> }>();

/**
 * Read one entry with a hard limit on the decompressed size: JSZip only compares the declared size after inflating everything,
 * so the stream is aborted as soon as the limit is exceeded, before the bytes pile up in memory.
 */
export function readEntry(zip: JSZip, path: string, type: "string", limit?: number): Promise<string | null>;
export function readEntry(zip: JSZip, path: string, type: "uint8array", limit?: number): Promise<Uint8Array | null>;
export function readEntry(zip: JSZip, path: string, type: "string" | "uint8array", limit?: number): Promise<string | Uint8Array | null> {
  const f = zip.file(path);
  if (!f) return Promise.resolve(null);
  const max = limit ?? (isXmlPart(path) ? MAX_XML_PART : MAX_MEDIA_PART);
  return new Promise((resolve, reject) => {
    const chunks: Uint8Array[] = [];
    let size = 0;
    let done = false;
    // internalStream is part of JSZip's public API (used by nodeStream / async) but missing from its type definitions
    const stream = (f as unknown as { internalStream(type: "uint8array"): JSZipStream }).internalStream("uint8array");
    const fail = (err: Error) => {
      if (done) return;
      done = true;
      chunks.length = 0;
      stream.pause();
      reject(err);
    };
    let budget = budgets.get(zip);
    if (!budget) budgets.set(zip, (budget = { spent: 0, seen: new Set() }));
    const charge = !budget.seen.has(path);
    stream
      .on("data", (chunk: Uint8Array) => {
        if (done) return;
        size += chunk.length;
        if (charge) budget.spent += chunk.length;
        if (size > max || budget.spent > MAX_TOTAL) return fail(new Error(TOO_LARGE));
        chunks.push(chunk);
      })
      .on("error", (e: Error) => fail(e))
      .on("end", () => {
        if (done) return;
        done = true;
        budget.seen.add(path);
        const out = new Uint8Array(size);
        let at = 0;
        for (const c of chunks) {
          out.set(c, at);
          at += c.length;
        }
        resolve(type === "string" ? new TextDecoder().decode(out) : out);
      })
      .resume();
  });
}

export class OoxmlPackage {
  private docs = new Map<string, Promise<Document | null>>();
  private relsCache = new Map<string, Promise<Rel[]>>();
  private urls = new Map<string, Promise<string | null>>();
  private created: string[] = [];

  private constructor(readonly zip: JSZip) {}

  static async open(buf: ArrayBuffer): Promise<OoxmlPackage> {
    const zip = await JSZip.loadAsync(buf);
    checkZipSizes(zip);
    return new OoxmlPackage(zip);
  }

  /** Reuse an already-unzipped archive (e.g. when the spreadsheet already read the same file) */
  static fromZip(zip: JSZip): OoxmlPackage {
    return new OoxmlPackage(zip);
  }

  has(path: string) {
    return !!this.zip.file(path);
  }

  paths(prefix: string) {
    return Object.keys(this.zip.files).filter((p) => p.startsWith(prefix));
  }

  text(path: string): Promise<string | null> {
    return readEntry(this.zip, path, "string");
  }

  /** Parse XML (each file is parsed only once) */
  xml(path: string): Promise<Document | null> {
    let p = this.docs.get(path);
    if (!p) {
      p = this.text(path).then((t) => (t === null ? null : parseXml(t)));
      this.docs.set(path, p);
    }
    return p;
  }

  /** Relationships of a part (word/document.xml → word/_rels/document.xml.rels) */
  rels(part: string): Promise<Rel[]> {
    let p = this.relsCache.get(part);
    if (!p) {
      const i = part.lastIndexOf("/");
      const relsPath = `${part.slice(0, i + 1)}_rels/${part.slice(i + 1)}.rels`;
      p = this.xml(relsPath).then((doc) =>
        descendants(doc, "Relationship").map((r) => {
          const external = attr(r, "TargetMode") === "External";
          const target = attr(r, "Target") ?? "";
          return { id: attr(r, "Id") ?? "", type: attr(r, "Type") ?? "", target: external ? target : resolvePath(part, target), external };
        }),
      );
      this.relsCache.set(part, p);
    }
    return p;
  }

  async rel(part: string, id: string | null | undefined): Promise<Rel | undefined> {
    if (!id) return undefined;
    return (await this.rels(part)).find((r) => r.id === id);
  }

  /** Find a target by relationship type (suffix match, e.g. "/theme", "/slideLayout") */
  async relOfType(part: string, typeSuffix: string): Promise<Rel | undefined> {
    return (await this.rels(part)).find((r) => r.type.endsWith(typeSuffix));
  }

  /**
   * Turn a media file into a blob: URL (created once per file); returns null for image formats that can't be displayed.
   * Call dispose() to release them.
   */
  mediaUrl(path: string): Promise<string | null> {
    let p = this.urls.get(path);
    if (!p) {
      p = (async () => {
        const data = await readEntry(this.zip, path, "uint8array");
        if (!data) return null;
        const type = sniffImage(data) ?? IMAGE_TYPES.find(([re]) => re.test(path))?.[1];
        // Skip formats the browser can't display, such as EMF/WMF
        if (!type || type === "image/tiff") return null;
        // Use a data: URL for SVG: blob: URLs share this site's origin, so if opened on their own, scripts in the SVG would run on this site
        if (type === "image/svg+xml") return `data:image/svg+xml;base64,${base64(data)}`;
        const url = URL.createObjectURL(new Blob([data as BlobPart], { type }));
        this.created.push(url);
        return url;
      })();
      this.urls.set(path, p);
    }
    return p;
  }

  dispose() {
    for (const u of this.created) URL.revokeObjectURL(u);
    this.created = [];
  }
}

function base64(data: Uint8Array) {
  let s = "";
  for (let i = 0; i < data.length; i += 0x8000) s += String.fromCharCode(...data.subarray(i, i + 0x8000));
  return btoa(s);
}

// ───────────── DOM construction ─────────────

type Attrs = Record<string, string | number | undefined | null>;

/** Create an HTML element: h("div", { class: "x" }, "text", child) */
export function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs?: Attrs | null, ...children: (Node | string | null | undefined | false)[]) {
  const el = document.createElement(tag);
  if (attrs) for (const [k, v] of Object.entries(attrs)) if (v !== undefined && v !== null) el.setAttribute(k, String(v));
  for (const c of children) if (c !== null && c !== undefined && c !== false) el.append(c);
  return el;
}

export const SVG_NS = "http://www.w3.org/2000/svg";

/** Create an SVG element */
export function s(tag: string, attrs?: Attrs | null, ...children: (Node | string | null | undefined | false)[]): SVGElement {
  const el = document.createElementNS(SVG_NS, tag) as SVGElement;
  if (attrs) for (const [k, v] of Object.entries(attrs)) if (v !== undefined && v !== null) el.setAttribute(k, String(v));
  for (const c of children) if (c !== null && c !== undefined && c !== false) el.append(c);
  return el;
}

/** Convert an object to a style string (skipping empty values) */
export function css(styles: Record<string, string | number | undefined | null | false>): string {
  return Object.entries(styles)
    .filter(([, v]) => v !== undefined && v !== null && v !== false && v !== "")
    .map(([k, v]) => `${k}:${v}`)
    .join(";");
}

/** Links in documents: only http(s)/mailto are kept; others (e.g. javascript:) are not linked */
export function safeHref(href: string | null | undefined): string | null {
  if (!href) return null;
  const v = href.trim();
  return /^(https?:|mailto:)/i.test(v) ? v : null;
}
