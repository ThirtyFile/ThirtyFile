/**
 * The style layer: each interface style provides its parts (types.ts), and the explorer asks for the current one's with
 * `useStyleKit()`. A style that isn't offered yet (lib/style `READY_STYLES`) gets the Windows one.
 */
import { createContext, useContext } from "react";
import type { ViewMode } from "@/components/fileList/layout";
import { useMediaQuery } from "@/lib/focus";
import { usePersisted } from "@/lib/session";
import { useInterfaceStyle, type Style } from "@/lib/style";
import type { StyleKit, ViewChoice } from "./types";
import { macKit } from "./mac";
import { windowsKit } from "./windows";

export type { FrameParts, FramePlace, IconSet, OwnViewProps, ShortcutGroup, ShortcutRow, StyleKit, ViewChoice } from "./types";

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
