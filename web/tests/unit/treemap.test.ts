// lib/treemap.ts: the rectangles fill the area, follow the sizes, and stay near squares
import { describe, expect, test } from "vite-plus/test";
import { squarify, type Placed, type Rect } from "@/lib/treemap";

const BOX: Rect = { x: 10, y: 20, w: 600, h: 400 };
const area = (r: Rect) => r.w * r.h;
const overlap = (a: Rect, b: Rect) => Math.max(0, Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x)) * Math.max(0, Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y));
const near = (a: number, b: number) => Math.abs(a - b) < 1e-6 * Math.max(1, Math.abs(b));

function check(placed: Placed<number>[], total: number) {
  expect(
    near(
      placed.reduce((s, p) => s + area(p), 0),
      area(BOX),
    ),
  ).toBe(true);
  for (const p of placed) {
    expect(p.x).toBeGreaterThanOrEqual(BOX.x - 1e-9);
    expect(p.y).toBeGreaterThanOrEqual(BOX.y - 1e-9);
    expect(p.x + p.w).toBeLessThanOrEqual(BOX.x + BOX.w + 1e-9);
    expect(p.y + p.h).toBeLessThanOrEqual(BOX.y + BOX.h + 1e-9);
    expect(near(area(p), (p.item / total) * area(BOX))).toBe(true);
  }
  for (let i = 0; i < placed.length; i++) for (let j = i + 1; j < placed.length; j++) expect(overlap(placed[i], placed[j])).toBeLessThan(1e-6);
}

describe("squarify", () => {
  test("the rectangles fill the box without overlapping, each in proportion to its size", () => {
    const sizes = [6, 6, 4, 3, 2, 2, 1];
    const placed = squarify(sizes, (v) => v, BOX);
    expect(placed).toHaveLength(sizes.length);
    check(
      placed,
      sizes.reduce((a, b) => a + b, 0),
    );
  });

  test("keeps the order of the sizes, largest first, whatever order they come in", () => {
    const placed = squarify([2, 9, 4, 1], (v) => v, BOX);
    expect(placed.map((p) => p.item)).toEqual([9, 4, 2, 1]);
  });

  test("many items of very different sizes still fill the box", () => {
    const sizes = Array.from({ length: 200 }, (_, i) => Math.round(10000 / (i + 1)) + 1);
    const placed = squarify(sizes, (v) => v, BOX);
    expect(placed).toHaveLength(200);
    check(
      placed,
      sizes.reduce((a, b) => a + b, 0),
    );
  });

  test("the shapes stay near squares for sizes that are alike", () => {
    const placed = squarify(
      Array.from({ length: 16 }, () => 1),
      (v) => v,
      { x: 0, y: 0, w: 400, h: 400 },
    );
    for (const p of placed) expect(Math.max(p.w / p.h, p.h / p.w)).toBeLessThan(1.01);
  });

  test("one item takes the whole box, and a long thin box still works", () => {
    const [only] = squarify([5], (v) => v, BOX);
    expect([only.x, only.y, only.w, only.h]).toEqual([BOX.x, BOX.y, BOX.w, BOX.h]);
    const thin: Rect = { x: 0, y: 0, w: 1000, h: 20 };
    const placed = squarify([5, 3, 2], (v) => v, thin);
    expect(
      near(
        placed.reduce((s, p) => s + area(p), 0),
        area(thin),
      ),
    ).toBe(true);
  });

  test("items without a size, and nothing at all, leave nothing to draw", () => {
    expect(squarify([0, -3, NaN], (v) => v, BOX)).toEqual([]);
    expect(squarify([], (v: number) => v, BOX)).toEqual([]);
    expect(squarify([1, 2], (v) => v, { x: 0, y: 0, w: 0, h: 10 })).toEqual([]);
    expect(squarify([0, 4], (v) => v, BOX)).toHaveLength(1);
  });

  test("works on what the items carry", () => {
    const placed = squarify(
      [
        { name: "a", bytes: 3 },
        { name: "b", bytes: 1 },
      ],
      (i) => i.bytes,
      BOX,
    );
    expect(placed.map((p) => p.item.name)).toEqual(["a", "b"]);
  });
});
