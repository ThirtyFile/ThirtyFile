// Turns `pnpm licenses list --prod --json` into plain-text licence notices, with the licence files each package ships.
// Used by the Dockerfile to write THIRD-PARTY-NOTICES into the image:
//   pnpm licenses list --prod --json | node scripts/third-party-notices.mjs > notices.txt
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const byLicence = JSON.parse(readFileSync(0, "utf8"));
const packages = Object.values(byLicence)
  .flat()
  .sort((a, b) => a.name.localeCompare(b.name));

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
