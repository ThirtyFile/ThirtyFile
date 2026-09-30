// What a change to files updates: rows changed in place, and only the lists it touched loaded again
import { afterEach, describe, expect, test } from "vitest";
import { QueryClient, QueryObserver, type InfiniteData, type QueryKey } from "@tanstack/react-query";
import type { CursorPage, Node, NodeInfo } from "@/api";
import { forgetUnreachable, invalidateFiles, refreshFiles, renamed, rowsOf } from "@/lib/queries";

const node = (id: string, parent: string, extra: Partial<Node> = {}): Node => ({
  id,
  parent_id: parent,
  kind: "file",
  name: id,
  size: 1,
  mime: "",
  created_at: 0,
  updated_at: 0,
  trashed_at: null,
  drive_id: "d1",
  owner_name: "",
  is_favorite: false,
  ...extra,
});
const folder = (id: string, parent: string) => node(id, parent, { kind: "folder" });
const pages = (...lists: Node[][]): InfiniteData<CursorPage<Node>> => ({
  pages: lists.map((items, i) => ({ items, next: i < lists.length - 1 ? `c${i}` : null })),
  pageParams: lists.map((_, i) => (i ? `c${i - 1}` : null)),
});
const info = (n: Node, path: string[]): NodeInfo =>
  ({ node: n, path: path.map((id) => ({ id, name: id })), drive: { id: "d1", name: "D", kind: "team", root_id: "root1" }, role: "owner", via_share: false }) as unknown as NodeInfo;

let stop: (() => void)[] = [];
afterEach(() => {
  stop.forEach((s) => s());
  stop = [];
});

/** A cache with lists in it; `shown` lists have an observer (they load again when out of date), and count their loads */
function setup(lists: [QueryKey, unknown, boolean][]) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const loads = new Map<string, number>();
  for (const [key, data, shown] of lists) {
    qc.setQueryData(key, data);
    if (!shown) continue;
    const observer = new QueryObserver(qc, {
      queryKey: key,
      staleTime: Infinity,
      queryFn: () => {
        loads.set(JSON.stringify(key), (loads.get(JSON.stringify(key)) ?? 0) + 1);
        return qc.getQueryData(key) ?? null;
      },
    });
    stop.push(observer.subscribe(() => {}));
  }
  const loaded = (key: QueryKey) => loads.get(JSON.stringify(key)) ?? 0;
  const ids = (key: QueryKey) => rowsOf(qc.getQueryData(key))?.map((n) => n.id);
  const stale = (key: QueryKey) => qc.getQueryState(key)?.isInvalidated ?? false;
  return { qc, loaded, ids, stale };
}

const big = ["children", "big", "name", "asc"];
const other = ["children", "other", "name", "asc"];

describe("refreshFiles", () => {
  test("a new favorite shows its star in every list, and no folder loads again", async () => {
    const { qc, loaded } = setup([
      [big, pages([node("a", "big"), node("b", "big")], [node("c", "big")]), true],
      [other, pages([node("x", "other")]), true],
      [["search", "a", {}], { items: [node("a", "big")], truncated: false }, true],
      [["favorites", "name", "asc"], [], true],
    ]);
    await refreshFiles(qc, { updated: [{ id: "a", is_favorite: true }], favorites: true });
    expect(rowsOf(qc.getQueryData(big))?.find((n) => n.id === "a")).toMatchObject({ is_favorite: true, name: "a" });
    expect(rowsOf(qc.getQueryData(["search", "a", {}]))?.[0]).toMatchObject({ is_favorite: true });
    expect(loaded(big)).toBe(0);
    expect(loaded(other)).toBe(0);
    expect(loaded(["search", "a", {}])).toBe(0);
    expect(loaded(["favorites", "name", "asc"])).toBe(1);
  });

  test("a rename changes the row, and puts only its folder in order again", async () => {
    const { qc, loaded, stale } = setup([
      [big, pages([node("a", "big"), node("b", "big")]), true],
      [other, pages([node("x", "other")]), true],
      [["children", "third", "name", "asc"], pages([node("y", "third")]), false],
    ]);
    await refreshFiles(qc, renamed(node("a", "big", { name: "z", updated_at: 5, is_favorite: false })));
    expect(loaded(big)).toBe(1);
    expect(loaded(other)).toBe(0);
    expect(stale(["children", "third", "name", "asc"])).toBe(false);
  });

  test("renaming a folder refreshes the paths of what is opened inside it", async () => {
    const { qc, loaded } = setup([
      [["node", "deep"], info(node("deep", "sub"), ["top", "sub", "deep"]), true],
      [["node", "elsewhere"], info(node("elsewhere", "x"), ["x", "elsewhere"]), true],
    ]);
    await refreshFiles(qc, renamed(folder("sub", "top")));
    expect(loaded(["node", "deep"])).toBe(1);
    expect(loaded(["node", "elsewhere"])).toBe(0);
  });

  test("a move takes the rows out of the folder they left at once, and loads the destination", async () => {
    const { qc, loaded, ids } = setup([
      [big, pages([node("a", "big"), node("b", "big")], [node("c", "big")]), true],
      [other, pages([node("x", "other")]), true],
      [["children", "unrelated", "name", "asc"], pages([node("u", "unrelated")]), true],
      [["recent"], [node("a", "big")], true],
      [["node", "a"], info(node("a", "big"), ["big", "a"]), true],
    ]);
    await refreshFiles(qc, { moved: [{ ids: ["a", "c"], to: "other" }] });
    expect(ids(big)).toEqual(["b"]);
    // Not loaded again: taking the rows out is enough
    expect(loaded(big)).toBe(0);
    expect(loaded(other)).toBe(1);
    expect(loaded(["children", "unrelated", "name", "asc"])).toBe(0);
    // Recent shows where it is now, and so does the moved item's own page
    expect(ids(["recent"])).toEqual(["a"]);
    expect(loaded(["recent"])).toBe(1);
    expect(loaded(["node", "a"])).toBe(1);
  });

  test("items gone to the trash leave every list, with what was loaded inside them", async () => {
    const { qc, loaded, ids } = setup([
      [big, pages([folder("f", "big"), node("b", "big")]), true],
      [["children", "f", "name", "asc"], pages([folder("g", "f")]), false],
      [["children", "g", "folders"], [folder("h", "g")], false],
      [["children", "keep", "name", "asc"], pages([node("k", "keep")]), false],
      [["trash", "pages", "everyone"], pages([]), true],
      [other, pages([node("x", "other")]), true],
    ]);
    await refreshFiles(qc, { removed: ["f"] });
    expect(ids(big)).toEqual(["b"]);
    expect(loaded(big)).toBe(0);
    expect(loaded(other)).toBe(0);
    expect(loaded(["trash", "pages", "everyone"])).toBe(1);
    // The lists inside the folder aren't kept; others are
    expect(qc.getQueryData(["children", "f", "name", "asc"])).toBeUndefined();
    expect(qc.getQueryData(["children", "g", "folders"])).toBeUndefined();
    expect(ids(["children", "keep", "name", "asc"])).toEqual(["k"]);
  });

  test("an uploaded folder refreshes the folder it went to and the lists already loaded below it, not others", async () => {
    const { qc, loaded, stale } = setup([
      [big, pages([folder("photos", "big")]), true],
      [["children", "photos", "name", "asc"], pages([folder("2024", "photos")]), false],
      [["children", "2024", "name", "asc"], pages([node("p", "2024")]), false],
      [other, pages([node("x", "other")]), true],
    ]);
    await refreshFiles(qc, { trees: ["big"] });
    expect(loaded(big)).toBe(1);
    expect(stale(["children", "photos", "name", "asc"])).toBe(true);
    expect(stale(["children", "2024", "name", "asc"])).toBe(true);
    expect(loaded(other)).toBe(0);
  });

  test("changes made in the same moment are applied together", async () => {
    const { qc, loaded } = setup([[big, pages([node("a", "big")]), true]]);
    await Promise.all([refreshFiles(qc, { folders: ["big"] }), refreshFiles(qc, { folders: ["big"], contents: true }), refreshFiles(qc, { folders: ["big"] })]);
    expect(loaded(big)).toBe(1);
  });

  test("a list loading while a row is taken out loads again, so the older answer doesn't bring the row back", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    let answers = 0;
    let release: (v: InfiniteData<CursorPage<Node>>) => void = () => {};
    qc.setQueryData(big, pages([node("a", "big"), node("b", "big")]));
    const observer = new QueryObserver(qc, {
      queryKey: big,
      staleTime: Infinity,
      // The first answer was made before the change; the next one after it
      queryFn: () =>
        ++answers === 1
          ? new Promise<InfiniteData<CursorPage<Node>>>((r) => (release = r))
          : Promise.resolve(pages([node("b", "big")])),
    });
    stop.push(observer.subscribe(() => {}));
    const first = observer.refetch();
    await refreshFiles(qc, { removed: ["a"] });
    release(pages([node("a", "big"), node("b", "big")]));
    await first;
    await qc.getQueryCache().find({ queryKey: big })?.promise;
    expect(rowsOf(qc.getQueryData(big))?.map((n) => n.id)).toEqual(["b"]);
    expect(answers).toBe(2);
  });

  test("a drop on My files refreshes the list of the person's own root folder", async () => {
    const { qc, loaded } = setup([
      [["me"], { root_id: "mine", shared_root: "all" }, false],
      [["children", "mine", "name", "asc"], pages([]), true],
    ]);
    await refreshFiles(qc, { folders: ["root"] });
    expect(loaded(["children", "mine", "name", "asc"])).toBe(1);
  });

  test("leaving a shared folder forgets what was loaded inside it", async () => {
    const { qc, loaded } = setup([
      [["shared-with-me"], [folder("s", "elsewhere")], true],
      [["children", "s", "name", "asc"], pages([folder("t", "s")]), false],
      [["children", "t", "name", "asc"], pages([node("u", "t")]), false],
    ]);
    await refreshFiles(qc, { left: ["s"] });
    expect(qc.getQueryData(["children", "s", "name", "asc"])).toBeUndefined();
    expect(qc.getQueryData(["children", "t", "name", "asc"])).toBeUndefined();
    expect(loaded(["shared-with-me"])).toBe(1);
  });

  test("a change of unknown reach refreshes every file list", async () => {
    const { qc, loaded } = setup([
      [big, pages([node("a", "big")]), true],
      [other, pages([node("x", "other")]), true],
    ]);
    await refreshFiles(qc, { all: true });
    expect(loaded(big)).toBe(1);
    expect(loaded(other)).toBe(1);
    await invalidateFiles(qc);
    expect(loaded(big)).toBe(2);
  });
});

describe("forgetUnreachable", () => {
  test("a folder that can't be opened any more leaves nothing loaded inside it", () => {
    const { qc } = setup([
      [["node", "s"], info(folder("s", "root1"), ["s"]), false],
      [["children", "s", "name", "asc"], pages([folder("t", "s")]), false],
      [["children", "t", "folders"], [folder("u", "t")], false],
      [["node", "u"], info(folder("u", "t"), ["s", "t", "u"]), false],
      [["children", "keep", "name", "asc"], pages([node("k", "keep")]), false],
    ]);
    forgetUnreachable(qc, ["children", "s", "name", "asc"], 403);
    expect(qc.getQueryData(["children", "t", "folders"])).toBeUndefined();
    expect(qc.getQueryData(["node", "u"])).toBeUndefined();
    expect(qc.getQueryData(["children", "keep", "name", "asc"])).toBeDefined();
    // Other errors (the server was busy) keep it
    const again = setup([[["children", "t", "folders"], [folder("u", "t")], false]]);
    forgetUnreachable(again.qc, ["children", "t", "name", "asc"], 503);
    expect(again.qc.getQueryData(["children", "t", "folders"])).toBeDefined();
  });
});
