import { useEffect, useRef, useSyncExternalStore, type RefObject } from "react";

const FOCUSABLE = 'a[href], button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), iframe, [tabindex]:not([tabindex="-1"]), [contenteditable="true"]';
/** Popups opened from inside an overlay render elsewhere in the page; focus may go there */
const POPUP = "[role=menu], [role=dialog], [role=alertdialog], [role=listbox]";

/** Whether a media query currently matches; updates when it changes (e.g. the window is resized past a breakpoint) */
export function useMediaQuery(query: string) {
  return useSyncExternalStore(
    (cb) => {
      const m = matchMedia(query);
      m.addEventListener("change", cb);
      return () => m.removeEventListener("change", cb);
    },
    () => matchMedia(query).matches,
  );
}

/**
 * Keyboard handling for something shown over the page (preview, phone menu, details overlay) while `active`:
 * focus moves into the container, Esc calls `onClose` (when given), and focus goes back to where it was afterwards.
 * `modal` also keeps focus inside: Tab wraps around, and focus that lands on the page behind is brought back.
 */
export function useOverlayFocus(ref: RefObject<HTMLElement | null>, active: boolean, opts: { onClose?: () => void; modal?: boolean } = {}) {
  const closeRef = useRef(opts.onClose);
  closeRef.current = opts.onClose;
  const modal = opts.modal ?? true;

  useEffect(() => {
    const el = ref.current;
    if (!active || !el) return;
    const before = document.activeElement as HTMLElement | null;
    const visible = () => [...el.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((x) => x.getClientRects().length > 0);
    const focusFirst = () => {
      // Skip the resize handle at the pane's edge
      const first = visible().find((x) => x.getAttribute("role") !== "separator");
      if (first) first.focus();
      else {
        if (!el.hasAttribute("tabindex")) el.tabIndex = -1;
        el.focus();
      }
    };
    if (!el.contains(document.activeElement)) focusFirst();

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && closeRef.current && !e.defaultPrevented) {
        e.preventDefault();
        closeRef.current();
      } else if (e.key === "Tab" && modal) {
        const items = visible();
        if (!items.length) {
          e.preventDefault();
          return;
        }
        const first = items[0];
        const last = items[items.length - 1];
        if (e.shiftKey && (document.activeElement === first || document.activeElement === el)) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    const onFocusIn = (e: FocusEvent) => {
      const target = e.target as HTMLElement;
      if (!el.contains(target) && !target.closest?.(POPUP)) focusFirst();
    };
    el.addEventListener("keydown", onKey);
    if (modal) document.addEventListener("focusin", onFocusIn);
    return () => {
      el.removeEventListener("keydown", onKey);
      document.removeEventListener("focusin", onFocusIn);
      // Only return focus when it is still inside (or was lost with the overlay); don't pull it away from somewhere the user moved it
      const now = document.activeElement;
      if (before?.isConnected && (!now || now === document.body || el.contains(now))) before.focus();
    };
  }, [ref, active, modal]);
}
