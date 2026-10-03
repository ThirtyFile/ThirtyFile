/**
 * The interface style of the page: the Windows style, or the Mac style (to come, #325). Each person chooses one with
 * their account (`auto` by default, which follows the operating system: lib/style/device.ts); visitors who aren't
 * signed in (share links, the sign-in page) get `auto`. What each style provides is in components/style.
 */
import { createContext, useContext } from "react";
import { resolveStyle, thisDevice, type Resolved, type Style, type StyleChoice } from "@/lib/style/device";

export { STYLE_CHOICES, styleChoice, type Resolved, type Style, type StyleChoice } from "@/lib/style/device";

/** The styles that exist so far: a choice of another falls back to the Windows style */
export const READY_STYLES: readonly Style[] = ["windows"];

/** The signed-in person's choice (App.tsx provides it with the account); `auto` for visitors */
export const StyleChoiceContext = createContext<StyleChoice>("auto");

/** The style this page uses */
export function useInterfaceStyle(): Resolved {
  return resolveStyle(useContext(StyleChoiceContext), thisDevice, READY_STYLES);
}
