/**
 * The Mac style's icons for folders and files, drawn for ThirtyFile (no Apple artwork): a two-tone folder, and a page
 * with a folded corner. On the page goes what tells the file apart: its format's label on a coloured band ("RS",
 * "PDF", "DOCX"), its format's symbol in the format's colour (lib/fileTypes.ts, lucide's ISC-licensed symbols), or a
 * picture of its kind (a photo, a note, a film, a zipper, lines of text, code). The page keeps its light paper in the
 * dark theme too, as documents do on a Mac, so the colours on it are the light theme's.
 */
import type { ReactNode } from "react";
import type { LucideIcon } from "lucide-react";
import { cn } from "@/lib/utils";
import type { ItemIconProps } from "../../types";

/** What a kind of file shows on its page, when it has no specific type */
export type KindArt = "image" | "audio" | "video" | "archive" | "code" | "text" | "blank";

/** What an item's icon shows */
export type MacArt =
  | { shape: "folder" }
  | { shape: "page"; band: { label: string; bg: string } }
  | { shape: "page"; glyph: LucideIcon; color: string }
  | { shape: "page"; picture: KindArt; label?: string };

/** The colour of a lucide symbol's class ("text-[#e05d5d] dark:text-[#ed8585]"): its light theme's, or a neutral grey */
export function glyphColor(classes: string): string {
  return /text-\[(#[0-9a-f]{6})\]/i.exec(classes)?.[1] ?? NEUTRAL;
}

const NEUTRAL = "#6b717c";

/** Bands of the general kinds of documents (their extension, else this label), in colours of their own */
const KIND_BANDS = {
  pdf: { label: "PDF", bg: "#d93a30" },
  word: { label: "DOC", bg: "#2b6fd6" },
  sheet: { label: "XLS", bg: "#1f8f4e" },
  table: { label: "CSV", bg: "#16856f" },
  slides: { label: "PPT", bg: "#d4622a" },
  markdown: { label: "MD", bg: "#4b5563" },
} as const;

/** An extension short enough to go on a page (1 to 4 letters or digits) */
const shortExt = (ext: string) => (/^[a-z0-9]{1,4}$/i.test(ext) ? ext.toUpperCase() : undefined);

/** What the icon of an item shows: its kind, its specific type (lib/fileTypes.ts) when known, and its extension */
export function macArtFor({ kind, type, ext }: Pick<ItemIconProps, "kind" | "type" | "ext">): MacArt {
  if (kind === "folder") return { shape: "folder" };
  if (type) {
    const m = type.mark;
    return "icon" in m ? { shape: "page", glyph: m.icon, color: glyphColor(m.color) } : { shape: "page", band: { label: m.label, bg: m.bg } };
  }
  switch (kind) {
    case "pdf":
    case "word":
    case "sheet":
    case "table":
    case "slides":
    case "markdown": {
      const band = KIND_BANDS[kind];
      return { shape: "page", band: { label: (kind !== "markdown" && shortExt(ext)) || band.label, bg: band.bg } };
    }
    case "image":
    case "audio":
    case "video":
    case "archive":
    case "code":
    case "text":
      return { shape: "page", picture: kind };
    case "other":
      return { shape: "page", picture: "blank", label: shortExt(ext) };
  }
}

/** Whether dark or white text reads better on a colour */
export function inkOn(hex: string) {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.55 ? "#1a1a1a" : "#ffffff";
}

// ───────────── Drawings (on a 32-unit grid) ─────────────

const PAGE = "M8.5 3.5h10.3a1.6 1.6 0 0 1 1.13.47l5.6 5.6a1.6 1.6 0 0 1 .47 1.13V26.5a2 2 0 0 1-2 2h-15.5a2 2 0 0 1-2-2v-21a2 2 0 0 1 2-2z";
const FOLD = "M19 3.6v5.4a1.5 1.5 0 0 0 1.5 1.5h5.4z";

function Folder() {
  return (
    <>
      <path d="M2.5 8a2.5 2.5 0 0 1 2.5-2.5h6.1a2 2 0 0 1 1.45.62l1.55 1.63H27A2.5 2.5 0 0 1 29.5 10.25V25A2.5 2.5 0 0 1 27 27.5H5A2.5 2.5 0 0 1 2.5 25z" fill="var(--mac-folder-back)" />
      <path d="M2.5 12.6a2 2 0 0 1 2-2h23a2 2 0 0 1 2 2V25a2.5 2.5 0 0 1-2.5 2.5H5A2.5 2.5 0 0 1 2.5 25z" fill="var(--mac-folder-front)" />
      <path d="M4.6 11.5h22.8" stroke="var(--mac-folder-line)" strokeWidth={0.9} strokeLinecap="round" />
    </>
  );
}

function Page({ children }: { children?: ReactNode }) {
  return (
    <>
      <path d={PAGE} fill="var(--mac-page)" stroke="var(--mac-page-edge)" strokeWidth={0.9} strokeLinejoin="round" />
      <path d={FOLD} fill="var(--mac-page-fold)" stroke="var(--mac-page-edge)" strokeWidth={0.9} strokeLinejoin="round" />
      {children}
    </>
  );
}

/** A label on a band across the lower part of the page */
function Band({ label, bg }: { label: string; bg: string }) {
  const n = label.length;
  const size = n <= 2 ? 6.4 : n === 3 ? 5.6 : 4.6;
  return (
    <>
      <rect x={8} y={17.5} width={16} height={7.6} rx={1.8} fill={bg} />
      <text
        x={16}
        y={21.45}
        textAnchor="middle"
        dominantBaseline="central"
        fill={inkOn(bg)}
        fontSize={size}
        fontWeight={700}
        fontFamily="ui-sans-serif, system-ui, sans-serif"
        letterSpacing={-0.15}
      >
        {label}
      </text>
    </>
  );
}

/** Pictures of the general kinds, centred on the page's lower part */
function Picture({ kind, label }: { kind: KindArt; label?: string }) {
  switch (kind) {
    case "image":
      return (
        <>
          <rect x={8.6} y={12.6} width={14.8} height={11.4} rx={1.6} fill="#dbeafb" />
          <circle cx={19.6} cy={15.9} r={1.7} fill="#f4b740" />
          <path d="M8.6 22.4 13 17.8l3.3 3.3 2.5-2.3 4.6 3.6a1.6 1.6 0 0 1-1.6 1.6H10.2a1.6 1.6 0 0 1-1.6-1.6z" fill="#3e9a5c" />
        </>
      );
    case "audio":
      return (
        <g fill="#d6457e">
          <ellipse cx={12.6} cy={22.2} rx={2.3} ry={1.8} transform="rotate(-20 12.6 22.2)" />
          <ellipse cx={19.8} cy={20.6} rx={2.3} ry={1.8} transform="rotate(-20 19.8 20.6)" />
          <rect x={14.1} y={14.2} width={1.3} height={8} />
          <rect x={21.3} y={12.6} width={1.3} height={8} />
          <path d="M14.1 14.2 22.6 12.3v2.4l-8.5 1.9z" />
        </g>
      );
    case "video":
      return (
        <>
          <rect x={8.6} y={13} width={14.8} height={11} rx={2} fill="#3a3d48" />
          <path d="M14.2 15.8v5.4a.6.6 0 0 0 .9.52l4.5-2.7a.6.6 0 0 0 0-1.04l-4.5-2.7a.6.6 0 0 0-.9.52z" fill="#ffffff" />
        </>
      );
    case "archive":
      return (
        <>
          {[5.2, 8.2, 11.2, 14.2].map((y) => (
            <rect key={y} x={y % 6 < 3 ? 14.4 : 16} y={y} width={1.6} height={1.6} rx={0.3} fill="#7d838d" />
          ))}
          <rect x={13.6} y={17} width={4.8} height={6.4} rx={1.2} fill="#7d838d" />
          <rect x={14.9} y={20} width={2.2} height={1.8} rx={0.5} fill="var(--mac-page)" />
        </>
      );
    case "code":
      return <path d="M13.2 14.4l-3.6 3.6 3.6 3.6M18.8 14.4l3.6 3.6-3.6 3.6M17 13.2l-2 9.6" fill="none" stroke="#56627a" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round" />;
    case "text":
      return <path d="M10 13.2h12M10 16.4h12M10 19.6h12M10 22.8h7.5" fill="none" stroke="#9aa1ad" strokeWidth={1.2} strokeLinecap="round" />;
    case "blank":
      return label ? (
        <text
          x={16}
          y={22.6}
          textAnchor="middle"
          dominantBaseline="central"
          fill="var(--mac-page-ink)"
          fontSize={label.length <= 3 ? 5.4 : 4.6}
          fontWeight={600}
          fontFamily="ui-sans-serif, system-ui, sans-serif"
        >
          {label}
        </text>
      ) : null;
  }
}

/** An item's icon in the Mac style */
export function MacItemIcon({ kind, type, ext, className }: ItemIconProps) {
  const art = macArtFor({ kind, type, ext });
  return (
    <svg viewBox="0 0 32 32" className={cn("shrink-0", className)} aria-hidden="true" data-art="mac" data-kind={kind} data-type={type?.id}>
      {art.shape === "folder" ? (
        <Folder />
      ) : (
        <Page>
          {"band" in art && <Band {...art.band} />}
          {"glyph" in art && <art.glyph x={8.5} y={11.5} width={15} height={15} color={art.color} strokeWidth={2.3} aria-hidden />}
          {"picture" in art && <Picture kind={art.picture} label={art.label} />}
        </Page>
      )}
    </svg>
  );
}
