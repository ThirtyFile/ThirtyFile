// Vite+ is installed twice in web/pnpm-workspace.yaml: `vite-plus` (the command line) and `vite`, an alias of its core
// package. They have to be the same release, and a tool that raises one version number does not know about the other
// (Dependabot raised `vite-plus` alone), so this fails when they differ.
// Usage: node scripts/check-toolchain.mjs
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

/** The problems with the catalog text of pnpm-workspace.yaml (none: an empty list) */
export function catalogProblems(text) {
  const plus = /^ {2}vite-plus: *["']?([\w.+-]+)["']?\s*$/m.exec(text)?.[1];
  const core = /^ {2}vite: *["']?npm:@voidzero-dev\/vite-plus-core@([\w.+-]+)["']?\s*$/m.exec(text)?.[1];
  if (!plus) return ["the catalog has no `vite-plus: <version>` line"];
  if (!core) return ["the catalog has no `vite: npm:@voidzero-dev/vite-plus-core@<version>` line"];
  if (plus !== core) {
    return [`\`vite-plus\` is ${plus} but \`vite\` (vite-plus-core) is ${core}: raise both, in the same pull request`];
  }
  return [];
}

if (process.argv[1]?.endsWith("check-toolchain.mjs")) {
  const file = fileURLToPath(new URL("../pnpm-workspace.yaml", import.meta.url));
  const problems = catalogProblems(readFileSync(file, "utf8"));
  for (const p of problems) console.error(`web/pnpm-workspace.yaml: ${p}`);
  process.exit(problems.length ? 1 : 0);
}
