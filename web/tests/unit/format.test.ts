import { describe, expect, test } from "vitest";
import { formatCell, formatGeneral, formatValue, isDatePattern } from "@/ooxml/core/numfmt";
import { dateToSerial } from "@/ooxml/xlsx/formula";
import { estimateWidthEm, formatNumber } from "@/ooxml/docx/numfmt";

describe("Excel number formats", () => {
  test.each([
    [1234.5, undefined, "1234.5"],
    [0.1 + 0.2, "General", "0.3"],
    [1234.567, "#,##0.00", "1,234.57"],
    [-1234.567, "#,##0", "-1,235"],
    [0.256, "0.0%", "25.6%"],
    [12345, "0.00E+00", "1.23E+04"],
    [5, "000", "005"],
    [-5, "0;(0)", "(5)"],
    [0, '0;-0;"zero"', "zero"],
    [1.5, '"NT$"#,##0.00', "NT$1.50"],
    [dateToSerial(2024, 3, 5), "yyyy/m/d", "2024/3/5"],
    [dateToSerial(2024, 3, 5), "yyyy-mm-dd", "2024-03-05"],
    [dateToSerial(2024, 3, 5), "[$-409]d-mmm-yy", "5-Mar-24"],
    [dateToSerial(2024, 3, 5) + 0.5625, "yyyy/m/d h:mm", "2024/3/5 13:30"],
    [0.5625, "[$-409]h:mm AM/PM", "1:30 PM"],
    [1.25, "[h]:mm", "30:00"],
    [dateToSerial(2024, 3, 5), "[$-404]e/m/d", "113/3/5"],
  ])("%s with %s", (v, fmt, want) => expect(formatValue(v, fmt)).toBe(want));

  test("colors, text sections and other values", () => {
    expect(formatCell(-3, "0;[Red]-0")).toEqual({ text: "-3", color: "#FF0000" });
    expect(formatCell("abc", '0;0;0;"<"@">"').text).toBe("<abc>");
    expect(formatCell("abc", "0.00").text).toBe("abc");
    expect(formatCell(true).text).toBe("TRUE");
    expect(formatCell(null).text).toBe("");
  });

  test("General switches to scientific notation for very large or small numbers", () => {
    expect(formatGeneral(123456789012)).toBe("1.23457E+11");
    expect(formatGeneral(0.0000000001234)).toBe("1.234E-10");
    expect(formatGeneral(1 / 3)).toBe("0.3333333333");
  });

  test("isDatePattern", () => {
    expect(isDatePattern("yyyy/m/d")).toBe(true);
    expect(isDatePattern("h:mm")).toBe(true);
    expect(isDatePattern("#,##0")).toBe(false);
    expect(isDatePattern('"d"0')).toBe(false);
    expect(isDatePattern(undefined)).toBe(false);
  });
});

describe("Word list numbering", () => {
  test.each([
    [3, "decimal", "3"],
    [7, "decimalZero", "07"],
    [14, "upperRoman", "XIV"],
    [1999, "lowerRoman", "mcmxcix"],
    [28, "upperLetter", "BB"],
    [3, "lowerLetter", "c"],
    [22, "ordinal", "22nd"],
    [13, "ordinal", "13th"],
    [42, "cardinalText", "Forty-two"],
    [21, "ordinalText", "Twenty-first"],
    [255, "hex", "FF"],
    [6, "chicago", "\u2020\u2020"],
    [4, "numberInDash", "- 4 -"],
  ])("%i as %s", (n, fmt, want) => expect(formatNumber(n, fmt)).toBe(want));

  test("Chinese counting", () => {
    // 12 and 105 in Chinese numerals: shi-er, yi-bai-ling-wu
    expect(formatNumber(12, "taiwaneseCounting")).toBe("\u5341\u4e8c");
    expect(formatNumber(105, "taiwaneseCounting")).toBe("\u4e00\u767e\u96f6\u4e94");
  });

  test("unknown formats fall back to digits, width estimates count wide characters double", () => {
    expect(formatNumber(5, "somethingNew")).toBe("5");
    expect(formatNumber(5, null)).toBe("5");
    expect(estimateWidthEm("\u4e00\u4e8c")).toBeGreaterThan(estimateWidthEm("12"));
  });
});
