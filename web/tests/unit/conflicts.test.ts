// Name clashes before uploading, moving, copying or restoring: what is asked, and what the answers send
import { describe, expect, test } from "vitest";
import { type Answer, type Clash, applyToUpload, numberedName, resolveAll, topLevel } from "@/lib/conflicts";
import { savedName } from "@/uploads";

const file = (name: string, size = 1, lastModified = 5000) => new File(["x".repeat(size)], name, { lastModified });
const clash = (key: string): Clash => ({ key, name: key, kind: "file", existing: { kind: "file", size: 1, updated_at: 1 } });

describe("top-level items of a pick or drop", () => {
  test("files count by name, a folder's files count once as that folder", () => {
    const picked = [
      { file: file("a.txt", 3), relativePath: "" },
      { file: file("x.jpg"), relativePath: "Photos/2024" },
      { file: file("y.jpg"), relativePath: "Photos" },
    ];
    expect(topLevel(picked)).toEqual([
      { name: "a.txt", kind: "file", size: 3, modified: 5 },
      { name: "Photos", kind: "folder" },
    ]);
  });
});

describe("asking about each clash", () => {
  test("every clash is asked about until an answer is for all", async () => {
    const asked: [string, number][] = [];
    const replies: Answer[] = [
      { choice: "skip", forAll: false },
      { choice: "replace", forAll: true },
    ];
    const answers = await resolveAll([clash("a"), clash("b"), clash("c"), clash("d")], async (c, remaining) => {
      asked.push([c.key, remaining]);
      return replies.shift()!;
    });
    expect(asked).toEqual([
      ["a", 3],
      ["b", 2],
    ]);
    expect(Object.fromEntries(answers!)).toEqual({ a: "skip", b: "replace", c: "replace", d: "replace" });
  });

  test("cancelling a question cancels everything", async () => {
    expect(await resolveAll([clash("a"), clash("b")], async (c) => (c.key === "a" ? { choice: "keep", forAll: false } : null))).toBeNull();
  });

  test("nothing to ask when nothing clashes", async () => {
    expect((await resolveAll([], async () => null))!.size).toBe(0);
  });
});

describe("applying the answers to an upload", () => {
  test("skipped names are left out, the rest carry what to do when the name is taken", () => {
    const picked = [
      { file: file("a.txt"), relativePath: "" },
      { file: file("b.txt"), relativePath: "" },
      { file: file("x.jpg"), relativePath: "Photos/2024" },
      { file: file("new.txt"), relativePath: "" },
    ];
    const out = applyToUpload(
      picked,
      new Map([
        ["a.txt", "skip"],
        ["b.txt", "keep"],
        ["Photos", "replace"],
      ]),
    );
    expect(out.map((f) => [f.file.name, f.onConflict])).toEqual([
      ["b.txt", "keep"],
      ["x.jpg", "replace"],
      ["new.txt", "keep"],
    ]);
  });

  test("the name a kept copy gets matches the server's", () => {
    expect(numberedName("Report.docx", false)).toBe("Report (1).docx");
    expect(numberedName("archive.tar.gz", false)).toBe("archive.tar (1).gz");
    expect(numberedName(".env", false)).toBe(".env (1)");
    expect(numberedName("v1.0", true)).toBe("v1.0 (1)");
  });

  test("the panel shows the name the server gave only when it differs", () => {
    expect(savedName("Report%20(1).docx", "Report.docx")).toBe("Report (1).docx");
    expect(savedName("Report.docx", "Report.docx")).toBeUndefined();
    expect(savedName(undefined, "Report.docx")).toBeUndefined();
    expect(savedName("%E0%A4", "x")).toBeUndefined();
  });
});
