import { describe, expect, it } from "vitest";
import { findByPrefix, shortcut } from "@/lib/keys";

describe("shortcut", () => {
  it("writes Ctrl and Alt the macOS way on a Mac only", () => {
    expect(shortcut("Ctrl+Shift+N", false)).toBe("Ctrl+Shift+N");
    expect(shortcut("Ctrl+Shift+N", true)).toBe("⌘+Shift+N");
    expect(shortcut("Alt+Enter", true)).toBe("⌥+Enter");
    expect(shortcut("F2", true)).toBe("F2");
  });
});

describe("findByPrefix", () => {
  const names = ["Apple", "banana", "Avocado", "cherry", "apricot"];
  it("finds the next item starting with the typed text, ignoring case", () => {
    expect(findByPrefix(names, "a", -1)).toBe(0);
    expect(findByPrefix(names, "a", 0)).toBe(2);
    expect(findByPrefix(names, "A", 2)).toBe(4);
    expect(findByPrefix(names, "B", 0)).toBe(1);
  });
  it("wraps around, and checks the current item last", () => {
    expect(findByPrefix(names, "a", 4)).toBe(0);
    expect(findByPrefix(names, "ch", 3)).toBe(3);
    expect(findByPrefix(names, "av", 2)).toBe(2);
  });
  it("returns -1 when nothing matches", () => {
    expect(findByPrefix(names, "z", 0)).toBe(-1);
    expect(findByPrefix([], "a", -1)).toBe(-1);
  });
});
