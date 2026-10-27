// scripts/check-toolchain.mjs: `vite-plus` and the `vite` alias of its core are the same release
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, test } from "vite-plus/test";
// @ts-expect-error: a plain script without type declarations
import { catalogProblems, nodeProblems } from "../../scripts/check-toolchain.mjs";

const problems = catalogProblems as (text: string) => string[];
type NodeFiles = { nodeVersion: string; dockerfile: string; engines: string; workflows: Record<string, string> };
const nodeCheck = nodeProblems as (files: NodeFiles) => string[];

const ENGINES = "^22.18.0 || ^24.11.0 || >=26.0.0";
const IMAGE = "FROM --platform=$BUILDPLATFORM node:26.12.0-alpine3.24@sha256:abc AS web\n";
const GOOD_WORKFLOW = "      - uses: actions/setup-node@abc # v7\n        with:\n          node-version-file: .node-version\n          cache: pnpm\n";
const files = (over: Partial<NodeFiles> = {}): NodeFiles => ({ nodeVersion: "26.12.1\n", dockerfile: IMAGE, engines: ENGINES, workflows: { "checks.yml": GOOD_WORKFLOW }, ...over });

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

  test("the Node versions of the repository agree", () => {
    const root = join(process.cwd(), "..");
    const workflowDir = join(root, ".github", "workflows");
    const workflows = Object.fromEntries(readdirSync(workflowDir).map((f) => [f, readFileSync(join(workflowDir, f), "utf8")]));
    const engines = JSON.parse(readFileSync(join(process.cwd(), "package.json"), "utf8")).engines.node as string;
    expect(nodeCheck({ nodeVersion: readFileSync(join(root, ".node-version"), "utf8"), dockerfile: readFileSync(join(root, "Dockerfile"), "utf8"), engines, workflows })).toEqual([]);
  });

  test("accepts the image being a patch release ahead, and every engines clause", () => {
    expect(nodeCheck(files())).toEqual([]);
    expect(nodeCheck(files({ nodeVersion: "22.18.0", dockerfile: IMAGE.replace("26.12.0", "22.18.3") }))).toEqual([]);
    expect(nodeCheck(files({ nodeVersion: "24.11.1", dockerfile: IMAGE.replace("26.12.0", "24.11.0") }))).toEqual([]);
  });

  test("finds an image of another major or minor release", () => {
    expect(nodeCheck(files({ dockerfile: IMAGE.replace("26.12.0", "26.13.0") }))).toEqual([
      "the Dockerfile's node image is 26.13.0 but .node-version is 26.12.1: they must be the same major and minor release",
    ]);
    expect(nodeCheck(files({ dockerfile: IMAGE.replace("26.12.0", "24.12.0") }))).toHaveLength(1);
    expect(nodeCheck(files({ dockerfile: "FROM debian:13\n" }))).toEqual(["the Dockerfile has no `FROM node:<version>-…` line"]);
  });

  test("finds a version outside engines.node", () => {
    expect(nodeCheck(files({ nodeVersion: "24.10.0", dockerfile: IMAGE.replace("26.12.0", "24.10.0") }))).toHaveLength(1);
    expect(nodeCheck(files({ nodeVersion: "23.1.0", dockerfile: IMAGE.replace("26.12.0", "23.1.0") }))).toHaveLength(1);
    expect(nodeCheck(files({ engines: "^22.18.0 || ^24.11.0" }))).toHaveLength(1);
  });

  test("finds a version that is not exact", () => {
    for (const v of ["26", "26.12", "v26.12.1", "lts/*", ""]) expect(nodeCheck(files({ nodeVersion: v }))).toHaveLength(1);
  });

  test("finds a workflow with its own Node version, or none", () => {
    expect(nodeCheck(files({ workflows: { "build.yml": "uses: actions/setup-node@abc\nwith:\n  node-version: 26.11.1\n" } }))).toEqual([
      ".github/workflows/build.yml sets node-version: use node-version-file: .node-version",
    ]);
    expect(nodeCheck(files({ workflows: { "scan.yml": "uses: actions/setup-node@abc\nwith:\n  cache: pnpm\n" } }))).toEqual([
      ".github/workflows/scan.yml runs setup-node without node-version-file: .node-version",
    ]);
    expect(nodeCheck(files({ workflows: { "release.yml": "run: echo no node here\n" } }))).toEqual([]);
  });

  test("finds a catalog missing a line, rather than passing", () => {
    expect(problems("catalog:\n  vitest: 5.0.5\n")).toEqual(["the catalog has no `vite-plus: <version>` line"]);
    expect(problems("catalog:\n  vite-plus: 1.4.0\n")).toEqual(["the catalog has no `vite: npm:@voidzero-dev/vite-plus-core@<version>` line"]);
  });
});
