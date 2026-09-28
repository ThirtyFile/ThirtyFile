/** Keyboard differences between platforms */

/** macOS (and iPadOS with a keyboard): ⌘ takes the place of Ctrl, and Option the place of Alt */
export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent);

/** Holding Ctrl while dropping copies instead of moving, like File Explorer (Option on macOS, like Finder) */
export function wantsCopy(e: { ctrlKey: boolean; altKey: boolean }) {
  return isMac ? e.altKey : e.ctrlKey;
}

/** A shortcut as the platform writes it: "Ctrl+X" is "⌘+X" and "Alt+Enter" is "⌥+Enter" on macOS */
export function shortcut(keys: string, mac = isMac) {
  return mac ? keys.replace(/\bCtrl\b/g, "⌘").replace(/\bAlt\b/g, "⌥") : keys;
}

/**
 * Typing to find an item (like File Explorer): the index of the next name after `from` (wrapping around) that starts
 * with `prefix`, ignoring case, or -1. The item at `from` itself is checked last.
 */
export function findByPrefix(names: readonly string[], prefix: string, from: number) {
  const want = prefix.toLocaleLowerCase();
  for (let k = 1; k <= names.length; k++) {
    const i = (((from + k) % names.length) + names.length) % names.length;
    if (names[i].toLocaleLowerCase().startsWith(want)) return i;
  }
  return -1;
}
