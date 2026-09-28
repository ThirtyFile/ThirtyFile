// Localization check:
//  1. every t("…") / tc("context", "…") English source text used in the code has a Traditional Chinese entry
//  2. the same key isn't given different Chinese in different zh-TW dictionary files
//  3. no Chinese text is left outside the zh-TW dictionary values, in code AND in comments
//     (src/**/*.{ts,tsx,css} and scripts/*.mjs). Lines marked `// i18n-ignore: <reason>` are allowed,
//     but the reason itself must be English. The only Chinese allowed in zh-TW dictionary files is the values.
// Usage: node scripts/check-i18n.mjs [--files path,path] [--lenient]
//   --files    only check the given files
//   --lenient  skip comments (only report Chinese in code); the default is strict
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const toPath = (u) => fileURLToPath(new URL(u, import.meta.url));
const web = toPath("../");
const root = toPath("../src/");
const dictDir = join(root, "lib/i18n/zh-TW");
const only = process.argv.includes("--files") ? process.argv[process.argv.indexOf("--files") + 1].split(",") : null;
const strict = !process.argv.includes("--lenient");

function walk(dir, ext, out = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, ext, out);
    else if (ext.test(name)) out.push(p);
  }
  return out;
}

const STR = String.raw`"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'`;
const ESCAPES = { n: "\n", r: "\r", t: "\t", b: "\b", f: "\f", v: "\v", 0: "\0" };
/**
 * A string literal's value: "…", '…' or a template literal without ${}. JavaScript escapes are decoded directly
 * (JSON doesn't accept \x41, \0, \' or line continuations).
 */
function literal(raw) {
  if (raw.startsWith("`")) return raw.slice(1, -1);
  return raw
    .slice(1, -1)
    .replace(/\\(?:x([0-9a-fA-F]{2})|u\{([0-9a-fA-F]+)\}|u([0-9a-fA-F]{4})|(\r\n|[\s\S]))/g, (_, x, cp, u, c) => {
      if (x || u) return String.fromCharCode(parseInt(x ?? u, 16));
      if (cp) return String.fromCodePoint(parseInt(cp, 16));
      if (c === "\n" || c === "\r\n" || c === "\u2028" || c === "\u2029") return "";
      return ESCAPES[c] ?? c;
    });
}

// Dictionaries: each file is `export default { "English": "<Traditional Chinese>", ... }`
const entry = new RegExp(String.raw`^\s*(${STR})\s*:\s*(${STR}|\x60[^\x60]*\x60)\s*,?\s*$`, "gm");
const dict = new Map(); // key -> { zh, file }
const conflicts = [];
for (const f of walk(dictDir, /\.ts$/)) {
  if (f.endsWith("index.ts")) continue;
  const file = relative(root, f).replace(/\\/g, "/");
  const src = readFileSync(f, "utf8");
  for (const m of src.matchAll(entry)) {
    const key = literal(m[1]);
    const zh = literal(m[2]);
    const prev = dict.get(key);
    if (prev && prev.zh !== zh) conflicts.push(`${key}  →  ${prev.file}: ${prev.zh}  /  ${file}: ${zh}`);
    dict.set(key, { zh, file });
  }
}

// Chinese text: Han characters and Chinese (full-width) punctuation
const cjk = /[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\u3001\u3002\u300c-\u300f\uff01\uff08\uff09\uff0c\uff1a\uff1b\uff1f]/;
const isComment = (s) => /^(\/\/|\*|\/\*|\{\/\*)/.test(s);
/** The line with its comments removed (block comments on one line, JSX comments, trailing // comments) */
const stripComments = (s) =>
  s.replace(/\{\/\*.*?\*\/\}/g, "").replace(/\/\*.*?\*\//g, "").replace(/(^|[^:"'`\\])\/\/.*$/, "$1");

const missing = [];
const bare = [];
const files = [
  ...walk(root, /\.(ts|tsx|css)$/).map((f) => [f, relative(root, f)]),
  ...walk(toPath("./"), /\.mjs$/).map((f) => [f, relative(web, f)]),
];
for (const [f, r] of files) {
  const rel = r.replace(/\\/g, "/");
  if (only && !only.some((o) => rel === o || rel.endsWith(o))) continue;
  const src = readFileSync(f, "utf8");
  const isDict = rel.startsWith("lib/i18n/zh-TW/");
  if (!isDict && /\.(ts|tsx)$/.test(rel)) {
    const lineOf = (i) => src.slice(0, i).split("\n").length;
    // tc("context", "…"): either the context key or the plain key must exist
    for (const m of src.matchAll(new RegExp(String.raw`(?<![\w.])tc\(\s*"([^"]*)"\s*,\s*(${STR}|\x60[^\x60$]*\x60)`, "g"))) {
      const key = literal(m[2]);
      if (!dict.has(`${m[1]}::${key}`) && !dict.has(key)) missing.push(`${rel}:${lineOf(m.index)}: ${m[1]}::${key}`);
    }
    // t("…") / t('…') / t(`…`) (template literals without ${})
    for (const m of src.matchAll(new RegExp(String.raw`(?<![\w.])t\(\s*(${STR}|\x60[^\x60$]*\x60)`, "g"))) {
      const key = literal(m[1]);
      if (!dict.has(key)) missing.push(`${rel}:${lineOf(m.index)}: ${key}`);
    }
  }
  // Chinese left in code or comments
  src.split("\n").forEach((line, i) => {
    const s = line.trim();
    if (!cjk.test(s)) return;
    const report = (kind) => bare.push(`${rel}:${i + 1}: [${kind}] ${s.slice(0, 120)}`);
    const ignore = s.match(/i18n-ignore: (\S.*)$/);
    if (ignore) {
      if (strict && cjk.test(ignore[1])) report("i18n-ignore reason");
      return;
    }
    if (isDict) {
      // Values are Chinese by design; anything else (comments) must be English
      if (!strict) return;
      entry.lastIndex = 0;
      if (entry.test(line)) return;
      if (cjk.test(s.replace(new RegExp(STR, "g"), ""))) report("comment");
      return;
    }
    const inCode = !isComment(s) && cjk.test(stripComments(s));
    if (inCode) report("code");
    else if (strict) report("comment");
  });
}

// 4. Messages the server sends (AppError::…("…") in server/src, outside tests and WebDAV, whose clients show no
//    translations): each has an entry, shown with tServer(). Placeholders count as the same whatever their names.
const shape = (s) => s.replace(/\{[^{}]*\}/g, "{}");
const shapes = new Set([...dict.keys()].map(shape));
// A placeholder can also stand for a word the dictionary spells out ("{} {}" for "{n} files"), in either form of a
// plural entry ("… day|… days")
const forms = [...dict.keys()].flatMap((k) => k.split("|"));
const matchesSome = (msg) => {
  const re = new RegExp("^" + msg.split(/\{[^{}]*\}/).map((p) => p.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join(".+?") + "$");
  return forms.some((k) => re.test(k));
};
const serverDir = toPath("../../server/src/");
if (!only) {
  const call = new RegExp(
    String.raw`AppError::(?:bad_request|forbidden|conflict|not_found|new\(\s*[\w:]+\s*,)\(?\s*(?:format!\(\s*)?("(?:[^"\\]|\\.)*")`,
    "g",
  );
  for (const f of walk(serverDir, /\.rs$/)) {
    const rel = "server/src/" + relative(serverDir, f).replace(/\\/g, "/");
    if (rel.endsWith("/dav.rs")) continue;
    const full = readFileSync(f, "utf8");
    // Tests come last in each file
    const src = full.split(/\n#\[cfg\(test\)\]\n/)[0];
    const lineOf = (i) => src.slice(0, i).split("\n").length;
    for (const m of src.matchAll(call)) {
      const msg = literal(m[1]);
      if (!dict.has(msg) && !shapes.has(shape(msg)) && !matchesSome(msg)) missing.push(`${rel}:${lineOf(m.index)}: ${msg}`);
    }
  }
}

console.log(`Dictionary: ${dict.size} entries`);
console.log(`Conflicting entries across dictionary files: ${conflicts.length}`);
for (const c of conflicts) console.log("  ✗ " + c);
console.log(`Missing Traditional Chinese translations: ${new Set(missing).size}`);
for (const m of [...new Set(missing)]) console.log("  ✗ " + m);
console.log(
  `Chinese text outside the dictionary (not marked i18n-ignore${strict ? ", comments included" : ", comments skipped"}): ${bare.length}`,
);
for (const b of bare) console.log("  · " + b);
process.exitCode = missing.length || bare.length || conflicts.length ? 1 : 0;
