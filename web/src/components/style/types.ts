/**
 * What an interface style provides (lib/style): the parts of the explorer that differ between styles. Everything else
 * (the data, the selection, file operations, previews, uploads, background tasks, loading large folders) is shared, and
 * doesn't ask which style is in use.
 */
import type { ComponentType, ReactNode } from "react";
import type { LucideIcon } from "lucide-react";
import type { ExplorerProps } from "@/components/Explorer";
import type { ExplorerActions } from "@/components/explorer/actions";
import type { MenuEntries } from "@/components/explorer/menus";
import type { ExplorerState } from "@/components/explorer/state";
import type { FileCategory } from "@/components/FileIcon";
import type { ViewMode } from "@/components/fileList/layout";
import type { Style } from "@/lib/style";
import type { Action, KeyMap } from "@/lib/style/keymap";

/** The id of the element holding `content` in every style's frame: the target of "Skip to main content" (AppShell) */
export const MAIN_ID = "tf-main";

/** The parts of a page's frame (components/Frame.tsx), for the style to place */
export interface FrameParts {
  /** Where the path is shown, with back, forward and up, and the search box */
  pathBar: ReactNode;
  /** The page's commands */
  toolbar: ReactNode;
  /** The locations: the folder tree, Recent, Favorites, the trash… */
  nav: ReactNode;
  /** On narrow screens the locations are a panel, opened from the toolbar */
  navOpen: boolean;
  setNavOpen(open: boolean): void;
  /** The page itself: the file list, a list of share links… */
  content: ReactNode;
  /** The status line: what the page says about its items, how much of the space is used, and the page's own controls */
  status: ReactNode;
  used: ReactNode;
  statusEnd: ReactNode;
}

/** A row of the shortcuts dialog: the keys of `actions`, and what they do */
export interface ShortcutRow {
  actions: readonly Action[];
  label: string;
}

export interface ShortcutGroup {
  title: string;
  rows: readonly ShortcutRow[];
}

/** A view of the file list the style offers */
export interface ViewChoice {
  id: ViewMode;
  Icon: LucideIcon;
  label: string;
}

/** The icon of each kind of file; `table` is for CSV and TSV files */
export type IconSet = Record<FileCategory | "table", { Icon: LucideIcon; color: string }>;

export interface StyleKit {
  id: Style;
  /** Places the frame's parts */
  Frame: ComponentType<FrameParts>;
  /** The explorer's command bar; `newItems` is what its New menu holds */
  toolbar(p: ExplorerProps, s: ExplorerState, a: ExplorerActions, newItems: ReactNode): ReactNode;
  /** The explorer's context menu (on items, or on empty space when nothing is selected): its order and labels */
  menu(m: MenuEntries): ReactNode;
  /** The keys of every action */
  keys: KeyMap;
  /** The shortcuts dialog: what it says first, and its groups of rows */
  shortcuts(): { note: string; groups: readonly ShortcutGroup[] };
  icons: IconSet;
  /** The views of the file list, in the order offered */
  views(): readonly ViewChoice[];
  /** The view a list starts in, and the one used when a view kept from before isn't offered */
  defaultView: ViewMode;
  /** The views with a button of their own at the end of the status line */
  statusViews: readonly ViewMode[];
  /** Clicking the name of the item already selected on its own renames it (lib/clickToRename), in the list and the tree */
  clickToRename: boolean;
  /**
   * A new folder or text document shows at once, in rename mode, at the end of the list while the server makes it, and
   * stays there until a refresh, a change of sort or leaving the folder (components/explorer/newItems)
   */
  newAtEnd: boolean;
}
