/**
 * Titles (chart title, axis titles, display unit labels): text from c:tx/c:rich, strRef or cx:txData.
 */

import { kid } from "@/lib/office/ooxml";
import { strRefText } from "./model";
import { layoutBlock, plainLines, richLines, txPrStyle, wrapLines, type Block, type Line, type TextEnv, type TextStyle } from "./text";

/** Title element → text block; uses autoText when there is no text (returns null if that is missing too) */
export function titleBlock(title: Element, base: TextStyle, env: TextEnv, autoText: string | null, maxWidth?: number): Block | null {
  const style = txPrStyle(base, kid(title, "txPr"), env);
  const tx = kid(title, "tx");
  const rich = kid(tx, "rich");
  let lines: Line[] | null = null;
  if (rich) lines = richLines(rich, style, env);
  else {
    const text = strRefText(tx) ?? kid(kid(tx, "txData"), "v")?.textContent ?? kid(tx, "v")?.textContent ?? null;
    if (text) lines = plainLines(text, style);
  }
  if ((!lines || lines.every((l) => l.every((r) => !r.text))) && autoText) lines = plainLines(autoText, style);
  if (!lines || lines.every((l) => l.every((r) => !r.text))) return null;
  if (maxWidth) lines = wrapLines(lines, maxWidth);
  return layoutBlock(lines);
}
