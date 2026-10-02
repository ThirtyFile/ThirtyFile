// Exported logs (components/logs/exportCsv.tsx): cells spreadsheets won't run, local times, and a name for every action
// and sign-in event the server records, the same one the page shows
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, test } from "vitest";
import { ACTION_GROUPS, actionLabel } from "@/components/logs/actions";
import { LOGIN_EVENTS } from "@/components/logs/LoginLog";
import { csvField, csvFileName, csvTime, toCsv } from "@/components/logs/exportCsv";

/** The tests run in web/ */
const server = join(process.cwd(), "..", "server", "src");

function rustFiles(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) rustFiles(p, out);
    else if (name.endsWith(".rs")) out.push(p);
  }
  return out;
}

describe("CSV cells", () => {
  test("text a spreadsheet would run as a formula is kept as text, and separators are quoted", () => {
    expect(csvField('=HYPERLINK("x")')).toBe(`"'=HYPERLINK(""x"")"`);
    expect(csvField("a,b")).toBe('"a,b"');
    expect(csvField("plain text")).toBe("plain text");
    // A tab or carriage return before a formula doesn't hide it from spreadsheets
    expect(csvField("\t=1+1")).toBe("'\t=1+1");
    expect(csvField("\r=1+1")).toBe(`"'\r=1+1"`);
    expect(csvField("-5")).toBe("'-5");
  });

  test("a file starts with a byte order mark and the headers, then one line per row", () => {
    const text = toCsv(
      [
        { header: "Name", value: (r: { name: string; n: number | null }) => r.name },
        { header: "Count", value: (r) => r.n },
      ],
      [
        { name: "a, b", n: 1 },
        { name: "c", n: null },
      ],
    );
    expect(text).toBe('﻿Name,Count\n"a, b",1\nc,\n');
  });

  test("times are local, as spreadsheets read them", () => {
    const d = new Date(2026, 8, 25, 21, 10, 30);
    expect(csvTime(d.getTime() / 1000)).toBe("2026-09-25 21:10:30");
    expect(csvFileName("activity-log", d)).toBe("activity-log-20260925.csv");
  });
});

describe("every recorded action and sign-in event has a name", () => {
  test("each action in the server's list (logs/actions.rs) has a label and a place in the filter", () => {
    const source = readFileSync(join(server, "logs/actions.rs"), "utf8");
    const list = /pub const ACTIONS: &\[&str\] = &\[([\s\S]*?)\];/.exec(source)?.[1] ?? "";
    const actions = [...list.matchAll(/"([a-z_]+)"/g)].map((m) => m[1]);
    expect(actions.length).toBeGreaterThan(50);
    const filtered = new Set(ACTION_GROUPS.flatMap((g) => g.actions));
    expect(actions.filter((a) => actionLabel(a) === a)).toEqual([]);
    expect(actions.filter((a) => !filtered.has(a))).toEqual([]);
    // And nothing in the filter that the server doesn't record
    expect([...filtered].filter((a) => !actions.includes(a))).toEqual([]);
  });

  test("each sign-in event the server records has a label", () => {
    const events = new Set<string>();
    for (const file of rustFiles(server)) {
      for (const m of readFileSync(file, "utf8").matchAll(/record_login(?:_via)?\([^;{]*?"([a-z0-9_]+)"/g)) events.add(m[1]);
    }
    // Recorded through a variable; and the method record_login passes on, which is no event
    for (const e of ["bad_password", "unknown_user", "disabled"]) events.add(e);
    events.delete("password");
    expect(events.size).toBeGreaterThan(20);
    expect([...events].filter((e) => !LOGIN_EVENTS[e]?.label)).toEqual([]);
  });
});
