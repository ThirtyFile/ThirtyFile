import { useEffect, useRef, useState } from "react";
import { Loader2Icon } from "lucide-react";
import { ApiError, fetchOffice, type FileSource, type Node } from "@/api";
import { t } from "@/lib/i18n";
import { frameDocument, loadFrameScript } from "@/components/officeFrame";
import { extOf } from "@/lib/utils";
import SheetPreview from "@/components/sheet/SheetPreview";
import { TOO_LARGE } from "@/lib/office/ooxml";
import { MAX_OFFICE_PREVIEW_BYTES, MAX_OFFICE_PREVIEW_LABEL } from "@/lib/office/limits";

/** Office formats that can be previewed (legacy .doc / .xls / .ppt must be downloaded and opened) */
export const OFFICE_PREVIEW_EXTS = ["docx", "xlsx", "pptx"];

/** Time limit for Word / PowerPoint layout */
const RENDER_TIMEOUT = 60_000;

/** Error messages we produce ourselves (already translated) are shown as-is */
class ViewerError extends Error {}

/** Turn English errors from the preview components (JSZip etc.) into understandable explanations */
function viewError(e: unknown, fallback: string) {
  const msg = e instanceof Error ? e.message : "";
  if (!msg) return fallback;
  // Show our own messages (and translated server messages) as-is; only convert the preview components' English errors
  if (e instanceof ViewerError || e instanceof ApiError) return msg;
  if (msg === TOO_LARGE) return t("The file's content is too large to preview. Download it and open it in Office.");
  if (/central directory|zip|corrupt|invalid|unexpected/i.test(msg) && !/[一-鿿]/.test(msg)) // i18n-ignore: regex that detects CJK text in a message
    return t("The file is damaged or in an unrecognized format, so it can't be previewed. Download it and open it in Office to check.");
  return msg;
}

function Status({ loading, error }: { loading: boolean; error: string | null }) {
  if (error) return <div className="flex size-full items-center justify-center p-6 text-center text-sm text-destructive">{error}</div>;
  if (loading)
    return (
      <div className="absolute inset-0 flex items-center justify-center text-muted-foreground">
        <Loader2Icon className="size-6 animate-spin" />
      </div>
    );
  return null;
}

/**
 * Word / PowerPoint: laid out in a sandboxed iframe without same-origin rights (office-frame.html).
 * If a document carries scripts or javascript: links when turned into DOM, they can only run inside the isolated iframe and can't reach this site's sign-in state or API.
 */
function FramePreview({ node, source, kind }: { node: Node; source: FileSource; kind: "docx" | "pptx" }) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [srcDoc, setSrcDoc] = useState<string | null>(null);
  useEffect(() => {
    loadFrameScript().then(
      (js) => setSrcDoc(frameDocument(js)),
      (e) => setError(viewError(e, t("Couldn't load the previewer"))),
    );
  }, []);
  useEffect(() => {
    if (!srcDoc) return;
    let cancelled = false;
    let buffer: ArrayBuffer | null = null;
    let ready = false;
    setLoading(true);
    setError(null);
    let timer = 0;
    const send = () => {
      if (cancelled || !ready || !buffer) return;
      // Transfer rather than copy, so large files don't take up an extra copy in memory
      const el = frame.current;
      el?.contentWindow?.postMessage({ type: "render", kind, buffer, width: el.clientWidth, height: el.clientHeight }, "*", [buffer]);
      buffer = null;
      // Stop when layout takes too long (e.g. a file with abnormal content): showing the error removes the iframe
      timer = window.setTimeout(() => {
        if (cancelled) return;
        setError(t("Laying out this document took too long, so the preview was stopped. Download it and open it in Office."));
        setLoading(false);
      }, RENDER_TIMEOUT);
    };
    const onMessage = (e: MessageEvent) => {
      if (e.source !== frame.current?.contentWindow || cancelled) return;
      const msg = e.data as { type?: string; message?: string };
      if (msg.type === "ready") {
        ready = true;
        send();
      } else if (msg.type === "done") {
        window.clearTimeout(timer);
        setLoading(false);
      }
      else if (msg.type === "link") {
        // Only open http(s) / mailto links, and cut the link to this page
        const href = String((msg as { href?: unknown }).href ?? "");
        if (/^(https?:|mailto:)/i.test(href)) window.open(href, "_blank", "noopener,noreferrer");
      }
      else if (msg.type === "error") {
        window.clearTimeout(timer);
        setError(viewError(new Error(msg.message ?? ""), kind === "docx" ? t("Couldn't open this document") : t("Couldn't open this presentation")));
        setLoading(false);
      }
    };
    window.addEventListener("message", onMessage);
    fetchOffice(source.contentUrl(node))
      .then((buf) => {
        buffer = buf;
        send();
      })
      .catch((e) => {
        if (cancelled) return;
        setError(viewError(e, kind === "docx" ? t("Couldn't open this document") : t("Couldn't open this presentation")));
        setLoading(false);
      });
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      window.removeEventListener("message", onMessage);
    };
  }, [node.id, node.updated_at, source, kind, srcDoc]);
  return (
    <div className="relative size-full overflow-hidden bg-neutral-200 dark:bg-neutral-800">
      {!error && srcDoc && (
        <iframe
          // Use a fresh iframe on every reload, to be sure to receive the ready message
          key={`${node.id}-${node.updated_at}`}
          ref={frame}
          srcDoc={srcDoc}
          title={node.name}
          // Only allow scripts: no allow-same-origin (a separate opaque origin), can't open new windows or navigate this page
          sandbox="allow-scripts"
          className="size-full border-0"
        />
      )}
      <Status loading={loading} error={error} />
    </div>
  );
}

/** Excel: the same spreadsheet rendering as online editing (formats, borders, merged cells, images, charts) */
function XlsxPreview({ node, source }: { node: Node; source: FileSource }) {
  const [buffer, setBuffer] = useState<ArrayBuffer | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    setBuffer(null);
    setError(null);
    fetchOffice(source.contentUrl(node))
      .then((buf) => !cancelled && setBuffer(buf))
      .catch((e) => !cancelled && setError(viewError(e, t("Couldn't open this spreadsheet"))));
    return () => {
      cancelled = true;
    };
  }, [node.id, node.updated_at, source]);
  return (
    <div className="relative size-full bg-white">
      <Status loading={!buffer && !error} error={error} />
      {buffer && !error && <SheetPreview buffer={buffer} onError={(m) => setError(viewError(new ViewerError(m), t("Couldn't open this spreadsheet")))} />}
    </div>
  );
}

/** Office file preview: laid out in the browser (Word / PowerPoint in an isolated iframe, Excel drawn by the spreadsheet component) */
export default function OfficeViewer({ node, source }: { node: Node; source: FileSource }) {
  const ext = extOf(node.name);
  // Laying out very large files in the browser uses lots of memory: ask the user to download instead
  if (node.size > MAX_OFFICE_PREVIEW_BYTES) return <Status loading={false} error={t("The file is too large (over {size}) to preview online. Download it to open it.", { size: MAX_OFFICE_PREVIEW_LABEL })} />;
  if (ext === "docx") return <FramePreview node={node} source={source} kind="docx" />;
  if (ext === "xlsx") return <XlsxPreview node={node} source={source} />;
  if (ext === "pptx") return <FramePreview node={node} source={source} kind="pptx" />;
  return null;
}
