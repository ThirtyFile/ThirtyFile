/**
 * The keys of a page's frame, the same in every style's frame (with the style's keys): back, forward and up, the
 * search box, the path (the Windows style's address bar, the Mac style's "Go to folder"), refreshing the list and the
 * shortcuts dialog
 */
import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import { openShortcuts } from "@/components/ShortcutsDialog";
import { isTyping } from "@/components/explorer/types";
import { listRefreshed } from "@/components/explorer/newItems";
import { useStyleKit } from "@/components/style";
import { pressed } from "@/lib/style/keymap";
import { useTabActions, useTabsState } from "@/tabs";

/** What Refresh leaves alone: the account, the site's branding and the sign-in options don't change with a page */
const NOT_REFRESHED = new Set(["me", "branding", "auth-options", "sso-providers"]);

/** Refreshing: what the page shows loads again, and new items kept where they were made go to their sorted places */
export function useRefresh() {
  const qc = useQueryClient();
  const [refreshing, setRefreshing] = useState(false);
  const refresh = async () => {
    setRefreshing(true);
    listRefreshed();
    await qc.invalidateQueries({ predicate: (q) => !NOT_REFRESHED.has(q.queryKey[0] as string) });
    setRefreshing(false);
  };
  return { refreshing, refresh };
}

/** Back and forward in the tab's history, and whether there is somewhere to go */
export function useHistory() {
  const tabs = useTabsState();
  const { back, forward } = useTabActions();
  const tab = tabs.tabs.find((t) => t.id === tabs.active);
  return { back, forward, canBack: !!tab && tab.index > 0, canForward: !!tab && tab.index < tab.entries.length - 1 };
}

/**
 * Moving around with the keyboard, with the style's keys (see the shortcuts dialog). `enabled`: the page has the file
 * explorer's keys; `focusSearch` and `editPath` are what the search and path keys do in the style's frame.
 */
export function useFrameKeys(o: { enabled?: boolean; upTo?: string | null; refresh(): void; focusSearch(): void; editPath(): void }) {
  const navigate = useNavigate();
  const { back, forward } = useTabActions();
  const k = useStyleKit().keys;
  useEffect(() => {
    if (!o.enabled) return;
    // Keys with Alt are taken wherever the focus is (onAltArrow); the others only where nothing else wants them
    const withAlt = (combos: readonly string[]) => combos.filter((c) => /\bAlt\+/.test(c));
    const withoutAlt = (combos: readonly string[]) => combos.filter((c) => !/\bAlt\+/.test(c));
    const up = () => o.upTo && navigate(o.upTo);
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      // Refreshing refreshes the list rather than reloading the page, also while typing in a box
      if (pressed(e, k.refresh)) {
        e.preventDefault();
        o.refresh();
        return;
      }
      if (isTyping(e.target) || document.querySelector("[role=dialog]") || (e.target as HTMLElement | null)?.closest?.("[role=menu], [role=menuitem]")) return;
      if (pressed(e, withoutAlt(k.back))) back();
      else if (pressed(e, withoutAlt(k.forward))) forward();
      else if (pressed(e, withoutAlt(k.upFolder))) up();
      else if (pressed(e, k.search)) o.focusSearch();
      else if (pressed(e, k.addressBar)) o.editPath();
      else if (pressed(e, k.shortcuts)) openShortcuts();
      else return;
      e.preventDefault();
    };
    // Alt+arrows move around folders wherever the focus is: before a focused menu button takes Alt+↑ or Alt+↓ to open
    // its menu
    const onAltArrow = (e: KeyboardEvent) => {
      const toParent = pressed(e, withAlt(k.upFolder));
      const backward = pressed(e, withAlt(k.back));
      if (!toParent && !backward && !pressed(e, withAlt(k.forward))) return;
      const at = e.target as HTMLElement | null;
      if (isTyping(at) || at?.tagName === "SELECT" || document.querySelector("[role=dialog]") || at?.closest?.("[role=menu]")) return;
      if (toParent) up();
      else if (backward) back();
      else forward();
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("keydown", onAltArrow, true);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keydown", onAltArrow, true);
    };
  });
}
