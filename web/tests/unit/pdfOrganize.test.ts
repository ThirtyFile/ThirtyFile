// The page grid's choices (lib/pdfOrganize.ts): turning, moving, deleting, and what the server is asked for
import { describe, expect, test } from "vite-plus/test";
import { changed, initialEntries, moveBefore, picks, remove, rotate, step } from "@/lib/pdfOrganize";

const pages = (entries: { page: number }[]) => entries.map((e) => e.page);

describe("page grid", () => {
  test("a new grid has every page, in order, and nothing is changed", () => {
    const e = initialEntries(4);
    expect(pages(e)).toEqual([0, 1, 2, 3]);
    expect(changed(e, 4)).toBe(false);
  });

  test("a page is moved before another, or to the end", () => {
    const e = initialEntries(4);
    expect(pages(moveBefore(e, 3, 1))).toEqual([0, 3, 1, 2]);
    expect(pages(moveBefore(e, 0, null))).toEqual([1, 2, 3, 0]);
    expect(pages(moveBefore(e, 2, 2))).toEqual([0, 1, 2, 3]);
    expect(pages(moveBefore(e, 9, 1))).toEqual([0, 1, 2, 3]);
  });

  test("a page steps one place and stops at the ends", () => {
    const e = initialEntries(3);
    expect(pages(step(e, 1, -1))).toEqual([1, 0, 2]);
    expect(pages(step(e, 0, -1))).toEqual([0, 1, 2]);
    expect(pages(step(e, 2, 1))).toEqual([0, 1, 2]);
  });

  test("pages turn by quarter turns that wrap around", () => {
    let e = rotate(initialEntries(2), new Set([1]), -1);
    expect(e.map((x) => x.turn)).toEqual([0, 270]);
    e = rotate(rotate(e, new Set([1]), 1), new Set([1]), 1);
    expect(e.map((x) => x.turn)).toEqual([0, 90]);
    expect(changed(e, 2)).toBe(true);
  });

  test("deleted pages are left out, and the ask carries the order and the turns", () => {
    let e = initialEntries(4);
    e = rotate(e, new Set([2]), 1);
    e = remove(e, new Set([1]));
    expect(changed(e, 4)).toBe(true);
    expect(picks(e)).toEqual([
      { page: 0, rotate: 0 },
      { page: 2, rotate: 90 },
      { page: 3, rotate: 0 },
    ]);
    e = e.map((x) => ({ ...x, selected: x.page === 3 }));
    expect(picks(e, true)).toEqual([{ page: 3, rotate: 0 }]);
  });
});
