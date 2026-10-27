//! The layout of a treemap: nested rectangles whose areas follow the sizes (the squarified algorithm of Bruls, Huizing
//! and van Wijk: rows of rectangles that stay as close to squares as they can), no chart library.

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Placed<T> extends Rect {
  item: T;
}

/** The worst (largest) aspect ratio of a row of areas laid along a side of length `side` */
function worst(areas: number[], side: number): number {
  const sum = areas.reduce((a, b) => a + b, 0);
  if (sum <= 0 || side <= 0) return Infinity;
  const max = Math.max(...areas);
  const min = Math.min(...areas);
  return Math.max((side * side * max) / (sum * sum), (sum * sum) / (side * side * min));
}

/**
 * Places `items` (their `value`s, not necessarily sorted) in `rect` so that each one's area is its share of the total.
 * Items with no value are left out. The result is in the order of the values, largest first.
 */
export function squarify<T>(items: T[], value: (item: T) => number, rect: Rect): Placed<T>[] {
  const sized = items.map((item) => ({ item, v: value(item) })).filter((i) => i.v > 0 && Number.isFinite(i.v));
  sized.sort((a, b) => b.v - a.v);
  const total = sized.reduce((a, b) => a + b.v, 0);
  if (!sized.length || rect.w <= 0 || rect.h <= 0) return [];
  const scale = (rect.w * rect.h) / total;
  const out: Placed<T>[] = [];
  let free = { ...rect };
  let row: { item: T; area: number }[] = [];
  const queue = sized.map((i) => ({ item: i.item, area: i.v * scale }));

  /** Lays the row along the shorter side of what is free, and takes that strip off */
  const place = () => {
    const sum = row.reduce((a, b) => a + b.area, 0);
    if (free.w >= free.h) {
      // A column on the left
      const w = sum / free.h;
      let y = free.y;
      for (const r of row) {
        const h = r.area / w;
        out.push({ item: r.item, x: free.x, y, w, h });
        y += h;
      }
      free = { x: free.x + w, y: free.y, w: free.w - w, h: free.h };
    } else {
      // A row on top
      const h = sum / free.w;
      let x = free.x;
      for (const r of row) {
        const w = r.area / h;
        out.push({ item: r.item, x, y: free.y, w, h });
        x += w;
      }
      free = { x: free.x, y: free.y + h, w: free.w, h: free.h - h };
    }
    row = [];
  };

  for (const next of queue) {
    const side = Math.min(free.w, free.h);
    const current = row.map((r) => r.area);
    if (row.length === 0 || worst([...current, next.area], side) <= worst(current, side)) {
      row.push(next);
    } else {
      place();
      row.push(next);
    }
  }
  if (row.length) place();
  return out;
}
