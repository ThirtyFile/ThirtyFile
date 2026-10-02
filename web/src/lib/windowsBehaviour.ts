/**
 * File Explorer conventions the Windows style follows, kept in one place so another style (the Mac style) can turn them
 * off rather than finding them scattered through the explorer.
 */
export interface WindowsBehaviour {
  /** Windows 11 context menus: View, Sort by, Group by, New and Upload as submenus on empty space; a row of icon buttons on items */
  menus: boolean;
  /** Clicking the name of the item already selected on its own renames it, in the list and the folder tree (lib/clickToRename) */
  clickToRename: boolean;
  /**
   * A new folder or text document shows at once, in rename mode, at the end of the list while the server makes it, and
   * stays there until a refresh, a change of sort or leaving the folder (components/explorer/newItems)
   */
  newAtEnd: boolean;
}

const WINDOWS: WindowsBehaviour = { menus: true, clickToRename: true, newAtEnd: true };

/** The conventions the explorer follows: the Windows style's (the only style so far) */
export function useWindowsBehaviour(): WindowsBehaviour {
  return WINDOWS;
}
