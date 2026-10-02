import { useRef, useState } from "react";
import { FileUpIcon, FolderUpIcon, HistoryIcon, Loader2Icon, RotateCcwIcon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { confirm } from "@/lib/confirm";
import { t } from "@/lib/i18n";
import { formatBytes } from "@/lib/utils";
import { hasSession, type RecoveredBatch } from "@/lib/uploadRecovery";
import { discardRecovered, filesFromInput, resumeRecovered, type ResumeResult } from "@/uploads";

/** What happened when files were chosen to continue, in a message */
function describe(r: ResumeResult) {
  const parts = [
    r.continuing && t("{n} file continues where it stopped.|{n} files continue where they stopped.", { n: r.continuing }),
    r.restarted && t("{n} file starts again.|{n} files start again.", { n: r.restarted }),
    r.changed && t("{n} file changed since it was interrupted, so it starts again.|{n} files changed since they were interrupted, so they start again.", { n: r.changed }),
    r.missing && t("{n} file wasn't among the chosen ones and is still waiting.|{n} files weren't among the chosen ones and are still waiting.", { n: r.missing }),
  ].filter(Boolean);
  return parts.join(" ");
}

/**
 * Uploads that were interrupted by a reload or a closed browser, as they were added (a folder, or files picked at
 * once). The browser no longer has their files: choosing them again continues them where the server has them, or
 * starts them again when that isn't possible (and says so). Discard drops them, with what the server received.
 */
export function RecoveredUploads({ endpoint, batches }: { endpoint: string; batches: RecoveredBatch[] }) {
  return (
    <section aria-label={t("Interrupted uploads")} className="border-b">
      <div className="flex items-center gap-2 bg-muted/40 px-3 py-1.5 text-xs text-muted-foreground">
        <HistoryIcon className="size-3.5" />
        <span className="flex-1">{t("Interrupted uploads: choose their files again to continue.")}</span>
      </div>
      {batches.map((b) => (
        <RecoveredRow key={b.batch} endpoint={endpoint} batch={b} />
      ))}
    </section>
  );
}

function RecoveredRow({ endpoint, batch }: { endpoint: string; batch: RecoveredBatch }) {
  const files = useRef<HTMLInputElement>(null);
  const folder = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const restart = useRef(false);
  const { records } = batch;
  const loose = records.some((r) => !r.relativePath);
  const inFolder = records.some((r) => r.relativePath);
  const label = batch.folder ?? (records.length === 1 ? records[0].name : t("{n} file|{n} files", { n: records.length }));
  const resumable = records.some((r) => hasSession(endpoint, r));
  const failed = records.find((r) => r.state === "failed" && r.error);

  const choose = (input: HTMLInputElement | null, again: boolean) => {
    restart.current = again;
    input?.click();
  };
  const picked = async (list: FileList | null) => {
    if (!list?.length) return;
    setBusy(true);
    try {
      const result = await resumeRecovered(endpoint, records, filesFromInput(list), restart.current);
      if (result.missing === records.length) toast.error(t("None of the chosen files belong to this upload. Choose the same files or folder as before."));
      else toast.success(describe(result), { duration: 8000 });
    } finally {
      setBusy(false);
      if (files.current) files.current.value = "";
      if (folder.current) folder.current.value = "";
    }
  };
  const discard = async () => {
    const ok = await confirm({
      title: t("Discard this interrupted upload?"),
      description: t("What the server received of it is removed too. Files already uploaded stay."),
      confirmText: t("Discard"),
      destructive: true,
    });
    if (!ok) return;
    // The server lets go of what it received first, which can take a moment: the row says it is on it meanwhile
    setBusy(true);
    try {
      await discardRecovered(endpoint, records);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex items-center gap-2.5 border-b border-border/50 px-3 py-2 last:border-0">
      {inFolder ? <FolderUpIcon className="size-5 shrink-0 text-muted-foreground" /> : <FileUpIcon className="size-5 shrink-0 text-muted-foreground" />}
      <div className="min-w-0 flex-1">
        <div className="truncate text-sm" title={label}>
          {label}
        </div>
        <div className="truncate text-xs text-muted-foreground">
          {records.length > 1 && `${t("{n} file|{n} files", { n: records.length })} · `}
          {formatBytes(batch.sent)} / {formatBytes(batch.size)} · {resumable ? t("Can continue") : t("Starts again from the beginning")}
        </div>
        {failed && <div className="truncate text-xs text-destructive">{failed.error}</div>}
      </div>
      {busy ? (
        <Loader2Icon className="size-4 animate-spin text-muted-foreground" />
      ) : (
        <>
          {loose && (
            <Button size="xs" variant="outline" title={t("Choose the file to continue|Choose the files to continue", { n: records.length })} onClick={() => choose(files.current, false)}>
              {t("Resume")}
            </Button>
          )}
          {inFolder && (
            <Button size="xs" variant="outline" title={t("Choose the folder to continue")} onClick={() => choose(folder.current, false)}>
              {loose ? t("Resume folder") : t("Resume")}
            </Button>
          )}
          <Button
            size="icon-xs"
            variant="ghost"
            aria-label={t("Start over")}
            title={t("Start over: choose the files again and upload them from the beginning")}
            onClick={() => choose(inFolder ? folder.current : files.current, true)}
          >
            <RotateCcwIcon />
          </Button>
          <Button size="icon-xs" variant="ghost" aria-label={t("Discard")} title={t("Discard")} onClick={discard}>
            <Trash2Icon />
          </Button>
        </>
      )}
      <input ref={files} type="file" multiple hidden onChange={(e) => void picked(e.target.files)} />
      <input
        ref={folder}
        type="file"
        hidden
        // @ts-expect-error webkitdirectory isn't in the standard types
        webkitdirectory=""
        onChange={(e) => void picked(e.target.files)}
      />
    </div>
  );
}
