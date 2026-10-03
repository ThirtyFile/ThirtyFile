/** The filters a search offers (the search page) and a smart folder keeps (lib/smartFolders.ts) */
import type { SearchFilter } from "@/api";
import { t } from "@/lib/i18n";

/** Types to filter by: extensions, files or folders */
export const SEARCH_TYPES: { id: string; label: () => string; filter: SearchFilter }[] = [
  { id: "folder", label: () => t("Folders"), filter: { kind: "folder" } },
  { id: "doc", label: () => t("Documents"), filter: { ext: "doc,docx,odt,rtf,pdf,txt,md" } },
  { id: "sheet", label: () => t("Spreadsheets"), filter: { ext: "xls,xlsx,xlsm,ods,csv,tsv" } },
  { id: "slides", label: () => t("Presentations"), filter: { ext: "ppt,pptx,odp" } },
  { id: "image", label: () => t("Pictures"), filter: { ext: "jpg,jpeg,png,gif,webp,bmp,heic,heif,tif,tiff,svg" } },
  { id: "video", label: () => t("Videos"), filter: { ext: "mp4,mov,m4v,mkv,avi,webm,wmv" } },
  { id: "audio", label: () => t("Music and sound"), filter: { ext: "mp3,wav,flac,m4a,aac,ogg,wma" } },
  { id: "archive", label: () => t("Compressed archives"), filter: { ext: "zip,rar,7z,tar,gz" } },
];

export const DAY = 86400;

/** Modified in the last days */
export const SEARCH_DATES: { id: string; label: () => string; days: number }[] = [
  { id: "today", label: () => t("Today"), days: 1 },
  { id: "week", label: () => t("Last 7 days"), days: 7 },
  { id: "month", label: () => t("Last 30 days"), days: 30 },
  { id: "year", label: () => t("Last year"), days: 365 },
];

export const MB = 1024 * 1024;

export const SEARCH_SIZES: { id: string; label: () => string; filter: SearchFilter }[] = [
  { id: "small", label: () => t("Smaller than 1 MB"), filter: { max_size: MB - 1 } },
  { id: "medium", label: () => t("1 to 100 MB"), filter: { min_size: MB, max_size: 100 * MB } },
  { id: "large", label: () => t("Larger than 100 MB"), filter: { min_size: 100 * MB + 1 } },
];
