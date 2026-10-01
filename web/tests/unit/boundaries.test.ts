// scripts/check-boundaries.mjs: nothing under src/ooxml imports from the rest of the app
import { afterAll, describe, expect, test } from "vitest";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
// @ts-expect-error: a plain script without type declarations
import { crossings } from "../../scripts/check-boundaries.mjs";

const find = crossings as (root: string, packages: string[], base?: string) => string[];

describe("check-boundaries", () => {
  const dir = mkdtempSync(join(tmpdir(), "tf-boundaries-"));
  afterAll(() => rmSync(dir, { recursive: true, force: true }));

  test("src/ooxml imports only itself and jszip", () => {
    expect(find(join(process.cwd(), "src", "ooxml"), ["jszip"])).toEqual([]);
  });

  test("finds every kind of import that leaves the folder", () => {
    const root = join(dir, "lib");
    mkdirSync(join(root, "core"), { recursive: true });
    writeFileSync(
      join(root, "core", "a.ts"),
      [
        'import JSZip from "jszip";',
        'import { b } from "./b";',
        'import { up } from "../core/b";',
        '// import { commented } from "@/lib/i18n";',
        'import { t } from "@/lib/i18n";',
        "import type {",
        "  Node,",
        '} from "../../api/types";',
        'export * from "react";',
        'const later = () => import("../../elsewhere");',
      ].join("\n"),
    );
    writeFileSync(join(root, "core", "b.ts"), "export const b = 1;\n");
    expect(find(root, ["jszip"], dir)).toEqual([
      'lib/core/a.ts:5: "@/lib/i18n" isn\'t a relative path inside the folder or one of: jszip',
      'lib/core/a.ts:6: "../../api/types" leads outside the folder',
      'lib/core/a.ts:9: "react" isn\'t a relative path inside the folder or one of: jszip',
      'lib/core/a.ts:10: "../../elsewhere" leads outside the folder',
    ]);
  });
});
