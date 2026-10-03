/**
 * The Mac style: a Finder window's layout and views. Its name in the interface is "Mac style"; it uses no Apple
 * artwork. Data, file operations, previews and the phone layout are the ones every style shares.
 */
import type { StyleKit } from "../types";
import { WINDOWS_KEYS, windowsShortcuts } from "../windows/keys";
import { MacFrame } from "./frame";
import { MAC_ICONS, macViews } from "./look";
import { macMenu } from "./menus";
import { macToolbar } from "./toolbar";

export const macKit: StyleKit = {
  id: "mac",
  Frame: MacFrame,
  toolbar: macToolbar,
  menu: macMenu,
  // The Mac keyboard map comes with the style being offered (#325); until then, the Windows style's keys
  keys: WINDOWS_KEYS,
  shortcuts: windowsShortcuts,
  icons: MAC_ICONS,
  views: macViews,
  defaultView: "grid",
  // The view is chosen in the toolbar
  statusViews: [],
  // Finder's conventions: a click on a name doesn't rename it, and a new item goes to its place in the sorted list
  clickToRename: false,
  newAtEnd: false,
  // Folders in the List view have a triangle to expand them in place
  disclosure: true,
};
