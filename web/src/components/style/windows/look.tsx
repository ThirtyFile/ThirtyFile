/** The Windows style's icons for items, and the views of the file list it offers (File Explorer's) */
import {
  Columns4Icon,
  FileArchiveIcon,
  FileAudioIcon,
  FileCodeIcon,
  FileIcon,
  FileImageIcon,
  FileSpreadsheetIcon,
  FileTextIcon,
  FileTypeIcon,
  FileVideoIcon,
  FolderIcon,
  Grid2X2Icon,
  Grid3X3Icon,
  LayoutGridIcon,
  LayoutListIcon,
  ListIcon,
  PresentationIcon,
  Table2Icon,
  type LucideIcon,
} from "lucide-react";
import { TypeMark } from "@/components/FileIcon";
import { t, tc } from "@/lib/i18n";
import { cn } from "@/lib/utils";
import type { ItemIconProps, ViewChoice } from "../types";

/** The icon of each general kind of file; `table` is for CSV and TSV files */
export const WINDOWS_ICONS: Record<ItemIconProps["kind"], { Icon: LucideIcon; color: string }> = {
  folder: { Icon: FolderIcon, color: "text-[#d8b66c] fill-[#d8b66c]/25" },
  markdown: { Icon: FileCodeIcon, color: "text-[#6f86f0] dark:text-[#9aabf7]" },
  pdf: { Icon: FileTypeIcon, color: "text-[#e05d5d] dark:text-[#ed8585]" },
  word: { Icon: FileTextIcon, color: "text-[#3f86e0] dark:text-[#77acf2]" },
  sheet: { Icon: FileSpreadsheetIcon, color: "text-[#35a26c] dark:text-[#71c69c]" },
  table: { Icon: Table2Icon, color: "text-[#35a26c] dark:text-[#71c69c]" },
  slides: { Icon: PresentationIcon, color: "text-[#dc7a3c] dark:text-[#e9a071]" },
  image: { Icon: FileImageIcon, color: "text-[#9a63d8] dark:text-[#bd94ed]" },
  audio: { Icon: FileAudioIcon, color: "text-[#c9559a] dark:text-[#de8abd]" },
  video: { Icon: FileVideoIcon, color: "text-[#7b69da] dark:text-[#a99aee]" },
  archive: { Icon: FileArchiveIcon, color: "text-[#b58f35] dark:text-[#d8b66c]" },
  code: { Icon: FileCodeIcon, color: "text-[#2e9ea8] dark:text-[#71c7cf]" },
  text: { Icon: FileTextIcon, color: "text-muted-foreground" },
  other: { Icon: FileIcon, color: "text-muted-foreground" },
};

/** An item's icon in the Windows style: its specific type's mark when it has one, else its kind's outline */
export function WindowsItemIcon({ kind, type, className }: ItemIconProps) {
  if (type) return <TypeMark type={type} className={className} />;
  const { Icon, color } = WINDOWS_ICONS[kind];
  return <Icon className={cn("shrink-0", color, className)} strokeWidth={1.7} aria-hidden="true" />;
}

export function windowsViews(): ViewChoice[] {
  return [
    { id: "grid", Icon: Grid2X2Icon, label: t("Large icons") },
    { id: "medium", Icon: Grid3X3Icon, label: t("Medium icons") },
    { id: "compact", Icon: LayoutListIcon, label: t("List") },
    { id: "list", Icon: ListIcon, label: t("Details") },
    { id: "tiles", Icon: LayoutGridIcon, label: t("Tiles") },
    { id: "columns", Icon: Columns4Icon, label: tc("view", "Columns"), notOnPhones: true },
  ];
}
