// The built interface's size budget (run after `pnpm build`): what every page downloads before it shows anything, any
// one chunk, and the Mac style's chunks, which the Windows style never loads. Vite's own chunk-size warning is set high
// (vite.config.ts), so this is what notices a chunk that grew or an import that pulled a lazy part into the start.
//   node scripts/check-bundle.mjs
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const dist = new URL("../dist/", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
const KB = 1024;
/** JavaScript loaded with the page (the entry and what it preloads); 392 kB in October 2026 */
const START_BUDGET = 450 * KB;
/** Any chunk (the largest, pdf.js for PDF thumbnails, is 476 kB) */
const CHUNK_BUDGET = 600 * KB;
/** Chunks only the Mac style uses */
const MAC_ONLY = /^(macKit|art)-/;

const html = readFileSync(join(dist, "index.html"), "utf8");
const start = [...html.matchAll(/(?:src|href)="\/assets\/([^"]+\.js)"/g)].map((m) => m[1]);
const size = (file) => statSync(join(dist, "assets", file)).size;
const problems = [];

const startBytes = start.reduce((n, f) => n + size(f), 0);
if (startBytes > START_BUDGET) problems.push(`the page starts with ${(startBytes / KB).toFixed(0)} kB of JavaScript (budget ${START_BUDGET / KB} kB): ${start.join(", ")}`);
for (const f of start.filter((f) => MAC_ONLY.test(f))) problems.push(`${f} is the Mac style's, but loads with every page`);
for (const f of readdirSync(join(dist, "assets")).filter((f) => f.endsWith(".js"))) {
  if (size(f) > CHUNK_BUDGET) problems.push(`${f} is ${(size(f) / KB).toFixed(0)} kB (budget ${CHUNK_BUDGET / KB} kB)`);
}

console.log(`Interface size: ${(startBytes / KB).toFixed(0)} kB of JavaScript at the start (${start.length} files)`);
if (problems.length) {
  for (const p of problems) console.error(`  ${p}`);
  process.exit(1);
}
