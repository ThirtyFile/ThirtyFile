// Search as you type: text an input method is still composing (Zhuyin, Pinyin, Japanese, Korean) is never searched
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { liveSearch } from "@/lib/liveSearch";

describe("live search", () => {
  let runs: string[];
  beforeEach(() => {
    vi.useFakeTimers();
    runs = [];
  });
  afterEach(() => vi.useRealTimers());
  const debounced = () =>
    liveSearch(
      (q) => runs.push(q),
      () => 350,
    );

  test("typing searches the last text after a pause", () => {
    const s = debounced();
    s.input("a", false);
    s.input("ab", false);
    vi.advanceTimersByTime(349);
    expect(runs).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(runs).toEqual(["ab"]);
  });

  test("a pause while composing searches nothing; the committed text is searched once", () => {
    const s = debounced();
    s.compositionStart();
    s.input("ㄓ", true);
    s.input("ㄓㄨ", true);
    vi.advanceTimersByTime(1000);
    expect(runs).toEqual([]);
    s.compositionEnd("中");
    // Some browsers report the committed text once more as a plain change
    s.input("中", false);
    vi.advanceTimersByTime(350);
    expect(runs).toEqual(["中"]);
  });

  test("changes reported without the composing flag are still held back during composition", () => {
    const s = debounced();
    s.compositionStart();
    s.input("zh", false);
    vi.advanceTimersByTime(1000);
    expect(runs).toEqual([]);
    expect(s.composing()).toBe(true);
    s.compositionEnd("中");
    expect(s.composing()).toBe(false);
    vi.advanceTimersByTime(350);
    expect(runs).toEqual(["中"]);
  });

  test("starting to compose cancels a search about to run", () => {
    const s = debounced();
    s.input("report ", false);
    vi.advanceTimersByTime(200);
    s.compositionStart();
    vi.advanceTimersByTime(1000);
    expect(runs).toEqual([]);
    s.compositionEnd("report 中");
    vi.advanceTimersByTime(350);
    expect(runs).toEqual(["report 中"]);
  });

  test("cancelled composition searches the text left in the box, not the dropped candidate", () => {
    const s = debounced();
    s.compositionStart();
    s.input("abcㄓ", true);
    s.compositionEnd("abc");
    vi.advanceTimersByTime(350);
    expect(runs).toEqual(["abc"]);
  });

  test("a page filtering itself does so at once, but not while composing", () => {
    const s = liveSearch(
      (q) => runs.push(q),
      () => 0,
    );
    s.input("a", false);
    expect(runs).toEqual(["a"]);
    s.compositionStart();
    s.input("aㄓ", true);
    expect(runs).toEqual(["a"]);
    s.compositionEnd("a中");
    s.input("a中", false);
    expect(runs).toEqual(["a", "a中"]);
    s.input("a", false);
    expect(runs).toEqual(["a", "a中", "a"]);
  });

  test("leaving the page drops a waiting search", () => {
    const s = debounced();
    s.input("a", false);
    s.cancel();
    vi.advanceTimersByTime(1000);
    expect(runs).toEqual([]);
  });
});
