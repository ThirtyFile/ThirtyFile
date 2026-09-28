import { beforeEach, describe, expect, it } from "vitest";
import { COLUMN_WIDTH, columnPrefs, columnShown, columnsToHide, dateGroup, groupItems, pageRows, resetColumns, setColumnWidth, showColumn } from "@/lib/listView";

/** Unix seconds of a local time */
const at = (y: number, m: number, d: number, h = 12) => new Date(y, m - 1, d, h).getTime() / 1000;

describe("dateGroup", () => {
  // A Wednesday; the week started on Monday the 28th
  const now = new Date(2026, 8, 30, 10);
  it("names the groups File Explorer does", () => {
    expect(dateGroup(at(2026, 9, 30, 8), now)).toBe("today");
    expect(dateGroup(at(2026, 10, 2), now)).toBe("today");
    expect(dateGroup(at(2026, 9, 29, 23), now)).toBe("yesterday");
    expect(dateGroup(at(2026, 9, 28), now)).toBe("week");
    expect(dateGroup(at(2026, 9, 27), now)).toBe("last-week");
    expect(dateGroup(at(2026, 9, 21, 0), now)).toBe("last-week");
    expect(dateGroup(at(2026, 9, 20), now)).toBe("month");
    expect(dateGroup(at(2026, 9, 1), now)).toBe("month");
    expect(dateGroup(at(2026, 8, 31), now)).toBe("last-month");
    expect(dateGroup(at(2026, 7, 1), now)).toBe("year");
    expect(dateGroup(at(2025, 12, 31), now)).toBe("older");
  });
  it("counts from Monday and across the new year", () => {
    const monday = new Date(2026, 8, 28, 9);
    expect(dateGroup(at(2026, 9, 27), monday)).toBe("yesterday");
    expect(dateGroup(at(2026, 9, 26), monday)).toBe("last-week");
    expect(dateGroup(at(2026, 9, 21), monday)).toBe("last-week");
    expect(dateGroup(at(2026, 9, 20), monday)).toBe("month");
    const january = new Date(2027, 0, 20);
    expect(dateGroup(at(2026, 12, 15), january)).toBe("last-month");
    expect(dateGroup(at(2026, 11, 15), january)).toBe("older");
  });
});

describe("groupItems", () => {
  const now = new Date(2026, 8, 30, 10);
  const items = [
    { name: "b.pdf", kind: "file", t: at(2026, 9, 30) },
    { name: "Docs", kind: "folder", t: at(2026, 1, 5) },
    { name: "a.txt", kind: "file", t: at(2026, 9, 29) },
    { name: "c.pdf", kind: "file", t: at(2020, 1, 1) },
    { name: "d.txt", kind: "file", t: at(2026, 9, 30) },
  ];
  const o = { dateOf: (x: (typeof items)[number]) => x.t, typeOf: (x: (typeof items)[number]) => (x.kind === "folder" ? "File folder" : x.name.slice(-3).toUpperCase()), now };
  const names = (g: ReturnType<typeof groupItems<(typeof items)[number]>>) => g!.map((x) => [x.label, x.items.map((i) => i.name)]);

  it("leaves the list alone when not grouping", () => {
    expect(groupItems(items, "none", o)).toBeNull();
  });
  it("groups by date, newest group first, keeping the list's order in each", () => {
    expect(names(groupItems(items, "date", o))).toEqual([
      ["Today", ["b.pdf", "d.txt"]],
      ["Yesterday", ["a.txt"]],
      ["Earlier this year", ["Docs"]],
      ["A long time ago", ["c.pdf"]],
    ]);
    expect(groupItems(items, "date", { ...o, reversed: true })!.map((g) => g.key)).toEqual(["older", "year", "yesterday", "today"]);
  });
  it("groups by type, folders first", () => {
    expect(names(groupItems(items, "type", o))).toEqual([
      ["File folder", ["Docs"]],
      ["PDF", ["b.pdf", "c.pdf"]],
      ["TXT", ["a.txt", "d.txt"]],
    ]);
    expect(groupItems(items, "type", { ...o, reversed: true })!.map((g) => g.label)).toEqual(["TXT", "PDF", "File folder"]);
  });
});

describe("column settings", () => {
  beforeEach(() => resetColumns());
  it("are kept, and limited to sensible widths", () => {

    expect(columnShown(columnPrefs(), "type")).toBe(true);
    expect(columnShown(columnPrefs(), "created")).toBe(false);
    showColumn("created", true);
    showColumn("type", false);
    setColumnWidth("size", 20);
    setColumnWidth("name", 5000);
    setColumnWidth("date", 201.6);
    const p = columnPrefs();
    expect(columnShown(p, "created") && !columnShown(p, "type")).toBe(true);
    expect(p.widths).toEqual({ size: 50, name: 1000, date: 202 });
    expect(JSON.parse(localStorage.getItem("tf-columns")!)).toEqual(p);
    setColumnWidth("date", undefined);
    expect(columnPrefs().widths).toEqual({ size: 50, name: 1000 });
    resetColumns();
    expect(columnPrefs()).toEqual({ visible: {}, widths: {} });
  });
});

describe("columnsToHide", () => {
  const width = (id: keyof typeof COLUMN_WIDTH) => COLUMN_WIDTH[id];
  const ids = ["date", "type", "size"] as const;
  // The name takes 160 and the columns 170 + 120 + 100
  it("hides nothing when every column fits", () => {
    expect(columnsToHide([...ids], width, 160, 550)).toEqual([]);
  });
  it("hides Type first, then Size", () => {
    expect(columnsToHide([...ids], width, 160, 500)).toEqual(["type"]);
    expect(columnsToHide([...ids], width, 160, 400)).toEqual(["type", "size"]);
  });
  it("leaves the rest to scrolling sideways", () => {
    expect(columnsToHide([...ids], width, 160, 100)).toEqual(["type", "size"]);
    expect(columnsToHide(["date", "size"], width, 160, 300)).toEqual(["size"]);
  });
});

describe("pageRows", () => {
  it("moves by the rows in view, less one, and at least one", () => {
    expect(pageRows(280, 28)).toBe(9);
    expect(pageRows(290, 28)).toBe(9);
    expect(pageRows(30, 28)).toBe(1);
    expect(pageRows(0, 28)).toBe(1);
  });
});
