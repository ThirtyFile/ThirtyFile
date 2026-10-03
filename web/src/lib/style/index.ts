/**
 * The interface style of the page: the Windows style, or the Mac style (to come, #325). Each person chooses one with
 * their account (`auto` by default, which follows the operating system: lib/style/device.ts); visitors who aren't
 * signed in (share links, the sign-in page) get `auto`.
 */
import { useContext } from "react";
import { MeContext } from "@/lib/session";
import { resolveStyle, styleChoice, thisDevice, type Resolved, type Style } from "@/lib/style/device";

export { STYLE_CHOICES, type Resolved, type Style, type StyleChoice } from "@/lib/style/device";

/** The styles that exist so far: a choice of another falls back to the Windows style */
export const READY_STYLES: readonly Style[] = ["windows"];

/** The style this page uses: the signed-in person's choice, `auto` for visitors */
export function useInterfaceStyle(): Resolved {
  const me = useContext(MeContext);
  return resolveStyle(styleChoice(me?.style), thisDevice, READY_STYLES);
}
