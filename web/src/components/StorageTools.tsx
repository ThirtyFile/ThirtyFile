import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  CheckCircle2Icon,
  ChevronLeftIcon,
  ChevronRightIcon,
  CircleMinusIcon,
  DownloadIcon,
  FileIcon,
  FolderIcon,
  Link2Icon,
  Loader2Icon,
  LockIcon,
  XCircleIcon,
} from "lucide-react";
import {
  api,
  driveName,
  type LocationItem,
  type LocationSpace,
  type LocationTestStep,
  type StorageLocation,
  type UnusedJob,
} from "@/api";
import { Button, buttonVariants } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ConfirmDialog, ErrorText } from "@/components/dialogs";
import { cn, formatBytes, formatDateTime } from "@/lib/utils";
import { t, tServer } from "@/lib/i18n";

// Tools for one storage location (System settings › Storage locations): the step-by-step test, browsing what it
// holds, and removing content nothing uses

const STEP_LABEL: Record<LocationTestStep["id"], string> = {
  connect: t("Connect"),
  write: t("Write a small file"),
  read: t("Read it back and compare"),
  write_large: t("Upload a larger file"),
  read_large: t("Download it and compare"),
  delete: t("Delete the test files"),
  cleanup: t("Clean up the test files"),
};

function duration(ms: number) {
  return ms < 1000 ? t("{n} ms", { n: ms }) : t("{n} s", { n: (ms / 1000).toFixed(1) });
}

/** The step-by-step test: runs when opened, then lists each step with its time */
export function LocationTestDialog({ location, onClose }: { location: StorageLocation; onClose(): void }) {
  const qc = useQueryClient();
  const run = useMutation({
    mutationFn: () => api.testStorageSteps(location.id),
    onSettled: () => qc.invalidateQueries({ queryKey: ["storage-locations"] }),
  });
  // Once when opened (not again when React runs effects twice in development)
  const started = useRef(false);
  const { mutate } = run;
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    mutate();
  }, [mutate]);
  const steps: LocationTestStep[] =
    run.data?.steps ??
    (["connect", "write", "read", "write_large", "read_large", "delete"] as const).map((id) => ({
      id,
      outcome: "skipped",
      ms: 0,
      bytes: null,
      speed: null,
      message: null,
    }));

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("Test \"{name}\" step by step", { name: location.name })}</DialogTitle>
          <DialogDescription>
            {t("Writes a small and a larger file (24 MB, uploaded in parts on S3), reads them back, and deletes them. Takes up to two minutes.")}
          </DialogDescription>
        </DialogHeader>
        <div aria-live="polite" aria-busy={run.isPending}>
          {run.isPending && (
            <p className="mb-2 flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2Icon className="size-4 animate-spin" /> {t("Testing…")}
            </p>
          )}
          {run.data && (
            <p
              className={cn(
                "mb-2 flex items-center gap-1.5 text-sm font-medium",
                run.data.ok ? "text-emerald-600 dark:text-emerald-400" : "text-destructive",
              )}
            >
              {run.data.ok ? <CheckCircle2Icon className="size-4" /> : <XCircleIcon className="size-4" />}
              {run.data.ok ? t("Every step passed") : t("The test failed")}
            </p>
          )}
          <ErrorText>{run.error?.message}</ErrorText>
          <ol className="grid gap-1.5">
            {steps.map((s) => (
              <li key={s.id} className="flex items-start gap-2 rounded-md border px-2.5 py-2 text-sm">
                <span className="mt-0.5 shrink-0">
                  {run.isPending ? (
                    <CircleMinusIcon className="size-4 text-muted-foreground/50" aria-hidden />
                  ) : s.outcome === "ok" ? (
                    <CheckCircle2Icon className="size-4 text-emerald-600 dark:text-emerald-400" aria-label={t("Passed")} />
                  ) : s.outcome === "error" ? (
                    <XCircleIcon className="size-4 text-destructive" aria-label={t("Failed")} />
                  ) : (
                    <CircleMinusIcon className="size-4 text-muted-foreground" aria-label={t("Skipped")} />
                  )}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="flex flex-wrap items-baseline justify-between gap-x-3">
                    <span>{STEP_LABEL[s.id]}</span>
                    {s.outcome !== "skipped" && !run.isPending && (
                      <span className="text-xs text-muted-foreground tabular-nums">
                        {duration(s.ms)}
                        {s.speed !== null && ` · ${t("{speed} MB/s", { speed: s.speed.toFixed(1) })}`}
                      </span>
                    )}
                  </span>
                  {s.message && (
                    <span className={cn("block text-xs break-words", s.outcome === "error" ? "text-destructive" : "text-muted-foreground")}>
                      {tServer(s.message)}
                    </span>
                  )}
                </span>
              </li>
            ))}
          </ol>
        </div>
        <DialogFooter>
          <Button variant="outline" disabled={run.isPending} onClick={() => run.mutate()}>
            {t("Run again")}
          </Button>
          <Button onClick={onClose}>{t("Close")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** A space's name as the tools show it: personal spaces by their owner */
function spaceLabel(s: LocationSpace) {
  return s.kind === "personal" ? t("Personal space of {user}", { user: s.owner }) : driveName(s);
}

const STATUS_LABEL: Record<NonNullable<LocationItem["usage"]>["status"], string> = {
  used: t("In use"),
  version: t("Earlier version"),
  trash: t("In the trash"),
  pending: t("Waiting to be deleted"),
  unused: t("Unused"),
};

/** What an item is to ThirtyFile */
function Belongs({ item }: { item: LocationItem }) {
  if (item.space)
    return (
      <span className="flex items-center gap-1">
        {item.space.private && <LockIcon className="size-3 shrink-0" aria-label={t("Private")} />}
        <span className="truncate">{t("Folder of {space}", { space: spaceLabel(item.space) })}</span>
      </span>
    );
  if (item.usage) {
    const u = item.usage;
    return (
      <span className="flex min-w-0 flex-col">
        <span className={cn(u.status === "unused" ? "text-amber-600 dark:text-amber-400" : u.status === "used" ? "text-foreground" : "")}>
          {STATUS_LABEL[u.status]}
          {u.uses > 1 && ` · ${t("{n} use|{n} uses", { n: u.uses })}`}
        </span>
        {u.space && (
          <span className="truncate" title={u.file ?? undefined}>
            {u.space.private && <LockIcon className="mr-1 inline size-3" aria-label={t("Private")} />}
            {spaceLabel(u.space)}
            {u.file && ` › ${u.file}`}
          </span>
        )}
      </span>
    );
  }
  if (item.role === "internal") return <span>{t("Used by ThirtyFile")}</span>;
  if (item.role === "content") return <span>{t("Content store")}</span>;
  if (item.kind === "link") return <span>{t("Link (not followed)")}</span>;
  return null;
}

/** Browsing a location, read-only: one folder level at a time, a page at a time */
export function LocationBrowseDialog({ location, onClose }: { location: StorageLocation; onClose(): void }) {
  const [path, setPath] = useState("");
  // Where each page starts: the page shown is the last one
  const [cursors, setCursors] = useState<(string | undefined)[]>([undefined]);
  const after = cursors[cursors.length - 1];
  const q = useQuery({
    queryKey: ["storage-browse", location.id, path, after],
    queryFn: () => api.browseStorage(location.id, path, after),
  });
  const open = (p: string) => {
    setPath(p);
    setCursors([undefined]);
  };
  const parts = path ? path.split("/") : [];
  const page = q.data;

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="flex max-h-[90vh] flex-col sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>{t("Browse \"{name}\"", { name: location.name })}</DialogTitle>
          <DialogDescription>{t("Read-only. Files in other people's personal spaces aren't shown.")}</DialogDescription>
        </DialogHeader>
        <nav aria-label={t("Folder path")} className="flex flex-wrap items-center gap-0.5 text-sm">
          <button type="button" className="rounded px-1 hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none" onClick={() => open("")}>
            {location.name}
          </button>
          {parts.map((p, i) => (
            <span key={i} className="flex items-center gap-0.5">
              <ChevronRightIcon className="size-3.5 text-muted-foreground" aria-hidden />
              <button
                type="button"
                aria-current={i === parts.length - 1 ? "location" : undefined}
                className="max-w-48 truncate rounded px-1 hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                onClick={() => open(parts.slice(0, i + 1).join("/"))}
              >
                {p}
              </button>
            </span>
          ))}
        </nav>
        {page?.space && (
          <p className="text-xs text-muted-foreground">{t("This is the folder of {space}.", { space: spaceLabel(page.space) })}</p>
        )}
        <div className="min-h-40 flex-1 overflow-auto rounded-md border">
          {q.isLoading ? (
            <div className="flex h-40 items-center justify-center text-muted-foreground">
              <Loader2Icon className="size-5 animate-spin" aria-label={t("Loading…")} />
            </div>
          ) : q.error ? (
            <div className="p-3">
              <ErrorText>{q.error.message}</ErrorText>
            </div>
          ) : page && page.items.length === 0 ? (
            <p className="p-3 text-sm text-muted-foreground">{t("This folder is empty")}</p>
          ) : (
            <table className="w-full border-collapse text-xs">
              <thead>
                <tr className="text-left text-muted-foreground">
                  <th className="sticky top-0 bg-background px-2.5 py-1.5 font-normal">{t("Name")}</th>
                  <th className="sticky top-0 w-20 bg-background px-2.5 py-1.5 text-right font-normal max-sm:hidden">{t("Size")}</th>
                  <th className="sticky top-0 w-36 bg-background px-2.5 py-1.5 font-normal max-md:hidden">{t("Modified")}</th>
                  <th className="sticky top-0 bg-background px-2.5 py-1.5 font-normal">{t("Belongs to")}</th>
                  <th className="sticky top-0 w-10 bg-background px-2.5 py-1.5">
                    <span className="sr-only">{t("Download")}</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {page?.items.map((item) => {
                  const Icon = item.kind === "folder" ? FolderIcon : item.kind === "link" ? Link2Icon : FileIcon;
                  const closed = item.space?.private;
                  const downloadable = item.kind === "file" && !item.usage?.space?.private;
                  return (
                    <tr key={item.path} className="border-t border-border/40 align-top">
                      <td className="max-w-0 px-2.5 py-1.5">
                        <span className="flex min-w-0 items-center gap-1.5">
                          <Icon className={cn("size-4 shrink-0", item.kind === "folder" ? "text-amber-500" : "text-muted-foreground")} aria-hidden />
                          {item.kind === "folder" && !closed ? (
                            <button
                              type="button"
                              className="truncate rounded text-left hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                              onClick={() => open(item.path)}
                            >
                              {item.name}
                            </button>
                          ) : (
                            <span className="truncate font-mono text-[11px]" title={item.name}>
                              {item.name}
                            </span>
                          )}
                        </span>
                        <span className="block pl-5.5 text-muted-foreground sm:hidden">{item.kind === "file" && formatBytes(item.size)}</span>
                      </td>
                      <td className="px-2.5 py-1.5 text-right tabular-nums max-sm:hidden">{item.kind === "file" ? formatBytes(item.size) : ""}</td>
                      <td className="px-2.5 py-1.5 text-muted-foreground max-md:hidden">{item.modified ? formatDateTime(item.modified) : ""}</td>
                      <td className="max-w-0 px-2.5 py-1.5 text-muted-foreground">
                        <Belongs item={item} />
                      </td>
                      <td className="px-1.5 py-1">
                        {downloadable && (
                          <a
                            className={buttonVariants({ variant: "ghost", size: "icon-sm" })}
                            aria-label={t("Download {name}", { name: item.name })}
                            title={t("Download")}
                            href={api.storageDownloadUrl(location.id, item.path)}
                            download
                          >
                            <DownloadIcon />
                          </a>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </div>
        <DialogFooter className="items-center">
          <span className="text-xs text-muted-foreground sm:mr-auto">{t("Page {n}", { n: cursors.length })}</span>
          <Button variant="outline" disabled={cursors.length === 1} onClick={() => setCursors(cursors.slice(0, -1))}>
            <ChevronLeftIcon /> {t("Previous")}
          </Button>
          <Button variant="outline" disabled={!page?.next} onClick={() => page?.next && setCursors([...cursors, page.next])}>
            {t("Next")} <ChevronRightIcon />
          </Button>
          <Button onClick={onClose}>{t("Close")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** Finding content nothing uses, and removing it after confirmation */
export function UnusedContentDialog({ location, onClose }: { location: StorageLocation; onClose(): void }) {
  const qc = useQueryClient();
  const key = ["storage-unused", location.id];
  const busy = (j: UnusedJob | null | undefined) => j?.phase === "scanning" || j?.phase === "removing";
  const q = useQuery({
    queryKey: key,
    queryFn: () => api.unusedContent(location.id),
    refetchInterval: (query) => (busy(query.state.data) ? 1000 : false),
  });
  const [confirming, setConfirming] = useState(false);
  const find = useMutation({
    mutationFn: () => api.findUnusedContent(location.id),
    onSuccess: (job) => qc.setQueryData(key, job),
  });
  const job = q.data;
  const removed = job?.phase === "removed";
  // Once removed, the location's figures and listings change
  useEffect(() => {
    if (!removed) return;
    qc.invalidateQueries({ queryKey: ["storage-locations"] });
    qc.invalidateQueries({ queryKey: ["storage-browse", location.id] });
  }, [removed, qc, location.id]);

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="flex max-h-[90vh] flex-col sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("Unused content in \"{name}\"", { name: location.name })}</DialogTitle>
          <DialogDescription>
            {t("Finds stored content that no file, earlier version or item in the trash uses. Content written in the last 24 hours is left alone: uploads store their content before recording it.")}
          </DialogDescription>
        </DialogHeader>
        <div className="grid min-h-0 gap-2 overflow-auto" aria-live="polite">
          {q.isLoading && <Loader2Icon className="size-5 animate-spin text-muted-foreground" aria-label={t("Loading…")} />}
          <ErrorText>{q.error?.message ?? find.error?.message}</ErrorText>
          {job?.phase === "scanning" && (
            <p className="flex items-center gap-2 text-sm">
              <Loader2Icon className="size-4 animate-spin" /> {t("Looking… {n} stored item checked|Looking… {n} stored items checked", { n: job.scanned })}
            </p>
          )}
          {job?.phase === "failed" && <ErrorText>{tServer(job.error)}</ErrorText>}
          {job && (job.phase === "found" || job.phase === "removing" || removed) && (
            <>
              <p className="text-sm">
                {job.count === 0
                  ? t("No unused content found among {n} stored item.|No unused content found among {n} stored items.", { n: job.scanned })
                  : t("{n} unused item found ({size}).|{n} unused items found ({size}).", { n: job.count, size: formatBytes(job.bytes) })}
                {job.recent > 0 && ` ${t("{n} more written within the last day was left alone.|{n} more written within the last day were left alone.", { n: job.recent })}`}
              </p>
              {job.phase === "removing" && (
                <p className="flex items-center gap-2 text-sm">
                  <Loader2Icon className="size-4 animate-spin" />
                  {t("Removing… {done} of {total}", { done: job.removed + job.kept + job.failed, total: job.count })}
                </p>
              )}
              {removed && (
                <div className="rounded-md bg-muted/60 p-2.5 text-sm">
                  <p className="flex items-center gap-1.5">
                    <CheckCircle2Icon className="size-4 text-emerald-600 dark:text-emerald-400" />
                    {t("{n} item removed ({size}).|{n} items removed ({size}).", { n: job.removed, size: formatBytes(job.removed_bytes) })}
                  </p>
                  {job.kept > 0 && (
                    <p className="text-xs text-muted-foreground">
                      {t("{n} was kept: it was used or written again meanwhile.|{n} were kept: they were used or written again meanwhile.", { n: job.kept })}
                    </p>
                  )}
                  {job.failed > 0 && (
                    <p className="text-xs text-destructive">
                      {t("{n} couldn't be deleted; it will be retried automatically.|{n} couldn't be deleted; they will be retried automatically.", { n: job.failed })}
                    </p>
                  )}
                  {job.error && <ErrorText>{tServer(job.error)}</ErrorText>}
                </div>
              )}
              {job.items.length > 0 && !removed && (
                <div className="max-h-72 overflow-auto rounded-md border">
                  <table className="w-full border-collapse text-xs">
                    <caption className="sr-only">{t("Unused content")}</caption>
                    <thead>
                      <tr className="text-left text-muted-foreground">
                        <th className="sticky top-0 bg-background px-2.5 py-1.5 font-normal">{t("Path")}</th>
                        <th className="sticky top-0 w-20 bg-background px-2.5 py-1.5 text-right font-normal">{t("Size")}</th>
                        <th className="sticky top-0 w-36 bg-background px-2.5 py-1.5 font-normal max-sm:hidden">{t("Modified")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {job.items.map((i) => (
                        <tr key={i.path} className="border-t border-border/40">
                          <td className="max-w-0 truncate px-2.5 py-1 font-mono text-[11px]" title={i.path}>
                            {i.path}
                          </td>
                          <td className="px-2.5 py-1 text-right tabular-nums">{formatBytes(i.size)}</td>
                          <td className="px-2.5 py-1 text-muted-foreground max-sm:hidden">{i.modified ? formatDateTime(i.modified) : ""}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                  {job.count > job.items.length && (
                    <p className="border-t px-2.5 py-1.5 text-xs text-muted-foreground">
                      {t("…and {n} more", { n: job.count - job.items.length })}
                    </p>
                  )}
                </div>
              )}
            </>
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" disabled={busy(job) || find.isPending} onClick={() => find.mutate()}>
            {find.isPending && <Loader2Icon className="animate-spin" />}
            {job ? t("Look again") : t("Find unused content")}
          </Button>
          {job?.phase === "found" && job.count > 0 && (
            <Button variant="destructive" onClick={() => setConfirming(true)}>
              {t("Remove {n} item ({size})|Remove {n} items ({size})", { n: job.count, size: formatBytes(job.bytes) })}
            </Button>
          )}
          <Button onClick={onClose}>{t("Close")}</Button>
        </DialogFooter>
        {confirming && job && (
          <ConfirmDialog
            title={t("Remove {n} unused item?|Remove {n} unused items?", { n: job.count })}
            description={t("They are deleted from \"{name}\" for good ({size}). Each one is checked again right before: anything used or written again meanwhile is kept.", {
              name: location.name,
              size: formatBytes(job.bytes),
            })}
            confirmText={t("Remove")}
            destructive
            irreversible
            onClose={() => setConfirming(false)}
            onConfirm={async () => {
              const next = await api.removeUnusedContent(location.id, job.scan_id);
              qc.setQueryData(key, next);
              setConfirming(false);
            }}
          />
        )}
      </DialogContent>
    </Dialog>
  );
}
