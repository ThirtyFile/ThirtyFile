/** The PDF viewer's choices that don't need pdf.js: zoom steps, page numbers typed in, and search words */

/** How large pages are drawn: a fit pdf.js works out from the space there is, or a fixed size (1 = 100 %) */
export type Zoom = "page-width" | "page-fit" | number;

export const ZOOM_STEPS = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 2, 3, 4] as const;

/** The value pdf.js takes for a zoom (`currentScaleValue`) */
export function scaleValue(z: Zoom): string {
  return typeof z === "number" ? String(z) : z;
}

/** The next fixed size up (`dir` 1) or down (-1) from the scale pages are drawn at now */
export function stepZoom(current: number, dir: 1 | -1): number {
  if (dir > 0) return ZOOM_STEPS.find((z) => z > current + 0.001) ?? ZOOM_STEPS[ZOOM_STEPS.length - 1];
  return [...ZOOM_STEPS].reverse().find((z) => z < current - 0.001) ?? ZOOM_STEPS[0];
}

/** A scale shown as a percentage */
export function percent(scale: number): string {
  return `${Math.round(scale * 100)}%`;
}

/** The page a typed number goes to (within the document), or null when it isn't a number */
export function pageFromInput(input: string, count: number): number | null {
  const n = Number.parseInt(input.trim(), 10);
  if (!Number.isFinite(n) || count < 1) return null;
  return Math.min(Math.max(n, 1), count);
}

/** Quotation marks around a word, Western and East Asian (corner brackets) */
const QUOTES = /^["'‘’“”「-』]+|["'‘’“”「-』]+$/g; // i18n-ignore: quotation marks, not text

/**
 * The words a search inside files looked for, which the viewer marks in the document: separated by spaces, each once,
 * quotation marks left out. Matched as any of them, as the search does
 */
export function searchWords(term: string | null | undefined): string[] {
  if (!term) return [];
  const words = term
    .normalize("NFKC")
    .split(/\s+/)
    .map((w) => w.replace(QUOTES, ""))
    .filter(Boolean);
  const seen = new Set<string>();
  return words.filter((w) => {
    const k = w.toLowerCase();
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}

/** The address of a file opened from search results: with the words the search found inside it, for the viewer to show */
export function viewPath(id: string, find?: string): string {
  return find?.trim() ? `/view/${id}?find=${encodeURIComponent(find.trim())}` : `/view/${id}`;
}
