import { useCallback, useEffect, useRef, useState } from "react";
import { ChevronLeftIcon, ChevronRightIcon, DownloadIcon, XIcon } from "lucide-react";
import { triggerDownload, type FileSource, type Node } from "@/api";
import { Button } from "@/components/ui/button";
import { FileIcon } from "@/components/FileIcon";
import { FileViewer } from "@/components/FileViewer";
import { confirm } from "@/components/confirm";
import { t } from "@/lib/i18n";
import { useOverlayFocus } from "@/lib/focus";
import { formatBytes } from "@/lib/utils";

/** Floating full-screen preview (used by the public share page; signed-in files open in a tab instead) */

export function Preview(props: {
  files: Node[];
  index: number;
  source: FileSource;
  editable: boolean;
  allowDownload?: boolean;
  onIndexChange(i: number): void;
  onClose(): void;
  onSaved?(n: Node): void;
}) {
  const node = props.files[props.index];
  const [dirty, setDirty] = useState(false);
  const dirtyRef = useRef(false);
  dirtyRef.current = dirty;

  const guard = useCallback(
    async () =>
      !dirtyRef.current ||
      confirm({
        title: t("Discard unsaved changes?"),
        description: t("This file has unsaved changes. If you leave it, your changes will be lost."),
        confirmText: t("Leave without saving"),
        destructive: true,
      }),
    [],
  );
  const go = useCallback(
    async (delta: number) => {
      const next = props.index + delta;
      if (next < 0 || next >= props.files.length || !(await guard())) return;
      setDirty(false);
      props.onIndexChange(next);
    },
    [props.index, props.files.length, guard, props.onIndexChange],
  );
  const close = useCallback(async () => (await guard()) && props.onClose(), [guard, props.onClose]);
  // Esc is handled below (it must not close while typing in an editor)
  const root = useRef<HTMLDivElement>(null);
  useOverlayFocus(root, !!node);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Keys pressed in a dialog above the preview (e.g. Escape to answer "no") are the dialog's
      if ((e.target as HTMLElement)?.closest?.('[data-slot="dialog-content"]')) return;
      // Not while typing, editing, or seeking in a media player with the arrow keys
      const inEditor = (e.target as HTMLElement)?.closest?.(".cm-editor, video, audio, input, textarea, select, [contenteditable]");
      if (e.key === "Escape" && !inEditor && !e.defaultPrevented) close();
      else if (!inEditor && e.key === "ArrowLeft") go(-1);
      else if (!inEditor && e.key === "ArrowRight") go(1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [go, close]);

  if (!node) return null;
  const body = (
    <FileViewer
      node={node}
      source={props.source}
      editable={props.editable}
      allowDownload={props.allowDownload}
      onSaved={props.onSaved}
      onDirtyChange={setDirty}
    />
  );

  return (
    <div ref={root} className="fixed inset-0 z-50 flex flex-col bg-black/90 text-white backdrop-blur-sm" role="dialog" aria-modal="true" aria-label={node.name}>
      <div className="flex h-14 shrink-0 items-center gap-3 px-4">
        <FileIcon node={node} className="size-5" />
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm font-medium">{node.name}</div>
          <div className="text-xs text-white/60">
            {formatBytes(node.size)}
            {props.files.length > 1 && ` · ${props.index + 1} / ${props.files.length}`}
          </div>
        </div>
        {props.allowDownload !== false && (
          <Button variant="ghost" size="icon" className="text-white hover:bg-white/10 hover:text-white" aria-label={t("Download")} title={t("Download")} onClick={() => triggerDownload(props.source.contentUrl(node, true))}>
            <DownloadIcon />
          </Button>
        )}
        <Button variant="ghost" size="icon" className="text-white hover:bg-white/10 hover:text-white" aria-label={t("Close (Esc)")} title={t("Close (Esc)")} onClick={close}>
          <XIcon />
        </Button>
      </div>
      <div className="relative flex min-h-0 flex-1 items-center justify-center px-4 pb-4 sm:px-16" onClick={(e) => e.target === e.currentTarget && close()}>
        {body}
        {props.index > 0 && (
          <button
            type="button"
            onClick={() => go(-1)}
            className="absolute left-2 rounded-full bg-white/10 p-2 hover:bg-white/20 max-sm:hidden"
            aria-label={t("Previous (←)")}
            title={t("Previous (←)")}
          >
            <ChevronLeftIcon className="size-6" />
          </button>
        )}
        {props.index < props.files.length - 1 && (
          <button
            type="button"
            onClick={() => go(1)}
            className="absolute right-2 rounded-full bg-white/10 p-2 hover:bg-white/20 max-sm:hidden"
            aria-label={t("Next (→)")}
            title={t("Next (→)")}
          >
            <ChevronRightIcon className="size-6" />
          </button>
        )}
      </div>
    </div>
  );
}
