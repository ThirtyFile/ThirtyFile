// Turns `pnpm licenses list --json` into plain-text licence notices, with the licence files each package ships, and
// checks every licence against the allowed list below. Used by the Dockerfile to write THIRD-PARTY-NOTICES into the
// image, and by the tests on GitHub (which only check the licences):
//   pnpm licenses list --prod --json > prod.json
//   pnpm licenses list --dev --json > dev.json
//   node scripts/third-party-notices.mjs prod.json dev.json > notices.txt
// The notices cover the runtime dependencies, and the few development packages whose code ends up in the interface
// anyway (BUNDLED). A licence that isn't allowed stops it with a message.
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

// The licences server/deny.toml allows for crates, and the font licence of the typeface
const ALLOWED = new Set([
  "0BSD",
  "Apache-2.0",
  "Apache-2.0 WITH LLVM-exception",
  "BSD-1-Clause",
  "BSD-3-Clause",
  "BSL-1.0",
  "CC0-1.0",
  "CDLA-Permissive-2.0",
  "ISC",
  "MIT",
  "MIT-0",
  "OFL-1.1",
  "Unicode-3.0",
  "Unlicense",
  "Zlib",
]);
// Development packages built into the interface: Tailwind's base styles, and shadcn's Tailwind variants (style.css)
const BUNDLED = new Set(["shadcn", "tailwindcss"]);
// Packages whose metadata names no licence, with the one their own documentation gives
const DECLARED = { "combine-errors": "MIT" };

const [prodFile, devFile] = process.argv.slice(2);
if (!prodFile) {
  console.error("usage: node scripts/third-party-notices.mjs prod.json [dev.json]");
  process.exit(2);
}
const read = (file) => Object.values(JSON.parse(readFileSync(file, "utf8"))).flat();
const packages = [...read(prodFile), ...(devFile ? read(devFile).filter((p) => BUNDLED.has(p.name)) : [])]
  .map((p) => (p.license === "Unknown" && DECLARED[p.name] ? { ...p, license: DECLARED[p.name] } : p))
  .sort((a, b) => a.name.localeCompare(b.name));

// An SPDX expression is allowed when one of its alternatives (OR) has only allowed licences (AND)
const allowed = (expression) =>
  expression
    .replace(/[()]/g, "")
    .split(/\s+OR\s+/)
    .some((alternative) => alternative.split(/\s+AND\s+/).every((id) => ALLOWED.has(id.trim())));
const refused = packages.filter((p) => !allowed(p.license));
if (refused.length) {
  console.error("These npm packages have a licence that isn't allowed (web/scripts/third-party-notices.mjs):");
  for (const p of refused) console.error(`  ${p.name} ${p.versions.join(", ")}: ${p.license}`);
  process.exit(1);
}

// LICENSE, LICENCE.md, COPYING, NOTICE… but not e.g. license-checker.js
const noticeFile = /^(licen[cs]e|copying|notice)([-._][\w-]+)?(\.(md|markdown|txt))?$/i;
const rule = "=".repeat(80);

const out = [`JAVASCRIPT PACKAGES OF THE THIRTYFILE WEB INTERFACE (${packages.length})`];
for (const p of packages) {
  out.push("", rule, `${p.name} ${p.versions.join(", ")}`, `Licence: ${p.license}`);
  if (p.author) out.push(`Author: ${p.author}`);
  if (p.homepage) out.push(`Homepage: ${p.homepage}`);
  out.push("-".repeat(80));
  // One text per distinct file (several installed versions usually ship the same licence)
  const texts = new Set();
  for (const dir of p.paths ?? []) {
    for (const name of readdirSync(dir).filter((f) => noticeFile.test(f)).sort()) {
      texts.add(readFileSync(join(dir, name), "utf8").trim());
    }
  }
  out.push(texts.size ? [...texts].join(`\n${"-".repeat(80)}\n`) : `The package includes no licence file; its licence is ${p.license}.`);
}
process.stdout.write(`${out.join("\n")}\n`);
