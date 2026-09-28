// Where people start, with or without a personal space ("My files")
import { describe, expect, test } from "vitest";
import { folderOfPath, homeFolder } from "@/lib/home";

const space = (kind: "personal" | "company" | "team", root_id: string, disabled = false) => ({ kind, root_id, disabled });

describe("homeFolder", () => {
  test("is My files when the person has it", () => {
    expect(homeFolder({ root_id: "r1", shared_root: "c1" }, undefined)).toBe("root");
  });
  test("is All files, then the first other space, without My files", () => {
    expect(homeFolder({ root_id: null, shared_root: "c1" }, undefined)).toBe("shared");
    expect(homeFolder({ root_id: null, shared_root: null }, undefined)).toBeUndefined();
    expect(homeFolder({ root_id: null, shared_root: null }, [space("team", "t0", true), space("team", "t1"), space("team", "t2")])).toBe("t1");
  });
  test("is null without any space", () => {
    expect(homeFolder({ root_id: null, shared_root: null }, [])).toBeNull();
  });
});

describe("folderOfPath", () => {
  test("names My files only for someone who has it", () => {
    expect(folderOfPath("/files")).toBe("root");
    expect(folderOfPath("/files", false)).toBeNull();
    expect(folderOfPath("/files/root", false)).toBeNull();
    expect(folderOfPath("/files/shared", false)).toBe("shared");
    expect(folderOfPath("/files/a%20b?x=1", false)).toBe("a b");
    expect(folderOfPath("/recent")).toBeNull();
  });
});
