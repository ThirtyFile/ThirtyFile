import {
  FileArchiveIcon,
  FileAudioIcon,
  FileCodeIcon,
  FileIcon as GenericFileIcon,
  FileImageIcon,
  FileSpreadsheetIcon,
  FileTextIcon,
  FileTypeIcon,
  FileVideoIcon,
  FolderIcon,
  PresentationIcon,
  Table2Icon,
  type LucideIcon,
} from "lucide-react";
import type { Node } from "@/api";
import { t } from "@/lib/i18n";
import { cn, extOf } from "@/lib/utils";

export type FileCategory =
  | "folder"
  | "markdown"
  | "image"
  | "video"
  | "audio"
  | "pdf"
  | "word"
  | "sheet"
  | "slides"
  | "code"
  | "text"
  | "archive"
  | "other";

type NodeLike = Pick<Node, "kind" | "mime" | "name">;

export function categoryOf(n: NodeLike): FileCategory {
  if (n.kind === "folder") return "folder";
  const ext = extOf(n.name);
  const mime = n.mime.toLowerCase();
  if (/^(md|markdown|mdx)$/.test(ext) || mime === "text/markdown") return "markdown";
  if (ext === "pdf" || mime === "application/pdf") return "pdf";
  if (/^(doc|docx|odt|rtf)$/.test(ext)) return "word";
  if (/^(csv|tsv|xls|xlsx|xlsm|xlsb|ods)$/.test(ext)) return "sheet";
  if (/^(ppt|pptx|pptm|odp|key)$/.test(ext)) return "slides";
  if (mime.startsWith("image/") || /^(png|jpe?g|gif|webp|svg|bmp|ico|tiff?|avif|heic)$/.test(ext)) return "image";
  if (mime.startsWith("audio/") || /^(mp3|wav|ogg|flac|aac|m4a)$/.test(ext)) return "audio";
  if (mime.startsWith("video/") || /^(mp4|webm|mov|avi|mkv)$/.test(ext)) return "video";
  if (/^(zip|rar|7z|tar|gz|tgz|bz2|xz)$/.test(ext)) return "archive";
  if (
    /^(json|html?|xml|ya?ml|js|jsx|ts|tsx|css|scss|py|sh|ps1|bat|sql|toml|rs|go|java|c|h|cpp|cs|php|rb|kt|swift|vue|svelte|ini|conf|env)$/.test(ext) ||
    /json|xml|javascript/.test(mime)
  )
    return "code";
  if (mime.startsWith("text/") || /^(txt|log)$/.test(ext)) return "text";
  return "other";
}

const STYLE: Record<FileCategory, { Icon: LucideIcon; color: string; title: string }> = {
  folder: { Icon: FolderIcon, color: "text-[#d8b66c] fill-[#d8b66c]/25", title: t("Folder") },
  markdown: { Icon: FileCodeIcon, color: "text-[#6f86f0] dark:text-[#9aabf7]", title: t("Markdown document") },
  pdf: { Icon: FileTypeIcon, color: "text-[#e05d5d] dark:text-[#ed8585]", title: t("PDF document") },
  word: { Icon: FileTextIcon, color: "text-[#3f86e0] dark:text-[#77acf2]", title: t("Document") },
  sheet: { Icon: FileSpreadsheetIcon, color: "text-[#35a26c] dark:text-[#71c69c]", title: t("Spreadsheet") },
  slides: { Icon: PresentationIcon, color: "text-[#dc7a3c] dark:text-[#e9a071]", title: t("Presentation") },
  image: { Icon: FileImageIcon, color: "text-[#9a63d8] dark:text-[#bd94ed]", title: t("Image") },
  audio: { Icon: FileAudioIcon, color: "text-[#c9559a] dark:text-[#de8abd]", title: t("Audio") },
  video: { Icon: FileVideoIcon, color: "text-[#7b69da] dark:text-[#a99aee]", title: t("Video") },
  archive: { Icon: FileArchiveIcon, color: "text-[#b58f35] dark:text-[#d8b66c]", title: t("Compressed archive") },
  code: { Icon: FileCodeIcon, color: "text-[#2e9ea8] dark:text-[#71c7cf]", title: t("Code or data file") },
  text: { Icon: FileTextIcon, color: "text-muted-foreground", title: t("Text file") },
  other: { Icon: GenericFileIcon, color: "text-muted-foreground", title: t("File") },
};

/** Text shown in the Type column: uppercase extension; folders show "Folder" */
export function typeLabel(n: NodeLike) {
  if (n.kind === "folder") return t("File folder");
  const ext = extOf(n.name).slice(0, 8).toUpperCase();
  return ext ? t("{ext} File", { ext }) : t("File");
}

export function typeTitle(n: NodeLike) {
  return STYLE[categoryOf(n)].title;
}

export function FileIcon({ node, className }: { node: NodeLike; className?: string }) {
  const c = categoryOf(node);
  const { Icon, color } = c === "sheet" && /^(csv|tsv)$/.test(extOf(node.name)) ? { ...STYLE.sheet, Icon: Table2Icon } : STYLE[c];
  return <Icon className={cn("shrink-0", color, className)} strokeWidth={1.7} aria-hidden="true" />;
}

/** Pictures and videos most browsers can't show (HEIC, TIFF, AVI, MKV…): offered for download instead of a broken preview */
const NOT_IN_BROWSER_EXT = /^(heic|heif|tiff?|psd|avi|mkv|wmv|flv|wma|aiff?|ape)$/;
const NOT_IN_BROWSER_MIME = /^(image\/(heic|heif|tiff|vnd\.adobe\.photoshop)|video\/(x-msvideo|x-matroska|x-ms-wmv|x-flv)|audio\/(x-ms-wma|x-aiff|aiff))$/;

/** A picture, video or sound the browser can show */
export function isBrowserMedia(n: NodeLike) {
  const c = categoryOf(n);
  return (c === "image" || c === "video" || c === "audio") && !NOT_IN_BROWSER_EXT.test(extOf(n.name)) && !NOT_IN_BROWSER_MIME.test(n.mime.toLowerCase());
}

export function canThumbnail(n: Node) {
  return ["image/jpeg", "image/png", "image/gif", "image/webp", "image/bmp"].includes(n.mime) && n.size <= 60 * 1024 * 1024;
}

/** Largest PDF whose thumbnail the browser makes (pdf.js reads only the parts of the file it needs, but a huge file may still be slow) */
const MAX_PDF_THUMB = 200 * 1024 * 1024;

/**
 * PDFs and videos: the server can't read them, so the browser that shows them draws the thumbnail (the first page, a
 * frame) and uploads it for everyone (lib/thumbs.ts). The same types the server takes (files.rs, browser_thumbnailable)
 */
export function canBrowserThumbnail(n: Node) {
  if (n.kind !== "file" || n.size === 0) return false;
  if (n.mime === "application/pdf") return n.size <= MAX_PDF_THUMB;
  return n.mime.startsWith("video/") && isBrowserMedia(n);
}

/** Can be opened in the text editor */
export function isTextLike(n: Node) {
  const c = categoryOf(n);
  const textual = c === "code" || c === "text" || c === "markdown" || n.mime === "image/svg+xml" || /^(csv|tsv)$/.test(extOf(n.name));
  return (textual || n.size === 0) && n.size <= 5 * 1024 * 1024;
}
