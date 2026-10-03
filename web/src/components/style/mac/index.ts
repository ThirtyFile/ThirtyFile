/**
 * The Mac style: a Finder window's layout and views, with its own icons and look (./look.tsx, ./art). Its name in the
 * interface is "Mac style"; it uses no Apple artwork. Data, file operations, previews and the phone layout are the ones
 * every style shares.
 */
import type { StyleKit } from "../types";
import { MacFrame } from "./frame";
import { GalleryView } from "./gallery";
import { MAC_KEYS, macShortcuts } from "./keys";
import { MacItemIcon, macViews } from "./look";
import { macMenu } from "./menus";
import { QuickLook } from "./quickLook";
import { macToolbar } from "./toolbar";

export const macKit: StyleKit = {
  id: "mac",
  Frame: MacFrame,
  toolbar: macToolbar,
  menu: macMenu,
  Extras: QuickLook,
  keys: MAC_KEYS,
  shortcuts: macShortcuts,
  ItemIcon: MacItemIcon,
  views: macViews,
  defaultView: "grid",
  // The view is chosen in the toolbar
  statusViews: [],
  // Finder's conventions: a click on a name doesn't rename it, and a new item goes to its place in the sorted list
  clickToRename: false,
  newAtEnd: false,
  // Folders in the List view have a triangle to expand them in place
  disclosure: true,
  list: { rowHeight: 19, thumbnails: true, longDates: true, sortAtEnd: true },
  // A large preview of the item selected, above a strip of thumbnails
  ownViews: { gallery: GalleryView },
};
