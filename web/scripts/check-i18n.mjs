// Localization check, for every language with a dictionary folder (src/lib/i18n/<lang>/; English is the source text):
//  1. every t("…") / tc("context", "…") English source text used in the code has an entry
//  2. the same key isn't given different translations in different files of one language
//  3. no Chinese or Japanese text is left outside the dictionary values, in code AND in comments
//     (src/**/*.{ts,tsx,css} and scripts/*.mjs). Lines marked `// i18n-ignore: <reason>` are allowed,
//     but the reason itself must be English. The only such text allowed in dictionary files is the values.
//  4. every message the server sends has an entry (see below)
//  5. no entry outside server.ts is left over: its English text still appears in web/src or server/src
//  6. a translation fits its language: no parameter the English text doesn't have, and no more plural forms than the
//     language has (Intl.PluralRules: Chinese and Japanese have one)
//  7. no interface text is written in JSX without t(): text between tags (`<Label>Client ID</Label>`), or a string
//     beside a translated one (`cond ? t("Application (client) ID") : "Client ID"`), which 1 can't see. A heuristic:
//     English words starting with a capital; lines marked `i18n-ignore` are allowed
// Missing (1, 4) and left-over (5) entries fail the check for the languages that must be complete (COMPLETE, and
// --require); for the others they are reported only. Every language offered to people (`ready` in src/lib/i18n.ts) must
// be complete. In a language that isn't complete yet, entries still holding their placeholder (the Traditional Chinese
// text in zh-CN, the English text in ja) are counted, as a measure of what is left to translate; once it is complete,
// such an entry is a translation that is written the same (a word spelled alike in both Chinese scripts, "OK" in Japanese).
// Usage: node scripts/check-i18n.mjs [--files path,path] [--lenient] [--require lang,lang]
//   --files    only check the given files (for 1 and 3)
//   --lenient  skip comments (only report Chinese in code); the default is strict
//   --require  also require these languages to be complete
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

/** Languages whose dictionaries must be complete: a new English text needs their translation in the same change */
const COMPLETE = ["zh-TW", "zh-CN", "ja"];

const toPath = (u) => fileURLToPath(new URL(u, import.meta.url));
const web = toPath("../");
const root = toPath("../src/");
const i18nDir = join(root, "lib/i18n");
const arg = (name) => (process.argv.includes(name) ? process.argv[process.argv.indexOf(name) + 1] : null);
const only = arg("--files")?.split(",") ?? null;
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
  return raw.slice(1, -1).replace(/\\(?:x([0-9a-fA-F]{2})|u\{([0-9a-fA-F]+)\}|u([0-9a-fA-F]{4})|(\r\n|[\s\S]))/g, (_, x, cp, u, c) => {
    if (x || u) return String.fromCharCode(parseInt(x ?? u, 16));
    if (cp) return String.fromCodePoint(parseInt(cp, 16));
    if (c === "\n" || c === "\r\n" || c === "\u2028" || c === "\u2029") return "";
    return ESCAPES[c] ?? c;
  });
}

// Dictionaries: each file is `export default { "English": "<translation>", ... }`
const entry = new RegExp(String.raw`^\s*(${STR})\s*:\s*(${STR}|\x60[^\x60]*\x60)\s*,?\s*$`, "gm");
const langs = readdirSync(i18nDir).filter((name) => statSync(join(i18nDir, name)).isDirectory());

/** Each language's entries: `dict` key -> { value, file }, every entry with its file, and conflicting ones */
const dictionaries = new Map();
for (const lang of langs) {
  const dict = new Map();
  const conflicts = [];
  const entries = [];
  for (const f of walk(join(i18nDir, lang), /\.ts$/)) {
    if (f.endsWith("index.ts")) continue;
    const file = relative(root, f).replace(/\\/g, "/");
    const src = readFileSync(f, "utf8");
    for (const m of src.matchAll(entry)) {
      const key = literal(m[1]);
      const value = literal(m[2]);
      const prev = dict.get(key);
      if (prev && prev.value !== value) conflicts.push(`${key}  →  ${prev.file}: ${prev.value}  /  ${file}: ${value}`);
      dict.set(key, { value, file });
      entries.push({ key, file });
    }
  }
  dictionaries.set(lang, { dict, conflicts, entries });
}

// The languages offered to people (src/lib/i18n.ts: `{ id: "ja", label: "…", ready: true }`) must be complete
const ready = [...readFileSync(join(root, "lib/i18n.ts"), "utf8").matchAll(/\{ id: "([\w-]+)", label: [^}\n]*\bready: true\b/g)].map((m) => m[1]).filter((l) => l !== "en");
const required = new Set([...COMPLETE, ...(arg("--require")?.split(",") ?? [])]);
const unrequired = ready.filter((l) => !required.has(l));
const unknown = [...required, ...ready].filter((l) => !dictionaries.has(l));

// Chinese and Japanese text: Han characters, kana, and full-width punctuation
const cjk = /[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uff66-\uff9f\u3001\u3002\u300c-\u300f\uff01\uff08\uff09\uff0c\uff1a\uff1b\uff1f]/;
const isComment = (s) => /^(\/\/|\*|\/\*|\{\/\*)/.test(s);
/** The line with its comments removed (block comments on one line, JSX comments, trailing // comments) */
const stripComments = (s) =>
  s
    .replace(/\{\/\*.*?\*\/\}/g, "")
    .replace(/\/\*.*?\*\//g, "")
    .replace(/(^|[^:"'`\\])\/\/.*$/, "$1");

/** The texts the code translates: `{ key, context?, where }` (tc() with its context) */
const used = [];
const bare = [];
/** The code outside the dictionaries, where their entries are used */
const corpus = [];
const files = [...walk(root, /\.(ts|tsx|css)$/).map((f) => [f, relative(root, f)]), ...walk(toPath("./"), /\.mjs$/).map((f) => [f, relative(web, f)])];
for (const [f, r] of files) {
  const rel = r.replace(/\\/g, "/");
  if (only && !only.some((o) => rel === o || rel.endsWith(o))) continue;
  const src = readFileSync(f, "utf8");
  // Every language's dictionary folder (lib/i18n/<lang>/)
  const isDict = /^lib\/i18n\/[^/]+\//.test(rel);
  if (!isDict && /\.(ts|tsx)$/.test(rel)) {
    corpus.push(src);
    const lineOf = (i) => src.slice(0, i).split("\n").length;
    // tc("context", "…"): either the context key or the plain key must exist
    for (const m of src.matchAll(new RegExp(String.raw`(?<![\w.])tc\(\s*"([^"]*)"\s*,\s*(${STR}|\x60[^\x60$]*\x60)`, "g"))) {
      used.push({ key: literal(m[2]), context: m[1], where: `${rel}:${lineOf(m.index)}` });
    }
    // t("…") / t('…') / t(`…`) (template literals without ${})
    for (const m of src.matchAll(new RegExp(String.raw`(?<![\w.])t\(\s*(${STR}|\x60[^\x60$]*\x60)`, "g"))) {
      used.push({ key: literal(m[1]), where: `${rel}:${lineOf(m.index)}` });
    }
  }
  // Chinese or Japanese left in code or comments
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
      // Values are in their language by design; anything else (comments) must be English
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
const serverMessages = [];
const serverDir = toPath("../../server/src/");
if (!only) {
  const call = new RegExp(String.raw`AppError::(?:bad_request|forbidden|conflict|not_found|new\(\s*[\w:]+\s*,)\(?\s*(?:format!\(\s*)?("(?:[^"\\]|\\.)*")`, "g");
  for (const f of walk(serverDir, /\.rs$/)) {
    const rel = "server/src/" + relative(serverDir, f).replace(/\\/g, "/");
    if (rel.endsWith("/dav.rs") || rel.includes("/dav/")) continue;
    const full = readFileSync(f, "utf8");
    corpus.push(full);
    // Tests come last in each file
    const src = full.split(/\n#\[cfg\(test\)\]\n/)[0];
    const lineOf = (i) => src.slice(0, i).split("\n").length;
    for (const m of src.matchAll(call)) serverMessages.push({ msg: literal(m[1]), where: `${rel}:${lineOf(m.index)}` });
  }
}

const shape = (s) => s.replace(/\{[^{}]*\}/g, "{}");
const escape = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const code = corpus.join("\n");
// How a text is spelled in code: as it is, inside "…" (escaped as in JSON), or inside '…', which the formatter picks
// for a text with more " than ' in it
const spellings = (s) => [s, JSON.stringify(s).slice(1, -1), s.replace(/[\\']/g, "\\$&")];
const withOtherNames = (w) =>
  new RegExp(
    w
      .split(/\{[^{}]*\}/)
      .map(escape)
      .join("\\{[^{}]*\\}"),
  );
const writtenCache = new Map();
const written = (s) => {
  let hit = writtenCache.get(s);
  if (hit === undefined) {
    hit = spellings(s).some((w) => code.includes(w)) || spellings(s).some((w) => withOtherNames(w).test(code));
    writtenCache.set(s, hit);
  }
  return hit;
};
const params = (s) => new Set([...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]));

/** Each language's findings */
function check(lang) {
  const { dict, conflicts, entries } = dictionaries.get(lang);
  const missing = [];
  for (const u of used) {
    if (u.context !== undefined ? !dict.has(`${u.context}::${u.key}`) && !dict.has(u.key) : !dict.has(u.key)) {
      missing.push(`${u.where}: ${u.context !== undefined ? `${u.context}::` : ""}${u.key}`);
    }
  }
  // A placeholder can also stand for a word the dictionary spells out ("{} {}" for "{n} files"), in either form of a
  // plural entry ("… day|… days")
  const shapes = new Set([...dict.keys()].map(shape));
  const forms = [...dict.keys()].flatMap((k) => k.split("|"));
  const matchesSome = (msg) => {
    const re = new RegExp(
      "^" +
        msg
          .split(/\{[^{}]*\}/)
          .map(escape)
          .join(".+?") +
        "$",
    );
    return forms.some((k) => re.test(k));
  };
  for (const { msg, where } of serverMessages) {
    if (!dict.has(msg) && !shapes.has(shape(msg)) && !matchesSome(msg)) missing.push(`${where}: ${msg}`);
  }

  // 5. Entries left over (outside server.ts, whose messages are also built from parts): the English text, or a form
  //    of a plural, or the text of a tc() key, is written somewhere in the code, as it is or with other placeholder names
  const unused = [];
  if (!only) {
    for (const { key, file } of entries) {
      if (file.endsWith("/server.ts")) continue;
      const text = key.includes("::") ? key.slice(key.indexOf("::") + 2) : key;
      if (![text, ...text.split("|")].some(written)) unused.push(`${file}: ${key}`);
    }
  }

  // 6. Translations that don't fit the language
  const categories = new Intl.PluralRules(lang).resolvedOptions().pluralCategories.length;
  const misfits = [];
  const base = dictionaries.get("zh-TW")?.dict;
  let placeholders = 0;
  for (const [key, { value, file }] of dict) {
    const english = key.includes("::") ? key.slice(key.indexOf("::") + 2) : key;
    const extra = [...params(value)].filter((p) => !params(english).has(p));
    if (extra.length) misfits.push(`${file}: ${key}: {${extra.join("}, {")}} isn't a parameter of the English text`);
    if (value.split("|").length > categories) misfits.push(`${file}: ${key}: more plural forms than ${lang} has (${categories})`);
    // Still the placeholder: the English text in ja, the Traditional Chinese text in zh-CN (until the language is complete)
    if (required.has(lang)) continue;
    if (lang === "ja" && /[A-Za-z]/.test(value) && english.split("|").includes(value)) placeholders++;
    if (lang === "zh-CN" && cjk.test(value) && base?.get(key)?.value === value) placeholders++;
  }
  return { size: dict.size, conflicts, missing: [...new Set(missing)], unused, misfits, placeholders };
}

const results = [...langs].sort((a, b) => Number(required.has(b)) - Number(required.has(a)) || a.localeCompare(b)).map((lang) => ({ lang, required: required.has(lang), ...check(lang) }));
/** At most this many lines of a list that doesn't fail the check */
const SHOWN = 10;
const list = (items, all) => {
  for (const m of all ? items : items.slice(0, SHOWN)) console.log("    ✗ " + m);
  if (!all && items.length > SHOWN) console.log(`    … and ${items.length - SHOWN} more`);
};

let failed = bare.length > 0 || unknown.length > 0 || unrequired.length > 0;
console.log(`Languages: ${results.map((r) => `${r.lang}${r.required ? " (must be complete)" : ""}`).join(", ")}`);
for (const r of results) {
  console.log(`${r.lang}: ${r.size} entries${r.required ? "" : ", missing and left-over entries reported only"}`);
  console.log(`  Conflicting entries across dictionary files: ${r.conflicts.length}`);
  list(r.conflicts, true);
  console.log(`  Missing translations: ${r.missing.length}`);
  list(r.missing, r.required);
  console.log(`  Dictionary entries no longer used: ${r.unused.length}`);
  list(r.unused, r.required);
  console.log(`  Translations that don't fit the language: ${r.misfits.length}`);
  list(r.misfits, true);
  if (r.placeholders) console.log(`  Placeholders not translated yet: ${r.placeholders}`);
  if (r.conflicts.length || r.misfits.length || (r.required && (r.missing.length || r.unused.length))) failed = true;
}
if (unrequired.length) console.log(`Languages offered in the interface that aren't required to be complete (add them to COMPLETE): ${unrequired.join(", ")}`);
if (unknown.length) console.log(`Languages without a dictionary folder: ${unknown.join(", ")}`);
console.log(`Chinese or Japanese text outside the dictionaries (not marked i18n-ignore${strict ? ", comments included" : ", comments skipped"}): ${bare.length}`);
for (const b of bare) console.log("  · " + b);

// 7. Interface text in JSX that doesn't go through t()
const literals = [];
{
  const between = />\s*([A-Z][a-z]+(?: [A-Za-z][A-Za-z]*)*)\s*<\//;
  const beside = /[?:]\s*"([A-Z][A-Za-z]*(?: [A-Za-z][A-Za-z]*)*)"\s*[})]/;
  for (const [f, r] of files) {
    const rel = r.replaceAll("\\", "/");
    if (!rel.endsWith(".tsx") || (only && !only.some((o) => rel === o || rel.endsWith(o)))) continue;
    readFileSync(f, "utf8")
      .split("\n")
      .forEach((line, i) => {
        if (line.includes("i18n-ignore") || /^\s*(\/\/|\*|\/\*)/.test(line)) return;
        const m = line.match(between) ?? (/\bt\(|\btc\(/.test(line) ? line.match(beside) : null);
        if (m) literals.push(`${rel}:${i + 1}: ${m[1]}`);
      });
  }
}
console.log(`Interface text in JSX without t() (not marked i18n-ignore): ${literals.length}`);
for (const l of literals) console.log("  · " + l);
if (literals.length) failed = true;
process.exitCode = failed ? 1 : 0;
