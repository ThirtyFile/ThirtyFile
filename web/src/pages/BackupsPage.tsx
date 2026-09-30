import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArchiveIcon,
  ArchiveRestoreIcon,
  ArrowRightIcon,
  InfoIcon,
  Loader2Icon,
  PauseIcon,
  PlayIcon,
  RefreshCwIcon,
  ShieldCheckIcon,
  Trash2Icon,
  XIcon,
  type LucideIcon,
} from "lucide-react";
import { toast } from "sonner";
import { api, backupJobActive, type BackupJob, type BackupSet, type BackupSnapshot } from "@/api";
import { DataTable, EmptyState, type Column } from "@/components/DataTable";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { ConfirmDialog, ErrorText } from "@/components/dialogs";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { DRIVE_ICON } from "@/lib/drives";
import { activeJob, BACKUP_JOB_KIND_LABEL, BACKUP_JOB_STATE_LABEL, backupSpaceLabel, completeSnapshot, useBackups } from "@/lib/backups";
import { controlPanelItem, useSettingsSearch } from "@/lib/controlPanel";
import { t, tServer } from "@/lib/i18n";
import { invalidateFiles } from "@/lib/queries";
import { cn, formatBytes, formatDateTime } from "@/lib/utils";

/** A job's progress: a bar with what is done, in files and bytes */
function JobProgress({ j }: { j: BackupJob }) {
  const pct = j.bytes_total > 0 ? Math.min(100, (j.bytes_done / j.bytes_total) * 100) : j.state === "done" ? 100 : 0;
  const label = t("{done} of {total} files · {bytes} of {size}", {
    done: j.files_done,
    total: j.files_total,
    bytes: formatBytes(j.bytes_done),
    size: formatBytes(j.bytes_total),
  });
  return (
    <span className="grid min-w-0 gap-0.5">
      <span role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)} className="h-1.5 overflow-hidden rounded bg-muted">
        <span className={cn("block h-full transition-[width]", j.state === "failed" ? "bg-destructive" : "bg-brand")} style={{ width: `${pct}%` }} />
      </span>
      <span className="truncate text-[11px] text-muted-foreground tabular-nums">
        {label}
        {j.state === "running" && j.speed ? ` · ${t("{size}/s", { size: formatBytes(j.speed) })}` : ""}
      </span>
    </span>
  );
}

/** What a copy is doing, or what it is */
function SetState({ set, job }: { set: BackupSet; job: BackupJob | null }) {
  if (job) {
    return (
      <span className="grid min-w-0 gap-0.5">
        <span className={cn("truncate text-xs", job.state === "failed" ? "text-destructive" : job.state === "running" ? "text-brand" : "text-muted-foreground")}>
          {BACKUP_JOB_KIND_LABEL[job.kind]} · {BACKUP_JOB_STATE_LABEL[job.state]}
        </span>
        <JobProgress j={job} />
      </span>
    );
  }
  if (set.removing) return <span className="text-xs text-muted-foreground">{t("Being deleted")}</span>;
  const snap = completeSnapshot(set);
  return snap ? (
    <span className="text-xs text-emerald-600 dark:text-emerald-400">{t("Complete")}</span>
  ) : (
    <span className="text-xs text-muted-foreground">{t("Not complete")}</span>
  );
}

/** Control panel › Backups: copies of storage locations, and restoring from them */
export function BackupsPage() {
  const qc = useQueryClient();
  const { title, icon } = controlPanelItem("backups");
  const searchSettings = useSettingsSearch();
  const [announcement, setAnnouncement] = useState("");
  const q = useBackups(1500, 10_000, (changes) => {
    const said = changes.flatMap(({ job: j }) => {
      if (j.state === "done") return [t("{what} \"{name}\": done.", { what: BACKUP_JOB_KIND_LABEL[j.kind], name: j.label })];
      if (j.state === "failed") return [t("{what} \"{name}\" stopped by an error.", { what: BACKUP_JOB_KIND_LABEL[j.kind], name: j.label })];
      return [];
    });
    if (said.length) setAnnouncement(said.join(" "));
  });
  const sets = q.data?.sets ?? [];
  const jobs = q.data?.jobs ?? [];
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected = sets.find((s) => s.id === selectedId) ?? null;
  const [dialog, setDialog] = useState<{ t: "details" | "restore" | "delete" | "cancel"; set: BackupSet } | null>(null);
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["backups"] });
    invalidateFiles(qc, "admin-drives", "storage-locations");
  };
  const act = async (what: () => Promise<unknown>, done: string) => {
    try {
      await what();
      toast.success(done);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("Operation failed"));
    } finally {
      refresh();
    }
  };
  const jobOf = (s: BackupSet | null) => (s ? activeJob(s, jobs) : null);
  const canPause = (s: BackupSet | null) => ["running", "queued", "waiting"].includes(jobOf(s)?.state ?? "");
  const canResume = (s: BackupSet | null) => ["paused", "failed"].includes(jobOf(s)?.state ?? "");
  const canCancel = (s: BackupSet | null) => !!jobOf(s) && jobOf(s)!.kind !== "remove";
  const idle = (s: BackupSet | null) => !!s && !jobOf(s) && !s.removing;
  const canRestore = (s: BackupSet | null) => idle(s) && !!completeSnapshot(s!);
  const pause = (s: BackupSet) => act(() => api.pauseBackupJob(jobOf(s)!.id), t("\"{name}\" pauses after the item it is working on", { name: s.name }));
  const resume = (s: BackupSet) => act(() => api.resumeBackupJob(jobOf(s)!.id), t("\"{name}\" continues where it stopped", { name: s.name }));
  const verify = (s: BackupSet) => act(() => api.verifyBackup(s.id), t("\"{name}\" is being read back and checked", { name: s.name }));

  const toolbar = (
    <>
      <ToolButton
        icon={ArchiveRestoreIcon}
        label={t("Restore…")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!canRestore(selected)}
        onClick={() => selected && setDialog({ t: "restore", set: selected })}
      />
      <ToolSeparator />
      <ToolButton icon={PauseIcon} label={t("Pause")} disabled={!canPause(selected)} onClick={() => selected && pause(selected)} />
      <ToolButton icon={PlayIcon} label={t("Resume")} disabled={!canResume(selected)} onClick={() => selected && resume(selected)} />
      <ToolButton icon={XIcon} label={t("Cancel")} disabled={!canCancel(selected)} onClick={() => selected && setDialog({ t: "cancel", set: selected })} />
      <ToolSeparator />
      <ToolButton icon={ShieldCheckIcon} label={t("Check the copy")} disabled={!canRestore(selected)} onClick={() => selected && verify(selected)} />
      <ToolButton icon={Trash2Icon} label={t("Delete copy")} disabled={!idle(selected)} onClick={() => selected && setDialog({ t: "delete", set: selected })} />
      <ToolButton icon={InfoIcon} label={t("Details")} disabled={!selected} onClick={() => selected && setDialog({ t: "details", set: selected })} />
      <ToolButton icon={RefreshCwIcon} label={t("Refresh")} onClick={refresh} />
    </>
  );

  const columns: Column<BackupSet>[] = [
    {
      header: t("Copy"),
      cell: (s) => (
        <span className="flex min-w-0 items-center gap-2">
          <ArchiveIcon className="size-4 shrink-0 text-muted-foreground" />
          <span className="truncate">{s.name}</span>
        </span>
      ),
      title: (s) => s.name,
    },
    {
      header: t("From → to"),
      className: "w-[200px] max-md:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) => (
        <span className="flex min-w-0 items-center gap-1">
          <span className="truncate">{s.source_name}</span>
          <ArrowRightIcon className="size-3 shrink-0" aria-label={t("to")} />
          <span className="truncate">{s.dest_name}</span>
        </span>
      ),
      title: (s) => `${s.source_name} → ${s.dest_name}`,
    },
    { header: t("State"), className: "w-[230px]", cell: (s) => <SetState set={s} job={jobOf(s)} />, title: (s) => (jobOf(s)?.error ? tServer(jobOf(s)!.error) : undefined) },
    {
      header: t("As of"),
      className: "w-[150px] max-lg:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) => {
        const snap = completeSnapshot(s);
        return snap?.cutoff ? formatDateTime(snap.cutoff) : "—";
      },
    },
    {
      header: t("Size there"),
      className: "w-[110px] max-lg:hidden",
      cellClassName: "text-muted-foreground tabular-nums",
      cell: (s) => formatBytes(s.bytes),
    },
    {
      header: t("Made"),
      className: "w-[150px] max-xl:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) => formatDateTime(s.created_at),
      title: (s) => t("By {name}", { name: s.created_by_name }),
    },
  ];

  const menu = (s: BackupSet | null) =>
    s ? (
      <>
        <DropdownMenuItem disabled={!canRestore(s)} onClick={() => setDialog({ t: "restore", set: s })}>
          <ArchiveRestoreIcon /> {t("Restore…")}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => setDialog({ t: "details", set: s })}>
          <InfoIcon /> {t("Details")}
        </DropdownMenuItem>
        {canPause(s) && (
          <DropdownMenuItem onClick={() => pause(s)}>
            <PauseIcon /> {t("Pause")}
          </DropdownMenuItem>
        )}
        {canResume(s) && (
          <DropdownMenuItem onClick={() => resume(s)}>
            <PlayIcon /> {t("Resume")}
          </DropdownMenuItem>
        )}
        {canCancel(s) && (
          <DropdownMenuItem onClick={() => setDialog({ t: "cancel", set: s })}>
            <XIcon /> {t("Cancel")}
          </DropdownMenuItem>
        )}
        <DropdownMenuItem disabled={!canRestore(s)} onClick={() => verify(s)}>
          <ShieldCheckIcon /> {t("Check the copy")}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" disabled={!idle(s)} onClick={() => setDialog({ t: "delete", set: s })}>
          <Trash2Icon /> {t("Delete copy")}
        </DropdownMenuItem>
      </>
    ) : (
      <DropdownMenuItem onClick={refresh}>
        <RefreshCwIcon /> {t("Refresh")}
      </DropdownMenuItem>
    );

  const running = jobs.filter(backupJobActive).length;
  const cancelling = dialog?.t === "cancel" ? jobOf(dialog.set) : null;
  return (
    <Frame
      toolbar={toolbar}
      icon={icon as LucideIcon}
      crumbs={[{ label: t("Control panel"), to: "/admin" }, { label: title }]}
      upTo="/admin"
      searchPlaceholder={t("Search settings")}
      onSearch={searchSettings}
      footer={<span>{t("{n} copy|{n} copies", { n: sets.length })} · {t("{n} job not finished|{n} jobs not finished", { n: running })}</span>}
    >
      <DataTable
        label={title}
        rows={sets}
        rowKey={(s) => s.id}
        columns={columns}
        fixed
        loading={q.isLoading}
        error={q.error}
        onRetry={() => q.refetch()}
        selectedKey={selectedId}
        onSelect={setSelectedId}
        onOpen={(s) => setDialog({ t: "details", set: s })}
        menu={menu}
        empty={<EmptyState icon={ArchiveIcon} title={t("No copies yet")} hint={t("Copy a location from Control panel › Storage locations › Copy everything to….")} />}
      />
      <div className="sr-only" role="status" aria-live="polite" aria-atomic="true">
        {announcement}
      </div>
      {dialog?.t === "details" && (
        <SetDetails set={sets.find((s) => s.id === dialog.set.id) ?? dialog.set} jobs={jobs.filter((j) => j.set_id === dialog.set.id)} onClose={() => setDialog(null)} />
      )}
      {dialog?.t === "restore" && completeSnapshot(dialog.set) && (
        <RestoreDialog set={dialog.set} snapshot={completeSnapshot(dialog.set)!} onClose={() => setDialog(null)} onDone={refresh} />
      )}
      {dialog?.t === "delete" && (
        <ConfirmDialog
          title={t("Delete the copy \"{name}\"?", { name: dialog.set.name })}
          description={t("Its folder on {dest} is deleted, with everything in it; it can't be restored from any more. Nothing on {source} changes.", {
            dest: dialog.set.dest_name,
            source: dialog.set.source_name,
          })}
          confirmText={t("Delete copy")}
          destructive
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            await api.deleteBackup(dialog.set.id);
            toast.success(t("\"{name}\" is being deleted in the background", { name: dialog.set.name }));
            setDialog(null);
            refresh();
          }}
        />
      )}
      {dialog?.t === "cancel" && cancelling && (
        <ConfirmDialog
          title={t("Cancel \"{name}\"?", { name: cancelling.kind === "restore" ? cancelling.label : dialog.set.name })}
          description={
            cancelling.kind === "snapshot"
              ? completeSnapshot(dialog.set)
                ? t("It stops; the copy stays as it was.")
                : t("It stops, and what was already copied to {dest} is removed. Nothing on {source} changes.", { dest: dialog.set.dest_name, source: dialog.set.source_name })
              : cancelling.kind === "restore"
                ? t("It stops. What was already restored stays in its new folder, where it can be deleted.")
                : t("It stops. Nothing changes.")
          }
          confirmText={t("Cancel job")}
          destructive
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            await api.cancelBackupJob(cancelling.id);
            toast.success(t("Cancelled"));
            setDialog(null);
            refresh();
          }}
        />
      )}
    </Frame>
  );
}

/** Everything about a copy: where from and to, what it holds, and its jobs */
function SetDetails({ set: s, jobs, onClose }: { set: BackupSet; jobs: BackupJob[]; onClose(): void }) {
  const snap = completeSnapshot(s);
  const rows: [string, string][] = [
    [t("From"), s.source_name],
    [t("To"), s.dest_name],
    [t("Made"), `${formatDateTime(s.created_at)} · ${t("By {name}", { name: s.created_by_name })}`],
    ...(snap
      ? ([
          [t("As of"), snap.cutoff ? formatDateTime(snap.cutoff) : "—"],
          [t("Holds"), `${t("{n} file|{n} files", { n: snap.files })} · ${t("{n} folder|{n} folders", { n: snap.folders })} · ${t("{n} earlier version|{n} earlier versions", { n: snap.versions })}`],
          [t("Size"), t("{size} of files; {stored} kept there (each content once)", { size: formatBytes(snap.logical_bytes), stored: formatBytes(s.bytes) })],
        ] as [string, string][])
      : []),
  ];
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{s.name}</DialogTitle>
          <DialogDescription>
            {s.source_name} → {s.dest_name}
          </DialogDescription>
        </DialogHeader>
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          {rows.map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="text-muted-foreground">{k}</dt>
              <dd className="min-w-0 break-words">{v}</dd>
            </div>
          ))}
        </dl>
        {snap && (
          <div className="grid gap-1">
            <p className="text-sm">{t("Spaces in the copy")}</p>
            <ul className="max-h-36 divide-y overflow-y-auto rounded-md border text-xs">
              {snap.spaces.map((sp) => {
                const Icon = DRIVE_ICON[sp.kind];
                return (
                  <li key={sp.id} className="flex items-center gap-2 px-3 py-1.5">
                    <Icon className="size-3.5 shrink-0 text-muted-foreground" />
                    <span className="min-w-0 flex-1 truncate">{backupSpaceLabel(sp)}</span>
                    <span className="text-muted-foreground tabular-nums">{t("{n} file|{n} files", { n: sp.files })} · {formatBytes(sp.bytes)}</span>
                  </li>
                );
              })}
            </ul>
          </div>
        )}
        {jobs.length > 0 && (
          <div className="grid gap-1">
            <p className="text-sm">{t("Jobs")}</p>
            <ul className="max-h-64 divide-y overflow-y-auto rounded-md border text-xs">
              {jobs.map((j) => (
                <li key={j.id} className="grid gap-1 px-3 py-2">
                  <span className="flex items-center gap-2">
                    <span className="font-medium">{BACKUP_JOB_KIND_LABEL[j.kind]}</span>
                    {j.kind === "restore" && (
                      <span className="min-w-0 truncate text-muted-foreground">
                        {j.target ? t("{space} into {target}", { space: j.label, target: j.target }) : j.label}
                      </span>
                    )}
                    <span className={cn("ml-auto shrink-0", j.state === "failed" ? "text-destructive" : j.state === "done" ? "text-emerald-600 dark:text-emerald-400" : "text-muted-foreground")}>
                      {BACKUP_JOB_STATE_LABEL[j.state]}
                    </span>
                  </span>
                  {backupJobActive(j) && <JobProgress j={j} />}
                  <span className="text-muted-foreground">
                    {formatDateTime(j.created_at)} · {t("By {name}", { name: j.created_by_name })}
                    {j.finished_at ? ` · ${t("Finished {time}", { time: formatDateTime(j.finished_at) })}` : ""}
                  </span>
                  {j.error && <span className="text-destructive">{tServer(j.error)}</span>}
                  {j.note && <span className="whitespace-pre-line">{j.note.split("\n").map((l) => tServer(l)).join("\n")}</span>}
                  {j.failures.length > 0 && (
                    <ul className="grid gap-0.5 rounded bg-muted/50 px-2 py-1">
                      {j.failures.map((f, i) => (
                        <li key={i} className="grid">
                          <span className="truncate">{f.item ?? t("An item of a personal space")}</span>
                          <span className="text-muted-foreground">{tServer(f.error)}</span>
                        </li>
                      ))}
                    </ul>
                  )}
                </li>
              ))}
            </ul>
          </div>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t("Close")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * Restoring a space of a copy: into a new folder of the space itself, or of another company or team space. A
 * personal space only goes back into its owner's personal space, whose files administrators don't see.
 */
function RestoreDialog({ set, snapshot, onClose, onDone }: { set: BackupSet; snapshot: BackupSnapshot; onClose(): void; onDone(): void }) {
  const [space, setSpace] = useState(snapshot.spaces[0]?.id ?? "");
  const [target, setTarget] = useState<string | null>(null);
  const [trash, setTrash] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const tz = new Date().getTimezoneOffset();
  const preview = useQuery({
    queryKey: ["restore-preview", snapshot.id, space, target],
    queryFn: () => api.restorePreview(snapshot.id, { space, target_drive: target, tz }),
    enabled: !!space,
    retry: false,
  });
  const p = preview.data;
  const chosen = snapshot.spaces.find((s) => s.id === space);
  // A name can't hold "/" or ":", which dates in some languages have: 2026-10-01 14.05 in every language
  const when = (() => {
    if (!snapshot.cutoff) return "";
    const d = new Date(snapshot.cutoff * 1000);
    const two = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())} ${two(d.getHours())}.${two(d.getMinutes())}`;
  })();
  const folderName = chosen ? t("Restored {name} {date}", { name: chosen.name, date: when }) : "";
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <form
          className="grid gap-4"
          onSubmit={async (e) => {
            e.preventDefault();
            if (!p?.target_drive) return;
            setBusy(true);
            setError(null);
            try {
              await api.restoreBackup(snapshot.id, { space, target_drive: p.target_drive, trash, tz, folder_name: folderName });
              toast.success(t("\"{name}\" is being restored in the background", { name: chosen ? backupSpaceLabel(chosen) : "" }));
              onDone();
              onClose();
            } catch (err) {
              setError(err instanceof Error ? err.message : t("Couldn't make the change"));
            } finally {
              setBusy(false);
            }
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("Restore from \"{name}\"", { name: set.name })}</DialogTitle>
            <DialogDescription>
              {snapshot.cutoff ? t("The copy shows the spaces as they were on {time}.", { time: formatDateTime(snapshot.cutoff) }) : null}
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="restore-space">{t("Space to restore")}</Label>
            <NativeSelect
              id="restore-space"
              size="lg"
              value={space}
              onChange={(e) => {
                setSpace(e.target.value);
                setTarget(null);
              }}
            >
              {snapshot.spaces.map((s) => (
                <option key={s.id} value={s.id}>
                  {backupSpaceLabel(s)} · {t("{n} file|{n} files", { n: s.files })}
                </option>
              ))}
            </NativeSelect>
          </div>
          {preview.isLoading && (
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <Loader2Icon className="size-3.5 animate-spin" /> {t("Loading…")}
            </div>
          )}
          {preview.error && <ErrorText>{preview.error instanceof Error ? preview.error.message : t("Operation failed")}</ErrorText>}
          {p && (
            <>
              {p.space.kind !== "personal" ? (
                <div className="grid gap-2">
                  <Label htmlFor="restore-target">{t("Restore into")}</Label>
                  <NativeSelect id="restore-target" size="lg" value={p.target_drive ?? ""} onChange={(e) => setTarget(e.target.value)}>
                    <option value="" disabled>
                      {t("Choose a space")}
                    </option>
                    {p.targets.map((d) => (
                      <option key={d.id} value={d.id}>
                        {d.name}
                        {d.id === p.space.id ? ` (${t("the same space")})` : ""}
                      </option>
                    ))}
                  </NativeSelect>
                </div>
              ) : (
                p.target_name && <p className="text-sm">{t("It goes back into {name}'s personal space.", { name: p.space.owner })}</p>
              )}
              <label className="flex items-center gap-2 text-sm">
                <Checkbox checked={trash} onCheckedChange={(v) => setTrash(v === true)} />
                {t("Also restore what was in the trash")}
              </label>
              <div className="grid gap-1 text-xs text-muted-foreground">
                <p>
                  {t("Everything goes into a new folder, \"{folder}\", at the top of the space: nothing already there is replaced.", { folder: folderName })}
                </p>
                <p>{t("The folder takes the space's permissions. Earlier versions and the access recorded in the copy aren't restored.")}</p>
                {p.space.kind === "personal" && <p>{t("Administrators don't see the files of personal spaces: the whole space is restored, and only its owner sees it.")}</p>}
              </div>
              {p.problem && <ErrorText>{tServer(p.problem)}</ErrorText>}
            </>
          )}
          <ErrorText>{error}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !p?.target_drive || !!p?.problem}>
              {busy && <Loader2Icon className="animate-spin" />}
              {t("Restore")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
