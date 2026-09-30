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
import { fileTypeOf, isScriptNotVideo, type FileType } from "@/lib/fileTypes";
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

/** A file or folder as far as its type goes; the size, when known, tells TypeScript from video (`.ts`, `.mts`) */
type NodeLike = Pick<Node, "kind" | "mime" | "name"> & { size?: number };

/**
 * The general kind of a file, which decides what can be done with it: previews, the text editor, thumbnails. Its icon
 * can be more specific (lib/fileTypes.ts), without changing this.
 */
export function categoryOf(n: NodeLike): FileCategory {
  if (n.kind === "folder") return "folder";
  const ext = extOf(n.name);
  const mime = n.mime.toLowerCase();
  if (/^(md|markdown|mdx)$/.test(ext) || mime === "text/markdown") return "markdown";
  if (ext === "pdf" || mime === "application/pdf") return "pdf";
  // Templates and macro-enabled files are Office files too (their MIME types contain "xml"; they aren't text)
  if (/^(doc|docx|docm|dot|dotx|dotm|odt|ott|rtf)$/.test(ext)) return "word";
  if (/^(csv|tsv|xls|xlsx|xlsm|xlsb|xlt|xltx|xltm|xla|xlam|ods|ots)$/.test(ext)) return "sheet";
  if (/^(ppt|pptx|pptm|pot|potx|potm|pps|ppsx|ppsm|odp|otp|key)$/.test(ext)) return "slides";
  // Names whose extension other formats use: Go's module files aren't video (".mod"), packages aren't sound (".rpm")
  if (/(^|\/)go\.(mod|sum|work)$/i.test(n.name)) return "other";
  if (ext === "rpm") return "archive";
  // A small .ts / .mts file is TypeScript, not an MPEG transport stream (both get the video MIME type)
  if (isScriptNotVideo(n)) return "code";
  if (mime.startsWith("image/") || /^(png|jpe?g|gif|webp|svg|bmp|ico|tiff?|avif|heic)$/.test(ext)) return "image";
  if (mime.startsWith("audio/") || /^(mp3|wav|ogg|opus|flac|aac|m4a|wma|aiff?|ape|amr)$/.test(ext)) return "audio";
  if (mime.startsWith("video/") || /^(mp4|webm|mov|avi|mkv)$/.test(ext)) return "video";
  if (/^(zip|rar|7z|tar|gz|tgz|bz2|xz)$/.test(ext)) return "archive";
  if (
    /^(json|html?|xml|ya?ml|js|jsx|ts|tsx|css|scss|py|sh|ps1|bat|sql|toml|rs|go|java|c|h|cpp|cs|php|rb|kt|swift|vue|svelte|ini|conf|env)$/.test(ext) ||
    (/json|xml|javascript/.test(mime) && !/openxmlformats|vnd\.ms-|vnd\.oasis/.test(mime))
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

/** The specific kind of a file, when it has one (a format, a language, a tool's file) */
function specificType(n: NodeLike): FileType | "archive" | null {
  return n.kind === "folder" ? null : fileTypeOf(n);
}

/** What kind of file it is, in words ("Rust source file", "Spreadsheet") */
export function typeTitle(n: NodeLike) {
  const type = specificType(n);
  if (type && type !== "archive") return type.title;
  return STYLE[type === "archive" ? "archive" : categoryOf(n)].title;
}

export function FileIcon({ node, className }: { node: NodeLike; className?: string }) {
  const type = specificType(node);
  if (type && type !== "archive") return <TypeMark type={type} className={className} />;
  const c = type === "archive" ? "archive" : categoryOf(node);
  const { Icon, color } = c === "sheet" && /^(csv|tsv)$/.test(extOf(node.name)) ? { ...STYLE.sheet, Icon: Table2Icon } : STYLE[c];
  return <Icon className={cn("shrink-0", color, className)} strokeWidth={1.7} aria-hidden="true" />;
}

/** Whether white or black text reads better on a colour */
function textOn(hex: string) {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.55 ? "#1a1a1a" : "#ffffff";
}

/**
 * A specific type's icon: its symbol, or a page with its label ("RS", "GO"), cut out from the page so it reads on light
 * and dark backgrounds alike. The page is lucide's file outline (ISC licence), like the other file icons.
 */
function TypeMark({ type, className }: { type: FileType; className?: string }) {
  const m = type.mark;
  if ("icon" in m) {
    const Icon = m.icon;
    return <Icon className={cn("shrink-0", m.color, className)} strokeWidth={1.7} aria-hidden="true" data-type={type.id} />;
  }
  const n = m.label.length;
  const width = Math.min(22, 7 + n * 4.6);
  const size = n <= 2 ? 7.4 : n === 3 ? 6.4 : 5.4;
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.7}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={cn("shrink-0 text-muted-foreground", className)}
      aria-hidden="true"
      data-type={type.id}
    >
      <path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" />
      <path d="M14 2v5a1 1 0 0 0 1 1h5" />
      <rect x={12 - width / 2} y={11.5} width={width} height={9} rx={2} fill={m.bg} stroke="var(--background)" strokeWidth={1.2} />
      <text
        x={12}
        y={16.1}
        textAnchor="middle"
        dominantBaseline="central"
        fill={textOn(m.bg)}
        stroke="none"
        fontSize={size}
        fontWeight={700}
        fontFamily="ui-sans-serif, system-ui, sans-serif"
        letterSpacing={-0.2}
      >
        {m.label}
      </text>
    </svg>
  );
}

/** Pictures and videos most browsers can't show (HEIC, TIFF, AVI, MKV…): offered for download instead of a broken preview */
const NOT_IN_BROWSER_EXT = /^(heic|heif|tiff?|psd|avi|mkv|wmv|flv|wma|aiff?|ape|amr)$/;
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

/** A ZIP archive the server can extract (archive.rs, is_zip) */
export function isZip(n: NodeLike) {
  return n.kind === "file" && (extOf(n.name) === "zip" || n.mime === "application/zip" || n.mime === "application/x-zip-compressed");
}

/** Can be opened in the text editor */
export function isTextLike(n: Node) {
  const c = categoryOf(n);
  const textual = c === "code" || c === "text" || c === "markdown" || n.mime === "image/svg+xml" || /^(csv|tsv)$/.test(extOf(n.name));
  return (textual || n.size === 0) && n.size <= 5 * 1024 * 1024;
}
