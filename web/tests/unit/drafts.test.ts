// Unsaved drafts: a workbook's marker flags the tab like a text draft, but is never read back as text
import { describe, expect, test } from "vitest";
import { getDraft, hasDraft, onDraftRemoved, setDraft, textSaved } from "@/lib/drafts";

describe("drafts", () => {
  test("a workbook's marker is not a text draft", () => {
    setDraft("sheet-1", { kind: "sheet", base: 42 });
    expect(hasDraft("sheet-1")).toBe(true);
    expect(getDraft("sheet-1", "text")).toBeUndefined();
    expect(getDraft("sheet-1", "sheet")).toEqual({ kind: "sheet", base: 42 });
    setDraft("sheet-1", null);
    expect(hasDraft("sheet-1")).toBe(false);
  });

  test("a text draft is returned only as text", () => {
    setDraft("text-1", { kind: "text", text: "new", base: "old", version: 7 });
    expect(hasDraft("text-1")).toBe(true);
    expect(getDraft("text-1", "text")).toEqual({ kind: "text", text: "new", base: "old", version: 7 });
    expect(getDraft("text-1", "sheet")).toBeUndefined();
    setDraft("text-1", null);
    expect(getDraft("text-1", "text")).toBeUndefined();
  });

  test("edits typed while a save was on its way stay a draft based on what was saved", () => {
    // "A" was sent; "AB" was typed meanwhile, replacing the draft
    setDraft("text-2", { kind: "text", text: "AB", base: "", version: 1 });
    textSaved("text-2", "A", "AB", 2);
    expect(hasDraft("text-2")).toBe(true);
    expect(getDraft("text-2", "text")).toEqual({ kind: "text", text: "AB", base: "A", version: 2 });
    // Saving again with nothing typed meanwhile clears it
    textSaved("text-2", "AB", "AB", 3);
    expect(hasDraft("text-2")).toBe(false);
  });

  test("text changed back during a save is still a draft when it differs from what was saved", () => {
    // Everything was deleted while "A" was saved: the editor matches the old original, so no draft was left
    textSaved("text-3", "A", "", 2);
    expect(getDraft("text-3", "text")).toEqual({ kind: "text", text: "", base: "A", version: 2 });
    setDraft("text-3", null);
  });

  test("removing a draft of either kind is reported", () => {
    const removed: string[] = [];
    onDraftRemoved((id) => removed.push(id));
    setDraft("a", { kind: "sheet", base: 1 });
    setDraft("b", { kind: "text", text: "x", base: "" });
    setDraft("a", null);
    setDraft("b", null);
    setDraft("c", null);
    expect(removed).toEqual(["a", "b"]);
  });
});
