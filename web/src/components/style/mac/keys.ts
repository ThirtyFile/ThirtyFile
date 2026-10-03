/**
 * The Mac style's keys: Finder's, as far as a browser lets a page have them. "Ctrl" is ⌘ on a Mac (lib/style/keymap).
 * Keys the browser keeps for itself (⌘N, ⌘W, ⌘T, ⌘⇧N…) aren't used: New folder is ⌘⌥N rather than Finder's ⌘⇧N,
 * which opens a private window. There is no Cut: an item is moved by copying it, then "Move here" (⌘⌥V).
 */
import type { KeyMap } from "@/lib/style/keymap";
import { t } from "@/lib/i18n";
import type { ShortcutGroup } from "../types";

export const MAC_KEYS: KeyMap = {
  upFolder: ["Ctrl+↑"],
  back: ["Ctrl+["],
  forward: ["Ctrl+]"],
  // Go to folder
  addressBar: ["Ctrl+Shift+G"],
  search: ["Ctrl+F"],
  refresh: ["F5"],
  previousFile: ["←"],
  nextFile: ["→"],
  itemUp: ["↑"],
  itemDown: ["↓"],
  itemLeft: ["←"],
  itemRight: ["→"],
  previousColumn: ["←"],
  nextColumn: ["→"],
  expand: ["→"],
  collapse: ["←"],
  first: ["Home"],
  last: ["End"],
  pageUp: ["PgUp"],
  pageDown: ["PgDn"],
  // ⌘↑ and ⌘↓ go up and open: the focus moves with the selection
  focusOnly: [],
  // The arrows select; Space is Quick look
  select: [],
  toggleSelect: [],
  selectAll: ["Ctrl+A"],
  findByName: ["A–Z"],
  clearSelection: ["Esc"],
  open: ["Ctrl+↓"],
  menu: ["Shift+F10"],
  rename: ["Enter"],
  cut: [],
  copy: ["Ctrl+C"],
  paste: ["Ctrl+V"],
  moveHere: ["Ctrl+Alt+V"],
  undo: ["Ctrl+Z"],
  trash: ["Ctrl+Backspace"],
  deleteForever: ["Ctrl+Alt+Backspace"],
  newFolder: ["Ctrl+Alt+N"],
  details: ["Ctrl+I"],
  quickLook: ["Space"],
  shortcuts: ["?"],
};

/** The shortcuts dialog of the Mac style */
export function macShortcuts(): { note: string; groups: ShortcutGroup[] } {
  return {
    note: t("Like a Mac. ⌘ is Ctrl on other computers. Shortcuts don't apply while you're typing in a box."),
    groups: [
      {
        title: t("Getting around"),
        rows: [
          { actions: ["upFolder"], label: t("Up one folder, with the folder you came from selected") },
          { actions: ["back"], label: t("Back") },
          { actions: ["forward"], label: t("Forward") },
          { actions: ["addressBar"], label: t("Go to folder") },
          { actions: ["search"], label: t("Go to the search box") },
          { actions: ["refresh"], label: t("Refresh the list") },
          { actions: ["previousFile", "nextFile"], label: t("In an open file: the previous or next file of its folder") },
        ],
      },
      {
        title: t("Selecting"),
        rows: [
          {
            actions: ["itemUp", "itemDown", "first", "last", "pageUp", "pageDown"],
            label: t("Move through the list (also ← and → in the Icons and Gallery views); hold Shift to select as you go"),
          },
          { actions: ["expand", "collapse"], label: t("In the List view: expand or collapse the selected folder") },
          { actions: ["previousColumn", "nextColumn"], label: t("In the Columns view: back to the column before, or on to the column of the selected folder") },
          { actions: ["selectAll"], label: t("Select everything") },
          { actions: ["findByName"], label: t("Go to the next item whose name starts with the letters typed") },
          { actions: ["clearSelection"], label: t("Clear the selection") },
        ],
      },
      {
        title: t("Working with items"),
        rows: [
          { actions: ["open"], label: t("Open") },
          { actions: ["quickLook"], label: t("Quick look; the arrows go to the next item") },
          { actions: ["rename"], label: t("Rename") },
          { actions: ["details"], label: t("Get info") },
          { actions: ["menu"], label: t("Open the menu of the item with the focus") },
          { actions: ["copy", "paste"], label: t("Copy, paste") },
          { actions: ["moveHere"], label: t("Move the items copied here") },
          { actions: ["undo"], label: t("Undo the last move, rename or delete") },
          { actions: ["trash"], label: t("Move to trash") },
          { actions: ["deleteForever"], label: t("Delete permanently") },
          { actions: ["newFolder"], label: t("New folder") },
          { actions: ["shortcuts"], label: t("Show these shortcuts") },
        ],
      },
    ],
  };
}
