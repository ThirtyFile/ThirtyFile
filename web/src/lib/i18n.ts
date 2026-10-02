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
  { id: "zh-CN", label: "简体中文", ready: true }, // i18n-ignore: language name shown in its own language
  { id: "ja", label: "日本語", ready: true }, // i18n-ignore: language name shown in its own language
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
    /**
     * The language the person picked (saved with their account, or chosen in this browser) or, for someone signed in,
     * the system default: injected by the server into the page (absent = the browser's languages decide). See
     * server/src/i18n/
     */
    __TF_LANG__?: string;
    /** System default language set by an administrator, for a visitor whose browser has none of ours (absent = English) */
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
 * English); a language that isn't ready otherwise gives way to the browser's next one. Undefined when none matches.
 * The server matches `Accept-Language` the same way (server/src/i18n/)
 */
export function browserMatch(tags: readonly string[], can: (l: Lang) => boolean = usable): Lang | undefined {
  for (const tag of tags) {
    const match = matchLanguage(tag);
    if (!match) continue;
    if (can(match)) return match;
    if (match === "zh-CN" && can("zh-TW")) return "zh-TW";
  }
  return undefined;
}

/** {@link browserMatch}, English when none matches */
export function browserLanguage(tags: readonly string[], can: (l: Lang) => boolean = usable): Lang {
  return browserMatch(tags, can) ?? "en";
}

/**
 * The same order as the server's (server/src/i18n/): the language the person or the system default picked, as the
 * server says (`__TF_LANG__`); then a choice kept in this browser (pages the server didn't make, in development); then
 * the browser's languages; then the system default; then English
 */
function detect(): Lang {
  const picked = typeof window !== "undefined" ? window.__TF_LANG__ : undefined;
  if (usable(picked)) return picked;
  try {
    const saved = localStorage.getItem(KEY);
    if (usable(saved)) return saved;
  } catch {
    // storage unavailable: fall back to the browser's languages / the system default
  }
  const browser = typeof navigator === "undefined" ? undefined : browserMatch(navigator.languages?.length ? navigator.languages : [navigator.language ?? ""]);
  if (browser) return browser;
  const preset = typeof window !== "undefined" ? window.__TF_DEFAULT_LANG__ : undefined;
  return usable(preset) ? preset : "en";
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
  // The server remembers the language last used, for the emails of someone who chose none. Only an explicit choice
  // (setLang) also sets tf_lang_chosen, which makes the server pick it for this browser
  document.cookie = `tf_lang=${lang}; path=/; max-age=31536000; SameSite=Lax`;
}

/** Remembers, for this tab, the language the page reloaded in after signing in, so it never reloads for it twice */
const ADOPTED_KEY = "tf-lang-adopted";

/**
 * The language to switch to after signing in: the one the server says the person (their saved language follows them to
 * every device) or the system default picked (`ui_lang` of /api/auth/me), when the page shows another one that it can
 * use. Undefined when nothing changes
 */
export function languageToAdopt(target: string | null | undefined): Lang | undefined {
  if (!usable(target)) return undefined;
  try {
    if (target === lang) {
      sessionStorage.removeItem(ADOPTED_KEY);
      return undefined;
    }
    // Already reloaded for it: the page the server made still has another language (in development, it makes none)
    return sessionStorage.getItem(ADOPTED_KEY) === target ? undefined : target;
  } catch {
    return undefined; // storage unavailable: it can't tell whether it reloaded already
  }
}

/** Reloads the page in the language {@link languageToAdopt} returned; the server makes it in that language */
export function adoptLanguage(next: Lang) {
  try {
    sessionStorage.setItem(ADOPTED_KEY, next);
  } catch {
    return;
  }
  window.location.reload();
}

/**
 * Switch to another language, chosen by the person: kept in this browser, and the page reloads in it. The account menu
 * also saves it with the account first (`api.setLanguage`), so it follows them to other devices
 */
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

/** The order in which a text lists its plural forms, of the categories its language uses (CLDR's order) */
const CATEGORIES: Intl.LDMLPluralRule[] = ["zero", "one", "two", "few", "many", "other"];

/** A language's plural rules, and the categories it uses in the order a text lists their forms */
function pluralRules(tag: string) {
  const rules = new Intl.PluralRules(tag);
  const used = rules.resolvedOptions().pluralCategories;
  return { rules, order: CATEGORIES.filter((c) => used.includes(c)) };
}

/** English (the source text): `single|plural` */
const SOURCE_PLURALS = pluralRules("en");
/** The active language's: Chinese and Japanese have one form, so their texts have no `|` */
const PLURALS = lang === "en" ? SOURCE_PLURALS : pluralRules(locale);

/**
 * Plurals: a text lists its forms separated by `|`, one per plural category of its language (`Intl.PluralRules`),
 * picked by n (or count). The last form (`other`) when there is no number or no form for the category
 */
function plural(text: string, vars?: Vars, { rules, order } = SOURCE_PLURALS) {
  if (!text.includes("|")) return text;
  const forms = text.split("|");
  const raw = vars?.n ?? vars?.count;
  // A count the server wrote may have digit grouping ("1,234")
  const n = typeof raw === "string" ? Number(raw.replace(/,/g, "")) : Number(raw ?? NaN);
  if (!Number.isFinite(n)) return forms[forms.length - 1];
  return forms[order.indexOf(rules.select(n))] ?? forms[forms.length - 1];
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
  return fill(plural(value, vars, PLURALS), vars);
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
 * What separates the items of a list in the active language, between items other than the last two (Chinese and
 * Japanese: the enumeration comma U+3001)
 */
const LIST_SEPARATOR = (() => {
  const parts = new Intl.ListFormat(locale, { type: "conjunction" }).formatToParts(["a", "b", "c"]);
  return parts.find((p) => p.type === "literal")?.value ?? ", ";
})();

/**
 * A list of items joined with ", " (share link options, the changes made to something): each item translated, joined
 * with the active language's separator. The first item may start with a capital (the server writes the list as a
 * sentence). Undefined unless every item is known, or `loose` (a parameter that always holds a list)
 */
function list(v: string, depth: number, loose: boolean): string | undefined {
  const parts = v.split(", ");
  const done = parts.map((p, i) => known(p, depth) ?? (i === 0 ? known(p.charAt(0).toLowerCase() + p.slice(1), depth) : undefined));
  if (!loose && done.some((z) => z === undefined)) return undefined;
  return parts.map((p, i) => done[i] ?? p).join(LIST_SEPARATOR);
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
    const joined = list(v, depth + 1, LISTS.has(name));
    if (joined !== undefined) return joined;
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
    return fill(plural(tpl.text, vars, PLURALS), vars);
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
