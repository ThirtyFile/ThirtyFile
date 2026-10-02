/** Keyboard differences between platforms */
import { locale } from "@/lib/i18n";

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

/** Compares the start of a name with typed text the interface language's way: letter case and accents don't count */
const typedCollator = new Intl.Collator(locale, { sensitivity: "base", usage: "search" });

/** Whether `name` starts with `prefix`, compared with `typedCollator` */
const startsWith = (name: string, prefix: string) => typedCollator.compare(name.slice(0, prefix.length), prefix) === 0;

/**
 * Typing to find an item (like File Explorer): the index of the next name after `from` (wrapping around) that starts
 * with `prefix`, ignoring case and accents, or -1. The item at `from` itself is checked last.
 */
export function findByPrefix(names: readonly string[], prefix: string, from: number) {
  for (let k = 1; k <= names.length; k++) {
    const i = (((from + k) % names.length) + names.length) % names.length;
    if (startsWith(names[i], prefix)) return i;
  }
  return -1;
}
