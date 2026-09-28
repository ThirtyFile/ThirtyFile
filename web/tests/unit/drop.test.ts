// Files and folders dropped from the computer: what can't be read is skipped and counted, the rest is uploaded
import { describe, expect, test } from "vitest";
import { readEntries } from "@/uploads";

const fileEntry = (name: string, readable = true) =>
  ({
    name,
    isFile: true,
    isDirectory: false,
    file: (ok: (f: File) => void, fail: (e: Error) => void) => (readable ? ok(new File(["x"], name)) : fail(new Error("NotReadableError"))),
  }) as unknown as FileSystemEntry;

/** A folder listed in batches (as browsers do); `failAfter` batches, listing it fails */
const dirEntry = (name: string, batches: FileSystemEntry[][], failAfter = Infinity) =>
  ({
    name,
    isFile: false,
    isDirectory: true,
    createReader: () => {
      let i = 0;
      return {
        readEntries: (ok: (e: FileSystemEntry[]) => void, fail: (e: Error) => void) => (i >= failAfter ? fail(new Error("NotFoundError")) : ok(batches[i++] ?? [])),
      };
    },
  }) as unknown as FileSystemEntry;

describe("readEntries", () => {
  test("skips unreadable files and folders, and keeps everything else", async () => {
    const { files, skipped } = await readEntries([
      fileEntry("a.txt"),
      fileEntry("locked.txt", false),
      dirEntry("Photos", [[fileEntry("1.jpg"), fileEntry("2.jpg", false)], [dirEntry("Old", [[fileEntry("3.jpg")]], 1)]]),
    ]);
    expect(files.map((f) => `${f.relativePath}/${f.file.name}`)).toEqual(["/a.txt", "Photos/1.jpg", "Photos/Old/3.jpg"]);
    // locked.txt, 2.jpg, and the rest of Old
    expect(skipped).toBe(3);
  });
});
