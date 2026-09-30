// lib/store.ts: a value kept outside React, and the listeners told when it changes
import { describe, expect, test, vi } from "vitest";
import { createStore } from "@/lib/store";

describe("createStore", () => {
  test("set replaces the value and tells every listener; emit tells them without a new value", () => {
    const store = createStore(1);
    const a = vi.fn<() => void>();
    const b = vi.fn<() => void>(() => expect(store.get()).toBe(2));
    store.subscribe(a);
    const stopB = store.subscribe(b);
    store.set(2);
    expect(store.get()).toBe(2);
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1);

    stopB();
    store.emit();
    expect(a).toHaveBeenCalledTimes(2);
    expect(b).toHaveBeenCalledTimes(1);
  });

  test("a listener subscribed twice is told once", () => {
    const store = createStore("x");
    const l = vi.fn<() => void>();
    store.subscribe(l);
    store.subscribe(l);
    store.set("y");
    expect(l).toHaveBeenCalledTimes(1);
  });
});
