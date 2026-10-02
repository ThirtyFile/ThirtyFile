//! Localization: English, Traditional Chinese (zh-TW), Simplified Chinese (zh-CN) and Japanese (ja).
//!
//! Source text is English: `t("{n} item selected|{n} items selected", { n })`. English uses the source text as is;
//! every other language looks it up in its own dictionary (`lib/i18n/<lang>/`, the same files in each) and falls back
//! to the English source when an entry is missing.
//! - Parameters are written as `{name}`; English plurals are `single|plural`, picked by `n` (or `count`)
//! - Number parameters are formatted for the locale (1,234); write whole sentences, not pieces joined in code
//! - `tc(context, "English")` is for English text that needs different words in different places (key `context::English`)
//! - Messages returned by the server (errors, activity log details…) are English; `tServer()` translates them:
//!   exact match first, then templates with parameters
//! - The language is decided at load time (so module-level constants can call t); switching reloads the page
//! - Only the page's own language is downloaded: the server adds its dictionary script (`<lang>.js`) to the page, or
//!   `loadDictionary()` fetches it before the app starts

export type Lang = "en" | "zh-TW" | "zh-CN" | "ja";

interface Language {
  id: Lang;
  /** The language's name in its own language */
  label: string;
  /**
   * Offered in the language switch and picked from the browser's languages. A language whose dictionary isn't
   * translated yet stays hidden, so nobody gets half-translated pages, except with the preview flag (`PREVIEW_KEY`)
   */
  ready: boolean;
}

/** Every language the interface knows, English first */
export const LANGUAGES: Language[] = [
  { id: "en", label: "English", ready: true },
  { id: "zh-TW", label: "繁體中文", ready: true }, // i18n-ignore: language name shown in its own language
  { id: "zh-CN", label: "简体中文", ready: false }, // i18n-ignore: language name shown in its own language
  { id: "ja", label: "日本語", ready: false }, // i18n-ignore: language name shown in its own language
];

/** `localStorage[PREVIEW_KEY] = "1"` also offers the languages that aren't ready, to check their translations */
export const PREVIEW_KEY = "tf-lang-preview";

function previewing() {
  try {
    return localStorage.getItem(PREVIEW_KEY) === "1";
  } catch {
    return false; // storage unavailable
  }
}

/** The languages people can use: the ready ones, and the others too while previewing */
export const LANGS: Language[] = LANGUAGES.filter((l) => l.ready || previewing());

const usable = (v: unknown): v is Lang => LANGS.some((l) => l.id === v);

const KEY = "tf-lang";

declare global {
  interface Window {
    /** System default language set by an administrator, injected by the server into the home page HTML (absent = follow the browser) */
    __TF_DEFAULT_LANG__?: string;
  }
}

/**
 * The interface language for a browser language tag, if there is one: `zh-Hant`, `zh-TW`, `zh-HK` and `zh-MO` →
 * zh-TW; `zh-Hans`, `zh-CN`, `zh-SG` and plain `zh` → zh-CN; `ja…` → ja; `en…` → en. A script subtag decides before a
 * region (`zh-Hans-HK` is Simplified)
 */
export function matchLanguage(tag: string): Lang | undefined {
  const [base, ...rest] = tag.toLowerCase().split(/[-_]/);
  if (base === "en") return "en";
  if (base === "ja") return "ja";
  if (base !== "zh") return undefined;
  if (rest.includes("hant")) return "zh-TW";
  if (rest.includes("hans")) return "zh-CN";
  return rest.some((r) => r === "tw" || r === "hk" || r === "mo") ? "zh-TW" : "zh-CN";
}

/**
 * The language for the browser's languages, in their order of preference: the first that matches one people can use.
 * Simplified Chinese falls back to Traditional Chinese while it isn't ready (a Chinese reader rather reads that than
 * English); a language that isn't ready otherwise gives way to the browser's next one. English when none matches
 */
export function browserLanguage(tags: readonly string[], can: (l: Lang) => boolean = usable): Lang {
  for (const tag of tags) {
    const match = matchLanguage(tag);
    if (!match) continue;
    if (can(match)) return match;
    if (match === "zh-CN" && can("zh-TW")) return "zh-TW";
  }
  return "en";
}

/** Order: the user's own choice, then the system default, then the browser's languages */
function detect(): Lang {
  try {
    const saved = localStorage.getItem(KEY);
    if (usable(saved)) return saved;
  } catch {
    // storage unavailable: fall back to the system default / browser language
  }
  const preset = typeof window !== "undefined" ? window.__TF_DEFAULT_LANG__ : undefined;
  if (usable(preset)) return preset;
  if (typeof navigator === "undefined") return "en";
  return browserLanguage(navigator.languages?.length ? navigator.languages : [navigator.language ?? ""]);
}

/** A dictionary module (`lib/i18n/<lang>/index.ts`, also built as the script `dist/<lang>.js`) */
interface Dictionary {
  LANG: string;
  DICT: Record<string, string>;
}

declare global {
  interface Window {
    /** Dictionary script the server added to the page for the language it expects (dist/<lang>.js) */
    __TF_DICT__?: Dictionary;
  }
}

/** Each language's dictionary, a chunk of its own */
const DICTIONARIES: Record<Exclude<Lang, "en">, () => Promise<Dictionary>> = {
  "zh-TW": () => import("@/lib/i18n/zh-TW"),
  "zh-CN": () => import("@/lib/i18n/zh-CN"),
  ja: () => import("@/lib/i18n/ja"),
};

export const lang: Lang = detect();

/** The dictionary the server put on the page, when it is this language's */
function preloaded() {
  const d = typeof window !== "undefined" ? window.__TF_DICT__ : undefined;
  return d?.LANG === lang ? d.DICT : undefined;
}

/**
 * The active language's dictionary: the script the server put on the page, if any, else empty until `loadDictionary()`
 * fetched it (English needs none)
 */
let DICT: Record<string, string> = preloaded() ?? {};

/**
 * Load the dictionary for the active language. Called by main.tsx before the app's modules are evaluated.
 * Usually the server already put the dictionary on the page (no extra round trip); otherwise it's fetched now.
 */
export async function loadDictionary() {
  if (lang === "en") return;
  DICT = preloaded() ?? (await DICTIONARIES[lang]()).DICT;
}
/**
 * Locale used by Intl. English follows the browser's region when it is an English one (en-GB: day/month/year and
 * 24-hour times), else en-US
 */
function englishLocale() {
  const nav = typeof navigator !== "undefined" ? (navigator.languages?.[0] ?? navigator.language ?? "") : "";
  if (!nav.toLowerCase().startsWith("en")) return "en-US";
  try {
    return Intl.DateTimeFormat.supportedLocalesOf([nav])[0] ?? "en-US";
  } catch {
    return "en-US"; // malformed language tag
  }
}

export const locale = lang === "en" ? englishLocale() : lang;

/**
 * The page's `<html lang>`. Chinese and Japanese share characters that are drawn differently, so the tag carries the
 * script, and the fonts follow it (style.css): Japanese text gets Japanese glyphs, Simplified Chinese Simplified ones
 */
export const htmlLang = ({ en: "en", "zh-TW": "zh-Hant", "zh-CN": "zh-Hans", ja: "ja" } as const)[lang];

if (typeof document !== "undefined") {
  document.documentElement.lang = htmlLang;
  // Files generated by the server (CSV exports etc.) use this cookie to pick the language. Only an explicit choice
  // (setLang) also sets tf_lang_chosen: without it the server keeps following the system default language
  document.cookie = `tf_lang=${lang}; path=/; max-age=31536000; SameSite=Lax`;
}

export function setLang(next: Lang) {
  if (next === lang) return;
  try {
    localStorage.setItem(KEY, next);
  } catch {
    // storage unavailable: only affects this page load
  }
  document.cookie = `tf_lang=${next}; path=/; max-age=31536000; SameSite=Lax`;
  document.cookie = "tf_lang_chosen=1; path=/; max-age=31536000; SameSite=Lax";
  window.location.reload();
}

type Vars = Record<string, string | number | null | undefined>;

/** Insert parameters; numbers get the locale's digit grouping (1,234 items) */
function fill(text: string, vars?: Vars) {
  if (!vars) return text;
  return text.replace(/\{(\w+)\}/g, (m, k: string) => {
    const v = vars[k];
    if (v === undefined || v === null) return m;
    return typeof v === "number" ? v.toLocaleString(locale) : v;
  });
}

/** English plurals: `single|plural`, picked by n (plural when there is no n) */
function plural(text: string, vars?: Vars) {
  const bar = text.indexOf("|");
  if (bar < 0) return text;
  const n = vars?.n ?? vars?.count;
  return Number(n) === 1 ? text.slice(0, bar) : text.slice(bar + 1);
}

const missing = new Set<string>();

function lookup(key: string, fallback: string, vars?: Vars) {
  const value = DICT[key];
  if (value === undefined) {
    if (import.meta.env.DEV && !missing.has(fallback)) {
      missing.add(fallback);
      console.warn("[i18n] Missing translation:", fallback);
    }
    return fill(plural(fallback, vars), vars);
  }
  return fill(value, vars);
}

/** Translate UI text (the key is the English source text) */
export function t(en: string, vars?: Vars): string {
  if (lang === "en") return fill(plural(en, vars), vars);
  return lookup(en, en, vars);
}

/**
 * For English text that another language words differently in different places
 * (e.g. "Unlimited" takes a longer Chinese wording in forms but a shorter one in tables).
 * The dictionary key is `context::English`; falls back to the plain key.
 */
export function tc(context: string, en: string, vars?: Vars): string {
  if (lang === "en") return fill(plural(en, vars), vars);
  const key = `${context}::${en}`;
  return lookup(DICT[key] !== undefined ? key : en, en, vars);
}

// ───────────── Server messages ─────────────

interface Template {
  re: RegExp;
  names: string[];
  /** The translation, with the same parameters */
  text: string;
  /** Length of the fixed text (parameters excluded) */
  fixed: number;
}

let exact: Map<string, string> | null = null;
let templates: Template[] | null = null;
/** The templates of names ThirtyFile makes (MADE_NAME), tried apart from the others: see translate() */
let madeTemplates: Template[] = [];

/** Every English form of a dictionary key: `single|plural` keys give both forms */
const forms = (key: string) => (key.includes("|") ? key.split("|") : [key]);

/** Build the lookup tables once: exact messages and templates from keys with parameters */
function build() {
  exact = new Map();
  const list: Template[] = [];
  const made: Template[] = [];
  for (const [key, text] of Object.entries(DICT)) {
    if (key.includes("::")) continue;
    for (const en of forms(key)) {
      if (!/\{\w+\}/.test(en)) {
        exact.set(en, text);
        continue;
      }
      const names: string[] = [];
      const pattern = en
        .split(/(\{\w+\})/)
        .map((part) => {
          const m = /^\{(\w+)\}$/.exec(part);
          if (m) {
            names.push(m[1]);
            // counts only match numbers, so "{n} files" can't swallow arbitrary text
            return m[1] === "n" || m[1] === "count" ? "(\\d[\\d,.]*)" : "([\\s\\S]+?)";
          }
          return part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
        })
        .join("");
      const tpl = { re: new RegExp(`^${pattern}$`), names, text, fixed: en.replace(/\{\w+\}/g, "").length };
      if (MADE_NAME.test(en)) made.push(tpl);
      else list.push(tpl);
    }
  }
  // Templates with more fixed text are tried first, so loose ones like "{a}: {b}" don't win
  templates = list.sort((a, b) => b.fixed - a.fixed);
  madeTemplates = made;
}

/** Parameters that carry a whole nested message (e.g. "Connection test failed: {error}") */
const NESTED = new Set(["error", "detail", "reason", "message"]);
/** Parameters that carry a list of fragments joined with ", " (e.g. "{username}: {changes}") */
const LISTS = new Set(["changes", "providers"]);

/**
 * Names ThirtyFile gives what an administrator left unnamed, stored in English (server/src/backups/api.rs,
 * replicas/api.rs): shown in the interface's language wherever they appear, like "My files"
 */
const MADE_NAME = /^(?:Backup|Copy|Replicas) of .+$/s;

/** A list item's translation: a known term or a template's; undefined when there is none */
function known(part: string, depth: number): string | undefined {
  const hit = exact!.get(part);
  if (hit !== undefined) return hit;
  const out = templated(part, depth);
  return out === undefined || out === part ? undefined : out;
}

/**
 * A list of items joined with ", " (share link options, the changes made to something): each item translated, joined
 * with the Chinese enumeration comma U+3001. The first item may start with a capital (the server writes the list as a
 * sentence). Undefined unless every item is known, or `loose` (a parameter that always holds a list)
 */
function list(v: string, depth: number, loose: boolean): string | undefined {
  const parts = v.split(", ");
  const zh = parts.map((p, i) => known(p, depth) ?? (i === 0 ? known(p.charAt(0).toLowerCase() + p.slice(1), depth) : undefined));
  if (!loose && zh.some((z) => z === undefined)) return undefined;
  return parts.map((p, i) => zh[i] ?? p).join("、"); // i18n-ignore: Chinese list separator
}

/** Translate a parameter value: known terms, names ThirtyFile made, lists of known terms, and nested messages */
function fragment(name: string, v: string, depth: number): string {
  const hit = exact!.get(v);
  if (hit !== undefined) return hit;
  if (MADE_NAME.test(v) && depth < 2) {
    const made = templated(v, depth + 1, madeTemplates);
    if (made !== undefined) return made;
  }
  if (v.includes(", ")) {
    const zh = list(v, depth + 1, LISTS.has(name));
    if (zh !== undefined) return zh;
  }
  if (NESTED.has(name) && depth < 2) return translate(v, depth + 1);
  return v;
}

/** The first template that matches, filled with translated parameters; undefined when none does */
function templated(msg: string, depth: number, among: Template[] = templates!): string | undefined {
  if (depth > 3) return undefined;
  for (const tpl of among) {
    const m = tpl.re.exec(msg);
    if (!m) continue;
    const vars: Vars = {};
    tpl.names.forEach((name, i) => {
      vars[name] = fragment(name, m[i + 1], depth);
    });
    return fill(tpl.text, vars);
  }
  return undefined;
}

/**
 * An exact message first; then a list whose items are all known, before any template (a loose one would take the rest
 * of the list as its last parameter: "Expires {date}"); then the templates; and last the names ThirtyFile makes, which
 * would otherwise take a whole message about the name ("Backup of {name}" on "Backup of X: X → Y")
 */
function translate(msg: string, depth: number): string {
  const hit = exact!.get(msg);
  if (hit !== undefined) return hit;
  return (msg.includes(", ") ? list(msg, depth + 1, false) : undefined) ?? templated(msg, depth) ?? templated(msg, depth, madeTemplates) ?? msg;
}

/**
 * A name ThirtyFile gave a backup, a one-time copy or a replica policy ("Backup of Local disk", stored in English) in
 * the interface's language; any other name as it is
 */
export function tMadeName(name: string): string {
  if (lang === "en" || !MADE_NAME.test(name)) return name;
  if (!exact) build();
  return translate(name, 0);
}

/**
 * Translate an English message returned by the server (errors, activity log details, connection failure reasons…).
 * Parameter values that are themselves known terms (e.g. "My files"), lists of such terms, and nested error
 * messages are translated too.
 */
export function tServer(msg: string | null | undefined): string {
  if (!msg) return msg ?? "";
  if (lang === "en") return msg;
  if (!exact) build();
  // A message without an exact match is tried against every pattern: remember the result (log pages repeat the
  // same details on every render)
  let out = serverCache.get(msg);
  if (out === undefined) {
    if (serverCache.size >= 2000) serverCache.clear();
    out = translate(msg, 0);
    serverCache.set(msg, out);
  }
  return out;
}

const serverCache = new Map<string, string>();
