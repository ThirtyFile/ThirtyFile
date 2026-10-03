/** Window chrome is local to this browser and account, separate from the account's interface style. */
import { createContext } from "react";
import { useMe, usePersisted } from "@/lib/session";
import { useInterfaceStyle } from "@/lib/style";
import { useMediaQuery } from "@/lib/focus";
import { useTabsState } from "@/tabs";

export const CompactToolbar = createContext(false);

/** Both the strip and its banner disappear together; the frame then provides notifications. */
export function useSingleMacTab() {
  const mac = useInterfaceStyle().style === "mac";
  const desktop = useMediaQuery("(min-width: 48rem)");
  const { tabs } = useTabsState();
  return mac && desktop && tabs.length < 2;
}

export function useMacWindowPrefs() {
  const me = useMe();
  return usePersisted(`tf-mac-window-${me.id}`, { path: false, status: false });
}
