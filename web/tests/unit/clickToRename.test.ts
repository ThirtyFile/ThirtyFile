// Click the name of the selected item to rename it (lib/clickToRename.ts): which presses may rename, and the timing
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { RENAME_DELAY, clickToRename, mayRename, type NamePress } from "@/lib/clickToRename";

/** A press that renames: the main button, once, on the name of the item selected on its own, in a list that had the focus */
const PRESS: NamePress = { id: "a", onName: true, selectedAlone: true, hadFocus: true, canRename: true, button: 0, detail: 1, modified: false, pointerType: "mouse" };

beforeEach(() => void vi.useFakeTimers());
afterEach(() => void vi.useRealTimers());

describe("click to rename", () => {
  test("only a single plain click with the mouse or a pen on the name of the item selected alone, in the list that had the focus", () => {
    expect(mayRename(PRESS)).toBe(true);
    expect(mayRename({ ...PRESS, pointerType: "pen" })).toBe(true);
    expect(mayRename({ ...PRESS, onName: false })).toBe(false);
    expect(mayRename({ ...PRESS, selectedAlone: false })).toBe(false);
    expect(mayRename({ ...PRESS, hadFocus: false })).toBe(false);
    expect(mayRename({ ...PRESS, canRename: false })).toBe(false);
    expect(mayRename({ ...PRESS, button: 2 })).toBe(false);
    expect(mayRename({ ...PRESS, detail: 2 })).toBe(false);
    expect(mayRename({ ...PRESS, modified: true })).toBe(false);
    expect(mayRename({ ...PRESS, pointerType: "touch" })).toBe(false);
  });

  test("renaming starts once the double-click time has gone by after the click", () => {
    const start = vi.fn<(id: string) => void>();
    const timing = clickToRename(start);
    timing.press(PRESS);
    // Not yet while the button is held
    vi.advanceTimersByTime(RENAME_DELAY * 2);
    expect(start).not.toHaveBeenCalled();
    timing.click("a");
    expect(timing.waiting).toBe(true);
    vi.advanceTimersByTime(RENAME_DELAY - 1);
    expect(start).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(start).toHaveBeenCalledExactlyOnceWith("a");
    expect(timing.waiting).toBe(false);
    // A click without a press that may rename does nothing
    timing.click("a");
    vi.advanceTimersByTime(RENAME_DELAY * 2);
    expect(start).toHaveBeenCalledTimes(1);
  });

  test("a second press in time (a double-click) cancels it", () => {
    const start = vi.fn<(id: string) => void>();
    const timing = clickToRename(start);
    timing.press(PRESS);
    timing.click("a");
    vi.advanceTimersByTime(RENAME_DELAY / 2);
    timing.press({ ...PRESS, detail: 2 });
    timing.click("a");
    vi.advanceTimersByTime(RENAME_DELAY * 2);
    expect(start).not.toHaveBeenCalled();
  });

  test("a press anywhere else, a click on another item and anything that cancels stop it", () => {
    const start = vi.fn<(id: string) => void>();
    const timing = clickToRename(start);
    // Pressed elsewhere during the wait
    timing.press(PRESS);
    timing.click("a");
    timing.press(null);
    // Pressed on the item, but let go over another
    timing.press(PRESS);
    timing.click("b");
    // A key, a right click, dragging
    timing.press(PRESS);
    timing.click("a");
    timing.cancel();
    vi.advanceTimersByTime(RENAME_DELAY * 2);
    expect(start).not.toHaveBeenCalled();
    expect(timing.waiting).toBe(false);
  });
});
