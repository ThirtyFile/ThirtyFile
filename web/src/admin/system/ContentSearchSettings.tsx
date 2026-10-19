import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon, TextSearchIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type ContentSearchInfo, type ContentSearchSettings as Settings } from "@/api";
import { keys } from "@/api/queryKeys";
import { Section, Toggle } from "@/admin/SettingsFrame";
import { ConfirmDialog } from "@/components/dialogs";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { t } from "@/lib/i18n";
import { formatBytes } from "@/lib/utils";

/** Control panel › General › Search: searching inside files, with how far the reading has got */
export function ContentSearchSettings() {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: keys.contentSearch(),
    queryFn: ({ signal }) => api.contentSearch(signal),
    // Quicker while files wait to be read
    refetchInterval: (query) => (query.state.data?.queued ? 3_000 : 20_000),
  });
  const [confirmRebuild, setConfirmRebuild] = useState(false);
  const done = (info: ContentSearchInfo) => {
    qc.setQueryData(keys.contentSearch(), info);
    // Searches ask the account whether they may look inside files
    void qc.invalidateQueries({ queryKey: keys.me() });
  };
  const save = useMutation({
    mutationFn: (s: Settings) => api.saveContentSearch(s),
    onSuccess: (info) => {
      done(info);
      toast.success(t("Saved"));
    },
    onError: (e) => toast.error(e.message),
  });
  const rebuild = useMutation({
    mutationFn: () => api.rebuildContentSearch(),
    onSuccess: (info) => {
      done(info);
      setConfirmRebuild(false);
    },
    onError: (e) => toast.error(e.message),
  });
  const info = q.data;
  return (
    <Section title={t("Search inside files")}>
      <div className="flex items-start gap-4 p-4">
        <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-violet-500/10 text-violet-600 dark:text-violet-300">
          <TextSearchIcon className="size-4" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="font-medium">{t("Find files by what is inside them")}</div>
          <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
            {t(
              "Reads the text of plain text, Markdown, CSV and code files, web pages, and Word, Excel, PowerPoint and OpenDocument files, so searches can find words inside them. The text is kept on this server's disk, and people find only what they can open. PDFs and pictures are not read yet.",
            )}
          </p>
          {info?.enabled && (
            <p className="mt-2 text-xs leading-relaxed text-muted-foreground">
              {t("Files are read in the background, one at a time, so the server stays responsive. The first reading of many files takes a while.")}
            </p>
          )}
        </div>
        {info && (
          <Toggle
            checked={info.enabled}
            disabled={save.isPending}
            label={t("Search inside files")}
            onChange={(enabled) => save.mutate({ enabled, paused: info.paused, max_file_mb: info.max_file_mb, max_total_mb: info.max_total_mb })}
          />
        )}
      </div>
      {info?.enabled && (
        <>
          <Progress info={info} />
          <Limits key={`${info.max_file_mb}-${info.max_total_mb}`} info={info} saving={save.isPending} onSave={(s) => save.mutate(s)} />
          <div className="flex flex-wrap items-center gap-3 border-t p-4">
            <label className="flex items-center gap-2 text-sm">
              <Toggle
                checked={info.paused}
                disabled={save.isPending}
                label={t("Pause reading")}
                onChange={(paused) => save.mutate({ enabled: true, paused, max_file_mb: info.max_file_mb, max_total_mb: info.max_total_mb })}
              />
              {t("Pause reading")}
            </label>
            <Button variant="outline" size="sm" className="ml-auto" onClick={() => setConfirmRebuild(true)}>
              {t("Index again")}
            </Button>
          </div>
        </>
      )}
      {!info && !q.error && <p className="border-t p-4 text-sm text-muted-foreground">{t("Loading…")}</p>}
      {q.error && <p className="border-t p-4 text-sm text-destructive">{q.error.message}</p>}
      {confirmRebuild && (
        <ConfirmDialog
          title={t("Index again?")}
          description={t("The text read so far is deleted and every file is read again. Searches inside files find less until the reading is done.")}
          confirmText={t("Index again")}
          onClose={() => setConfirmRebuild(false)}
          onConfirm={async () => {
            await rebuild.mutateAsync();
          }}
        />
      )}
    </Section>
  );
}

function Progress({ info }: { info: ContentSearchInfo }) {
  const files = info.indexed + info.skipped + info.failed + info.queued;
  const read = info.indexed + info.skipped + info.failed;
  return (
    <div className="grid gap-1 border-t p-4 text-sm" role="status">
      {info.paused ? (
        <div className="font-medium">{t("Paused: {n} files are waiting", { n: info.queued })}</div>
      ) : info.full ? (
        <div className="font-medium text-destructive">{t("The limit for the text kept was reached: {n} files are waiting. Raise it below to go on.", { n: info.queued })}</div>
      ) : info.queued > 0 ? (
        <div className="flex items-center gap-2 font-medium">
          <Loader2Icon className="size-3.5 animate-spin" />
          {t("Reading files: {read} of {files}", { read, files })}
        </div>
      ) : (
        <div className="font-medium">{t("Up to date")}</div>
      )}
      <div className="text-xs text-muted-foreground">
        {t("{n} files can be searched, using {size} of text", { n: info.indexed, size: formatBytes(info.bytes) })}
        {info.skipped > 0 && ` · ${t("{n} files hold nothing to read (other kinds of file, too large, no text)", { n: info.skipped })}`}
        {info.failed > 0 && ` · ${t("{n} files could not be read", { n: info.failed })}`}
      </div>
    </div>
  );
}

function Limits({ info, saving, onSave }: { info: ContentSearchInfo; saving: boolean; onSave(s: Settings): void }) {
  const [file, setFile] = useState(String(info.max_file_mb));
  const [total, setTotal] = useState(String(info.max_total_mb));
  const [fileMb, totalMb] = [Number(file), Number(total)];
  const valid = Number.isInteger(fileMb) && fileMb >= 1 && fileMb <= 100 && Number.isInteger(totalMb) && totalMb >= 10 && totalMb <= 100_000;
  const changed = fileMb !== info.max_file_mb || totalMb !== info.max_total_mb;
  return (
    <form
      className="grid gap-3 border-t p-4"
      onSubmit={(e) => {
        e.preventDefault();
        if (valid && changed) onSave({ enabled: true, paused: info.paused, max_file_mb: fileMb, max_total_mb: totalMb });
      }}
    >
      <div className="flex flex-wrap gap-4">
        <label className="grid gap-1 text-sm">
          {t("Largest file read (MB)")}
          <Input type="number" min={1} max={100} className="w-28" value={file} onChange={(e) => setFile(e.target.value)} aria-invalid={!valid} />
        </label>
        <label className="grid gap-1 text-sm">
          {t("Most text kept in all (MB)")}
          <Input type="number" min={10} max={100_000} className="w-32" value={total} onChange={(e) => setTotal(e.target.value)} aria-invalid={!valid} />
        </label>
      </div>
      <p className="text-xs leading-relaxed text-muted-foreground">
        {t("The search index takes about three times the text on the disk. Larger files are left alone, and the text kept from one file is cut after 3 MB.")}
      </p>
      <div>
        <Button type="submit" size="sm" disabled={saving || !valid || !changed}>
          {t("Save")}
        </Button>
      </div>
    </form>
  );
}
