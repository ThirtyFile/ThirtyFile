/**
 * The style layer: each interface style provides its parts (types.ts), and the explorer asks for the current one's with
 * `useStyleKit()`. Only the Windows style exists so far; a style without a kit gets the Windows one.
 */
import { createContext, useContext } from "react";
import { useInterfaceStyle, type Style } from "@/lib/style";
import type { StyleKit } from "./types";
import { windowsKit } from "./windows";

export type { FrameParts, IconSet, ShortcutGroup, ShortcutRow, StyleKit, ViewChoice } from "./types";

/** The kits there are, looked up when asked for (the kits' modules import the explorer, which imports this) */
export function kits(): Partial<Record<Style, StyleKit>> {
  return { windows: windowsKit };
}

/** A kit to use instead of the person's style (tests) */
export const StyleKitContext = createContext<StyleKit | null>(null);

/** The parts of the style in use */
export function useStyleKit(): StyleKit {
  const forced = useContext(StyleKitContext);
  const { style } = useInterfaceStyle();
  return forced ?? kits()[style] ?? windowsKit;
}
