/** The Windows style's keys: File Explorer's, as far as a browser lets a page have them */
import type { KeyMap } from "@/lib/style/keymap";
import { t } from "@/lib/i18n";
import type { ShortcutGroup } from "../types";

export const WINDOWS_KEYS: KeyMap = {
  upFolder: ["Alt+↑"],
  back: ["Alt+←", "Backspace"],
  forward: ["Alt+→"],
  addressBar: ["Ctrl+L", "Alt+D"],
  search: ["Ctrl+F", "F3"],
  refresh: ["F5"],
  previousFile: ["←"],
  nextFile: ["→"],
  itemUp: ["↑"],
  itemDown: ["↓"],
  itemLeft: ["←"],
  itemRight: ["→"],
  previousColumn: ["←"],
  nextColumn: ["→"],
  // Unused: folders don't expand in place in this style (`disclosure`)
  expand: ["→"],
  collapse: ["←"],
  first: ["Home"],
  last: ["End"],
  pageUp: ["PgUp"],
  pageDown: ["PgDn"],
  focusOnly: ["Ctrl+↑", "Ctrl+↓"],
  select: ["Space"],
  toggleSelect: ["Ctrl+Space"],
  selectAll: ["Ctrl+A"],
  findByName: ["A–Z"],
  clearSelection: ["Esc"],
  open: ["Enter"],
  menu: ["Shift+F10"],
  rename: ["F2"],
  cut: ["Ctrl+X"],
  copy: ["Ctrl+C"],
  paste: ["Ctrl+V"],
  undo: ["Ctrl+Z"],
  trash: ["Delete"],
  deleteForever: ["Shift+Delete"],
  newFolder: ["Ctrl+Shift+N"],
  details: ["Alt+Enter"],
  // Not in this style: the details pane shows the item selected
  quickLook: [],
  shortcuts: ["?"],
};

/** The shortcuts dialog of the Windows style */
export function windowsShortcuts(): { note: string; groups: ShortcutGroup[] } {
  return {
    note: t("Like File Explorer. Shortcuts don't apply while you're typing in a box."),
    groups: [
      {
        title: t("Getting around"),
        rows: [
          { actions: ["upFolder"], label: t("Up one folder, with the folder you came from selected") },
          { actions: ["back"], label: t("Back") },
          { actions: ["forward"], label: t("Forward") },
          { actions: ["addressBar"], label: t("Go to the address bar") },
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
            label: t("Move through the list (also ← and → in the icon view); hold Shift to select as you go"),
          },
          { actions: ["previousColumn", "nextColumn"], label: t("In the Columns view: back to the column before, or on to the column of the selected folder") },
          { actions: ["focusOnly"], label: t("Move the focus without changing the selection") },
          { actions: ["select"], label: t("Select the item with the focus") },
          { actions: ["toggleSelect"], label: t("Add the item with the focus to the selection, or remove it") },
          { actions: ["selectAll"], label: t("Select everything") },
          { actions: ["findByName"], label: t("Go to the next item whose name starts with the letters typed") },
          { actions: ["clearSelection"], label: t("Clear the selection") },
        ],
      },
      {
        title: t("Working with items"),
        rows: [
          { actions: ["open"], label: t("Open") },
          { actions: ["menu"], label: t("Open the menu of the item with the focus") },
          { actions: ["rename"], label: t("Rename") },
          { actions: ["cut", "copy", "paste"], label: t("Cut, copy, paste") },
          { actions: ["undo"], label: t("Undo the last move, rename or delete") },
          { actions: ["trash"], label: t("Move to trash") },
          { actions: ["deleteForever"], label: t("Delete permanently") },
          { actions: ["newFolder"], label: t("New folder") },
          { actions: ["details"], label: t("Show details") },
          { actions: ["shortcuts"], label: t("Show these shortcuts") },
        ],
      },
    ],
  };
}
