/**
 * The style layer: each interface style provides its parts (types.ts), and the explorer asks for the current one's with
 * `useStyleKit()`. A style that isn't offered yet (lib/style `READY_STYLES`) gets the Windows one.
 */
import { createContext, useContext, useEffect } from "react";
import type { ViewMode } from "@/components/fileList/layout";
import { useMediaQuery } from "@/lib/focus";
import { usePersisted } from "@/lib/session";
import { useInterfaceStyle, type Style } from "@/lib/style";
import type { StyleKit, ViewChoice } from "./types";
import { macKit } from "./mac";
import { loadMacArt } from "./mac/loadArt";
import { windowsKit } from "./windows";

export type { FrameParts, FramePlace, ItemIconProps, OwnViewProps, ShortcutGroup, ShortcutRow, StyleKit, ViewChoice } from "./types";

/** The kits there are, looked up when asked for (the kits' modules import the explorer, which imports this) */
export function kits(): Partial<Record<Style, StyleKit>> {
  return { windows: windowsKit, mac: macKit };
}

/** A kit to use instead of the person's style (tests) */
export const StyleKitContext = createContext<StyleKit | null>(null);

/** The parts of the style in use */
export function useStyleKit(): StyleKit {
  const forced = useContext(StyleKitContext);
  const { style } = useInterfaceStyle();
  return forced ?? kits()[style] ?? windowsKit;
}

/** The style the page was last shown in (kept in the browser), whose assets start loading with the page */
const SHOWN_KEY = "tf-style-shown";
try {
  if (localStorage.getItem(SHOWN_KEY) === "mac") loadMacArt();
} catch {
  // Not in every environment the modules load in (tests), or storage is forbidden
}

/**
 * Puts the style in use on the page's root (`data-style`), where its look's tokens apply (style.css; the Mac style's in
 * mac/art/mac.css). Used by the pages that show a style's parts: the frame of signed-in pages, and share links.
 */
export function useApplyStyle() {
  const { id } = useStyleKit();
  useEffect(() => {
    document.documentElement.dataset.style = id;
    try {
      localStorage.setItem(SHOWN_KEY, id);
    } catch {
      // Ignore when the browser forbids storage
    }
  }, [id]);
}

/** The views of the file list the style offers on this screen (phones have fewer) */
export function useViews(): readonly ViewChoice[] {
  const kit = useStyleKit();
  const phone = useMediaQuery("(max-width: 47.99rem)");
  const views = kit.views();
  return phone ? views.filter((v) => !v.notOnPhones) : views;
}

/** The view of the file list: the one chosen (kept in the browser), when it is offered here, else the style's own */
export function useView(): [ViewMode, (view: ViewMode) => void] {
  const kit = useStyleKit();
  const views = useViews();
  const [kept, setView] = usePersisted<ViewMode>("tf-view", kit.defaultView);
  return [views.some((v) => v.id === kept) ? kept : kit.defaultView, setView];
}
