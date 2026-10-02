// The end-to-end tests run on several workers at once against one server, whose changes wait their turn for each other
// (a large one takes up to half a minute), and a test may run again on the same server. So in tests/e2e:
//   - no fixed waits: a test waits for the server's answer (answer, listing, uploadFinished in helpers.ts), an element,
//     or the page's clock moved on by the test (page.clock);
//   - no names made from the time, which two tests started in the same millisecond share: unique() in helpers.ts;
//   - folders and accounts are made with makeFolder and makeUser from helpers.ts, which give their names a random part.
// Usage: node scripts/check-e2e.mjs
import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const RULES = [
  {
    pattern: /\bwaitForTimeout\s*\(/g,
    why: "a fixed wait: wait for the server's answer (answer, uploadFinished in helpers.ts), an element, or move page.clock on",
  },
  {
    // (not test.setTimeout, which sets how long a test may take)
    pattern: /(?<!\btest\.)\bsetTimeout\s*\(/g,
    why: "a fixed delay: hold a request with a promise the test resolves, or move page.clock on",
  },
  {
    pattern: /["']networkidle["']/g,
    why: "networkidle comes once per page load, not after an action: wait for the server's answer (answer, listing in helpers.ts)",
  },
  {
    pattern: /\bDate\.now\(\)\.toString\(/g,
    why: "a name made from the time is the same for two tests started in the same millisecond: use unique() from helpers.ts",
  },
  {
    pattern: /\.post\(\s*["'`]\/api\/(?:folders|admin\/users)["'`]/g,
    why: "make folders and accounts with makeFolder and makeUser from helpers.ts, which give their names a random part",
    except: "helpers.ts",
  },
];

// Comments are left out first (a URL's "//" follows a colon, so it stays)
const stripComments = (s) => s.replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, " ")).replace(/(^|[^:"'`\\])\/\/.*$/gm, "$1");

/** What breaks the rules in one file's `source`: `file:line: why` */
export function problems(file, source) {
  const src = stripComments(source);
  const found = [];
  for (const rule of RULES) {
    if (rule.except && file.replace(/\\/g, "/").endsWith(rule.except)) continue;
    for (const m of src.matchAll(rule.pattern)) found.push({ line: src.slice(0, m.index).split("\n").length, why: rule.why });
  }
  return found.sort((a, b) => a.line - b.line).map((p) => `${file}:${p.line}: ${p.why}`);
}

/** What breaks the rules in the end-to-end tests under `dir` */
export function check(dir) {
  return readdirSync(dir)
    .filter((name) => name.endsWith(".ts"))
    .sort()
    .flatMap((name) => problems(`tests/e2e/${name}`, readFileSync(join(dir, name), "utf8")));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const found = check(fileURLToPath(new URL("../tests/e2e/", import.meta.url)));
  console.log(`Fixed waits and names without a unique part in tests/e2e: ${found.length}`);
  for (const p of found) console.log("  ✗ " + p);
  process.exitCode = found.length ? 1 : 0;
}
