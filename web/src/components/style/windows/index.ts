/** The Windows style: File Explorer's layout, keys, menus, icons and views */
import type { StyleKit } from "../types";
import { WindowsFrame } from "./frame";
import { WINDOWS_KEYS, windowsShortcuts } from "./keys";
import { WINDOWS_ICONS, windowsViews } from "./look";
import { windowsMenu } from "./menus";
import { windowsToolbar } from "./toolbar";

export const windowsKit: StyleKit = {
  id: "windows",
  Frame: WindowsFrame,
  toolbar: windowsToolbar,
  menu: windowsMenu,
  keys: WINDOWS_KEYS,
  shortcuts: windowsShortcuts,
  icons: WINDOWS_ICONS,
  views: windowsViews,
  defaultView: "list",
  statusViews: ["list", "grid"],
  clickToRename: true,
  newAtEnd: true,
  disclosure: false,
};
