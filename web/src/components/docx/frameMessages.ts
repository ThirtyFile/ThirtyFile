/**
 * Checks of what the editing frame sends the page: the frame shows the document's content, so its messages are taken
 * only in the shapes the page expects (a frame that sent anything else would be one the document took over).
 */
import type { CaretFormat } from "@/ooxml/docx/editor";
import type { Parts } from "./session";

/** A part's path in the package: folders and a name ending in .xml or .rels, nothing that climbs out */
const PART_PATH = /^(?![/.])(?!.*\.\.)[\w[\]. /-]+\.(xml|rels)$/;

/** The parts a message from the frame names, as text: anything else is left out */
export function partsFrom(value: unknown): Parts {
  const out: Parts = {};
  if (!value || typeof value !== "object") return out;
  for (const [path, text] of Object.entries(value)) if (PART_PATH.test(path) && typeof text === "string") out[path] = text;
  return out;
}

/** The formatting where the caret is, from a message of the frame, when it has the shape the toolbar expects */
export function caretFormatFrom(value: unknown): CaretFormat | null {
  if (!value || typeof value !== "object") return null;
  const f = value as Record<string, unknown>;
  const bools = ["b", "i", "u", "s"].every((k) => typeof f[k] === "boolean");
  const numbers = ["size", "before", "after"].every((k) => typeof f[k] === "number" && Number.isFinite(f[k]));
  const ok =
    bools &&
    numbers &&
    typeof f.font === "string" &&
    (f.style === null || typeof f.style === "string") &&
    (f.line === null || typeof f.line === "number") &&
    (f.list === null || f.list === "bullet" || f.list === "number") &&
    ["left", "center", "right", "both"].includes(f.align as string);
  return ok ? (f as unknown as CaretFormat) : null;
}
