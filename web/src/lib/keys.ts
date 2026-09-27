/** Keyboard differences between platforms */

/** macOS (and iPadOS with a keyboard): ⌘ takes the place of Ctrl, and Option the place of Alt */
export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent);

/** Holding Ctrl while dropping copies instead of moving, like File Explorer (Option on macOS, like Finder) */
export function wantsCopy(e: { ctrlKey: boolean; altKey: boolean }) {
  return isMac ? e.altKey : e.ctrlKey;
}
