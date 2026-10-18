// Markdown summary of an end-to-end run on CI (test-results/report.json, from the json reporter in playwright.config.ts),
// for the GitHub job summary: how many tests passed, failed and passed only when run again (CI retries once), with
// the names of those flaky ones. A retry lets the run pass; the summary keeps the flaky test from going unnoticed.
// Usage: node scripts/e2e-summary.mjs >> "$GITHUB_STEP_SUMMARY"
import { existsSync, readFileSync } from "node:fs";

const file = new URL("../test-results/report.json", import.meta.url);
if (!existsSync(file)) {
  console.log("## End-to-end tests\n\nNo report was written.\n");
  process.exit(0);
}
const report = JSON.parse(readFileSync(file, "utf8"));

/** Every test in the report, with its spec's file and title path */
function* tests(suite, path = []) {
  for (const spec of suite.specs ?? []) for (const test of spec.tests) yield { name: [...path, spec.title].join(" › "), file: spec.file, status: test.status };
  for (const child of suite.suites ?? []) yield* tests(child, suite.title && !suite.title.endsWith(".ts") ? [...path, suite.title] : path);
}

const all = report.suites.flatMap((suite) => [...tests(suite)]);
const flaky = all.filter((t) => t.status === "flaky");
const { expected = 0, unexpected = 0, skipped = 0 } = report.stats ?? {};
const out = ["## End-to-end tests", "", `${expected} passed, ${unexpected} failed, ${flaky.length} flaky (passed only when run again), ${skipped} skipped.`];
if (flaky.length) out.push("", "Flaky:", ...flaky.map((t) => `- \`${t.file}\`: ${t.name}`));
console.log(out.join("\n") + "\n");
