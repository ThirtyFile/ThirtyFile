import { describe, expect, test } from "vite-plus/test";
import type { ArchiveEntry } from "@/api";
import { archiveFolders } from "@/lib/archive";

const entry = (index: number, path: string, more: Partial<ArchiveEntry> = {}): ArchiveEntry => ({ index, path, dir: false, size: 1, modified: null, locked: null, ...more });

describe("a ZIP file's folders", () => {
  test("are made from the paths, with or without entries of their own", () => {
    const folders = archiveFolders([entry(0, "b/c/10.txt"), entry(1, "b/c/2.txt"), entry(2, "a.txt"), entry(3, "b", { dir: true, modified: 60 }), entry(4, "z", { dir: true })]);
    const names = (at: string) => folders.get(at)!.map((n) => `${n.name}${n.dir ? "/" : ""}`);
    // Folders first, then by name, numbers by their value
    expect(names("")).toEqual(["b/", "z/", "a.txt"]);
    expect(names("b")).toEqual(["c/"]);
    expect(names("b/c")).toEqual(["2.txt", "10.txt"]);
    expect(names("z")).toEqual([]);
    expect(folders.get("")![0]).toMatchObject({ path: "b", modified: 60, index: null });
    expect(folders.get("b/c")![0]).toMatchObject({ path: "b/c/2.txt", index: 1 });
  });

  test("hold both entries of one path, and a file named as a folder", () => {
    const folders = archiveFolders([entry(0, "x"), entry(1, "x"), entry(2, "x/inside")]);
    expect(folders.get("")!.map((n) => [n.name, n.dir, n.index])).toEqual([
      ["x", true, null],
      ["x", false, 0],
      ["x", false, 1],
    ]);
    expect(archiveFolders([]).get("")).toEqual([]);
  });
});
