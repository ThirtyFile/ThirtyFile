/**
 * Footnotes and endnotes: numbered in reference order; footnotes go at the bottom of the referencing page, endnotes at the end of the document.
 */

import { h } from "../core/package";
import { fillBlocks } from "./blocks";
import { childFlow, type Flow, type NoteRef } from "./context";
import { flatKids } from "./xml";

/** Footnote content (one copy per reference; an id referenced multiple times is numbered separately each time) */
export function renderNotes(refs: NoteRef[], kind: "foot" | "end", base: Flow, part: string | null): Map<string, HTMLElement> {
  const out = new Map<string, HTMLElement>();
  const doc = base.doc;
  const src = kind === "foot" ? doc.footnotes : doc.endnotes;
  const rels = part ? doc.rels.get(part) ?? [] : [];
  for (const ref of refs) {
    if (ref.kind !== kind || out.has(ref.id)) continue;
    const el = src.get(ref.id);
    if (!el) continue;
    const box = h("div", { class: "tf-docx-note" });
    const flow = childFlow(base, { story: "note", part: part ?? base.part, rels, noteMark: ref.mark, floats: [], depth: 0 });
    fillBlocks(box, flatKids(el), flow);
    out.set(ref.id, box);
  }
  return out;
}

/** Endnote area at the end of the document */
export function endnoteBlock(notes: Map<string, HTMLElement>): HTMLElement | null {
  if (!notes.size) return null;
  const box = h("div", { class: "tf-docx-endnotes" }, h("div", { class: "tf-docx-fnrule" }));
  for (const n of notes.values()) box.append(n);
  return box;
}
