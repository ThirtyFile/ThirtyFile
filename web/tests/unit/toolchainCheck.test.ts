// scripts/check-toolchain.mjs: `vite-plus` and the `vite` alias of its core are the same release
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, test } from "vite-plus/test";
// @ts-expect-error: a plain script without type declarations
import { catalogProblems } from "../../scripts/check-toolchain.mjs";

const problems = catalogProblems as (text: string) => string[];

const catalog = (plus: string, core: string) => `catalog:\n  vite: npm:@voidzero-dev/vite-plus-core@${core}\n  vitest: 5.0.5\n  vite-plus: ${plus}\n`;

describe("check-toolchain", () => {
  test("the workspace file keeps the two together", () => {
    expect(problems(readFileSync(join(process.cwd(), "pnpm-workspace.yaml"), "utf8"))).toEqual([]);
  });

  test("accepts the same release", () => {
    expect(problems(catalog("1.4.0", "1.4.0"))).toEqual([]);
  });

  test("finds one raised without the other", () => {
    expect(problems(catalog("1.4.0", "1.3.0"))).toEqual(["`vite-plus` is 1.4.0 but `vite` (vite-plus-core) is 1.3.0: raise both, in the same pull request"]);
    expect(problems(catalog("1.3.0", "1.4.0"))).toHaveLength(1);
  });

  test("finds a catalog missing a line, rather than passing", () => {
    expect(problems("catalog:\n  vitest: 5.0.5\n")).toEqual(["the catalog has no `vite-plus: <version>` line"]);
    expect(problems("catalog:\n  vite-plus: 1.4.0\n")).toEqual(["the catalog has no `vite: npm:@voidzero-dev/vite-plus-core@<version>` line"]);
  });
});
