// Taking back moves, renames and deletions (lib/undo.ts): Undo in the message, Ctrl+Z, and moving items back
import { afterEach, describe, expect, test, vi } from "vitest";
import { api, type Job } from "@/api";
import { movedBack, moveBack, originsOf, toastWithUndo, undoLast } from "@/lib/undo";

const done: Job = { id: "", kind: "move", state: "done", done: 0, total: 0, error: null, node_id: null, name: null };

afterEach(() => vi.restoreAllMocks());

describe("undo", () => {
  test("Ctrl+Z takes back the last action once, then runs what follows", async () => {
    const undo = vi.fn<() => Promise<void>>(() => Promise.resolve());
    const after = vi.fn<() => void>();
    toastWithUndo("Moved", { undo, undoneText: "Moved back", label: "Undo move", after });
    expect(undoLast()).toBe(true);
    await vi.waitFor(() => expect(after).toHaveBeenCalled());
    expect(undo).toHaveBeenCalledTimes(1);
    expect(undoLast()).toBe(false);
  });

  test("only the last action is taken back", () => {
    const first = vi.fn<() => Promise<void>>(() => Promise.resolve());
    const second = vi.fn<() => Promise<void>>(() => Promise.resolve());
    toastWithUndo("Renamed", { undo: first, undoneText: "Renamed back", label: "Undo rename" });
    toastWithUndo("Deleted", { undo: second, undoneText: "Restored", label: "Undo delete" });
    undoLast();
    expect(second).toHaveBeenCalled();
    expect(first).not.toHaveBeenCalled();
    expect(undoLast()).toBe(false);
  });

  test("where moved items came from: items already in the destination, or without a folder, are left out", () => {
    const items = [
      { id: "a", parent_id: "f1" },
      { id: "b", parent_id: "f2" },
      { id: "c", parent_id: "dest" },
      { id: "d", parent_id: null },
      { id: "e", parent_id: "f1" },
    ];
    expect([...originsOf(items, ["a", "b", "c", "d"], "dest")]).toEqual([
      ["a", "f1"],
      ["b", "f2"],
    ]);
  });

  test("moving back sends each folder's items to it, and says what moved where", async () => {
    const move = vi.spyOn(api, "move").mockResolvedValue(done);
    const origins = new Map([
      ["a", "f1"],
      ["b", "f2"],
      ["c", "f1"],
    ]);
    await moveBack(origins);
    expect(move.mock.calls).toEqual([
      [["a", "c"], "f1"],
      [["b"], "f2"],
    ]);
    expect(movedBack(origins)).toEqual({
      moved: [
        { ids: ["a", "c"], to: "f1" },
        { ids: ["b"], to: "f2" },
      ],
      usage: true,
    });
  });
});
