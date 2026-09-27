/**
 * Excel number formats.
 *
 * Excel's format syntax is large; this covers the parts common in cloud files:
 * General, thousands separators, decimals, percentages, scientific notation, positive/negative/zero/text sections, colors like [Red], currency symbols, dates and times.
 * Rare syntax such as fractions (# ?/?) and conditions ([>100]) is displayed as General.
 */

const EPOCH = Date.UTC(1899, 11, 30);
const DAY = 86400000;

const COLORS: Record<string, string> = {
  black: "#000000",
  blue: "#0000FF",
  cyan: "#00FFFF",
  green: "#008000",
  magenta: "#FF00FF",
  red: "#FF0000",
  white: "#FFFFFF",
  yellow: "#FFFF00",
};

export interface Formatted {
  text: string;
  color?: string;
}

/** Split sections on ; (a ; inside quotes or after an escape does not count) */
function sections(pattern: string) {
  const out: string[] = [];
  let cur = "";
  let quoted = false;
  for (let i = 0; i < pattern.length; i++) {
    const ch = pattern[i];
    if (ch === '"') quoted = !quoted;
    if (ch === "\\" && !quoted) {
      cur += ch + (pattern[i + 1] ?? "");
      i++;
      continue;
    }
    if (ch === ";" && !quoted) {
      out.push(cur);
      cur = "";
    } else cur += ch;
  }
  out.push(cur);
  return out;
}

/** General format: up to about 10 significant digits; very large or very small values switch to scientific notation */
export function formatGeneral(n: number) {
  if (Number.isInteger(n) && Math.abs(n) < 1e11) return String(n);
  const abs = Math.abs(n);
  if (abs !== 0 && (abs >= 1e11 || abs < 1e-9)) {
    const [m, e] = n.toExponential(5).split("e");
    const exp = Number(e);
    return `${m.replace(/\.?0+$/, "")}E${exp < 0 ? "-" : "+"}${String(Math.abs(exp)).padStart(2, "0")}`;
  }
  return String(Number(n.toPrecision(10)));
}

interface Section {
  body: string;
  color?: string;
  /** Taiwan locale ([$-404]): e is the ROC (Minguo) year */
  roc?: boolean;
  /** Explicit English locale ([$-409] etc.): month and weekday names in English */
  en?: boolean;
}

/** Extract brackets like [Red] and [$NT$-404]; keep elapsed-time tokens [h], [mm], [ss] */
function stripBrackets(sec: string): Section {
  let color: string | undefined;
  let roc = false;
  let en = false;
  const body = sec.replace(/\[([^\]]*)\]/g, (m, inner: string) => {
    const lower = inner.toLowerCase();
    if (COLORS[lower]) {
      color = COLORS[lower];
      return "";
    }
    if (/^color\d+$/.test(lower)) return "";
    if (/^(h+|m+|s+)$/.test(lower)) return m;
    if (inner.startsWith("$")) {
      if (/-(0*404|zh-tw)/i.test(inner)) roc = true;
      // English-speaking locales (409 US, 809 UK, c09 Australia, 1009 Canada…, language code 09)
      if (/-(?:[0-9a-f]*09|en(-[a-z]+)?)$/i.test(inner)) en = true;
      const sym = inner.slice(1).split("-")[0];
      return sym ? `"${sym}"` : "";
    }
    // Skip conditions and other locales
    return "";
  });
  return { body, color, roc, en };
}

const isDateFormat = (body: string) => /[ydg]|h|s|am\/pm|a\/p|\[h\]|e(?![+-])/i.test(body.replace(/"[^"]*"|\\./g, ""));

// ───────────── Dates ─────────────

const WEEK = ["日", "一", "二", "三", "四", "五", "六"]; // i18n-ignore: weekday names produced by Excel number formats

const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

function formatDate(serial: number, body: string, roc = false, en = false) {
  if (serial < 0) return "#".repeat(8);
  const ms = Math.round(serial * DAY);
  const d = new Date(EPOCH + ms);
  const Y = d.getUTCFullYear();
  const M = d.getUTCMonth() + 1;
  const D = d.getUTCDate();
  let h = d.getUTCHours();
  const mi = d.getUTCMinutes();
  const s = d.getUTCSeconds();
  const ampm = /am\/pm|a\/p|上午\/下午/i.test(body); // i18n-ignore: Taiwanese AM/PM number format token
  const pm = h >= 12;
  if (ampm) h = h % 12 || 12;
  const pad = (n: number, w = 2) => String(n).padStart(w, "0");

  // Tokenize first so we can tell whether m means month or minute
  const tokens: { t: string; lit?: boolean }[] = [];
  const re = /"([^"]*)"|\\(.)|(\[h+\]|\[m+\]|\[s+\]|yyyy|yy|mmmmm|mmmm|mmm|mm|m|dddd|ddd|dd|d|hh|h|ss|s|am\/pm|a\/p|上午\/下午|g+|\.0+|e+)|(.)/gi; // i18n-ignore: Taiwanese AM/PM number format token
  for (let m: RegExpExecArray | null; (m = re.exec(body));) {
    if (m[1] !== undefined) tokens.push({ t: m[1], lit: true });
    else if (m[2] !== undefined) tokens.push({ t: m[2], lit: true });
    else if (m[3] !== undefined) tokens.push({ t: m[3] });
    else tokens.push({ t: m[4], lit: true });
  }
  const kinds = tokens.map((x) => (x.lit ? "" : x.t.toLowerCase()));
  let out = "";
  tokens.forEach((tok, i) => {
    if (tok.lit) {
      out += tok.t;
      return;
    }
    const k = kinds[i];
    const prev = kinds
      .slice(0, i)
      .reverse()
      .find((x) => x !== "");
    const next = kinds.slice(i + 1).find((x) => x !== "");
    const minute = (k === "m" || k === "mm") && (prev?.startsWith("h") || prev?.startsWith("[h") || next?.startsWith("s"));
    switch (k) {
      case "yyyy":
        out += Y;
        break;
      case "e":
      case "ee":
        // In the Taiwan locale, e is the ROC (Minguo) year
        out += roc ? Y - 1911 : Y;
        break;
      case "g":
      case "gg":
      case "ggg":
        if (roc) out += "民國"; // i18n-ignore: ROC era prefix produced by Excel number formats
        break;
      case "上午/下午": // i18n-ignore: Taiwanese AM/PM number format token
        out += pm ? "下午" : "上午"; // i18n-ignore: AM/PM markers produced by Excel number formats
        break;
      case "yy":
        out += pad(Y % 100);
        break;
      case "m":
        out += minute ? mi : M;
        break;
      case "mm":
        out += minute ? pad(mi) : pad(M);
        break;
      case "mmm":
        out += en ? MONTHS[M - 1].slice(0, 3) : `${M}月`; // i18n-ignore: month names produced by Excel number formats
        break;
      case "mmmm":
        out += en ? MONTHS[M - 1] : `${M}月`; // i18n-ignore: month names produced by Excel number formats
        break;
      case "mmmmm":
        out += en ? MONTHS[M - 1][0] : M;
        break;
      case "d":
        out += D;
        break;
      case "dd":
        out += pad(D);
        break;
      case "ddd":
        out += en ? DAYS[d.getUTCDay()].slice(0, 3) : `週${WEEK[d.getUTCDay()]}`; // i18n-ignore: weekday names produced by Excel number formats
        break;
      case "dddd":
        out += en ? DAYS[d.getUTCDay()] : `星期${WEEK[d.getUTCDay()]}`; // i18n-ignore: weekday names produced by Excel number formats
        break;
      case "h":
        out += h;
        break;
      case "hh":
        out += pad(h);
        break;
      case "s":
        out += s;
        break;
      case "ss":
        out += pad(s);
        break;
      case "am/pm":
        out += pm ? "下午" : "上午"; // i18n-ignore: AM/PM markers produced by Excel number formats
        break;
      case "a/p":
        out += pm ? "P" : "A";
        break;
      default:
        if (k.startsWith("[h")) out += Math.floor(serial * 24);
        else if (k.startsWith("[m")) out += Math.floor(serial * 1440);
        else if (k.startsWith("[s")) out += Math.floor(serial * 86400);
        else if (k.startsWith(".")) out += ((ms % 1000) / 1000).toFixed(k.length - 1).slice(1);
        else out += tok.t;
    }
  });
  return out;
}

// ───────────── Numbers ─────────────

function groupThousands(int: string) {
  return int.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

function formatNumber(n: number, body: string): string {
  // Split out literal text and the number template
  const list: ({ lit: string } | { num: string })[] = [];
  let numRun = "";
  const flush = () => {
    if (numRun) list.push({ num: numRun });
    numRun = "";
  };
  for (let i = 0; i < body.length; i++) {
    const ch = body[i];
    if (ch === '"') {
      const end = body.indexOf('"', i + 1);
      flush();
      list.push({ lit: body.slice(i + 1, end < 0 ? undefined : end) });
      i = end < 0 ? body.length : end;
    } else if (ch === "\\") {
      flush();
      list.push({ lit: body[i + 1] ?? "" });
      i++;
    } else if (ch === "_") {
      flush();
      list.push({ lit: " " });
      i++;
    } else if (ch === "*") {
      flush();
      i++;
    } else if ("0#?.,".includes(ch) || ((ch === "E" || ch === "e") && /[+-]/.test(body[i + 1] ?? ""))) {
      if (ch === "E" || ch === "e") {
        numRun += body.slice(i, i + 2);
        i++;
      } else numRun += ch;
    } else {
      flush();
      list.push({ lit: ch });
    }
  }
  flush();

  const pct = (body.replace(/"[^"]*"|\\./g, "").match(/%/g) ?? []).length;
  let value = n * Math.pow(100, pct);
  const numParts = list.filter((p): p is { num: string } => "num" in p);
  if (numParts.length === 0) return list.map((p) => ("lit" in p ? p.lit : "")).join("");
  // The first number template determines the format; remaining digit placeholders are treated as part of the same number
  const pattern = numParts.map((p) => p.num).join("");

  let text: string;
  const sci = /[eE][+-]/.exec(pattern);
  if (sci) {
    const mant = pattern.slice(0, sci.index);
    const dec = (mant.split(".")[1] ?? "").length;
    const expDigits = pattern.slice(sci.index + 2).length || 1;
    const [m, e] = value.toExponential(dec).split("e");
    const expNum = Number(e);
    text = `${m}E${expNum < 0 ? "-" : "+"}${String(Math.abs(expNum)).padStart(expDigits, "0")}`;
  } else {
    // A comma after the digits means divide by 1000
    const trailing = /,+$/.exec(pattern.replace(/\.[0#?]*$/, ""))?.[0].length ?? 0;
    const core = trailing ? pattern.replace(new RegExp(`,{${trailing}}(?=\\.|$)`), "") : pattern;
    value /= Math.pow(1000, trailing);
    const hasDot = core.includes(".");
    const [intPat, decPat = ""] = core.split(".");
    const maxDec = decPat.replace(/[^0#?]/g, "").length;
    const minDec = decPat.replace(/[^0]/g, "").length;
    const minInt = intPat.replace(/[^0]/g, "").length;
    let [i, d = ""] = Math.abs(value).toFixed(maxDec).split(".");
    while (d.length > minDec && d.endsWith("0")) d = d.slice(0, -1);
    const zero = Number(i) === 0 && Number(d || 0) === 0;
    if (i === "0" && minInt === 0) i = "";
    i = i.padStart(minInt, "0");
    if (intPat.includes(",")) i = groupThousands(i);
    text = (value < 0 && !zero ? "-" : "") + i + (hasDot ? "." + d : "");
  }

  let placed = false;
  return list
    .map((p) => {
      if ("lit" in p) return p.lit;
      if (placed) return "";
      placed = true;
      return text;
    })
    .join("");
}

/** Display a cell value according to its format */
export function formatCell(v: unknown, pattern?: string): Formatted {
  if (v === null || v === undefined) return { text: "" };
  if (typeof v === "boolean") return { text: v ? "TRUE" : "FALSE" };
  if (typeof v === "object") return { text: String(v) };
  const secs = pattern && pattern !== "General" ? sections(pattern) : null;
  if (typeof v === "string") {
    const textSec = secs?.[3] ?? (secs?.find((s) => s.includes("@")) || null);
    if (!textSec) return { text: v };
    const { body, color } = stripBrackets(textSec);
    return {
      text: body
        .replace(/"([^"]*)"/g, "$1")
        .replace(/\\(.)/g, "$1")
        .replace(/@/g, v),
      color,
    };
  }
  const n = v as number;
  if (!secs) return { text: formatGeneral(n) };
  let sec = secs[0];
  let value = n;
  if (n < 0 && secs.length >= 2 && secs[1] !== "") {
    sec = secs[1];
    value = -n;
  } else if (n === 0 && secs.length >= 3 && secs[2] !== "") sec = secs[2];
  const { body, color, roc, en } = stripBrackets(sec);
  if (/^\s*general\s*$/i.test(body) || body.trim() === "") return { text: formatGeneral(value), color };
  if (/\?\/\?|#\/#/.test(body)) return { text: formatGeneral(value), color };
  try {
    return { text: isDateFormat(body) ? formatDate(value, body, roc, en) : formatNumber(value, body), color };
  } catch {
    return { text: formatGeneral(n) };
  }
}

export function formatValue(v: unknown, pattern?: string) {
  return formatCell(v, pattern).text;
}

/** Cells with a date format are shown as yyyy/m/d while editing (Excel's behavior) */
export function isDatePattern(pattern?: string) {
  if (!pattern) return false;
  return isDateFormat(stripBrackets(sections(pattern)[0]).body);
}
