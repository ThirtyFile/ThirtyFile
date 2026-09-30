//! How the file list is laid out: its views, their rows, and where each row is

import type { Node } from "@/api";
import type { Group } from "@/lib/listView";

/** "list" is Details and "grid" Large icons (the names they were saved under); "compact" is File Explorer's List */
export type ViewMode = "list" | "grid" | "medium" | "compact" | "tiles";
/** An item of the list: a file or folder, with where it is in lists of several places */
export type Item = Node & { location?: string };

/** Details view: height of a row (its cells are h-7), of the column headers and of a group's heading */
export const ROW = 28;
export const HEAD = 30;
export const GROUP_ROW = 32;
/** Icon views: items of one size (at least `w` wide, `h` high, `gap` apart), so rows are placed and hit-tested by their index */
export const TILED: Record<Exclude<ViewMode, "list">, { w: number; h: number; gap: number }> = {
  grid: { w: 116, h: 148, gap: 8 },
  medium: { w: 88, h: 112, gap: 6 },
  tiles: { w: 250, h: 72, gap: 6 },
  compact: { w: 220, h: 26, gap: 2 },
};
export const PAD = 12;
/** Large icons on phones: the smallest width of an item */
export const PHONE_GRID_W = 100;
/** Icon views: height of a group's heading, with the space above it */
export const GROUP_H = 36;
/** The nearest scrolling ancestor, which the list is virtualised against */
export function scrollParent(el: HTMLElement): HTMLElement {
  for (let p = el.parentElement; p; p = p.parentElement) {
    const o = getComputedStyle(p).overflowY;
    if (o === "auto" || o === "scroll") return p;
  }
  return document.documentElement;
}

/** An element's position in a scroll container's content */
export function offsetIn(el: Element, container: HTMLElement) {
  const r = el.getBoundingClientRect();
  const c = container === document.documentElement ? { top: 0, left: 0 } : container.getBoundingClientRect();
  return { top: r.top - c.top + container.scrollTop, left: r.left - c.left + container.scrollLeft, width: r.width };
}

/** First and last of `count` boxes (`size` long, `stride` apart from `start`) that touch the span lo–hi */
export function touching(start: number, size: number, stride: number, count: number, lo: number, hi: number): [number, number] {
  return [Math.max(0, Math.ceil((lo - start - size) / stride)), Math.min(count - 1, Math.floor((hi - start) / stride))];
}

/** A row of the layout: a group's heading, or items (one in Details, a row of them in the icon views) */
export interface LayoutRow {
  /** The items it holds, from `start` up to `end` (in the order shown) */
  start: number;
  end: number;
  /** Height, with the gap below it */
  size: number;
  /** Where it starts, from the top of the first row */
  top: number;
  group?: Group<Item>;
}

/** The rows of the list: worked out as needed when all rows are alike, so a folder of any size costs nothing to lay out */
export interface Layout {
  count: number;
  row(r: number): LayoutRow;
  /** The row holding the item at a position */
  rowOf(index: number): number;
  /** The first row that reaches below `y` (from the top of the first row) */
  rowAt(y: number): number;
}

export function evenLayout(n: number, cols: number, size: number): Layout {
  const count = Math.ceil(n / cols);
  return {
    count,
    row: (r) => ({ start: r * cols, end: Math.min(n, r * cols + cols), size, top: r * size }),
    rowOf: (i) => Math.floor(i / cols),
    rowAt: (y) => Math.max(0, Math.min(count - 1, Math.floor(y / size))),
  };
}

export function groupedLayout(groups: Group<Item>[], cols: number, size: number, headSize: number): Layout {
  const rows: LayoutRow[] = [];
  const rowOf: number[] = [];
  let top = 0;
  const add = (r: Omit<LayoutRow, "top">) => {
    rows.push({ ...r, top });
    top += r.size;
  };
  let start = 0;
  for (const group of groups) {
    const end = start + group.items.length;
    add({ start, end: start, size: headSize, group });
    for (let i = start; i < end; i += cols) {
      const last = Math.min(i + cols, end);
      for (let k = i; k < last; k++) rowOf[k] = rows.length;
      add({ start: i, end: last, size });
    }
    start = end;
  }
  return { count: rows.length, row: (r) => rows[r], rowOf: (i) => rowOf[i], rowAt: () => 0 };
}
