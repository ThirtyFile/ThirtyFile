// Folders in pages: a newer first page while files are uploaded
import { describe, expect, test } from "vitest";
import { QueryClient, QueryObserver, type InfiniteData } from "@tanstack/react-query";
import type { CursorPage } from "@/api";
import { allItems, refreshFirstPage, withFirstPage } from "@/lib/pages";

type Item = { id: string; kind: "file" | "folder" };
const file = (id: string): Item => ({ id, kind: "file" });
const folder = (id: string): Item => ({ id, kind: "folder" });
const list = (...pages: Item[][]): InfiniteData<CursorPage<Item>> => ({
  pages: pages.map((items, i) => ({ items, next: i < pages.length - 1 ? `c${i}` : null })),
  pageParams: pages.map((_, i) => (i ? `c${i - 1}` : null)),
});

describe("withFirstPage", () => {
  test("a new item shows, and the one it pushed out of the first page stays", () => {
    const data = list([file("a"), file("c")], [file("d")]);
    const next = withFirstPage(data, { items: [file("a"), file("b")], next: "other" });
    expect(allItems(next).map((n) => n.id)).toEqual(["a", "b", "c", "d"]);
    // The next page still starts where it did
    expect(next.pages[0].next).toBe("c0");
    expect(next.pageParams).toBe(data.pageParams);
  });
});


describe("refreshFirstPage", () => {
  test("replaces the first page of lists shown, and leaves other lists alone", async () => {
    const qc = new QueryClient();
    const key = ["children", "f", "name", "asc"];
    qc.setQueryData(key, list([file("a")], [file("c")]));
    qc.setQueryData(["children", "f", "folders"], [folder("x")]);
    // Shown: an observer keeps the list active (its data is fresh, so nothing is fetched)
    const shown = (queryKey: string[]) => new QueryObserver(qc, { queryKey, queryFn: () => new Promise<never>(() => {}), staleTime: Infinity }).subscribe(() => {});
    const unsubscribe = [shown(key), shown(["children", "f", "folders"])];
    const asked: unknown[] = [];
    await refreshFirstPage<Item>(qc, ["children", "f"], (k, limit) => {
      asked.push([k, limit]);
      return Promise.resolve({ items: [file("a"), file("b")], next: null });
    });
    expect(allItems(qc.getQueryData<InfiniteData<CursorPage<Item>>>(key)).map((n) => n.id)).toEqual(["a", "b", "c"]);
    expect(qc.getQueryData(["children", "f", "folders"])).toEqual([folder("x")]);
    expect(asked).toEqual([[key, 200]]);
    unsubscribe.forEach((u) => u());
  });
});
