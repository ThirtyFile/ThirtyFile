import { Suspense, lazy, useEffect, useEffectEvent, useState, type ReactNode } from "react";
import { ArrowLeftIcon, DownloadIcon, FileTextIcon, Loader2Icon } from "lucide-react";
import type { FileSource, Node } from "@/api";
import { triggerDownload } from "@/downloads";
import { Button } from "@/components/ui/button";
import { FileIcon, MAX_TEXT_BYTES, categoryOf, isBrowserMedia, isTextLike, mayOpenAsText } from "@/components/FileIcon";
import { hasDraft } from "@/lib/drafts";
import { t } from "@/lib/i18n";
import { reportShown } from "@/lib/errorReport";
import { cn, extOf, formatBytes } from "@/lib/utils";
import { MAX_OFFICE_PREVIEW_BYTES } from "@/lib/officeLimits";

const TextEditor = lazy(() => import("@/components/TextEditor"));
const OfficeViewer = lazy(() => import("@/components/OfficeViewer"));
const MarkdownPreview = lazy(() => import("@/components/MarkdownPreview"));

/** Office files that can be previewed (.docx / .xlsx / .pptx; legacy formats need downloading) */
export function isOfficePreviewable(n: Node) {
  return ["docx", "xlsx", "pptx"].includes(extOf(n.name)) && n.size <= MAX_OFFICE_PREVIEW_BYTES;
}

export function canPreview(n: Node) {
  return isBrowserMedia(n) || categoryOf(n) === "pdf" || isTextLike(n) || isOfficePreviewable(n);
}

/** Show content by file type: image, video, audio, PDF, text editor, or a can't-preview notice */
export function FileViewer(props: {
  node: Node;
  source: FileSource;
  editable: boolean;
  allowDownload?: boolean;
  /** Embedded in a tab (light background, fills the area); otherwise a black floating preview */
  embedded?: boolean;
  /** Video and audio start playing when shown (the default); the Gallery view, which shows each item passed, says no */
  autoPlay?: boolean;
  onSaved?(n: Node): void;
  onDirtyChange?(dirty: boolean): void;
}) {
  const { node, embedded } = props;
  const cat = categoryOf(node);
  const url = props.source.contentUrl(node);

  // key: a new element (and a fresh error state) for each file
  if (isBrowserMedia(node)) return <Media key={node.id} node={node} url={url} source={props.source} allowDownload={props.allowDownload} autoPlay={props.autoPlay !== false} />;
  if (cat === "pdf") return <iframe key={node.id} src={props.source.viewUrl(node)} title={node.name} className={cn("size-full bg-white", !embedded && "max-w-5xl rounded-lg")} />;
  if (isOfficePreviewable(node))
    return (
      <div className={cn("size-full overflow-hidden", !embedded && "max-w-6xl rounded-lg")}>
        <Suspense fallback={<Loader2Icon className="m-auto size-6 animate-spin text-muted-foreground" />}>
          <OfficeViewer node={node} source={props.source} />
        </Suspense>
      </div>
    );
  if (isTextLike(node) && cat === "markdown") return <MarkdownFile key={node.id} {...props} />;
  if (isTextLike(node))
    return (
      <Suspense fallback={<Loader2Icon className={cn("size-6 animate-spin", embedded ? "text-muted-foreground" : "text-white/70")} />}>
        <TextEditor key={node.id} node={node} source={props.source} editable={props.editable} embedded={embedded} onSaved={props.onSaved} onDirtyChange={props.onDirtyChange} />
      </Suspense>
    );
  return <OtherFile key={node.id} {...props} />;
}

/**
 * A file without a preview: its name, size and Download, and for a file that may be text (no extension, one ThirtyFile
 * doesn't know, text too large to open by itself), a way to open it in the text editor. That doesn't change the file:
 * it is read as text, and saved only by Save, with the same checks as any text file (write permission, a newer version
 * saved meanwhile, an encoding the editor can't write). A link that only allows viewing isn't offered more than it
 * offered before. Keyed by file, so each file opens in its usual view; unsaved edits reopen in the editor.
 */
function OtherFile(props: Parameters<typeof FileViewer>[0]) {
  const { node, embedded } = props;
  const [asText, setAsText] = useState(() => hasDraft(node.id));
  // The editor reports its own unsaved changes; the plain view has none
  const reportClean = useEffectEvent(() => props.onDirtyChange?.(false));
  useEffect(() => {
    if (!asText) reportClean();
  }, [asText]);
  const offered = mayOpenAsText(node) && props.allowDownload !== false;
  const tooLarge = node.size > MAX_TEXT_BYTES;
  if (asText && offered && !tooLarge) {
    const back = (
      <Button variant="ghost" size="sm" className="-ml-2 h-7 px-2 text-xs" onClick={() => setAsText(false)} title={t("Back to the file's usual view")}>
        <ArrowLeftIcon /> {t("Default view")}
      </Button>
    );
    return (
      // The editor's own right-click menu (copy, paste) rather than the page's
      <div className="flex size-full min-h-0 items-center justify-center" onContextMenu={(e) => e.stopPropagation()}>
        <Suspense fallback={<Loader2Icon className={cn("size-6 animate-spin", embedded ? "text-muted-foreground" : "text-white/70")} />}>
          <TextEditor
            key={node.id}
            node={node}
            source={props.source}
            editable={props.editable}
            embedded={embedded}
            asText
            toolbar={back}
            onSaved={props.onSaved}
            onDirtyChange={props.onDirtyChange}
          />
        </Suspense>
      </div>
    );
  }
  let extra = null;
  if (offered && tooLarge)
    extra = (
      <p className="max-w-sm text-xs text-muted-foreground">
        {t("This file is too large to open in the text editor (over {size}). Download it to open it.", { size: formatBytes(MAX_TEXT_BYTES) })}
      </p>
    );
  else if (offered)
    extra = (
      <>
        {hasDraft(node.id) && <p className="text-xs text-muted-foreground">{t("Your unsaved changes are kept in the text editor.")}</p>}
        <Button variant="outline" onClick={() => setAsText(true)}>
          <FileTextIcon /> {t("Open in text editor")}
        </Button>
      </>
    );
  return <NoPreview node={node} source={props.source} allowDownload={props.allowDownload} reason={t("Preview isn't available for this file type")} extra={extra} />;
}

/**
 * A Markdown file: shown rendered, with a switch to the text editor (or, read-only, to the source). Unsaved edits
 * open straight in the editor; they are kept as a draft, so switching back and forth loses nothing
 */
function MarkdownFile(props: Parameters<typeof FileViewer>[0]) {
  const { node, embedded } = props;
  const [mode, setMode] = useState<"preview" | "edit">(() => (hasDraft(node.id) ? "edit" : "preview"));
  // Unsaved changes are reported by the editor; the rendered view has none of its own
  const reportClean = useEffectEvent(() => props.onDirtyChange?.(false));
  useEffect(() => {
    if (mode === "preview") reportClean();
  }, [mode]);
  const toggle = (
    <div role="group" aria-label={t("View")} className="flex shrink-0 overflow-hidden rounded-md border text-xs">
      {(["preview", "edit"] as const).map((m) => (
        <button
          key={m}
          type="button"
          aria-pressed={mode === m}
          onClick={() => setMode(m)}
          className={cn("px-2.5 py-1 hover:bg-muted", mode === m && "bg-secondary font-medium text-foreground")}
        >
          {m === "preview" ? t("Preview") : props.editable ? t("Edit") : t("Source")}
        </button>
      ))}
    </div>
  );
  return (
    <Suspense fallback={<Loader2Icon className={cn("size-6 animate-spin", embedded ? "text-muted-foreground" : "text-white/70")} />}>
      {mode === "preview" ? (
        <MarkdownPreview node={node} source={props.source} embedded={embedded} toolbar={toggle} />
      ) : (
        <TextEditor
          key={node.id}
          node={node}
          source={props.source}
          editable={props.editable}
          embedded={embedded}
          toolbar={toggle}
          onSaved={props.onSaved}
          onDirtyChange={props.onDirtyChange}
        />
      )}
    </Suspense>
  );
}

/** Picture or video; if the browser can't show it after all (format or codec), offer the download instead of a broken image or an empty player */
function Media({ node, url, source, allowDownload, autoPlay }: { node: Node; url: string; source: FileSource; allowDownload?: boolean; autoPlay: boolean }) {
  const [failed, setFailed] = useState(false);
  const fail = () => {
    setFailed(true);
    reportShown("preview", new Error(`The browser couldn't show this ${categoryOf(node)} file (${node.mime || "unknown type"})`), node.id);
  };
  if (failed) return <NoPreview node={node} source={source} allowDownload={allowDownload} reason={t("Your browser can't show this file")} />;
  if (categoryOf(node) === "image") return <img src={url} alt={node.name} onError={fail} className="max-h-full max-w-full object-contain select-none" />;
  if (categoryOf(node) === "audio")
    return (
      <div className="flex w-full max-w-md flex-col items-center gap-6 rounded-2xl border bg-background p-8 text-foreground">
        <FileIcon node={node} className="size-16" />
        <div className="text-sm font-medium break-all">{node.name}</div>
        <audio src={url} controls autoPlay={autoPlay} onError={fail} className="w-full" />
      </div>
    );
  return <video src={url} controls autoPlay={autoPlay} onError={fail} className="max-h-full max-w-full rounded-lg bg-black" />;
}

function NoPreview({ node, source, allowDownload, reason, extra }: { node: Node; source: FileSource; allowDownload?: boolean; reason: string; extra?: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-4 rounded-2xl border bg-background px-10 py-8 text-center text-foreground">
      <FileIcon node={node} className="size-16" />
      <div>
        <div className="font-medium break-all">{node.name}</div>
        <div className="text-sm text-muted-foreground">
          {formatBytes(node.size)} · {reason}
        </div>
      </div>
      <div className="flex flex-wrap justify-center gap-2">
        {allowDownload !== false && (
          <Button onClick={() => triggerDownload(source.contentUrl(node, true))}>
            <DownloadIcon /> {t("Download")}
          </Button>
        )}
      </div>
      {extra}
    </div>
  );
}
