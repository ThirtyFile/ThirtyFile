// Coloured tags (lib/tags.ts, components/tags.tsx): what a selection has, putting tags on and taking them off a batch at a
// time, and how an item's tags show (dots labelled with their names, and the names themselves)
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, test, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { api, type Node, type Tag } from "@/api";
import { keys } from "@/api/queryKeys";
import { ItemMarks } from "@/components/fileList/marks";
import { TagNames } from "@/components/tags";
import { changeTags, createTag, deleteTag, tagNames, tagState, tagsOn, updateTag, withTags } from "@/lib/tags";
import { rowsOf } from "@/lib/queries";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

vi.mock("sonner", () => ({ toast: { error: vi.fn<(m: string) => void>(), loading: vi.fn<() => void>(), dismiss: vi.fn<() => void>() } }));
vi.mock("@/lib/errorReport", () => ({ reportShown: vi.fn<() => void>() }));

const TAGS: Tag[] = [
  { id: 1, name: "Urgent", color: "red" },
  { id: 2, name: "later", color: "blue" },
  { id: 3, name: "Invoices 10", color: "green" },
  { id: 4, name: "Invoices 9", color: "yellow" },
];
const byId = new Map(TAGS.map((tag) => [tag.id, tag]));

const node = (id: string, tags?: number[], extra: Partial<Node> = {}): Node => ({
  id,
  parent_id: "folder",
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
  tags,
  ...extra,
});

afterEach(() => vi.restoreAllMocks());

describe("what a selection has", () => {
  test("every item, some of them or none", () => {
    const items = [node("a", [1, 2]), node("b", [1]), node("c")];
    expect(tagState(items, 1, false)).toBe("mixed");
    expect(tagState(items.slice(0, 2), 1, false)).toBe(true);
    expect(tagState(items.slice(0, 2), 2, false)).toBe("mixed");
    expect(tagState(items, 3, false)).toBe(false);
    // In a large folder whose selected items aren't all loaded, those loaded don't speak for the others
    expect(tagState(items.slice(0, 2), 1, true)).toBe("mixed");
    expect(tagState([], 1, false)).toBe(false);
  });

  test("an item's tags after a change, and as they are listed", () => {
    expect(withTags([1, 2], [3], [2])).toEqual([1, 3]);
    expect(withTags(undefined, [1, 1], [])).toEqual([1]);
    expect(withTags([1], [2], [2]), "added and taken off at once: off").toEqual([1]);
    // By name as people read numbers, and a tag just deleted is left out
    expect(tagsOn([1, 3, 4, 2, 99], byId).map((tag) => tag.name)).toEqual(["Invoices 9", "Invoices 10", "later", "Urgent"]);
    expect(tagNames(tagsOn([2, 1], byId))).toBe("later, Urgent");
  });
});

describe("changing tags", () => {
  const listKey = keys.childrenPages("folder", "name", "asc");
  const setup = () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    qc.setQueryData(keys.tags(), TAGS);
    qc.setQueryData(listKey, { pages: [{ items: [node("a", [1]), node("b"), node("c", [2])], next: null }], pageParams: [null] });
    return qc;
  };
  const tagsOf = (qc: QueryClient) => Object.fromEntries((rowsOf(qc.getQueryData(listKey)) as Node[]).map((n) => [n.id, n.tags ?? []]));
  /** Lets the changes the list merges in the same moment be applied */
  const settle = () => new Promise((r) => setTimeout(r, 10));

  test("the items picked one by one and those of a span, a batch at a time; the rows change at once", async () => {
    const qc = setup();
    const apply = vi.spyOn(api, "applyTags").mockResolvedValue(undefined);
    const selection = vi.spyOn(api, "selection").mockResolvedValue({ ids: ["x", "a"], next: null });
    const nodes = [node("a", [1]), node("b")];
    const span = { folder: "folder", sort: "name" as const, order: "asc" as const, except: new Set<string>(), count: 2 };
    expect(await changeTags(qc, { ids: ["a", "b"], span, count: 4 }, nodes, [2], [1])).toBe(true);
    expect(apply.mock.calls).toEqual([
      [["a", "b"], [2], [1]],
      // The span's items not already done
      [["x"], [2], [1]],
    ]);
    expect(selection).toHaveBeenCalledTimes(1);
    await settle();
    expect(tagsOf(qc)).toEqual({ a: [2], b: [2], c: [2] });
  });

  test("a change the server refuses says so, and the rows stay as they were", async () => {
    const qc = setup();
    vi.spyOn(api, "applyTags").mockRejectedValue(new Error("Tag not found"));
    const { toast } = await import("sonner");
    expect(await changeTags(qc, { ids: ["b"], span: null, count: 1 }, [node("b")], [1], [])).toBe(false);
    expect(toast.error).toHaveBeenCalledWith("Tag not found");
    await settle();
    expect(tagsOf(qc).b).toEqual([]);
  });

  test("tags made, changed and deleted keep the list of tags in order", async () => {
    const qc = setup();
    vi.spyOn(api, "createTag").mockResolvedValue({ id: 5, name: "Archive", color: "gray" });
    vi.spyOn(api, "updateTag").mockResolvedValue({ id: 1, name: "Zzz", color: "purple" });
    vi.spyOn(api, "deleteTag").mockResolvedValue(undefined);
    const names = () => qc.getQueryData<Tag[]>(keys.tags())!.map((tag) => tag.name);
    await createTag(qc, "Archive", "gray");
    expect(names()).toEqual(["Archive", "Invoices 9", "Invoices 10", "later", "Urgent"]);
    await updateTag(qc, 1, { name: "Zzz", color: "purple" });
    expect(names()).toEqual(["Archive", "Invoices 9", "Invoices 10", "later", "Zzz"]);
    qc.setQueryData(keys.tagged(2, "name", "asc"), { items: [], truncated: false });
    await deleteTag(qc, 2);
    expect(names()).toEqual(["Archive", "Invoices 9", "Invoices 10", "Zzz"]);
    expect(qc.getQueryData(keys.tagged(2, "name", "asc")), "its list is forgotten").toBeUndefined();
  });
});

describe("showing tags", () => {
  let root: Root | null = null;
  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    document.body.innerHTML = "";
  });
  const show = (el: React.ReactNode) => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    qc.setQueryData(keys.tags(), TAGS);
    const container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    act(() => root!.render(<QueryClientProvider client={qc}>{el}</QueryClientProvider>));
    return container;
  };

  test("dots after the name, with the names as their label", () => {
    const el = show(<ItemMarks item={node("a", [1, 2], { is_favorite: true })} />);
    const dots = el.querySelector("[data-tags]")!;
    expect(dots.getAttribute("role")).toBe("img");
    expect(dots.getAttribute("aria-label")).toBe("Tags: later, Urgent");
    expect(dots.getAttribute("title")).toBe("Tags: later, Urgent");
    expect(dots.children).toHaveLength(2);
    expect(dots.children[0].className).toContain("bg-blue-500");
    expect(el.querySelector("[aria-label=Favorite]")).not.toBeNull();
  });

  test("nothing for an item without tags, and no list of tags is asked for", () => {
    const tags = vi.spyOn(api, "tags");
    const el = show(<ItemMarks item={node("a")} />);
    expect(el.innerHTML).toBe("");
    expect(tags).not.toHaveBeenCalled();
  });

  test("the names, each with its dot", () => {
    const el = show(<TagNames ids={[3, 1]} />);
    expect([...el.querySelectorAll("li")].map((li) => li.textContent)).toEqual(["Invoices 10", "Urgent"]);
    expect(el.querySelector("ul")!.getAttribute("aria-label")).toBe("Tags");
    const none = show(<TagNames ids={[]} empty="None" />);
    expect(none.textContent).toBe("None");
  });
});
