// Markdown summary of the unit test coverage (coverage/coverage-summary.json, written by `pnpm test --coverage`),
// for the GitHub job summary: the totals, then every file the tests reach, least covered first.
// Usage: node scripts/coverage-summary.mjs >> "$GITHUB_STEP_SUMMARY"
import { readFileSync } from "node:fs";
import { relative } from "node:path";
import { fileURLToPath } from "node:url";

const web = fileURLToPath(new URL("../", import.meta.url));
const summary = JSON.parse(readFileSync(new URL("../coverage/coverage-summary.json", import.meta.url), "utf8"));
const KINDS = ["lines", "statements", "functions", "branches"];
const pct = (m) => `${m.pct.toFixed(1)}% (${m.covered}/${m.total})`;

const { total, ...files } = summary;
const out = ["## Frontend unit test coverage", "", "| | Lines | Statements | Functions | Branches |", "|---|---|---|---|---|"];
out.push(`| **Total** | ${KINDS.map((k) => pct(total[k])).join(" | ")} |`);
const reached = Object.entries(files)
  .filter(([, m]) => m.lines.covered > 0)
  .sort(([, a], [, b]) => a.lines.pct - b.lines.pct);
for (const [path, m] of reached) out.push(`| ${relative(web, path).replace(/\\/g, "/")} | ${KINDS.map((k) => pct(m[k])).join(" | ")} |`);
out.push("", `${Object.keys(files).length - reached.length} other files aren't reached by the unit tests.`, "");
console.log(out.join("\n"));
