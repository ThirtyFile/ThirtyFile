// The Mac style's keyboard map (components/style/mac/keys.ts), and the shortcuts dialog listing it
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, test } from "vitest";
import { StyleKitContext } from "@/components/style";
import { macKit } from "@/components/style/mac";
import { MAC_KEYS } from "@/components/style/mac/keys";
import { openShortcuts, ShortcutsHost } from "@/components/ShortcutsDialog";
import { keysOf, pressed, type Action, type KeyPress } from "@/lib/style/keymap";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/** A key press on a Mac: ⌘ is `meta` */
const press = (key: string, mods: Partial<KeyPress> = {}): KeyPress => ({ key, code: "", ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...mods });
const cmd = (key: string, mods: Partial<KeyPress> = {}) => press(key, { metaKey: true, ...mods });

/** The actions a key press does, of those the explorer and the frame take from anywhere on the page */
const COMMANDS: Action[] = [
  "upFolder",
  "back",
  "forward",
  "addressBar",
  "search",
  "refresh",
  "selectAll",
  "clearSelection",
  "open",
  "menu",
  "rename",
  "cut",
  "copy",
  "paste",
  "moveHere",
  "undo",
  "trash",
  "deleteForever",
  "newFolder",
  "details",
  "quickLook",
  "shortcuts",
];
const doing = (e: KeyPress) => COMMANDS.filter((a) => pressed(e, MAC_KEYS[a]));

describe("the Mac style's keys", () => {
  test("are Finder's: Enter renames, ⌘↓ opens, ⌘↑ goes up, ⌘⌫ moves to the trash, ⌘I shows details, Space is Quick look", () => {
    expect(doing(press("Enter"))).toEqual(["rename"]);
    expect(doing(cmd("ArrowDown"))).toEqual(["open"]);
    expect(doing(cmd("ArrowUp"))).toEqual(["upFolder"]);
    expect(doing(cmd("Backspace"))).toEqual(["trash"]);
    expect(doing(cmd("Backspace", { altKey: true }))).toEqual(["deleteForever"]);
    expect(doing(cmd("i"))).toEqual(["details"]);
    expect(doing(press(" "))).toEqual(["quickLook"]);
    expect(doing(cmd("["))).toEqual(["back"]);
    expect(doing(cmd("]"))).toEqual(["forward"]);
    expect(doing(cmd("G", { shiftKey: true }))).toEqual(["addressBar"]);
    expect(doing(cmd("f"))).toEqual(["search"]);
    expect(doing(cmd("a"))).toEqual(["selectAll"]);
    expect(doing(cmd("c"))).toEqual(["copy"]);
    expect(doing(cmd("v"))).toEqual(["paste"]);
    // ⌥ types another letter on a Mac: the key itself counts
    expect(doing(cmd("√", { altKey: true, code: "KeyV" }))).toEqual(["moveHere"]);
    expect(doing(cmd("z"))).toEqual(["undo"]);
    expect(doing(cmd("˜", { altKey: true, code: "KeyN" }))).toEqual(["newFolder"]);
    expect(doing(press("Escape"))).toEqual(["clearSelection"]);
    expect(doing(press("?", { shiftKey: true }))).toEqual(["shortcuts"]);
    // Ctrl is ⌘ on other computers
    expect(doing(press("ArrowDown", { ctrlKey: true }))).toEqual(["open"]);
    // There is no Cut: an item is moved by copying it, then "Move here"
    expect(doing(cmd("x"))).toEqual([]);
    expect(MAC_KEYS.cut).toEqual([]);
  });

  test("leave out the keys the browser keeps for itself", () => {
    for (const key of ["n", "N", "t", "T", "w", "W", "q", "r", "l"]) for (const shiftKey of [false, true]) expect(doing(cmd(key, { shiftKey })), `⌘${shiftKey ? "⇧" : ""}${key}`).toEqual([]);
  });

  test("no key does two things", () => {
    const seen = new Map<string, Action>();
    for (const a of COMMANDS)
      for (const combo of MAC_KEYS[a]) {
        expect(seen.get(combo), `${combo}: ${a} and ${seen.get(combo)}`).toBeUndefined();
        seen.set(combo, a);
      }
  });

  test("→ and ← expand and collapse folders in the List view, and go between columns in the Columns view", () => {
    expect(pressed(press("ArrowRight"), MAC_KEYS.expand)).toBe(true);
    expect(pressed(press("ArrowLeft"), MAC_KEYS.collapse)).toBe(true);
    expect(pressed(press("ArrowRight"), MAC_KEYS.nextColumn)).toBe(true);
    expect(pressed(press("ArrowLeft"), MAC_KEYS.previousColumn)).toBe(true);
  });

  test("every action has keys, but those the style does differently: no Cut, no keys to select (the arrows do), ⌘↑ and ⌘↓ aren't for the focus", () => {
    expect(
      Object.entries(MAC_KEYS)
        .filter(([, keys]) => !keys.length)
        .map(([a]) => a),
    ).toEqual(["focusOnly", "select", "toggleSelect", "cut"]);
  });

  test("the shortcuts dialog lists only actions with keys", () => {
    const rows = macKit.shortcuts().groups.flatMap((g) => g.rows);
    expect(rows.filter((row) => !keysOf(MAC_KEYS, row.actions)).map((row) => row.label)).toEqual([]);
  });
});

describe("the shortcuts dialog in the Mac style", () => {
  let root: Root | null = null;
  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    document.body.replaceChildren();
  });

  test("lists the Mac map", () => {
    const el = document.createElement("div");
    document.body.append(el);
    root = createRoot(el);
    act(() =>
      root!.render(
        <StyleKitContext.Provider value={macKit}>
          <ShortcutsHost />
        </StyleKitContext.Provider>,
      ),
    );
    act(() => openShortcuts());
    const shown = [...document.querySelectorAll('[role="dialog"] tr')].map(
      (tr) => [...tr.querySelectorAll("kbd")].map((k) => k.textContent).join(" / ") + " = " + tr.lastElementChild!.textContent,
    );
    // The test runs with Ctrl for the modifier (not on a Mac)
    expect(shown).toContain("Enter = Rename");
    expect(shown).toContain("Ctrl+↓ = Open");
    expect(shown).toContain("Ctrl+↑ = Up one folder, with the folder you came from selected");
    expect(shown).toContain("Ctrl+Backspace = Move to trash");
    expect(shown).toContain("Ctrl+I = Get info");
    expect(shown).toContain("Space = Quick look; the arrows go to the next item");
    expect(shown).toContain("Ctrl+Alt+V = Move the items copied here");
    expect(shown).toContain("Ctrl+Shift+G = Go to folder");
    expect(shown).toContain("→ / ← = In the List view: expand or collapse the selected folder");
    expect(document.querySelector('[role="dialog"]')!.textContent).toContain("Like a Mac.");
  });
});
