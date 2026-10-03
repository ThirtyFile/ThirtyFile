/**
 * The Mac style's look: its icons for items and its symbols for the sidebar and toolbar, which come with its own assets
 * (./art, loaded only with the style: ./loadArt.ts); and the views of the file list it offers: Icons, List (with
 * folders that expand in place), Columns and Gallery.
 */
import {
  ArrowDownUpIcon,
  BuildingIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  ClockIcon,
  Columns3Icon,
  DatabaseIcon,
  EllipsisIcon,
  FolderIcon,
  FolderSearchIcon,
  GalleryThumbnailsIcon,
  LayersIcon,
  LayoutGridIcon,
  Link2Icon,
  ListIcon,
  SettingsIcon,
  Share2Icon,
  StarIcon,
  Trash2Icon,
  UserIcon,
  UsersRoundIcon,
  type LucideIcon,
} from "lucide-react";
import { t, tc } from "@/lib/i18n";
import { cn } from "@/lib/utils";
import type { ItemIconProps, ViewChoice } from "../types";
import { WindowsItemIcon } from "../windows/look";
import type { MacSymbol } from "./art/symbols";
import { macArt, useMacArt } from "./loadArt";

/** An item's icon in the Mac style */
export function MacItemIcon(p: ItemIconProps) {
  const art = useMacArt();
  if (art) return <art.MacItemIcon {...p} />;
  // While the assets load, the icon's place; if they couldn't load, the shared icons
  if (art === undefined) return <span aria-hidden className={cn("inline-block shrink-0", p.className)} />;
  return <WindowsItemIcon {...p} />;
}

/** The shared symbols, used when the Mac style's assets couldn't load */
const SHARED: Record<MacSymbol, LucideIcon> = {
  favorites: StarIcon,
  spaces: LayersIcon,
  personal: UserIcon,
  company: BuildingIcon,
  team: DatabaseIcon,
  folder: FolderIcon,
  smartFolder: FolderSearchIcon,
  sharedWithMe: UsersRoundIcon,
  shareLinks: Link2Icon,
  recent: ClockIcon,
  trash: Trash2Icon,
  controlPanel: SettingsIcon,
  back: ChevronLeftIcon,
  forward: ChevronRightIcon,
  viewIcons: LayoutGridIcon,
  viewList: ListIcon,
  viewColumns: Columns3Icon,
  viewGallery: GalleryThumbnailsIcon,
  sort: ArrowDownUpIcon,
  share: Share2Icon,
  actions: EllipsisIcon,
};

/** The symbols of the sidebar and the toolbar, as loaded now (the frame shows once they are: ./frame.tsx) */
export function macSymbols(): Record<MacSymbol, LucideIcon> {
  return macArt()?.MAC_SYMBOLS ?? SHARED;
}

/** The symbols of the sidebar and the toolbar */
export function useMacSymbols(): Record<MacSymbol, LucideIcon> {
  return useMacArt()?.MAC_SYMBOLS ?? SHARED;
}

export function macViews(): ViewChoice[] {
  const s = macSymbols();
  return [
    { id: "grid", Icon: s.viewIcons, label: t("Icons") },
    { id: "list", Icon: s.viewList, label: t("List") },
    { id: "columns", Icon: s.viewColumns, label: tc("view", "Columns"), notOnPhones: true },
    // A large preview above a strip of thumbnails: too little room for both on a phone
    { id: "gallery", Icon: s.viewGallery, label: t("Gallery"), notOnPhones: true },
  ];
}
