import { describe, expect, test } from "vitest";
import { Calculator, checkFormula, dateToSerial, evaluateAt, parseNumber, serialToDate, shiftFormula, toScalar } from "@/lib/sheet/formula";
import { key, parseCellName } from "@/lib/sheet/model";
import { sheet, workbook } from "./sheet";

const DATA = {
  A1: 10,
  A2: 20,
  A3: 30,
  A4: "40",
  A5: true,
  B1: "apple",
  B2: "Banana",
  B3: "cherry",
  B4: null,
  C1: 1.5,
  C2: -2.25,
  D1: "  Hello   World ",
};

/** The result of a formula typed into an empty cell (Z100) of a sheet holding DATA, as saved to the file */
function calc(formula: string, extra: Record<string, string | number | boolean | null> = {}) {
  const book = workbook(sheet("Sheet1", { ...DATA, ...extra, Z100: formula }), sheet("Other data", { A1: 5, B2: "x" }));
  return toScalar(new Calculator(book).value(0, 99, 25));
}

describe("arithmetic and references", () => {
  test.each([
    ["=1+2*3", 7],
    ["=(1+2)*3", 9],
    ["=2^3^2", 64],
    ["=-2^2", 4],
    ["=10%", 0.1],
    ["=7/2", 3.5],
    ["=1/0", "#DIV/0!"],
    ['="a"&"b"&1', "ab1"],
    ["=A1+A4", 50],
    ["=A1+A5", 11],
    ["=A1+B1", "#VALUE!"],
    ["=A1>A2", false],
    ['="abc"="ABC"', true],
    ["=A1<>A2", true],
    ["='Other data'!A1*2", 10],
    ["=Missing!A1", "#REF!"],
    ["=B4", null],
    ["=B4+1", 1],
  ])("%s", (f, want) => expect(calc(f)).toBe(want));
});

describe("functions", () => {
  test.each([
    ["=SUM(A1:A3)", 60],
    // Text and booleans in a range are skipped by SUM
    ["=SUM(A1:A5)", 60],
    ["=SUM(A:A)", 60],
    ["=AVERAGE(A1:A3)", 20],
    ["=AVERAGE(B1:B3)", "#DIV/0!"],
    ["=MIN(A1:A3,5)", 5],
    ["=MAX(A1:A3)", 30],
    ["=MEDIAN(1,3,2,4)", 2.5],
    ["=COUNT(A1:B4)", 3],
    ["=COUNTA(A1:B4)", 7],
    ["=COUNTBLANK(B1:B4)", 1],
    ["=ROUND(2.345,2)", 2.35],
    ["=ROUND(-2.5,0)", -3],
    ["=ROUNDUP(1.21,1)", 1.3],
    ["=ROUNDDOWN(-1.29,1)", -1.2],
    ["=INT(-1.5)", -2],
    ["=MOD(-3,2)", 1],
    ["=SQRT(-1)", "#NUM!"],
    ["=POWER(2,10)", 1024],
    ["=SUMPRODUCT(A1:A3,A1:A3)", 1400],
    ['=SUMIF(A1:A3,">15")', 50],
    ['=COUNTIF(B1:B3,"b*")', 1],
    ['=SUMIFS(A1:A3,B1:B3,"<>apple")', 50],
    ['=AVERAGEIF(A1:A3,">=20")', 25],
    ['=IF(A1>5,"big","small")', "big"],
    ['=IFS(A1>50,"a",A1>5,"b")', "b"],
    ["=IFERROR(1/0,-1)", -1],
    ['=IFNA(MATCH("zzz",B1:B3,0),"none")', "none"],
    ["=AND(TRUE,A1>5)", true],
    ["=OR(FALSE,A1>50)", false],
    ["=NOT(A5)", false],
    ['=CONCAT(B1,"-",A1)', "apple-10"],
    ['=TEXTJOIN(",",TRUE,B1:B4)', "apple,Banana,cherry"],
    ['=LEFT("abcdef",2)&RIGHT("abcdef",2)&MID("abcdef",3,2)', "abefcd"],
    ["=LEN(D1)", 16],
    ["=TRIM(D1)", "Hello World"],
    ['=UPPER("ab")&LOWER("CD")&PROPER("hello world")', "ABcdHello World"],
    ['=SUBSTITUTE("a-b-c","-","+",2)', "a-b+c"],
    ['=FIND("b","abcb")', 2],
    ['=SEARCH("B","abcb")', 2],
    ['=FIND("z","abc")', "#VALUE!"],
    ['=EXACT("a","A")', false],
    ['=TEXT(1234.5,"#,##0.00")', "1,234.50"],
    ['=VALUE("1,234")', 1234],
    ["=DATE(2024,2,29)", 45351],
    ["=YEAR(45351)&\"/\"&MONTH(45351)&\"/\"&DAY(45351)", "2024/2/29"],
    ["=WEEKDAY(45351)", 5],
    ["=EDATE(DATE(2024,1,31),1)", 45351],
    ["=EOMONTH(DATE(2023,2,10),0)", 44985],
    ['=VLOOKUP(20,A1:B3,2,FALSE)', "Banana"],
    ['=HLOOKUP("Banana",B2:B3,2,FALSE)', "cherry"],
    ['=MATCH("cherry",B1:B3,0)', 3],
    ["=INDEX(A1:B3,2,2)", "Banana"],
    ['=XLOOKUP("cherry",B1:B3,A1:A3)', 30],
    ['=XLOOKUP("none",B1:B3,A1:A3,"-")', "-"],
    ['=CHOOSE(2,"a","b","c")', "b"],
    ["=ROW(A7)+COLUMN(C1)", 10],
    ["=ROWS(A1:B3)*COLUMNS(A1:B3)", 6],
    ["=ISBLANK(B4)", true],
    ["=ISNUMBER(A4)", false],
    ["=NOSUCHFUNCTION(1)", "#NAME?"],
  ])("%s", (f, want) => expect(calc(f)).toBe(want));
});

describe("calculator", () => {
  test("formulas follow other formulas, across sheets", () => {
    const book = workbook(sheet("S1", { A1: 2, A2: "=A1*3", A3: "=S2!A1+A2" }), sheet("S2", { A1: "=S1!A1+1" }));
    const c = new Calculator(book);
    expect(c.value(0, 2, 0)).toBe(9);
    // After an edit, invalidate() recomputes
    book.sheets[0].cells.set(key(0, 0), { v: 10 });
    expect(c.value(0, 2, 0)).toBe(9);
    c.invalidate();
    expect(c.value(0, 2, 0)).toBe(41);
  });

  test("circular references are reported, not looped over", () => {
    const book = workbook(sheet("S", { A1: "=B1+1", B1: "=A1+1", C1: "=C1" }));
    const c = new Calculator(book);
    expect(toScalar(c.value(0, 0, 0))).toBe("#CYCLE!");
    expect(toScalar(c.value(0, 0, 2))).toBe("#CYCLE!");
  });

  test("a long chain of formulas doesn't overflow the stack", () => {
    const data: Record<string, number | string> = { A1: 1 };
    for (let r = 2; r <= 5000; r++) data[`A${r}`] = `=A${r - 1}+1`;
    const c = new Calculator(workbook(sheet("S", data)));
    expect(c.value(0, 4999, 0)).toBe(5000);
  });

  test("unsupported functions keep the value Excel stored", () => {
    const s = sheet("S", {});
    s.cells.set(key(0, 0), { v: 42, f: "=FORECAST.ETS(1,2,3)" });
    expect(new Calculator(workbook(s)).value(0, 0, 0)).toBe(42);
  });

  test("evaluateAt shifts relative references to the cell being checked", () => {
    const book = workbook(sheet("S", { A1: 1, A2: 5, B1: 3 }));
    const c = new Calculator(book);
    const get = (si: number, r: number, col: number) => c.value(si, r, col);
    expect(evaluateAt(book, "=A1>2", 0, [0, 0], [1, 0], get)).toBe(true);
    expect(evaluateAt(book, "=$A$1>2", 0, [0, 0], [1, 0], get)).toBe(false);
  });
});

describe("helpers", () => {
  test("checkFormula", () => {
    expect(checkFormula("=SUM(A1:A2)")).toBeNull();
    expect(checkFormula("=SUM(A1")).not.toBeNull();
    expect(checkFormula('="unterminated')).toBe("#VALUE!");
  });

  test("shiftFormula moves relative references only", () => {
    expect(shiftFormula("=A1+$B$2+C$3+$D4", 1, 1)).toBe("=B2+$B$2+D$3+$D5");
    // Text, sheet names and function names are left alone
    expect(shiftFormula('="A1"&\'Q1 A1\'!A1&LOG10(A1)', 1, 0)).toBe('="A1"&\'Q1 A1\'!A2&LOG10(A2)');
    expect(shiftFormula("=A1", -1, 0)).toBe("=#REF!");
  });

  test("parseNumber", () => {
    expect(parseNumber("1,234.5")).toBe(1234.5);
    expect(parseNumber("12%")).toBe(0.12);
    expect(parseNumber("-1e3")).toBe(-1000);
    expect(parseNumber("12abc")).toBeNull();
    expect(parseNumber("  ")).toBeNull();
  });

  test("dates use Excel's 1900 serial numbers", () => {
    expect(dateToSerial(1900, 3, 1)).toBe(61);
    expect(dateToSerial(2024, 1, 1)).toBe(45292);
    expect(serialToDate(45292).toISOString()).toBe("2024-01-01T00:00:00.000Z");
    expect(serialToDate(45292.5).toISOString()).toBe("2024-01-01T12:00:00.000Z");
  });

  test("parseCellName", () => {
    expect(parseCellName("$B$3")).toEqual([2, 1]);
    expect(parseCellName("XFD1048576")).toEqual([1048575, 16383]);
    expect(parseCellName("XFE1")).toBeNull();
    expect(parseCellName("A0")).toBeNull();
  });
});
