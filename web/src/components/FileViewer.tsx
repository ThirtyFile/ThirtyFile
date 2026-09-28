import { Suspense, lazy, useEffect, useState } from "react";
import { DownloadIcon, Loader2Icon } from "lucide-react";
import { triggerDownload, type FileSource, type Node } from "@/api";
import { Button } from "@/components/ui/button";
import { FileIcon, categoryOf, isBrowserMedia, isTextLike } from "@/components/FileIcon";
import { hasDraft } from "@/lib/drafts";
import { t } from "@/lib/i18n";
import { cn, extOf, formatBytes } from "@/lib/utils";
import { MAX_OFFICE_PREVIEW_BYTES } from "@/lib/office/limits";

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
  onSaved?(n: Node): void;
  onDirtyChange?(dirty: boolean): void;
}) {
  const { node, embedded } = props;
  const cat = categoryOf(node);
  const url = props.source.contentUrl(node);

  // key: a new element (and a fresh error state) for each file
  if (isBrowserMedia(node)) return <Media key={node.id} node={node} url={url} source={props.source} allowDownload={props.allowDownload} />;
  if (cat === "pdf")
    return <iframe key={node.id} src={url} title={node.name} className={cn("size-full bg-white", !embedded && "max-w-5xl rounded-lg")} />;
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
        <TextEditor
          node={node}
          source={props.source}
          editable={props.editable}
          embedded={embedded}
          onSaved={props.onSaved}
          onDirtyChange={props.onDirtyChange}
        />
      </Suspense>
    );
  return <NoPreview node={node} source={props.source} allowDownload={props.allowDownload} reason={t("Preview isn't available for this file type")} />;
}

/**
 * A Markdown file: shown rendered, with a switch to the text editor (or, read-only, to the source). Unsaved edits
 * open straight in the editor; they are kept as a draft, so switching back and forth loses nothing
 */
function MarkdownFile(props: Parameters<typeof FileViewer>[0]) {
  const { node, embedded } = props;
  const [mode, setMode] = useState<"preview" | "edit">(() => (hasDraft(node.id) ? "edit" : "preview"));
  // Unsaved changes are reported by the editor; the rendered view has none of its own
  useEffect(() => {
    if (mode === "preview") props.onDirtyChange?.(false);
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
function Media({ node, url, source, allowDownload }: { node: Node; url: string; source: FileSource; allowDownload?: boolean }) {
  const [failed, setFailed] = useState(false);
  if (failed) return <NoPreview node={node} source={source} allowDownload={allowDownload} reason={t("Your browser can't show this file")} />;
  if (categoryOf(node) === "image")
    return <img src={url} alt={node.name} onError={() => setFailed(true)} className="max-h-full max-w-full object-contain select-none" />;
  if (categoryOf(node) === "audio")
    return (
      <div className="flex w-full max-w-md flex-col items-center gap-6 rounded-2xl border bg-background p-8 text-foreground">
        <FileIcon node={node} className="size-16" />
        <div className="text-sm font-medium break-all">{node.name}</div>
        <audio src={url} controls autoPlay onError={() => setFailed(true)} className="w-full" />
      </div>
    );
  return <video src={url} controls autoPlay onError={() => setFailed(true)} className="max-h-full max-w-full rounded-lg bg-black" />;
}

function NoPreview({ node, source, allowDownload, reason }: { node: Node; source: FileSource; allowDownload?: boolean; reason: string }) {
  return (
    <div className="flex flex-col items-center gap-4 rounded-2xl border bg-background px-10 py-8 text-center text-foreground">
      <FileIcon node={node} className="size-16" />
      <div>
        <div className="font-medium break-all">{node.name}</div>
        <div className="text-sm text-muted-foreground">{formatBytes(node.size)} · {reason}</div>
      </div>
      {allowDownload !== false && (
        <Button onClick={() => triggerDownload(source.contentUrl(node, true))}>
          <DownloadIcon /> {t("Download")}
        </Button>
      )}
    </div>
  );
}
