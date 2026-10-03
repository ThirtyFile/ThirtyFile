/** The Windows style's icons for each kind of file, and the views of the file list it offers (File Explorer's) */
import {
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
} from "lucide-react";
import { t } from "@/lib/i18n";
import type { IconSet, ViewChoice } from "../types";

export const WINDOWS_ICONS: IconSet = {
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

export function windowsViews(): ViewChoice[] {
  return [
    { id: "grid", Icon: Grid2X2Icon, label: t("Large icons") },
    { id: "medium", Icon: Grid3X3Icon, label: t("Medium icons") },
    { id: "compact", Icon: LayoutListIcon, label: t("List") },
    { id: "list", Icon: ListIcon, label: t("Details") },
    { id: "tiles", Icon: LayoutGridIcon, label: t("Tiles") },
  ];
}
