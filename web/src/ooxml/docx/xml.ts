/**
 * Word-specific XML helpers: compatibility blocks (mc:AlternateContent), w:val reading, length units.
 */

import { attr, kid, kids } from "../core/package";

/** Supported extension namespaces: use mc:Choice when all it requires are here, otherwise Fallback */
const SUPPORTED_NS = new Set([
  "http://schemas.microsoft.com/office/word/2010/wordprocessingShape",
  "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup",
  "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing",
  "http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas",
  "http://schemas.microsoft.com/office/word/2010/wordml",
  "http://schemas.microsoft.com/office/word/2012/wordml",
  "http://schemas.microsoft.com/office/drawing/2010/main",
  "http://schemas.microsoft.com/office/drawing/2007/8/2/chart",
  "http://schemas.microsoft.com/office/drawing/2014/main",
  "http://schemas.openxmlformats.org/markup-compatibility/2006",
]);
const SUPPORTED_PREFIX = new Set(["wps", "wpg", "wp14", "wpc", "w14", "w15", "a14", "c14", "a16"]);

function choiceSupported(choice: Element) {
  const req = (attr(choice, "Requires") ?? "").split(/\s+/).filter(Boolean);
  return req.every((p) => {
    const ns = choice.lookupNamespaceURI(p);
    return ns ? SUPPORTED_NS.has(ns) : SUPPORTED_PREFIX.has(p);
  });
}

/** mc:AlternateContent → children of the chosen branch */
export function alternate(ac: Element): Element[] {
  for (const c of kids(ac)) {
    if (c.localName === "Choice" && choiceSupported(c)) return kids(c);
    if (c.localName === "Fallback") return kids(c);
  }
  return [];
}

/** Child elements, with mc:AlternateContent expanded */
export function flatKids(el: Element | null | undefined): Element[] {
  if (!el) return [];
  const out: Element[] = [];
  for (let c = el.firstElementChild; c; c = c.nextElementSibling) {
    if (c.localName === "AlternateContent") out.push(...alternate(c));
    else out.push(c);
  }
  return out;
}

/** Find a child element (including those in the chosen AlternateContent branch) */
export function flatKid(el: Element | null | undefined, name: string): Element | null {
  if (!el) return null;
  for (let c = el.firstElementChild; c; c = c.nextElementSibling) {
    if (c.localName === name) return c;
    if (c.localName === "AlternateContent") {
      const hit = alternate(c).find((x) => x.localName === name);
      if (hit) return hit;
    }
  }
  return null;
}

/** w:val of a child element */
export const val = (el: Element | null | undefined, name: string) => attr(kid(el, name), "val");

/**
 * Length (twips): usually integer twips, but newer files may carry units ("2.54cm", "1in", "12pt")
 */
export function twips(v: string | null | undefined): number | null {
  if (v === null || v === undefined || v === "") return null;
  const m = /^(-?[\d.]+)\s*(mm|cm|in|pt|pc|pi)?$/.exec(v.trim());
  if (!m) return null;
  const n = Number(m[1]);
  if (!Number.isFinite(n)) return null;
  switch (m[2]) {
    case "mm":
      return (n / 25.4) * 1440;
    case "cm":
      return (n / 2.54) * 1440;
    case "in":
      return n * 1440;
    case "pt":
      return n * 20;
    case "pc":
    case "pi":
      return n * 240;
    default:
      return n;
  }
}

export const twAttr = (el: Element | null | undefined, name: string) => twips(attr(el, name));

/** twips → px */
export const tw = (t: number) => (t / 1440) * 96;

/** Whether a color string is 6-digit hex (checked before putting it into CSS) */
export const isHex = (v: string | null | undefined): v is string => !!v && /^[0-9a-f]{6}$/i.test(v);

/** Percentage value: Word writes either "50%" or 5000 (in 1/50 %) */
export function pctValue(v: string | null | undefined): number | null {
  if (!v) return null;
  if (v.endsWith("%")) {
    const n = Number(v.slice(0, -1));
    return Number.isFinite(n) ? n : null;
  }
  const n = Number(v);
  return Number.isFinite(n) ? n / 50 : null;
}
