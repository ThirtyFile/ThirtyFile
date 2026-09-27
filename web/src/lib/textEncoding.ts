/**
 * Reading and writing text files in the encoding they came in, so editing doesn't change what other programs see:
 * a UTF-8 file saved by Excel keeps its byte order mark, UTF-16 stays UTF-16. Big5 (common for older files in
 * Taiwan) is converted to UTF-8 on save, as the editor says. Anything else is opened read-only, because decoding it
 * with a guessed encoding and saving would damage it for good.
 */

export type TextEncodingName = "UTF-8" | "UTF-8 BOM" | "UTF-16 LE" | "UTF-16 BE" | "Big5";

export interface DecodedText {
  text: string;
  /** null when the encoding couldn't be recognised: the text is shown for reading only */
  encoding: TextEncodingName | null;
}

const starts = (b: Uint8Array, ...prefix: number[]) => prefix.every((v, i) => b[i] === v);

function tryDecode(label: string, bytes: Uint8Array): string | null {
  try {
    // ignoreBOM: the byte order mark was already recognised and skipped by the caller
    return new TextDecoder(label, { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    return null;
  }
}

export function decodeText(buf: ArrayBuffer): DecodedText {
  const b = new Uint8Array(buf);
  if (starts(b, 0xef, 0xbb, 0xbf)) {
    const text = tryDecode("utf-8", b.subarray(3));
    if (text !== null) return { text, encoding: "UTF-8 BOM" };
  } else if (starts(b, 0xff, 0xfe)) {
    const text = tryDecode("utf-16le", b.subarray(2));
    if (text !== null) return { text, encoding: "UTF-16 LE" };
  } else if (starts(b, 0xfe, 0xff)) {
    const text = tryDecode("utf-16be", b.subarray(2));
    if (text !== null) return { text, encoding: "UTF-16 BE" };
  } else {
    const utf8 = tryDecode("utf-8", b);
    // NUL characters: UTF-16 without a byte order mark, or not a text file
    if (utf8 !== null && !utf8.includes("\0")) return { text: utf8, encoding: "UTF-8" };
    const big5 = utf8 === null ? tryDecode("big5", b) : null;
    if (big5 !== null && !big5.includes("\0")) return { text: big5, encoding: "Big5" };
  }
  return { text: new TextDecoder("utf-8").decode(b), encoding: null };
}

function utf16(text: string, littleEndian: boolean): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(2 + text.length * 2);
  const view = new DataView(out.buffer);
  view.setUint16(0, 0xfeff, littleEndian);
  for (let i = 0; i < text.length; i++) view.setUint16(2 + i * 2, text.charCodeAt(i), littleEndian);
  return out;
}

/** The bytes to save, and the encoding the file has afterwards */
export function encodeText(text: string, encoding: TextEncodingName): { body: Uint8Array<ArrayBuffer>; encoding: TextEncodingName } {
  switch (encoding) {
    case "UTF-8 BOM": {
      const utf8 = new TextEncoder().encode(text);
      const out = new Uint8Array(3 + utf8.length);
      out.set([0xef, 0xbb, 0xbf]);
      out.set(utf8, 3);
      return { body: out, encoding };
    }
    case "UTF-16 LE":
      return { body: utf16(text, true), encoding };
    case "UTF-16 BE":
      return { body: utf16(text, false), encoding };
    default:
      return { body: new TextEncoder().encode(text), encoding: "UTF-8" };
  }
}

/** The line ending a text uses (Windows files keep CRLF when saved: the editor itself always works with "\n") */
export function lineEnding(text: string): "\r\n" | "\n" {
  return text.includes("\r\n") ? "\r\n" : "\n";
}

/** The text with every line ending as "\n", as the editor holds it */
export function normalizeLines(text: string): string {
  return text.replace(/\r\n?/g, "\n");
}
