// A large folder a part at a time (lib/windows), and what is selected in it without being loaded (lib/span)
import { afterEach, describe, expect, test, vi } from "vitest";
import { api } from "@/api";
import { assemble, KEEP, neighbours, toKeep, WINDOW } from "@/lib/windows";
import { batchesOf, chunks, eachBatch, inSpan, spanCount, type FolderSpan } from "@/lib/span";

type Item = { id: string; kind: "file" | "folder" };
const item = (id: string, kind: Item["kind"] = "file"): Item => ({ id, kind });
const ids = (list: readonly (Item | undefined)[]) => Array.from({ length: list.length }, (_, i) => list[i]?.id ?? null);

afterEach(() => vi.restoreAllMocks());

describe("assemble", () => {
  test("puts each part's items at their positions, and leaves the rest empty", () => {
    const built = assemble([
      { start: 4, items: [item("e"), item("f")], total: 8, at: 2 },
      { start: 0, items: [item("a"), item("b")], total: 8, at: 1 },
    ]);
    expect(built.total).toBe(8);
    expect(ids(built.at)).toEqual(["a", "b", null, null, "e", "f", null, null]);
    expect(built.loaded.map((n) => n.id)).toEqual(["a", "b", "e", "f"]);
    expect(built.index.get("e")).toBe(4);
    expect(built.outdated).toEqual([]);
  });

  test("leaves out parts loaded when the folder had another number of items, and shows an item found twice once", () => {
    const built = assemble([
      // Loaded first, when there were 6 items
      { start: 0, items: [item("a"), item("b"), item("c")], total: 6, at: 1 },
      // Since then an item was added before "c": the newest part says there are 7
      { start: 3, items: [item("c"), item("d")], total: 7, at: 3 },
      { start: 1, items: [item("b"), item("x"), item("c")], total: 7, at: 2 },
    ]);
    expect(built.total).toBe(7);
    expect(built.outdated).toEqual([0]);
    // "c" is where the newest part has it; its older place stays empty until that part loads again
    expect(ids(built.at)).toEqual([null, "b", "x", "c", "d", null, null]);
  });

  test("knows nothing before the first part", () => {
    expect(assemble([])).toMatchObject({ total: -1, loaded: [], at: [] });
  });
});

describe("toKeep", () => {
  test("keeps the first part and those nearest the parts shown", () => {
    const starts = Array.from({ length: 40 }, (_, i) => i * WINDOW);
    const keep = toKeep(starts, [30, 30]);
    expect(keep.size).toBe(KEEP);
    expect(keep.has(0)).toBe(true);
    expect(keep.has(30 * WINDOW)).toBe(true);
    expect(keep.has(2 * WINDOW)).toBe(false);
    expect(toKeep([0, WINDOW], [0, 0])).toEqual(new Set([0, WINDOW]));
  });
});

describe("neighbours", () => {
  test("are the files before and after, past folders, as far as the list is loaded", () => {
    const list = [item("f1", "folder"), item("a"), item("f2", "folder"), item("b"), undefined, item("c")];
    expect(neighbours(list, 3)).toEqual({ prev: item("a"), next: undefined });
    expect(neighbours(list, 1)).toEqual({ prev: undefined, next: item("b") });
    expect(neighbours(list, 5)).toEqual({ prev: undefined, next: undefined });
    expect(neighbours(list, null)).toEqual({});
  });
});

describe("spans", () => {
  const span = (from?: number, to?: number, except: string[] = []): FolderSpan => ({
    folder: "f",
    sort: "name",
    order: "asc",
    from: from === undefined ? undefined : { id: `i${from}`, index: from },
    to: to === undefined ? undefined : { id: `i${to}`, index: to },
    except: new Set(except),
    count: 0,
  });

  test("hold the items from one position to another, less those left out", () => {
    expect(inSpan(span(), 99_999, "x")).toBe(true);
    expect(inSpan(span(10, 20), 9, "x")).toBe(false);
    expect(inSpan(span(10, 20), 20, "x")).toBe(true);
    expect(inSpan(span(10, 20, ["x"]), 15, "x")).toBe(false);
    expect(inSpan(null, 0, "x")).toBe(false);
    expect(spanCount(span(), 100_000)).toBe(100_000);
    expect(spanCount(span(undefined, undefined, ["a", "b"]), 100_000)).toBe(99_998);
    expect(spanCount(span(10, 20, ["a"]), 100_000)).toBe(10);
    expect(spanCount(span(90), 100)).toBe(10);
  });

  test("are asked of the server a batch at a time, after the items picked one by one", async () => {
    const pages = [
      { ids: ["s1", "s2"], next: "c1" },
      { ids: ["s3"], next: null },
    ];
    const selection = vi.spyOn(api, "selection").mockImplementation(async () => pages.shift()!);
    const got: string[][] = [];
    for await (const batch of batchesOf({ ids: ["p1"], span: span(5, 9, ["x"]), count: 4 })) got.push(batch);
    expect(got).toEqual([["p1"], ["s1", "s2"], ["s3"]]);
    expect(selection.mock.calls).toEqual([
      ["f", { sort: "name", order: "asc", from: "i5", to: "i9", except: ["x"], after: undefined }],
      ["f", { sort: "name", order: "asc", from: "i5", to: "i9", except: ["x"], after: "c1" }],
    ]);
  });

  test("an item picked one by one that the span holds too is changed once", async () => {
    vi.spyOn(api, "selection").mockResolvedValue({ ids: ["p1", "s1"], next: null });
    const got: string[][] = [];
    for await (const batch of batchesOf({ ids: ["p1", "p2"], span: span(5, 9), count: 3 })) got.push(batch);
    expect(got).toEqual([["p1", "p2"], ["s1"]]);
  });

  test("a change is made batch after batch, each asked for after the one before is done", async () => {
    const order: string[] = [];
    vi.spyOn(api, "selection").mockImplementation(async (_, req) => {
      order.push(`ask ${req.after ?? "first"}`);
      return req.after ? { ids: ["b"], next: null } : { ids: ["a"], next: "after-a" };
    });
    const done = await eachBatch({ ids: [], span: span(), count: 2 }, "Working…", async (batch) => void order.push(`change ${batch.join()}`));
    expect(done).toBe(2);
    expect(order).toEqual(["ask first", "change a", "ask after-a", "change b"]);
  });

  test("items picked one by one go 1,000 at a time", () => {
    const many = Array.from({ length: 2500 }, (_, i) => `n${i}`);
    expect(chunks(many).map((c) => c.length)).toEqual([1000, 1000, 500]);
  });
});
