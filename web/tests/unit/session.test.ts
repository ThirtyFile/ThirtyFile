// Saved preferences: stored values of the wrong shape are ignored, and every user of a key sees a change at once
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, test } from "vitest";
import { sameShape, usePersisted } from "@/lib/session";
import { validTabs } from "@/tabs";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/** Renders two components using the same key; returns what each shows and the setter of the first */
function mountTwo<T>(key: string, initial: T, valid?: (v: T) => boolean) {
  const shown: T[] = [];
  const setters: ((v: T) => void)[] = [];
  function Probe({ n }: { n: number }) {
    const [value, set] = usePersisted(key, initial, valid);
    shown[n] = value;
    setters[n] = set;
    return null;
  }
  const root = createRoot(document.createElement("div"));
  act(() => root.render([createElement(Probe, { n: 0, key: 0 }), createElement(Probe, { n: 1, key: 1 })]));
  return { shown, set: (v: T) => act(() => setters[0](v)), unmount: () => act(() => root.unmount()) };
}

describe("saved preferences", () => {
  test("a value is only used when it has the shape of the default", () => {
    expect(sameShape({ key: "name", order: "asc" }, { key: "size", order: "desc" })).toBe(true);
    expect(sameShape({ key: "name" }, { key: "size", order: "desc" })).toBe(false);
    expect(sameShape("wide", 240)).toBe(false);
    expect(sameShape(Number.NaN, 240)).toBe(false);
    expect(sameShape(null, false)).toBe(false);
    expect(sameShape([], {})).toBe(false);
  });

  test("a stored value of another shape, or one the check refuses, gives the default", () => {
    localStorage.setItem("t-width", JSON.stringify("wide"));
    const a = mountTwo("t-width", 240);
    expect(a.shown).toEqual([240, 240]);
    a.unmount();

    localStorage.setItem("t-sort", JSON.stringify({ key: "colour", order: "asc" }));
    const b = mountTwo("t-sort", { key: "name", order: "asc" }, (s) => ["name", "size"].includes(s.key));
    expect(b.shown[0]).toEqual({ key: "name", order: "asc" });
    b.unmount();
  });

  test("every component using a key sees a change, and it is saved", () => {
    const c = mountTwo("t-pane", false);
    c.set(true);
    expect(c.shown).toEqual([true, true]);
    expect(localStorage.getItem("t-pane")).toBe("true");
    c.unmount();
  });

  test("a change made in another window arrives", () => {
    const d = mountTwo("t-nav", 200);
    act(() => {
      window.dispatchEvent(new StorageEvent("storage", { key: "t-nav", newValue: "320" }));
    });
    expect(d.shown).toEqual([320, 320]);
    // Not a number: ignored
    act(() => {
      window.dispatchEvent(new StorageEvent("storage", { key: "t-nav", newValue: '"x"' }));
    });
    expect(d.shown).toEqual([320, 320]);
    d.unmount();
  });

  test("saved tabs of an older format are refused", () => {
    expect(validTabs({ tabs: [{ id: "a", entries: ["/files"], index: 0, title: "" }], active: "a" })).not.toBeNull();
    expect(validTabs({ tabs: [{ id: "a", entries: ["/files"], index: 1, title: "" }], active: "a" })).toBeNull();
    expect(validTabs({ tabs: [], active: "a" })).toBeNull();
  });
});
