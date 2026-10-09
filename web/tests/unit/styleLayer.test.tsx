// The style layer: the keyboard map and how key presses are matched against it (lib/style/keymap.ts), the Windows
// style's map, the kits there are (components/style), and the shortcuts dialog listing the keys of the style in use
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, test, vi } from "vitest";
import { kits, loadMacKit, StyleKitContext, useStyleKit, useView, useViews, type StyleKit } from "@/components/style";

// The Mac kit is a chunk of its own, loaded when the style is in use
beforeAll(() => loadMacKit());
import { windowsKit } from "@/components/style/windows";
import { WINDOWS_KEYS } from "@/components/style/windows/keys";
import { openShortcuts, ShortcutsHost } from "@/components/ShortcutsDialog";
import { READY_STYLES, STYLE_CHOICES, StyleChoiceContext } from "@/lib/style";
import { keysOf, pressed, pressedKey, type Action, type KeyPress } from "@/lib/style/keymap";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/** A key press: `key` as the browser names it, with modifiers */
const press = (key: string, mods: Partial<KeyPress> = {}): KeyPress => ({ key, code: "", ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...mods });

describe("matching key presses", () => {
  test("a key with modifiers is pressed with exactly those", () => {
    expect(pressed(press("x", { ctrlKey: true }), ["Ctrl+X"])).toBe(true);
    // ⌘ is the Ctrl of macOS
    expect(pressed(press("x", { metaKey: true }), ["Ctrl+X"])).toBe(true);
    expect(pressed(press("x"), ["Ctrl+X"])).toBe(false);
    expect(pressed(press("x", { ctrlKey: true, shiftKey: true }), ["Ctrl+X"])).toBe(false);
    expect(pressed(press("x", { ctrlKey: true, altKey: true }), ["Ctrl+X"])).toBe(false);
    expect(pressed(press("N", { ctrlKey: true, shiftKey: true }), ["Ctrl+Shift+N"])).toBe(true);
    expect(pressed(press("n", { ctrlKey: true }), ["Ctrl+Shift+N"])).toBe(false);
  });

  test("letters in either case, and by the key itself with Alt (which types other letters on macOS)", () => {
    expect(pressed(press("X", { ctrlKey: true }), ["Ctrl+X"])).toBe(true);
    expect(pressed(press("∂", { altKey: true, code: "KeyD" }), ["Alt+D"])).toBe(true);
    expect(pressed(press("d", { altKey: true, code: "KeyD" }), ["Alt+D"])).toBe(true);
    // Without Alt the letter typed counts, wherever the key is on the keyboard
    expect(pressed(press("q", { ctrlKey: true, code: "KeyA" }), ["Ctrl+A"])).toBe(false);
  });

  test("keys by the names people read", () => {
    expect(pressed(press("ArrowUp", { altKey: true }), ["Alt+↑"])).toBe(true);
    expect(pressed(press("Escape"), ["Esc"])).toBe(true);
    expect(pressed(press(" "), ["Space"])).toBe(true);
    expect(pressed(press(" ", { ctrlKey: true }), ["Ctrl+Space"])).toBe(true);
    expect(pressed(press("PageDown"), ["PgDn"])).toBe(true);
    expect(pressed(press("Delete", { shiftKey: true }), ["Shift+Delete"])).toBe(true);
    expect(pressed(press("Delete", { shiftKey: true }), ["Delete"])).toBe(false);
  });

  test("a sign is typed with or without Shift, depending on the keyboard", () => {
    expect(pressed(press("?", { shiftKey: true }), ["?"])).toBe(true);
    expect(pressed(press("?"), ["?"])).toBe(true);
    expect(pressed(press("?", { ctrlKey: true, shiftKey: true }), ["?"])).toBe(false);
  });

  test("any of the alternatives", () => {
    expect(pressed(press("Backspace"), ["Alt+←", "Backspace"])).toBe(true);
    expect(pressed(press("ArrowLeft", { altKey: true }), ["Alt+←", "Backspace"])).toBe(true);
    expect(pressed(press("ArrowLeft"), ["Alt+←", "Backspace"])).toBe(false);
    expect(pressed(press("ArrowLeft"), [])).toBe(false);
  });

  test("moving through the list: the key, whatever Ctrl and Shift, but not with Alt", () => {
    expect(pressedKey(press("ArrowDown"), ["↓"])).toBe(true);
    expect(pressedKey(press("ArrowDown", { shiftKey: true }), ["↓"])).toBe(true);
    expect(pressedKey(press("ArrowDown", { ctrlKey: true }), ["↓"])).toBe(true);
    expect(pressedKey(press("ArrowDown", { altKey: true }), ["↓"])).toBe(false);
    expect(pressedKey(press("End", { shiftKey: true }), ["End"])).toBe(true);
  });

  test("the keys of several actions, as the shortcuts dialog lists them", () => {
    expect(keysOf(WINDOWS_KEYS, ["cut", "copy", "paste"])).toBe("Ctrl+X / Ctrl+C / Ctrl+V");
    expect(keysOf(WINDOWS_KEYS, ["back"])).toBe("Alt+← / Backspace");
  });
});

describe("the Windows style's keys", () => {
  /** The actions a key press does, of those the explorer and the address bar take from anywhere on the page */
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
    "undo",
    "trash",
    "deleteForever",
    "newFolder",
    "details",
    "shortcuts",
  ];
  const doing = (e: KeyPress) => COMMANDS.filter((a) => pressed(e, WINDOWS_KEYS[a]));

  test("are File Explorer's", () => {
    expect(doing(press("x", { ctrlKey: true }))).toEqual(["cut"]);
    expect(doing(press("c", { ctrlKey: true }))).toEqual(["copy"]);
    expect(doing(press("v", { ctrlKey: true }))).toEqual(["paste"]);
    expect(doing(press("a", { ctrlKey: true }))).toEqual(["selectAll"]);
    expect(doing(press("z", { ctrlKey: true }))).toEqual(["undo"]);
    expect(doing(press("z", { ctrlKey: true, shiftKey: true }))).toEqual([]);
    expect(doing(press("Delete"))).toEqual(["trash"]);
    expect(doing(press("Delete", { shiftKey: true }))).toEqual(["deleteForever"]);
    expect(doing(press("N", { ctrlKey: true, shiftKey: true }))).toEqual(["newFolder"]);
    expect(doing(press("F2"))).toEqual(["rename"]);
    expect(doing(press("Enter"))).toEqual(["open"]);
    expect(doing(press("Enter", { altKey: true }))).toEqual(["details"]);
    expect(doing(press("F10", { shiftKey: true }))).toEqual(["menu"]);
    expect(doing(press("Escape"))).toEqual(["clearSelection"]);
    expect(doing(press("ArrowUp", { altKey: true }))).toEqual(["upFolder"]);
    expect(doing(press("ArrowLeft", { altKey: true }))).toEqual(["back"]);
    expect(doing(press("Backspace"))).toEqual(["back"]);
    expect(doing(press("ArrowRight", { altKey: true }))).toEqual(["forward"]);
    expect(doing(press("l", { ctrlKey: true }))).toEqual(["addressBar"]);
    expect(doing(press("d", { altKey: true, code: "KeyD" }))).toEqual(["addressBar"]);
    expect(doing(press("f", { ctrlKey: true }))).toEqual(["search"]);
    expect(doing(press("F3"))).toEqual(["search"]);
    expect(doing(press("F5"))).toEqual(["refresh"]);
    expect(doing(press("?", { shiftKey: true }))).toEqual(["shortcuts"]);
    // Letters alone go to typing the start of a name
    expect(doing(press("a"))).toEqual([]);
  });

  test("no key does two things", () => {
    const seen = new Map<string, Action>();
    for (const a of COMMANDS)
      for (const combo of WINDOWS_KEYS[a]) {
        expect(seen.get(combo), `${combo}: ${a} and ${seen.get(combo)}`).toBeUndefined();
        seen.set(combo, a);
      }
  });

  test("Left and Right go from column to column in the Columns view, without Alt (Back and Forward)", () => {
    expect(pressed(press("ArrowLeft"), WINDOWS_KEYS.previousColumn)).toBe(true);
    expect(pressed(press("ArrowRight"), WINDOWS_KEYS.nextColumn)).toBe(true);
    expect(pressed(press("ArrowLeft", { altKey: true }), WINDOWS_KEYS.previousColumn)).toBe(false);
    expect(pressed(press("ArrowRight", { shiftKey: true }), WINDOWS_KEYS.nextColumn)).toBe(false);
  });

  test("every action has keys, but those of features the style doesn't have", () => {
    expect(Object.entries(WINDOWS_KEYS).filter(([, keys]) => !keys.length)).toEqual([
      ["moveHere", []],
      ["quickLook", []],
    ]);
  });
});

describe("the kits", () => {
  test("every style is ready, with a kit of its own", () => {
    for (const style of READY_STYLES) expect(kits()[style]?.id).toBe(style);
    expect(STYLE_CHOICES).toContain("mac");
    expect(READY_STYLES).toEqual(["windows", "mac"]);
  });

  test("the Windows style offers File Explorer's views and Columns, Details first in the status bar", () => {
    expect(windowsKit.views().map((v) => v.id)).toEqual(["grid", "medium", "compact", "list", "tiles", "columns"]);
    // Phones show one folder level at a time already
    expect(
      windowsKit
        .views()
        .filter((v) => v.notOnPhones)
        .map((v) => v.id),
    ).toEqual(["columns"]);
    expect(windowsKit.defaultView).toBe("list");
    expect(windowsKit.statusViews).toEqual(["list", "grid"]);
  });

  test("the shortcuts dialog lists only actions with keys", () => {
    const rows = windowsKit.shortcuts().groups.flatMap((g) => g.rows);
    expect(rows.filter((row) => !keysOf(windowsKit.keys, row.actions)).map((row) => row.label)).toEqual([]);
  });
});

describe("the views offered on a screen", () => {
  afterEach(() => vi.unstubAllGlobals());

  /** The views offered, and the view shown when Columns was chosen, on a phone or a wider screen */
  function offered(phone: boolean) {
    vi.stubGlobal("matchMedia", (query: string) => ({ matches: phone && query.includes("max-width"), addEventListener() {}, removeEventListener() {} }));
    localStorage.setItem("tf-view", JSON.stringify("columns"));
    const seen = { ids: [] as string[], view: "" };
    function Probe() {
      seen.ids = useViews().map((v) => v.id);
      seen.view = useView()[0];
      return null;
    }
    const r = createRoot(document.createElement("div"));
    act(() => r.render(<Probe />));
    act(() => r.unmount());
    return seen;
  }

  test("Columns is offered on wider screens, not on phones, which show the style's own view instead", () => {
    expect(offered(false)).toEqual({ ids: ["grid", "medium", "compact", "list", "tiles", "columns"], view: "columns" });
    expect(offered(true)).toEqual({ ids: ["grid", "medium", "compact", "list", "tiles"], view: "list" });
  });
  /** The kit the Mac style uses on a phone or a wider screen */
  function macKitOn(phone: boolean): StyleKit {
    vi.stubGlobal("matchMedia", (query: string) => ({ matches: phone && query.includes("max-width"), addEventListener() {}, removeEventListener() {} }));
    const seen: { kit?: StyleKit } = {};
    function Probe() {
      seen.kit = useStyleKit();
      return null;
    }
    const r = createRoot(document.createElement("div"));
    act(() =>
      r.render(
        <StyleChoiceContext.Provider value="mac">
          <Probe />
        </StyleChoiceContext.Provider>,
      ),
    );
    act(() => r.unmount());
    return seen.kit!;
  }

  test("on a phone the Mac style has the Windows style's parts, menus and keys, with its own look and icons", () => {
    const mac = kits().mac!;
    expect(macKitOn(false)).toBe(mac);
    const phone = macKitOn(true);
    expect(phone.id).toBe("mac");
    expect(phone.ItemIcon).toBe(mac.ItemIcon);
    expect(phone.keys).toBe(windowsKit.keys);
    expect(phone.menu).toBe(windowsKit.menu);
    expect(phone.Frame).toBe(windowsKit.Frame);
    expect([phone.clickToRename, phone.newAtEnd]).toEqual([windowsKit.clickToRename, windowsKit.newAtEnd]);
    // The same object each time, so what depends on it doesn't change on every render
    expect(macKitOn(true)).toBe(phone);
  });
});

describe("the shared parts", () => {
  /** Files and folders that are the same in every style: data, selection, file operations, previews, uploads, background tasks, large folders */
  const SHARED = [
    "api",
    "ooxml",
    "components/sheet",
    "uploads.ts",
    "downloads.ts",
    "lib/clipboard.ts",
    "lib/conflicts.ts",
    "lib/dnd.ts",
    "lib/jobs.tsx",
    "lib/listSelection.ts",
    "lib/liveSearch.ts",
    "lib/queries.ts",
    "lib/span.ts",
    "lib/thumbs.ts",
    "lib/transfer.ts",
    "lib/undo.ts",
    "lib/uploadRecovery.ts",
    "lib/windows.ts",
    "components/Preview.tsx",
    "components/FileViewer.tsx",
    "components/OfficeViewer.tsx",
    "components/TextEditor.tsx",
    "pages/FileViewPage.tsx",
  ];
  const files = (path: string): string[] => (statSync(path).isDirectory() ? readdirSync(path).flatMap((name) => files(join(path, name))) : /\.tsx?$/.test(path) ? [path] : []);

  test("don't ask which style is in use", () => {
    const src = join(process.cwd(), "src");
    // Types are fine: the API carries the setting itself
    const asking = SHARED.flatMap((p) => files(join(src, p))).filter((f) => /^import (?!type )[^;]*from "@\/(lib|components)\/style\b/m.test(readFileSync(f, "utf8")));
    expect(asking).toEqual([]);
  });
});

describe("the shortcuts dialog", () => {
  let root: Root | null = null;
  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    document.body.replaceChildren();
  });
  /** The rows of the dialog: the keys, and what they do */
  function rows(kit?: StyleKit) {
    const el = document.createElement("div");
    document.body.append(el);
    root = createRoot(el);
    act(() => root!.render(kit ? <StyleKitContext.Provider value={kit}>{<ShortcutsHost />}</StyleKitContext.Provider> : <ShortcutsHost />));
    act(() => openShortcuts());
    return [...document.querySelectorAll('[role="dialog"] tr')].map((tr) => [...tr.querySelectorAll("kbd")].map((k) => k.textContent).join(" / ") + " = " + tr.lastElementChild!.textContent);
  }

  test("lists the Windows style's keys", () => {
    const shown = rows();
    expect(shown).toContain("Alt+← / Backspace = Back");
    expect(shown).toContain("Ctrl+L / Alt+D = Go to the address bar");
    expect(shown).toContain("↑ / ↓ / Home / End / PgUp / PgDn = Move through the list (also ← and → in the icon view); hold Shift to select as you go");
    expect(shown).toContain("Ctrl+X / Ctrl+C / Ctrl+V = Cut, copy, paste");
    expect(shown).toContain("Shift+Delete = Delete permanently");
    expect(shown).toContain("Enter = Open");
    expect(shown).toContain("F2 = Rename");
    expect(shown).toContain("← / → = In the Columns view: back to the column before, or on to the column of the selected folder");
    expect(shown).toHaveLength(windowsKit.shortcuts().groups.reduce((n, g) => n + g.rows.length, 0));
  });

  test("lists the keys of the style in use", () => {
    const other: StyleKit = {
      ...windowsKit,
      keys: { ...windowsKit.keys, open: ["Ctrl+↓"], rename: ["Enter"], trash: ["Ctrl+Backspace"] },
      shortcuts: () => ({
        note: "Another style",
        groups: [
          {
            title: "Items",
            rows: [
              { actions: ["open"], label: "Open" },
              { actions: ["rename"], label: "Rename" },
              { actions: ["trash"], label: "Move to trash" },
            ],
          },
        ],
      }),
    };
    expect(rows(other)).toEqual(["Ctrl+↓ = Open", "Enter = Rename", "Ctrl+Backspace = Move to trash"]);
    expect(document.querySelector('[role="dialog"]')!.textContent).toContain("Another style");
  });
});
