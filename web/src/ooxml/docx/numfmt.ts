/**
 * Number formats (w:numFmt): turn a counter value into list number, page number, and footnote number text.
 * Covers Traditional/Simplified Chinese, Japanese and Korean formats; unknown formats fall back to Arabic numerals.
 */

// oxfmt-ignore
const ROMAN: [number, string][] = [
  [1000, "M"], [900, "CM"], [500, "D"], [400, "CD"], [100, "C"], [90, "XC"],
  [50, "L"], [40, "XL"], [10, "X"], [9, "IX"], [5, "V"], [4, "IV"], [1, "I"],
];

function roman(n: number) {
  if (n <= 0 || n >= 4000) return String(n);
  let out = "";
  for (const [v, s] of ROMAN)
    while (n >= v) {
      out += s;
      n -= v;
    }
  return out;
}

/** Word's letter numbering: A…Z, AA…ZZ, AAA… (repeats the same letter, unlike spreadsheet column carry-over) */
function letters(n: number, base: string[]) {
  if (n <= 0 || n > 780) return String(n);
  const ch = base[(n - 1) % base.length];
  return ch.repeat(Math.floor((n - 1) / base.length) + 1);
}

/** Pick the n-th glyph in sequence (Arabic numerals when out of range) */
function pick(n: number, list: string[] | string) {
  const arr = typeof list === "string" ? Array.from(list) : list;
  return n >= 1 && n <= arr.length ? arr[n - 1] : String(n);
}

const range = (from: number, count: number) => Array.from({ length: count }, (_, i) => String.fromCodePoint(from + i));

// Circled numbers: ①…⑳, ㉑…㉟, ㊱…㊿
const CIRCLED = [...range(0x2460, 20), ...range(0x3251, 15), ...range(0x32b1, 15)];
const PAREN = range(0x2474, 20); // ⑴…⒇
const FULLSTOP = range(0x2488, 20); // ⒈…⒛
const IDEO_CIRCLE = range(0x3220, 10); // ㈠…㈩ (actually parenthesized Chinese numerals)

const TIANGAN = "甲乙丙丁戊己庚辛壬癸"; // i18n-ignore: heavenly-stem numbering glyphs
const DIZHI = "子丑寅卯辰巳午未申酉戌亥"; // i18n-ignore: earthly-branch numbering glyphs

interface CjkDigits {
  digits: string; // 0–9
  units: string; // characters for tens, hundreds and thousands
  big: string[]; // characters for 10^4 and 10^8
  zero: string; // zero used for internal gaps
  /** Whether the teens are written with a leading "one" before the tens character; omitted only in the leading position */
  tenOne: boolean;
}

const ZH_TRAD: CjkDigits = { digits: "〇一二三四五六七八九", units: "十百千", big: ["萬", "億"], zero: "零", tenOne: false }; // i18n-ignore: Chinese numeral glyphs
const ZH_SIMP: CjkDigits = { digits: "〇一二三四五六七八九", units: "十百千", big: ["万", "亿"], zero: "〇", tenOne: false }; // i18n-ignore: Chinese numeral glyphs
const LEGAL_TRAD: CjkDigits = { digits: "零壹貳參肆伍陸柒捌玖", units: "拾佰仟", big: ["萬", "億"], zero: "零", tenOne: true }; // i18n-ignore: Chinese financial numeral glyphs
const LEGAL_SIMP: CjkDigits = { digits: "零壹贰叁肆伍陆柒捌玖", units: "拾佰仟", big: ["万", "亿"], zero: "零", tenOne: true }; // i18n-ignore: Chinese financial numeral glyphs
const JA: CjkDigits = { digits: "〇一二三四五六七八九", units: "十百千", big: ["万", "億"], zero: "", tenOne: false }; // i18n-ignore: Japanese numeral glyphs
const JA_LEGAL: CjkDigits = { digits: "〇壱弐参四伍六七八九", units: "拾百阡", big: ["萬", "億"], zero: "", tenOne: true }; // i18n-ignore: Japanese legal (daiji) numeral glyphs
const KO: CjkDigits = { digits: "영일이삼사오육칠팔구", units: "십백천", big: ["만", "억"], zero: "", tenOne: false };

/** Chinese numeral form for up to four digits (thousands, hundreds, tens, ones) */
function cjkGroup(n: number, d: CjkDigits, leading: boolean) {
  const parts = [Math.floor(n / 1000), Math.floor(n / 100) % 10, Math.floor(n / 10) % 10, n % 10];
  let out = "";
  let pendingZero = false;
  for (let i = 0; i < 4; i++) {
    const v = parts[i];
    const unit = i < 3 ? d.units[2 - i] : "";
    if (v === 0) {
      if (out) pendingZero = true;
      continue;
    }
    if (pendingZero && d.zero) out += d.zero;
    pendingZero = false;
    // When the leading digit is in the tens place, write "ten" rather than "one ten" (12 is written as ten-two)
    const omitOne = i === 2 && v === 1 && !out && leading && !d.tenOne;
    out += (omitOne ? "" : d.digits[v]) + unit;
  }
  return out;
}

function cjkNumber(n: number, d: CjkDigits) {
  if (n === 0) return d.digits[0];
  if (n < 0 || n >= 1e12) return String(n);
  const groups: number[] = [];
  for (let v = n; v > 0; v = Math.floor(v / 10000)) groups.push(v % 10000);
  let out = "";
  let needZero = false;
  for (let i = groups.length - 1; i >= 0; i--) {
    const g = groups[i];
    if (g === 0) {
      needZero = !!out;
      continue;
    }
    if (out && (needZero || g < 1000) && d.zero) out += d.zero;
    needZero = false;
    out += cjkGroup(g, d, !out) + (i > 0 ? (d.big[i - 1] ?? "") : "");
  }
  return out;
}

/** Digit-by-digit numerals (120 is written one-two-zero): ideographDigital, taiwaneseDigital */
const digitByDigit = (n: number, digits: string) => String(n).replace(/\d/g, (c) => digits[Number(c)]);

const ONES = [
  "",
  "one",
  "two",
  "three",
  "four",
  "five",
  "six",
  "seven",
  "eight",
  "nine",
  "ten",
  "eleven",
  "twelve",
  "thirteen",
  "fourteen",
  "fifteen",
  "sixteen",
  "seventeen",
  "eighteen",
  "nineteen",
];
const TENS = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];

function englishWords(n: number): string {
  if (n === 0) return "zero";
  if (n < 20) return ONES[n];
  if (n < 100) return TENS[Math.floor(n / 10)] + (n % 10 ? "-" + ONES[n % 10] : "");
  if (n < 1000) return ONES[Math.floor(n / 100)] + " hundred" + (n % 100 ? " " + englishWords(n % 100) : "");
  if (n < 1e6) return englishWords(Math.floor(n / 1000)) + " thousand" + (n % 1000 ? " " + englishWords(n % 1000) : "");
  return String(n);
}

const ORDINAL_WORDS: Record<string, string> = { one: "first", two: "second", three: "third", five: "fifth", eight: "eighth", nine: "ninth", twelve: "twelfth" };

function englishOrdinalWords(n: number) {
  const w = englishWords(n);
  return w.replace(/([a-z]+)$/, (last) => ORDINAL_WORDS[last] ?? (last.endsWith("y") ? last.slice(0, -1) + "ieth" : last + "th"));
}

function ordinalSuffix(n: number) {
  const m100 = n % 100;
  if (m100 >= 11 && m100 <= 13) return `${n}th`;
  return `${n}${["th", "st", "nd", "rd"][n % 10] ?? "th"}`;
}

const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

const AIUEO = "ｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜｦﾝ"; // i18n-ignore: half-width katakana numbering glyphs
const AIUEO_FULL = "アイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワヲン";
const IROHA = "ｲﾛﾊﾆﾎﾍﾄﾁﾘﾇﾙｦﾜｶﾖﾀﾚｿﾂﾈﾅﾗﾑｳヰﾉｵｸﾔﾏｹﾌｺｴﾃｱｻｷﾕﾒﾐｼヱﾋﾓｾｽ"; // i18n-ignore: half-width katakana numbering glyphs
const IROHA_FULL = "イロハニホヘトチリヌルヲワカヨタレソツネナラムウヰノオクヤマケフコエテアサキユメミシヱヒモセス";
const GANADA = "\uac00나다라마바사아자차카타파하";
const CHOSUNG = "ㄱㄴㄷㄹㅁㅂㅅㅇㅈㅊㅋㅌㅍㅎ";
const RU_LOWER = Array.from("абвгдежзиклмнопрстуфхцчшщэюя");
const HEBREW = Array.from("אבגדהוזחטיכלמנסעפצקרשת");
const ARABIC = Array.from("أبتثجحخدذرزسشصضطظعغفقكلمنهوي");
const LOWER = Array.from("abcdefghijklmnopqrstuvwxyz");
const UPPER = LOWER.map((c) => c.toUpperCase());
const CHICAGO = ["*", "†", "‡", "§"];
const FULLWIDTH_DIGITS = "０１２３４５６７８９"; // i18n-ignore: full-width digit glyphs

/**
 * Counter value → text.
 * @param format The w:numFmt val; for the custom format, w:format is passed separately (e.g. "001, 002, 003, ...")
 */
export function formatNumber(n: number, format: string | null | undefined, custom?: string | null): string {
  switch (format ?? "decimal") {
    case "decimal":
    case "decimalHalfWidth":
      return String(n);
    case "decimalZero":
      return n >= 0 && n < 10 ? `0${n}` : String(n);
    case "upperRoman":
      return roman(n);
    case "lowerRoman":
      return roman(n).toLowerCase();
    case "upperLetter":
      return letters(n, UPPER);
    case "lowerLetter":
      return letters(n, LOWER);
    case "ordinal":
      return ordinalSuffix(n);
    case "cardinalText":
      return cap(englishWords(n));
    case "ordinalText":
      return cap(englishOrdinalWords(n));
    case "hex":
      return n.toString(16).toUpperCase();
    case "chicago":
      return n >= 1 ? CHICAGO[(n - 1) % 4].repeat(Math.floor((n - 1) / 4) + 1) : String(n);
    case "numberInDash":
      return `- ${n} -`;
    case "decimalFullWidth":
    case "decimalFullWidth2":
      return String(n).replace(/\d/g, (c) => FULLWIDTH_DIGITS[Number(c)]);
    case "decimalEnclosedCircle":
    case "decimalEnclosedCircleChinese":
      return pick(n, CIRCLED);
    case "decimalEnclosedParen":
      return pick(n, PAREN);
    case "decimalEnclosedFullstop":
      return pick(n, FULLSTOP);
    case "ideographEnclosedCircle":
      return pick(n, IDEO_CIRCLE);
    case "ideographParenthesized":
      return pick(n, IDEO_CIRCLE);
    case "ideographTraditional":
      return pick(n, TIANGAN);
    case "ideographZodiac":
      return pick(n, DIZHI);
    case "ideographZodiacTraditional":
      // Sexagenary cycle: combinations of the ten heavenly stems and twelve earthly branches
      return n >= 1 && n <= 60 ? TIANGAN[(n - 1) % 10] + DIZHI[(n - 1) % 12] : String(n);
    case "ideographDigital":
    case "taiwaneseDigital":
    case "japaneseDigitalTenThousand":
      return digitByDigit(n, ZH_TRAD.digits);
    case "koreanDigital":
    case "koreanDigital2":
      return digitByDigit(n, KO.digits);
    case "taiwaneseCounting":
    case "taiwaneseCountingThousand":
      return cjkNumber(n, ZH_TRAD);
    case "chineseCounting":
    case "chineseCountingThousand":
      return cjkNumber(n, ZH_SIMP);
    case "ideographLegalTraditional":
      return cjkNumber(n, LEGAL_TRAD);
    case "chineseLegalSimplified":
      return cjkNumber(n, LEGAL_SIMP);
    case "japaneseCounting":
      return cjkNumber(n, JA);
    case "japaneseLegal":
      return cjkNumber(n, JA_LEGAL);
    case "koreanCounting":
    case "koreanLegal":
      return cjkNumber(n, KO);
    case "aiueo":
      return pick(n, AIUEO);
    case "aiueoFullWidth":
      return pick(n, AIUEO_FULL);
    case "iroha":
      return pick(n, IROHA);
    case "irohaFullWidth":
      return pick(n, IROHA_FULL);
    case "ganada":
      return pick(n, GANADA);
    case "chosung":
      return pick(n, CHOSUNG);
    case "russianLower":
      return letters(n, RU_LOWER);
    case "russianUpper":
      return letters(
        n,
        RU_LOWER.map((c) => c.toUpperCase()),
      );
    case "hebrew2":
      return letters(n, HEBREW);
    case "arabicAlpha":
      return letters(n, ARABIC);
    case "none":
      return "";
    case "custom": {
      // E.g. "001, 002, 003, ...": zero-pad to the digit count of the first value
      const m = custom ? /^(\d+)/.exec(custom.trim()) : null;
      return m ? String(n).padStart(m[1].length, "0") : String(n);
    }
    default:
      return String(n);
  }
}

/** Estimate the width of number text (in font-size units), used to place the tab after a list number */
export function estimateWidthEm(text: string) {
  let w = 0;
  for (const ch of text) {
    const c = ch.codePointAt(0) ?? 0;
    if (c >= 0x2e80 || (c >= 0x2460 && c <= 0x24ff) || (c >= 0xff00 && c <= 0xff60)) w += 1;
    else if (/[ilIj.,:;'|!()[\]\s]/.test(ch)) w += 0.3;
    else if (/[mwMW]/.test(ch)) w += 0.85;
    else if (/[A-Z0-9]/.test(ch)) w += 0.62;
    else w += 0.52;
  }
  return w;
}
