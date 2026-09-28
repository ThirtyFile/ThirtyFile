// Compress and extract tasks: which files offer "Extract all", and the progress shown while a task runs
import { describe, expect, test } from "vitest";
import { isZip } from "@/components/FileIcon";
import { jobPercent } from "@/lib/jobs";

describe("compress and extract", () => {
  test("ZIP files are recognised by extension or type, never folders", () => {
    expect(isZip({ kind: "file", name: "Photos.ZIP", mime: "application/octet-stream" })).toBe(true);
    expect(isZip({ kind: "file", name: "download", mime: "application/zip" })).toBe(true);
    expect(isZip({ kind: "file", name: "a.7z", mime: "application/x-7z-compressed" })).toBe(false);
    expect(isZip({ kind: "folder", name: "old.zip", mime: "" })).toBe(false);
  });

  test("progress is a whole percentage, and 0 before anything is known", () => {
    expect(jobPercent({ done: 0, total: 0 })).toBe(0);
    expect(jobPercent({ done: 1, total: 3 })).toBe(33);
    expect(jobPercent({ done: 5, total: 5 })).toBe(100);
    // A folder space's file that grew while being read
    expect(jobPercent({ done: 7, total: 5 })).toBe(100);
  });
});
