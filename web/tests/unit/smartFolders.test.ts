// Smart folders (lib/smartFolders.ts): a search saved as one, the dialog's fields and the query they make, keeping the
// list of smart folders in order, and selecting a span of one a batch at a time (lib/span.ts)
import { afterEach, describe, expect, test, vi } from "vitest";
import { QueryClient } from "@tanstack/react-query";
import { api, type SmartFolder, type SmartQuery } from "@/api";
import { keys } from "@/api/queryKeys";
import { MB } from "@/lib/searchFilters";
import { createSmartFolder, dayOf, dayStart, deleteSmartFolder, formOf, queryFromSearch, queryOf, updateSmartFolder } from "@/lib/smartFolders";
import { batchesOf, smartListing, smartOf } from "@/lib/span";
import { refreshFiles } from "@/lib/queries";

afterEach(() => vi.restoreAllMocks());

describe("a search saved as a smart folder", () => {
  test("keeps the term, the folder, the type, the size, the period and the tag", () => {
    expect(queryFromSearch({ term: " report ", within: "f1", type: "doc", date: "week", size: "medium", tag: "3" })).toEqual({
      name: "report",
      ext: "doc,docx,odt,rtf,pdf,txt,md",
      min_size: MB,
      max_size: 100 * MB,
      modified_days: 7,
      scope: { kind: "folder", id: "f1" },
      tags: [3],
    });
    // Folders, and everywhere with nothing else chosen
    expect(queryFromSearch({ term: "x", type: "folder" })).toEqual({ name: "x", kind: "folder", scope: { kind: "all" } });
    // A tag alone
    expect(queryFromSearch({ term: "", tag: "5" })).toEqual({ scope: { kind: "all" }, tags: [5] });
  });
});

describe("the dialog's fields", () => {
  const roundTrip = (q: SmartQuery) => queryOf(formOf("Name", q));

  test("the search's choices show as they are, and give the same query back", () => {
    const q: SmartQuery = { name: "a", ext: "ppt,pptx,odp", max_size: MB - 1, modified_days: 30, scope: { kind: "space", id: "d1" }, tags: [1, 2] };
    const f = formOf("Slides", q);
    expect([f.name, f.term, f.type, f.size, f.date, f.scope, f.tags]).toEqual(["Slides", "a", "slides", "small", "month", "space:d1", [1, 2]]);
    expect(roundTrip(q)).toEqual(q);
    expect(roundTrip({ kind: "file", scope: { kind: "all" } })).toEqual({ kind: "file", scope: { kind: "all" } });
  });

  test("what the choices don't have: other types, a period, dates and sizes", () => {
    const from = dayStart("2026-01-10")!;
    const to = dayStart("2026-02-01")!;
    const q: SmartQuery = { ext: "psd,ai", min_size: 2 * MB, max_size: 5.5 * MB, modified_from: from, modified_to: to, scope: { kind: "folder", id: "f" } };
    const f = formOf("", q);
    expect([f.type, f.ext, f.size, f.minMb, f.maxMb, f.date, f.from, f.to]).toEqual(["custom", "psd,ai", "range", "2", "5.5", "range", "2026-01-10", "2026-01-31"]);
    expect(roundTrip(q)).toEqual(q);
    // A period the choices don't have stays as it is
    const days = formOf("", { modified_days: 14, scope: { kind: "all" } });
    expect([days.date, days.days]).toEqual(["days", 14]);
    expect(queryOf(days).modified_days).toBe(14);
  });

  test("the last day of a range is counted in full, and empty fields are left out", () => {
    const f = { ...formOf("", { scope: { kind: "all" } }), date: "range", to: "2026-03-01", term: "  ", size: "range", minMb: "", maxMb: "1" };
    const q = queryOf(f);
    expect(q).toEqual({ modified_to: dayStart("2026-03-02"), max_size: MB, scope: { kind: "all" } });
    expect(dayOf(dayStart("2026-03-02")!)).toBe("2026-03-02");
  });
});

describe("the list of smart folders", () => {
  const folder = (id: number, name: string): SmartFolder => ({ id, name, query: { name: "x", scope: { kind: "all" } } });

  test("stays in order by name after making, changing and deleting one; a changed query loads its items again", async () => {
    const qc = new QueryClient();
    qc.setQueryData(keys.smartFolders(), [folder(1, "B"), folder(2, "d")]);
    vi.spyOn(api, "createSmartFolder").mockResolvedValue(folder(3, "a 10"));
    vi.spyOn(api, "updateSmartFolder").mockResolvedValue(folder(2, "a 9"));
    vi.spyOn(api, "deleteSmartFolder").mockResolvedValue(undefined);
    const names = () => qc.getQueryData<SmartFolder[]>(keys.smartFolders())!.map((f) => f.name);
    await createSmartFolder(qc, "a 10", { name: "x", scope: { kind: "all" } });
    expect(names()).toEqual(["a 10", "B", "d"]);
    qc.setQueryData([...keys.smartAt(2, "name", "asc"), 0], { items: [], next: null, total: 0 });
    await updateSmartFolder(qc, 2, { name: "a 9", query: { kind: "file", scope: { kind: "all" } } });
    expect(names()).toEqual(["a 9", "a 10", "B"]);
    expect(qc.getQueryData([...keys.smartAt(2, "name", "asc"), 0]), "its parts are loaded again").toBeUndefined();
    await deleteSmartFolder(qc, 1);
    expect(names()).toEqual(["a 9", "a 10"]);
  });
});

describe("a span of a smart folder", () => {
  test("is selected a batch at a time from the smart folder", async () => {
    expect(smartOf(smartListing(12))).toBe(12);
    expect(smartOf("abc")).toBeNull();
    const smart = vi
      .spyOn(api, "smartSelection")
      .mockResolvedValueOnce({ ids: ["a", "b"], next: "c1" })
      .mockResolvedValueOnce({ ids: ["c"], next: null });
    const folder = vi.spyOn(api, "selection");
    const span = { folder: smartListing(12), sort: "name" as const, order: "asc" as const, except: new Set(["x"]), count: 3 };
    const got: string[][] = [];
    for await (const ids of batchesOf({ ids: [], span, count: 3 })) got.push(ids);
    expect(got).toEqual([["a", "b"], ["c"]]);
    expect(smart.mock.calls.map((c) => [c[0], c[1].after, c[1].except])).toEqual([
      [12, undefined, ["x"]],
      [12, "c1", ["x"]],
    ]);
    expect(folder).not.toHaveBeenCalled();
  });

  test("a change to it loads its lists again, as a folder's", async () => {
    const qc = new QueryClient();
    const key = [...keys.smartAt(12, "name", "asc"), 0];
    qc.setQueryData(key, { items: [{ id: "a" }], next: null, total: 1 });
    const other = [...keys.smartAt(13, "name", "asc"), 0];
    qc.setQueryData(other, { items: [{ id: "b" }], next: null, total: 1 });
    await refreshFiles(qc, { folders: [smartListing(12)], trash: true });
    expect(qc.getQueryState(key)?.isInvalidated).toBe(true);
    // Moving into folders can change what any smart folder finds
    expect(qc.getQueryState(other)?.isInvalidated).toBe(true);
  });
});
