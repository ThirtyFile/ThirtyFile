import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Loader2Icon } from "lucide-react";
import { fetchOk, type FileSource, type Node } from "@/api";
import { getDraft } from "@/lib/drafts";
import { t } from "@/lib/i18n";
import { renderMarkdown } from "@/lib/markdown";
import { decodeText, normalizeLines } from "@/lib/textEncoding";

/** A Markdown file rendered as a page (sanitised, see lib/markdown.ts); unsaved edits are shown as they would be saved */
export default function MarkdownPreview(props: { node: Node; source: FileSource; embedded?: boolean; toolbar?: ReactNode }) {
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setText(null);
    setError(null);
    fetchOk(props.source.contentUrl(props.node))
      .then((r) => r.arrayBuffer())
      .then((buf) => {
        if (cancelled) return;
        const draft = getDraft(props.node.id);
        setText(draft ? draft.text : normalizeLines(decodeText(buf).text));
      })
      .catch((e) => !cancelled && setError(e.message));
    return () => {
      cancelled = true;
    };
  }, [props.node.id, props.node.updated_at, props.source]);

  const html = useMemo(() => (text === null ? "" : renderMarkdown(text)), [text]);
  const muted = props.embedded ? "text-muted-foreground" : "text-white/70";
  if (error) return <div className={`flex h-full items-center justify-center text-sm ${muted}`}>{error}</div>;

  return (
    <div
      className={
        props.embedded
          ? "flex h-full w-full flex-col overflow-hidden bg-background text-foreground"
          : "flex h-full w-full max-w-5xl flex-col overflow-hidden rounded-xl bg-background text-foreground shadow-2xl"
      }
    >
      <div className="flex h-10 shrink-0 items-center gap-2 border-b px-3 text-xs text-muted-foreground">
        {props.toolbar}
        <span>{t("Markdown preview")}</span>
      </div>
      {text === null ? (
        <div className="flex flex-1 items-center justify-center text-muted-foreground">
          <Loader2Icon className="size-6 animate-spin" />
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-auto">
          {/* Sanitised by renderMarkdown */}
          <article className="markdown-body mx-auto max-w-3xl px-6 py-6 select-text" dangerouslySetInnerHTML={{ __html: html }} />
        </div>
      )}
    </div>
  );
}
