/**
 * Tabs: widths are computed from actual positions after layout (left, center, right, decimal tabs, leader dots),
 * plus width correction for horizontally scaled text (w:w).
 *
 * The n-th tab of each paragraph is handled in round n; each round reads all positions first and then writes them at once to avoid repeated reflows.
 */

import type { TabRef } from "./context";

const LEADER_CLASS: Record<string, string> = {
  dot: "tf-docx-lead-dot",
  middleDot: "tf-docx-lead-mid",
  hyphen: "tf-docx-lead-dash",
  underscore: "tf-docx-lead-line",
  heavy: "tf-docx-lead-line",
};

/** Width of the text after a tab up to the next tab (or paragraph end), same line only */
function segmentWidth(tab: HTMLElement, next: HTMLElement | null, p: HTMLElement, top: number): number {
  const range = document.createRange();
  range.setStartAfter(tab);
  if (next) range.setEndBefore(next);
  else range.setEnd(p, p.childNodes.length);
  const start = tab.getBoundingClientRect().right;
  let right = start;
  for (const r of Array.from(range.getClientRects())) {
    // Same line only (stop where the next line begins)
    if (r.top > top + Math.max(4, r.height * 0.6)) continue;
    if (r.width) right = Math.max(right, r.right);
  }
  return right - start;
}

interface Plan {
  ref: TabRef;
  width: number;
  leader?: string;
}

function plan(ref: TabRef, next: HTMLElement | null): Plan {
  const rect = ref.el.getBoundingClientRect();
  const pRect = ref.p.getBoundingClientRect();
  const origin = pRect.left - ref.pLeft;
  const x = rect.left - origin;
  const avail = pRect.right - origin;
  if (ref.ptab) {
    const seg = segmentWidth(ref.el, next, ref.p, rect.top);
    const target = ref.ptab.align === "center" ? ref.ptab.width / 2 - seg / 2 : ref.ptab.align === "right" ? ref.ptab.width - seg : 0;
    return { ref, width: Math.max(0, target - x) };
  }
  // Next tab stop: custom tabs → hanging indent position → default tabs (default tabs left of a custom tab don't count)
  const eps = 0.5;
  let stop = ref.stops.find((s) => s.pos > x + eps && s.val !== "bar");
  // Hanging indent position: also counts as reached when the number text fills it exactly (within 1px)
  if (ref.indent > x - 1 && (!stop || ref.indent < stop.pos)) stop = { pos: Math.max(ref.indent, x), val: "left" };
  if (!stop) {
    const lastCustom = ref.stops.length ? ref.stops[ref.stops.length - 1].pos : 0;
    const dt = Math.max(8, (ref.defaultTab / 1440) * 96);
    const from = Math.max(x, lastCustom);
    const pos = (Math.floor(from / dt + 1e-6) + 1) * dt;
    stop = { pos, val: "left" };
  }
  let width = stop.pos - x;
  if (stop.val === "right" || stop.val === "end" || stop.val === "center" || stop.val === "decimal") {
    const seg = segmentWidth(ref.el, next, ref.p, rect.top);
    if (stop.val === "center") width = stop.pos - x - seg / 2;
    else width = stop.pos - x - seg;
  }
  // Default tab beyond the line width: wrap to the next line (width uses the remaining space)
  if (stop.val === "left" && x + width > avail + 1 && !ref.stops.length) width = Math.max(0, avail - x - 1);
  return { ref, width: Math.max(0, width), leader: stop.leader };
}

export function resolveTabs(groups: TabRef[][]) {
  const live = groups.filter((g) => g.length && g[0].el.isConnected);
  for (let i = 0; ; i++) {
    const batch = live.filter((g) => g.length > i);
    if (!batch.length) break;
    // Read
    const plans = batch.map((g) => plan(g[i], g[i + 1]?.el ?? null));
    // Write
    for (const pl of plans) {
      const el = pl.ref.el;
      el.style.width = `${Math.round(pl.width * 100) / 100}px`;
      const cls = pl.leader ? LEADER_CLASS[pl.leader] : undefined;
      if (cls) el.classList.add(cls);
    }
  }
}

/** Horizontal scale (w:w): transform doesn't affect layout, so compensate with margins based on actual width */
export function fixScaled(els: HTMLElement[]) {
  const live = els.filter((e) => e.isConnected);
  const widths = live.map((e) => e.getBoundingClientRect().width);
  live.forEach((e, i) => {
    const s = Number(e.dataset.sx) || 1;
    e.style.marginRight = `${Math.round(widths[i] * (s - 1) * 100) / 100}px`;
  });
}
