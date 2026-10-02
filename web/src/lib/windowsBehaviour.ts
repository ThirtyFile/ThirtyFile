/**
 * File Explorer conventions the Windows style follows, kept in one place so another style (the Mac style) can turn them
 * off rather than finding them scattered through the explorer.
 */
export interface WindowsBehaviour {
  /** Windows 11 context menus: View, Sort by, Group by, New and Upload as submenus on empty space; a row of icon buttons on items */
  menus: boolean;
}

const WINDOWS: WindowsBehaviour = { menus: true };

/** The conventions the explorer follows: the Windows style's (the only style so far) */
export function useWindowsBehaviour(): WindowsBehaviour {
  return WINDOWS;
}
