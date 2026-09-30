import { describe, expect, test } from "vitest";
import { decodeText, encodeText, lineEnding, looksBinary, normalizeLines, type TextEncodingName } from "@/lib/textEncoding";

const TEXT = "Line one\nCaf\u00e9 \u4e2d\u6587 \u{1F600}\n";
const bytes = (...b: number[]) => new Uint8Array(b).buffer;

describe("text files keep their encoding", () => {
  test.each<TextEncodingName>(["UTF-8", "UTF-8 BOM", "UTF-16 LE", "UTF-16 BE"])("%s round trip", (encoding) => {
    const { body, encoding: saved } = encodeText(TEXT, encoding);
    expect(saved).toBe(encoding);
    expect(decodeText(body.buffer)).toEqual({ text: TEXT, encoding });
  });

  test("byte order marks", () => {
    expect(Array.from(encodeText("a", "UTF-8 BOM").body)).toEqual([0xef, 0xbb, 0xbf, 0x61]);
    expect(Array.from(encodeText("a", "UTF-16 LE").body)).toEqual([0xff, 0xfe, 0x61, 0x00]);
    expect(Array.from(encodeText("a", "UTF-16 BE").body)).toEqual([0xfe, 0xff, 0x00, 0x61]);
  });

  test("Big5 is read, and saved as UTF-8", () => {
    // "zhong wen" in Big5
    const big5 = bytes(0xa4, 0xa4, 0xa4, 0xe5);
    expect(decodeText(big5)).toEqual({ text: "\u4e2d\u6587", encoding: "Big5" });
    const { body, encoding } = encodeText("\u4e2d\u6587", "Big5");
    expect(encoding).toBe("UTF-8");
    expect(new TextDecoder().decode(body)).toBe("\u4e2d\u6587");
  });

  test("anything else is only shown, not saved", () => {
    // UTF-16 without a byte order mark
    expect(decodeText(bytes(0x61, 0x00, 0x62, 0x00)).encoding).toBeNull();
    // Not UTF-8, and Big5 with a NUL character: a binary file
    expect(decodeText(bytes(0xa4, 0xa4, 0x00)).encoding).toBeNull();
  });

  test("line endings", () => {
    expect(lineEnding("a\r\nb")).toBe("\r\n");
    expect(lineEnding("a\nb")).toBe("\n");
    expect(normalizeLines("a\r\nb\rc\n")).toBe("a\nb\nc\n");
  });
});

describe("files opened as text by choice", () => {
  test("text in any supported encoding, with tabs, form feeds and terminal colours, is text", () => {
    expect(looksBinary("FROM node:22\nRUN pnpm install\n")).toBe(false);
    expect(looksBinary("col\tcol\r\n\fpage\x1b[31mred\x1b[0m")).toBe(false);
    expect(looksBinary(decodeText(encodeText(TEXT, "UTF-16 LE").body.buffer).text)).toBe(false);
    expect(looksBinary(decodeText(bytes(0xa4, 0xa4, 0xa4, 0xe5)).text)).toBe(false);
  });

  test("control characters and undecodable bytes are a binary file's, so it stays read-only", () => {
    // A PNG header: UTF-8 can't decode it, so the replacement character shows up
    expect(looksBinary(decodeText(bytes(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)).text)).toBe(true);
    // Valid UTF-8 with control characters (an ELF header)
    expect(looksBinary("\x7fELF\x02\x01\x01")).toBe(true);
    expect(looksBinary("a\x00b")).toBe(true);
  });
});
