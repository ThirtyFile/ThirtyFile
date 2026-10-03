/**
 * The keyboard map of a style: which keys do what. Each style gives every action its keys (components/style), and the
 * file list, the address bar, the explorer, its menus and the shortcuts dialog all read them from there, so a style
 * changes its keys in one place.
 *
 * Keys are written as people read them: "Ctrl+Shift+N", "Alt+←", "Shift+Delete", "Esc", "PgDn", "?". Ctrl is the
 * modifier key of the operating system: ⌘ on macOS, where it is shown as ⌘ too (lib/keys `shortcut`), whatever the
 * style. A key with modifiers is pressed only with exactly those (Shift aside for "?" and other signs typed with it).
 */

export type Action =
  // Getting around
  | "upFolder"
  | "back"
  | "forward"
  | "addressBar"
  | "search"
  | "refresh"
  /** In an open file (previews and the file page are shared: the same in every style) */
  | "previousFile"
  | "nextFile"
  // Moving through the list: Shift held extends the selection, Ctrl moves only the focus (`focusOnly`)
  | "itemUp"
  | "itemDown"
  /** In the icon views */
  | "itemLeft"
  | "itemRight"
  /** In the Columns view: back to the column before, and on to the column of the folder selected */
  | "previousColumn"
  | "nextColumn"
  /** In a list whose folders expand in place (the Mac style's List view): expanding the folder with the focus, or collapsing it */
  | "expand"
  | "collapse"
  | "first"
  | "last"
  | "pageUp"
  | "pageDown"
  | "focusOnly"
  // Selecting
  | "select"
  | "toggleSelect"
  | "selectAll"
  /** Typing the start of a name (shown only: any letter does it) */
  | "findByName"
  | "clearSelection"
  // Working with items
  | "open"
  /** The menu of the item with the focus; the keyboard's own menu key opens it too, in every style */
  | "menu"
  | "rename"
  | "cut"
  | "copy"
  | "paste"
  | "undo"
  | "trash"
  | "deleteForever"
  | "newFolder"
  | "details"
  /** A preview of the item selected in a window over the list, and the next one with the arrows (the Mac style's Quick look) */
  | "quickLook"
  | "shortcuts";

/** Every action's keys, the first one being the one menus and tooltips show */
export type KeyMap = { readonly [A in Action]: readonly string[] };

/** What a key event says (a DOM or React keyboard event) */
export interface KeyPress {
  key: string;
  code?: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

/** Keys written as people read them, by the name the browser gives them */
const NAMES: Record<string, string> = {
  "↑": "ArrowUp",
  "↓": "ArrowDown",
  "←": "ArrowLeft",
  "→": "ArrowRight",
  Esc: "Escape",
  Space: " ",
  PgUp: "PageUp",
  PgDn: "PageDown",
};

interface Combo {
  key: string;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
}

const parsed = new Map<string, Combo>();
function parse(combo: string): Combo {
  let c = parsed.get(combo);
  if (!c) {
    const parts = combo.split("+");
    const key = parts.pop()!;
    c = { key: NAMES[key] ?? key, ctrl: parts.includes("Ctrl"), alt: parts.includes("Alt"), shift: parts.includes("Shift") };
    parsed.set(combo, c);
  }
  return c;
}

/** Whether the event is the key of `c`, modifiers aside */
function sameKey(e: KeyPress, c: Combo) {
  if (/^[A-Z]$/.test(c.key)) {
    // A letter by the character typed; with Alt, which types other characters on macOS, by the key itself
    return e.key.toLowerCase() === c.key.toLowerCase() || (c.alt && e.code === `Key${c.key}`);
  }
  return e.key === c.key;
}

/** A sign typed with Shift on some keyboards and without it on others ("?", "/"): Shift doesn't count */
const typedSign = (c: Combo) => c.key.length === 1 && !/[A-Za-z0-9 ]/.test(c.key);

/** Whether the event is one of `combos`, with exactly its modifiers */
export function pressed(e: KeyPress, combos: readonly string[]): boolean {
  return combos.some((s) => {
    const c = parse(s);
    return sameKey(e, c) && (e.ctrlKey || e.metaKey) === c.ctrl && e.altKey === c.alt && (typedSign(c) || e.shiftKey === c.shift);
  });
}

/**
 * Whether the event is one of the keys of `combos` whatever Ctrl and Shift (but not Alt) say: for moving through the
 * list, where those decide how the selection follows
 */
export function pressedKey(e: KeyPress, combos: readonly string[]): boolean {
  return !e.altKey && combos.some((s) => sameKey(e, parse(s)));
}

/** The keys of some actions as the shortcuts dialog lists them: "Ctrl+X / Ctrl+C / Ctrl+V" */
export function keysOf(map: KeyMap, actions: readonly Action[]): string {
  return actions.flatMap((a) => map[a]).join(" / ");
}
