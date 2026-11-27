import { describe, expect, test } from "vite-plus/test";
import { nameError, nameProblems, newName, type RenameRule, splitName, takenKey } from "@/lib/batchRename";

const file = (name: string) => ({ kind: "file", name });
const folder = (name: string) => ({ kind: "folder", name });
const replace = (find: string, to: string, more: Partial<{ matchCase: boolean; extensions: boolean }> = {}): RenameRule => ({
  mode: "replace",
  find,
  replace: to,
  matchCase: false,
  extensions: false,
  ...more,
});

describe("new names", () => {
  test("keep a file's extension unless asked to change it; folders and names like .env have none", () => {
    expect(splitName("a.tar.gz", false)).toEqual(["a.tar", ".gz"]);
    expect(splitName(".env", false)).toEqual([".env", ""]);
    expect(splitName("v1.2", true)).toEqual(["v1.2", ""]);
    expect(newName(file("txt notes.txt"), 0, replace("txt", "md"))).toBe("md notes.txt");
    expect(newName(file("txt notes.txt"), 0, replace("txt", "md", { extensions: true }))).toBe("md notes.md");
    expect(newName(folder("v1.2"), 0, replace(".2", ".3"))).toBe("v1.3");
  });

  test("replace every occurrence, taking the text as it is", () => {
    expect(newName(file("IMG img.jpg"), 0, replace("img", "photo"))).toBe("photo photo.jpg");
    expect(newName(file("IMG img.jpg"), 0, replace("img", "photo", { matchCase: true }))).toBe("IMG photo.jpg");
    // Neither the text looked for nor the replacement means anything to a pattern
    expect(newName(file("a (1)+.txt"), 0, replace("(1)+", "$&"))).toBe("a $&.txt");
    expect(newName(file("a.txt"), 0, replace("", "x"))).toBe("a.txt");
    expect(newName(file("a.txt"), 0, replace("z", "x"))).toBe("a.txt");
  });

  test("add text before or after the name", () => {
    const add = (text: string, before: boolean): RenameRule => ({ mode: "add", text, before, extensions: false });
    expect(newName(file("a.txt"), 0, add("2026 ", true))).toBe("2026 a.txt");
    expect(newName(file("a.txt"), 0, add(" (old)", false))).toBe("a (old).txt");
    // As the server would keep it
    expect(newName(folder("a"), 0, add("  ", false))).toBe("a");
  });

  test("number the items in their order", () => {
    const number: RenameRule = { mode: "number", base: "Photo ", start: 9, digits: 3 };
    expect([file("x.jpg"), file("y.png"), folder("z")].map((n, i) => newName(n, i, number))).toEqual(["Photo 009.jpg", "Photo 010.png", "Photo 011"]);
    expect(newName(file("x.jpg"), 0, { mode: "number", base: "", start: 0, digits: 1 })).toBe("0.jpg");
  });
});

describe("names that can't be used", () => {
  test("as the server refuses them", () => {
    expect(nameError("report.txt")).toBeNull();
    expect(nameError("COM0.txt")).toBeNull();
    expect(nameError("")).toBe("Name can't be blank");
    expect(nameError("..")).toBe("Invalid name");
    expect(nameError("a/b")).toBe("Name can't contain /");
    expect(nameError("a\tb")).toBe("Name can't contain control characters");
    expect(nameError("a.")).toBe("Name can't end with a period");
    expect(nameError("con.txt")).toBe("This name is reserved by Windows");
    expect(nameError("LPT3")).toBe("This name is reserved by Windows");
    // The limit is in bytes
    expect(nameError("a".repeat(255))).toBeNull();
    expect(nameError("檔".repeat(86))).toBe("Name is too long");
  });

  test("repeated in a folder, or taken by an item that keeps its name", () => {
    const item = (id: string, name: string, parent_id = "p") => ({ id, parent_id, kind: "file", name });
    const items = [item("1", "a"), item("2", "b"), item("3", "c"), item("4", "d", "q"), item("5", "kept")];
    // The first two swap; two get names that differ only in case; another folder may have the same name
    expect(nameProblems(items, ["b", "a", "X", "x", "kept"], new Set())).toEqual([null, null, null, null, null]);
    expect(nameProblems(items, ["x", "b", "X", "x", "kept"], new Set())).toEqual([{ type: "repeated" }, null, { type: "repeated" }, null, null]);
    // The name of an item that keeps it, selected or not
    expect(nameProblems(items, ["KEPT", "b", "there", "d", "kept"], new Set([takenKey("p", "there")]))).toEqual([{ type: "repeated" }, null, { type: "taken" }, null, null]);
    expect(nameProblems(items, ["a?", "b", "c", "d", "kept"], new Set())[0]).toEqual({ type: "invalid", message: "Name can't contain ?" });
  });
});
