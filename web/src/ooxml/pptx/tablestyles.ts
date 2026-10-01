/**
 * PowerPoint built-in table styles: used when the file only records the style GUID without writing tableStyles.xml.
 * Generates a:tblStyle elements by style family (light/medium/dark…) and accent color, then hands them to the regular style application flow.
 * Appearance is an approximation based on the PowerPoint style gallery.
 */

import { parseXml } from "../core/package";

type Family = "T1" | "T2" | "L1" | "L2" | "L3" | "M1" | "M2" | "M3" | "M4" | "D1" | "D2";

/** GUID → [family, accent number (0 for neutral)] */
const GUIDS: Record<string, [Family, number]> = {};
const reg = (f: Family, ids: string[]) => ids.forEach((id, i) => id && (GUIDS[`{${id}}`] = [f, i]));
reg("T1", [
  "2D5ABB26-0587-4C30-8999-92F81FD0307C",
  "3C2FFA5D-87B4-456A-9821-1D502468CF0F",
  "284E427A-3D55-4303-BF80-6455036E1DE7",
  "69C7853C-536D-4A76-A0AE-DD22124D55A5",
  "775DCB02-9BB8-47FD-8907-85C794F793BA",
  "35758FB7-9AC5-4552-8A53-C91805E547FA",
  "08FB837D-C827-4EFA-A057-4D05807E0F7C",
]);
reg("T2", [
  "5940675A-B579-460E-94D1-54222C63F5DA",
  "D113A9D2-9D6B-4929-AA2D-F23B5EE8CBE7",
  "18603FDC-E32A-4AB5-989C-0864C3EAD2B8",
  "306799F8-075E-4A3A-A7F6-7FBC6576F1A4",
  "E269D01E-BC32-4049-B463-5C60D7B0CCD2",
  "327F97BB-C833-4FB7-BDE5-3F7075034690",
  "638B1855-1B75-4FBE-930C-398BA8C253C6",
]);
reg("L1", [
  "9D7B26C5-4107-4FEC-AEDC-1716B250A1EF",
  "3B4B98B0-60AC-42C2-AFA5-B58CD77FA1E5",
  "0E3FDE45-AF77-4B5C-9715-49D594BDF05E",
  "C083E6E3-FA7D-4D7B-A595-EF9225AFEA82",
  "D27102A9-8310-4765-A935-A1911B00CA55",
  "5FD0F851-EC5A-4D38-B0AD-8093EC10F338",
  "68D230F3-CF80-4859-8CE7-A43EE81993B5",
]);
reg("L2", [
  "7E9639D4-E3E2-4D34-9284-5A2195B3D0D7",
  "69012ECD-51FC-41F1-AA8D-1B2483CD663E",
  "72833802-FEF1-4C79-8D5D-14CF1EAF98D9",
  "F2DE63D5-997A-4646-A377-4702673A728D",
  "17292A2E-F333-43FB-9621-5CBBE7FDCDCB",
  "5A111915-BE36-4E01-A7E5-04B1672EAD32",
  "912C8C85-51F0-491E-9774-3900AFEF0FD7",
]);
reg("L3", [
  "616DA210-FB5B-4158-B5E0-FEB733F419BA",
  "BC89EF96-8CEA-46FF-86C4-4CE0E7609802",
  "5DA37D80-6434-44D0-A028-1B22A696006F",
  "8799B23B-EC83-4686-B30A-512413B5E67A",
  "ED083AE6-46FA-4A59-8FB0-9F97EB10719F",
  "BDBED569-4797-4DF1-A0F4-6AAB3CD982D8",
  "E8B1032C-EA38-4F05-BA0D-38AFFFC7BED3",
]);
reg("M1", [
  "793D81CF-94F2-401A-BA57-92F5A7B2D0C5",
  "B301B821-A1FF-4177-AEE7-76D212191A09",
  "9DCAF9ED-07DC-4A11-8D7F-57B35C25682E",
  "1FECB4D8-DB02-4DC6-A0A2-4F2EBAE1DC90",
  "1E171933-4619-4E11-9A3F-F7608DF75F80",
  "FABFCF23-3B69-468F-B69F-88F6DE6A72F2",
  "10A1B5D5-9B99-4C35-A422-299274C87663",
]);
reg("M2", [
  "073A0DAA-6AF3-43AB-8588-CEC1D06C72B9",
  "5C22544A-7EE6-4342-B048-85BDC9FD1C3A",
  "21E4AEA4-8DFA-4A89-87EB-49C32662AFE0",
  "F5AB1C69-6EDB-4FF4-983F-18BD219EF322",
  "00A15C55-8517-42AA-B614-E9B94910E393",
  "7DF18680-E054-41AD-8BC1-D1AEF772440D",
  "93296810-A885-4BE3-A3E7-6D5BEEA58F35",
]);
reg("M3", [
  "8EC20E35-A176-4012-BC5E-935CFFF8708E",
  "6E25E649-3F16-4E02-A733-19D2CDBF48F0",
  "85BE263C-DBD7-4A20-BB59-AAB30ACAA65A",
  "EB344D84-9AFB-497E-A393-DC336BA19D2E",
  "EB9631B5-78F2-41C9-869B-9F39066F8104",
  "74C1A8A3-306A-4EB7-A6B1-4F7E0EB9C5D6",
  "2A488322-F2BA-4B5B-9748-0D474271808F",
]);
reg("M4", [
  "D7AC3CCA-C797-4891-BE02-D94E43425B78",
  "69CF1AB2-1976-4502-BF36-3FF5EA218861",
  "8A107856-5554-42FB-B03E-39F5DBC370BA",
  "0505E3EF-67EA-436B-97B2-0124C06EBD24",
  "C4B1156A-380E-4F78-BDF5-A606A8083BF9",
  "22838BEF-8BB2-4498-84A7-C5851F593DF1",
  "16D9F66E-5EB9-4882-86FB-DCBF35E3C3E4",
]);
reg("D1", [
  "E8034E78-7F5D-4C2E-B375-FC64B27BC917",
  "125E5076-3810-47DD-B79F-674D7AD40C01",
  "37CE84F3-28C3-443E-9E96-99CF82512B78",
  "D03447BB-5D67-496B-8E87-E561075AD55C",
  "E929F9F4-4A8F-4326-A1B4-22849713DDAB",
  "8FD4443E-F989-4FC4-A0C8-D5A2AF1F390B",
  "AF606853-7671-496A-8E4F-DF71F8EC918B",
]);
reg("D2", ["5202B0CA-FC54-4496-8BCA-5EF66A818D29", "0660B408-B3CF-4A94-85FC-2B1E0A45F4A2", "", "91EBBBCC-DAD2-459C-BE2E-F6DE35CF9A28", "", "46F890A9-2807-4EBB-B81D-B2AA78EC7F39"]);

// ───────────── Style XML generation ─────────────

const clr = (name: string, mods = "") => `<a:schemeClr val="${name}">${mods}</a:schemeClr>`;
const tint = (v: number) => `<a:tint val="${v * 1000}"/>`;
const shade = (v: number) => `<a:shade val="${v * 1000}"/>`;
const alpha = (v: number) => `<a:alpha val="${v * 1000}"/>`;
const solid = (c: string) => `<a:solidFill>${c}</a:solidFill>`;
const ln = (pt: number, c: string, cmpd = "sng") => `<a:ln w="${Math.round(pt * 12700)}" cmpd="${cmpd}">${solid(c)}</a:ln>`;

interface Part {
  bold?: boolean;
  text?: string;
  fill?: string;
  borders?: Partial<Record<"left" | "right" | "top" | "bottom" | "insideH" | "insideV", string>>;
}

function part(name: string, p: Part | undefined): string {
  if (!p) return "";
  const tx = p.bold || p.text ? `<a:tcTxStyle${p.bold ? ' b="on"' : ""}>${p.text ?? ""}</a:tcTxStyle>` : "";
  const bdr = p.borders
    ? Object.entries(p.borders)
        .map(([k, v]) => `<a:${k}>${v}</a:${k}>`)
        .join("")
    : "";
  const st = `<a:tcStyle><a:tcBdr>${bdr}</a:tcBdr>${p.fill ? `<a:fill>${solid(p.fill)}</a:fill>` : ""}</a:tcStyle>`;
  return `<a:${name}>${tx}${st}</a:${name}>`;
}

const ALL = (v: string) => ({ left: v, right: v, top: v, bottom: v, insideH: v, insideV: v });
const OUTER = (v: string) => ({ left: v, right: v, top: v, bottom: v });

function familyParts(f: Family, A: string, neutral: boolean): Record<string, Part | undefined> {
  const tx1 = clr("tx1");
  const dk1 = clr("dk1");
  const lt1 = clr("lt1");
  const a = clr(A);
  switch (f) {
    case "T1":
      if (neutral) return { wholeTbl: { text: tx1 }, firstRow: { bold: true }, lastRow: { bold: true }, firstCol: { bold: true }, lastCol: { bold: true } };
      return {
        wholeTbl: { text: dk1, borders: ALL(ln(1, a)) },
        band1H: { fill: clr(A, alpha(40)) },
        band1V: { fill: clr(A, alpha(40)) },
        firstRow: { bold: true, text: lt1, fill: a },
        lastRow: { bold: true, borders: { top: ln(2, a) } },
        firstCol: { bold: true },
        lastCol: { bold: true },
      };
    case "T2":
      if (neutral) return { wholeTbl: { text: tx1, borders: ALL(ln(1, tx1)) }, firstRow: { bold: true }, lastRow: { bold: true }, firstCol: { bold: true }, lastCol: { bold: true } };
      return {
        wholeTbl: { text: lt1, fill: a, borders: { ...OUTER(ln(1, clr(A, tint(50)))), insideH: ln(1, lt1), insideV: ln(1, lt1) } },
        band1H: { fill: clr("lt1", alpha(20)) },
        band1V: { fill: clr("lt1", alpha(20)) },
        firstRow: { bold: true, borders: { bottom: ln(2, lt1) } },
        lastRow: { bold: true, borders: { top: ln(2, lt1) } },
        firstCol: { bold: true },
        lastCol: { bold: true },
      };
    case "L1":
      return {
        wholeTbl: { text: tx1, borders: { top: ln(1, a), bottom: ln(1, a) } },
        band1H: { fill: clr(A, alpha(20)) },
        band1V: { fill: clr(A, alpha(20)) },
        firstRow: { bold: true, borders: { bottom: ln(1, a) } },
        lastRow: { bold: true, borders: { top: ln(1, a) } },
        firstCol: { bold: true },
        lastCol: { bold: true },
      };
    case "L2":
      return {
        wholeTbl: { text: tx1, borders: OUTER(ln(1, a)) },
        band1H: { borders: { top: ln(1, a), bottom: ln(1, a) } },
        band1V: { borders: { left: ln(1, a), right: ln(1, a) } },
        band2V: { borders: { left: ln(1, a), right: ln(1, a) } },
        firstRow: { bold: true, text: clr("bg1"), fill: a },
        lastRow: { bold: true, borders: { top: ln(3, a, "dbl") } },
        firstCol: { bold: true },
        lastCol: { bold: true },
      };
    case "L3":
      return {
        wholeTbl: { text: tx1, borders: ALL(ln(1, a)) },
        band1H: { fill: clr(A, alpha(20)) },
        band1V: { fill: clr(A, alpha(20)) },
        firstRow: { bold: true, text: a, borders: { bottom: ln(2, a) } },
        lastRow: { bold: true, borders: { top: ln(3, a, "dbl") } },
        firstCol: { bold: true },
        lastCol: { bold: true },
      };
    case "M1":
      return {
        wholeTbl: { text: dk1, fill: lt1, borders: { ...OUTER(ln(1, a)), insideH: ln(1, a) } },
        band1H: { fill: clr(A, tint(20)) },
        band1V: { fill: clr(A, tint(20)) },
        firstRow: { bold: true, text: lt1, fill: a },
        lastRow: { bold: true, fill: lt1, borders: { top: ln(3, a, "dbl") } },
        firstCol: { bold: true },
        lastCol: { bold: true },
      };
    case "M3":
      return {
        wholeTbl: { text: dk1, fill: lt1, borders: { top: ln(2, dk1), bottom: ln(2, dk1) } },
        band1H: { fill: clr("dk1", tint(20)) },
        band1V: { fill: clr("dk1", tint(20)) },
        firstRow: { bold: true, text: lt1, fill: a, borders: { bottom: ln(2, dk1) } },
        lastRow: { bold: true, fill: lt1, borders: { top: ln(3, dk1, "dbl") } },
        firstCol: { bold: true, text: lt1, fill: a },
        lastCol: { bold: true, text: lt1, fill: a },
      };
    case "M4":
      return {
        wholeTbl: { text: dk1, fill: clr(A, tint(20)), borders: ALL(ln(1, a)) },
        band1H: { fill: clr(A, tint(40)) },
        band1V: { fill: clr(A, tint(40)) },
        firstRow: { bold: true, text: a, fill: clr(A, tint(20)) },
        lastRow: { bold: true, fill: clr(A, tint(20)), borders: { top: ln(2, a) } },
        firstCol: { bold: true },
        lastCol: { bold: true },
      };
    case "D1": {
      const base = neutral ? clr("dk1", tint(75)) : clr(A, shade(20));
      const band = neutral ? clr("dk1", tint(60)) : clr(A, shade(40));
      const edge = neutral ? clr("dk1", tint(90)) : clr(A, shade(60));
      return {
        wholeTbl: { text: lt1, fill: base },
        band1H: { fill: band },
        band1V: { fill: band },
        firstRow: { bold: true, fill: dk1, borders: { bottom: ln(2, lt1) } },
        lastRow: { bold: true, fill: base, borders: { top: ln(2, lt1) } },
        firstCol: { bold: true, fill: edge, borders: { right: ln(2, lt1) } },
        lastCol: { bold: true, fill: edge, borders: { left: ln(2, lt1) } },
      };
    }
    case "D2":
      return {
        wholeTbl: { text: dk1, fill: clr(A, tint(20)) },
        band1H: { fill: clr(A, tint(40)) },
        band1V: { fill: clr(A, tint(40)) },
        firstRow: { bold: true, text: lt1, fill: neutral ? dk1 : a },
        lastRow: { bold: true, fill: clr(A, tint(20)), borders: { top: ln(3, dk1, "dbl") } },
        firstCol: { bold: true },
        lastCol: { bold: true },
      };
    default: {
      // Medium Style 2 (PowerPoint default)
      const white = ln(1, lt1);
      return {
        wholeTbl: { text: dk1, fill: clr(A, tint(20)), borders: ALL(white) },
        band1H: { fill: clr(A, tint(40)) },
        band1V: { fill: clr(A, tint(40)) },
        firstRow: { bold: true, text: lt1, fill: a, borders: { bottom: ln(3, lt1) } },
        lastRow: { bold: true, text: lt1, fill: a, borders: { top: ln(3, lt1) } },
        firstCol: { bold: true, text: lt1, fill: a },
        lastCol: { bold: true, text: lt1, fill: a },
      };
    }
  }
}

const cache = new Map<string, Element | null>();

/** Built-in style GUID (uppercase) → a:tblStyle; unrecognized GUIDs fall back to "Medium Style 2 – Accent 1" */
export function builtinTableStyle(guid: string): Element | null {
  const [family, accent] = GUIDS[guid] ?? ["M2", 1];
  const key = `${family}${accent}`;
  let el = cache.get(key);
  if (el !== undefined) return el;
  const neutral = accent === 0;
  const A = neutral ? "dk1" : `accent${accent}`;
  const parts = familyParts(family, A, neutral);
  const order = ["wholeTbl", "band1H", "band2H", "band1V", "band2V", "lastCol", "firstCol", "lastRow", "firstRow"];
  const xml = `<a:tblStyle xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">${order.map((n) => part(n, parts[n])).join("")}</a:tblStyle>`;
  el = parseXml(xml)?.documentElement ?? null;
  cache.set(key, el);
  return el;
}
