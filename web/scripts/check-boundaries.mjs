// Folders that must not depend on the rest of the app, so that they can move out of it (into a package of their own)
// with `git mv`. Every import in them, static, dynamic, `export … from` or type-only, must be a relative path that stays
// inside the folder, or one of the packages the folder may use.
//   src/ooxml: the Office code (reading and showing Word, PowerPoint and Excel files, charts, formulas); the app says
//   its coded errors in words (lib/officeErrors.ts)
// Usage: node scripts/check-boundaries.mjs
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const BOUNDARIES = [{ folder: "src/ooxml", packages: ["jszip"] }];

const walk = (dir, out = []) => {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, out);
    else if (/\.(ts|tsx|js|mjs)$/.test(name)) out.push(p);
  }
  return out;
};

// import … from "x", import "x", export … from "x", import("x"), import type … from "x"; comments are left out first
const SPECIFIER = /\b(?:import|export)\s+(?:type\s+)?(?:[\w*{}\s,$]+\s+from\s+)?["']([^"']+)["']|\bimport\(\s*["']([^"']+)["']\s*\)/g;
const stripComments = (s) => s.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`\\])\/\/.*$/gm, "$1");

/** The imports of the files under `root` that leave it: `file:line: why`, paths relative to `base` */
export function crossings(root, packages, base = root) {
  const problems = [];
  for (const file of walk(root)) {
    const src = stripComments(readFileSync(file, "utf8"));
    for (const m of src.matchAll(SPECIFIER)) {
      const spec = m[1] ?? m[2];
      const where = `${relative(base, file).replace(/\\/g, "/")}:${src.slice(0, m.index).split("\n").length}`;
      if (spec.startsWith(".")) {
        const target = resolve(dirname(file), spec);
        if (target !== root && !target.startsWith(root + sep)) problems.push(`${where}: "${spec}" leads outside the folder`);
      } else if (!packages.some((p) => spec === p || spec.startsWith(p + "/"))) {
        problems.push(`${where}: "${spec}" isn't a relative path inside the folder${packages.length ? ` or one of: ${packages.join(", ")}` : ""}`);
      }
    }
  }
  return problems;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const web = fileURLToPath(new URL("../", import.meta.url));
  const problems = BOUNDARIES.flatMap(({ folder, packages }) => crossings(join(web, folder), packages, web));
  console.log(`Imports that leave ${BOUNDARIES.map((b) => b.folder).join(", ")}: ${problems.length}`);
  for (const p of problems) console.log("  ✗ " + p);
  process.exitCode = problems.length ? 1 : 0;
}
