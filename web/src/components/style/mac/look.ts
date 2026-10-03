/**
 * The Mac style's views of the file list: Icons, List (with folders that expand in place) and Columns. Its own icons
 * and look come later (#326): until then it draws files with the Windows style's icons.
 */
import { Columns3Icon, LayoutGridIcon, ListIcon } from "lucide-react";
import { t, tc } from "@/lib/i18n";
import type { IconSet, ViewChoice } from "../types";
import { WINDOWS_ICONS } from "../windows/look";

export const MAC_ICONS: IconSet = WINDOWS_ICONS;

export function macViews(): ViewChoice[] {
  return [
    { id: "grid", Icon: LayoutGridIcon, label: t("Icons") },
    { id: "list", Icon: ListIcon, label: t("List") },
    { id: "columns", Icon: Columns3Icon, label: tc("view", "Columns"), notOnPhones: true },
  ];
}
