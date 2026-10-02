//! Context menus: opening one from the keyboard (Shift+F10 or the Menu key, as right-clicking does), and waiting for
//! them to go

/**
 * Resolves once no menu is open (at most half a second): a menu that closes puts the focus back where it was, which
 * would take it from a rename box shown meanwhile
 */
export function menusClosed() {
  const giveUp = performance.now() + 500;
  return new Promise<void>((done) => {
    const look = () => {
      if (!document.querySelector("[role=menu]") || performance.now() > giveUp) setTimeout(done);
      else requestAnimationFrame(look);
    };
    look();
  });
}

/** Shift+F10 or the Menu key */
export function isMenuKey(e: { key: string; shiftKey: boolean; ctrlKey: boolean; altKey: boolean; metaKey: boolean }) {
  return e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey);
}

/**
 * Opens the context menu of `el` as right-clicking it at `at` does (browsers don't all do it for what has the focus),
 * and moves the focus into the menu, so the arrow keys go through its items. When the menu closes without anything
 * else taking the focus (a dialog, the rename box), the focus goes back to `back`. The browser's own menu event that
 * follows the key is held back: it would open the menu again, or the browser's menu where no menu of ours is.
 */
export function openMenuByKey(el: HTMLElement, at: { x: number; y: number }, back: HTMLElement | null = el) {
  const until = Date.now() + 500;
  const hold = (e: MouseEvent) => {
    if (!e.isTrusted || Date.now() > until) return;
    e.preventDefault();
    e.stopPropagation();
  };
  window.addEventListener("contextmenu", hold, true);
  setTimeout(() => window.removeEventListener("contextmenu", hold, true), 500);
  el.dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2, clientX: at.x, clientY: at.y }));
  // The menu shows a frame or two later
  const giveUp = performance.now() + 500;
  const focusMenu = () => {
    const menu = document.querySelector<HTMLElement>("[role=menu][data-open]");
    if (!menu) {
      if (performance.now() < giveUp) requestAnimationFrame(focusMenu);
      return;
    }
    menu.focus();
    const done = new MutationObserver(() => {
      if (menu.isConnected) return;
      done.disconnect();
      const now = document.activeElement;
      if (back?.isConnected && (!now || now === document.body || !now.closest("[role=menu], [role=dialog], input, textarea"))) back.focus({ preventScroll: true });
    });
    done.observe(document.body, { childList: true, subtree: true });
  };
  requestAnimationFrame(focusMenu);
}

/** Where a row's menu opens from the keyboard: by its name (the start of the row), as File Explorer does */
export function menuPointOf(row: HTMLElement) {
  const r = (row.querySelector("[data-drag-handle]") ?? row).getBoundingClientRect();
  return { x: r.left + Math.min(r.width / 2, 100), y: r.top + r.height / 2 };
}
