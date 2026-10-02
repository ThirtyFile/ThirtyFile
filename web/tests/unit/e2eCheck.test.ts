// scripts/check-e2e.mjs: no fixed waits, and no names without a unique part, in the end-to-end tests
import { describe, expect, test } from "vitest";
import { join } from "node:path";
// @ts-expect-error: a plain script without type declarations
import { check, problems } from "../../scripts/check-e2e.mjs";

const find = problems as (file: string, source: string) => string[];

describe("check-e2e", () => {
  test("the end-to-end tests keep the rules", () => {
    expect((check as (dir: string) => string[])(join(process.cwd(), "tests", "e2e"))).toEqual([]);
  });

  test("finds fixed waits, names made from the time, and folders and accounts made by hand", () => {
    const source = [
      "await page.waitForTimeout(700);",
      "await new Promise((r) => setTimeout(r, 1500));",
      'await page.waitForLoadState("networkidle");',
      "const name = `kim-${Date.now().toString(36)}`;",
      'await page.request.post("/api/folders", { data: { parent_id: root, name } });',
      "await page.request.post(`/api/admin/users`, { data: { username: name } });",
    ].join("\n");
    expect(find("a.spec.ts", source).map((p) => p.split(": ")[0])).toEqual(["a.spec.ts:1", "a.spec.ts:2", "a.spec.ts:3", "a.spec.ts:4", "a.spec.ts:5", "a.spec.ts:6"]);
  });

  test("leaves alone comments, the helpers' own requests, and other uses of the time", () => {
    const source = [
      "// page.waitForTimeout(700) would be a fixed wait",
      "/* setTimeout(r, 10) */",
      'await page.goto("http://example.com/files");',
      "const expires = Math.floor(Date.now() / 1000) + 86_400;",
      "test.setTimeout(120_000);",
      'await page.request.post("/api/shares", { data: { node_id: id } });',
      'await page.route("**/api/folders", (route) => route.continue());',
    ].join("\n");
    expect(find("a.spec.ts", source)).toEqual([]);
    expect(find("tests/e2e/helpers.ts", 'await page.request.post("/api/folders", { data });')).toEqual([]);
  });
});
