// New items kept where they were made (components/explorer/newItems.ts): where the list shows them
import { describe, expect, test } from "vitest";
import type { Node } from "@/api";
import { arrange, isPending, notLoaded, type NewItem } from "@/components/explorer/newItems";

const node = (id: string, name = id): Node => ({
  id,
  parent_id: "folder",
  kind: "folder",
  name,
  size: 0,
  mime: "",
  created_at: 0,
  updated_at: 0,
  trashed_at: null,
  drive_id: null,
  owner_name: "",
  is_favorite: false,
});
const made = (item: Node, id: string | null, at = Infinity, seen = false): NewItem => ({ item, at, id, made: Promise.resolve(id ?? ""), seen });
const ids = (list: readonly (Node | undefined)[]) => list.map((n) => n?.id ?? "·");
const byId = (...nodes: Node[]) => new Map(nodes.map((n) => [n.id, n]));

describe("new items", () => {
  test("being made, it shows at the end; made, it stays there instead of its sorted place, as the server has it", () => {
    const a = node("a");
    const z = node("z");
    expect(ids(arrange([a, z], [made(node("new:1", "New folder"), null)], byId(a, z)))).toEqual(["a", "z", "new:1"]);
    // Made and renamed: the list has it between a and z, with the new name
    const b = node("b", "Beta");
    const shown = arrange([a, b, z], [made(b, "b")], byId(a, b, z));
    expect(ids(shown)).toEqual(["a", "z", "b"]);
    expect(shown[2]?.name).toBe("Beta");
    // Still being renamed: the item with the temporary id shows, not the server's
    expect(ids(arrange([a, b, z], [made(node("new:1"), "b")], byId(a, b, z)))).toEqual(["a", "z", "new:1"]);
  });

  test("in a large folder, it stays after the items that were loaded, not in a part that isn't", () => {
    const a = node("a");
    const b = node("b");
    // Two loaded, two not loaded; the new item sorts into the part not loaded
    const base = [a, b, undefined, undefined, undefined];
    expect(ids(arrange(base, [made(node("n"), "n", 2)], byId(a, b)))).toEqual(["a", "b", "n", "·", "·", "·"]);
    // The new item isn't loaded, so it is known from what was shown
    expect(notLoaded([made(node("n"), "n", 2)], byId(a, b)).map((n) => n.id)).toEqual(["n"]);
  });

  test("one the list had and no longer has was deleted or moved away", () => {
    const a = node("a");
    expect(ids(arrange([a], [made(node("n"), "n", Infinity, true)], byId(a)))).toEqual(["a"]);
    expect(ids(arrange([a], [made(node("n"), "n", Infinity, false)], byId(a)))).toEqual(["a", "n"]);
    expect(isPending("new:1")).toBe(true);
    expect(isPending("a")).toBe(false);
  });
});
