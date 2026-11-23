/** The page grid's choices, apart from how they are shown: which pages the new PDF has, in which order, turned how far */

/** A page of the new PDF: `page` counts from 0 in the original, `turn` is degrees clockwise on top of its own rotation */
export interface Entry {
  /** What tells entries apart in the grid (a page of the original can't be told apart from itself) */
  key: number;
  page: number;
  turn: number;
  selected: boolean;
}

/** One entry for each of `count` pages, in order, as they are */
export function initialEntries(count: number): Entry[] {
  return Array.from({ length: count }, (_, i) => ({ key: i, page: i, turn: 0, selected: false }));
}

/** Turns the pages by a quarter turn (`dir` 1: clockwise) */
export function rotate(entries: Entry[], keys: ReadonlySet<number>, dir: 1 | -1): Entry[] {
  return entries.map((e) => (keys.has(e.key) ? { ...e, turn: (e.turn + 90 * dir + 360) % 360 } : e));
}

/** Moves the entry `key` to stand before the entry `before` (or at the end, when `before` is null) */
export function moveBefore(entries: Entry[], key: number, before: number | null): Entry[] {
  const moving = entries.find((e) => e.key === key);
  if (!moving || key === before) return entries;
  const rest = entries.filter((e) => e.key !== key);
  const at = before === null ? rest.length : rest.findIndex((e) => e.key === before);
  if (at < 0) return entries;
  return [...rest.slice(0, at), moving, ...rest.slice(at)];
}

/** Moves the entry `key` one place earlier (-1) or later (1) */
export function step(entries: Entry[], key: number, dir: -1 | 1): Entry[] {
  const i = entries.findIndex((e) => e.key === key);
  const j = i + dir;
  if (i < 0 || j < 0 || j >= entries.length) return entries;
  const next = [...entries];
  [next[i], next[j]] = [next[j], next[i]];
  return next;
}

export function remove(entries: Entry[], keys: ReadonlySet<number>): Entry[] {
  return entries.filter((e) => !keys.has(e.key));
}

/** Whether the new PDF differs from the original: pages left out, moved or turned */
export function changed(entries: Entry[], count: number): boolean {
  return entries.length !== count || entries.some((e, i) => e.page !== i || e.turn !== 0);
}

/** What the server is asked for: the pages in order (of all of them, or of those selected) */
export function picks(entries: Entry[], onlySelected = false) {
  return entries.filter((e) => !onlySelected || e.selected).map((e) => ({ page: e.page, rotate: e.turn }));
}
