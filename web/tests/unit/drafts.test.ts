// Unsaved drafts: a workbook's marker flags the tab like a text draft, but is never read back as text
import { describe, expect, test } from "vitest";
import { getDraft, hasDraft, onDraftRemoved, setDraft } from "@/lib/drafts";

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
