// The Columns view's trail of folders, the widths of its columns, and what happens on arriving in another column
// (lib/columns.ts)
import { afterEach, describe, expect, test, vi } from "vitest";
import { arriveAt, depthOf, KEPT_WIDTHS, MAX_COLUMN_WIDTH, MIN_COLUMN_WIDTH, openFrom, selectsOnArrival, takeArrival, trailFor, validWidths, withWidth } from "@/lib/columns";

const c = (id: string) => ({ id, name: id.toUpperCase() });
const ids = (trail: readonly { id: string }[]) => trail.map((x) => x.id).join("/");

describe("the trail", () => {
  test("a folder shows a column for each folder of its path", () => {
    expect(ids(trailFor(null, [c("root"), c("a"), c("b")]))).toBe("root/a/b");
  });

  test("returning to a folder on the trail keeps the columns opened beyond it", () => {
    const kept = [c("root"), c("a"), c("b"), c("c")];
    expect(trailFor(kept, [c("root"), c("a")])).toBe(kept);
    expect(trailFor(kept, [c("root"), c("a"), c("b"), c("c")])).toBe(kept);
    // Somewhere else: its own path
    expect(ids(trailFor(kept, [c("root"), c("x")]))).toBe("root/x");
    expect(ids(trailFor(kept, [c("other")]))).toBe("other");
  });

  test("a folder renamed since takes its new name, and the columns beyond stay", () => {
    const kept = [c("root"), c("a"), c("b")];
    const next = trailFor(kept, [c("root"), { id: "a", name: "Renamed" }]);
    expect(next.map((x) => x.name)).toEqual(["ROOT", "Renamed", "B"]);
  });

  test("opening a folder from a column replaces the columns to its right", () => {
    const trail = [c("root"), c("a"), c("b"), c("c")];
    expect(ids(openFrom(trail, 1, c("x")))).toBe("root/a/x");
    expect(ids(openFrom(trail, 0, c("y")))).toBe("root/y");
    // The folder already open there: the columns beyond stay
    expect(openFrom(trail, 1, c("b"))).toBe(trail);
    // A file or several items selected: nothing opens to the right
    expect(ids(openFrom(trail, 1, null))).toBe("root/a");
    const short = [c("root"), c("a")];
    expect(openFrom(short, 1, null)).toBe(short);
  });

  test("where a folder is on it", () => {
    const trail = [c("root"), c("a")];
    expect(depthOf(trail, "a")).toBe(1);
    expect(depthOf(trail, "x")).toBe(-1);
    expect(depthOf(trail, undefined)).toBe(-1);
  });
});

describe("column widths", () => {
  test("are kept within limits, and back to the usual width when reset", () => {
    expect(withWidth({}, "a", 10)).toEqual({ a: MIN_COLUMN_WIDTH });
    expect(withWidth({}, "a", 10_000)).toEqual({ a: MAX_COLUMN_WIDTH });
    expect(withWidth({ a: 300, b: 250.4 }, "b", 260.6)).toEqual({ a: 300, b: 261 });
    expect(withWidth({ a: 300, b: 250 }, "a", undefined)).toEqual({ b: 250 });
  });

  test("are kept for the folders resized last", () => {
    let widths = {};
    for (let i = 0; i < KEPT_WIDTHS + 5; i++) widths = withWidth(widths, `f${i}`, 300);
    expect(Object.keys(widths)).toHaveLength(KEPT_WIDTHS);
    expect(widths).not.toHaveProperty("f0");
    expect(widths).toHaveProperty(`f${KEPT_WIDTHS + 4}`);
    // Resizing one again keeps it longest
    widths = withWidth(widths, "f5", 400);
    expect(Object.keys(widths).at(-1)).toBe("f5");
  });

  test("only numbers are read back", () => {
    expect(validWidths({ a: 300 })).toBe(true);
    expect(validWidths({ a: "wide" } as never)).toBe(false);
    expect(validWidths({ a: Number.NaN })).toBe(false);
  });
});

describe("arriving in another column", () => {
  afterEach(() => {
    vi.useRealTimers();
    takeArrival("a");
  });

  test("is done once, in the folder it was planned for", () => {
    arriveAt({ folder: "a", select: "x", focus: true });
    expect(selectsOnArrival("a")).toBe(true);
    expect(selectsOnArrival("b")).toBe(false);
    expect(takeArrival("b")).toBeNull();
    expect(takeArrival("a")).toMatchObject({ folder: "a", select: "x", focus: true });
    expect(takeArrival("a")).toBeNull();
    expect(selectsOnArrival("a")).toBe(false);
  });

  test("opening a column's folder with nothing selected selects nothing there", () => {
    arriveAt({ folder: "a" });
    expect(selectsOnArrival("a")).toBe(false);
    arriveAt({ folder: "a", first: true });
    expect(selectsOnArrival("a")).toBe(true);
  });

  test("is forgotten when its folder didn't open soon", () => {
    vi.useFakeTimers();
    arriveAt({ folder: "a", select: "x" });
    vi.advanceTimersByTime(60_000);
    expect(takeArrival("a")).toBeNull();
  });
});
