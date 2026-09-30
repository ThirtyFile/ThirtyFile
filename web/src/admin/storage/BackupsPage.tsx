import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useSearchParams } from "react-router";
import {
  ArchiveIcon,
  ArchiveRestoreIcon,
  ArrowRightIcon,
  DatabaseBackupIcon,
  FolderSearchIcon,
  InfoIcon,
  PauseIcon,
  PlayIcon,
  PlusIcon,
  RefreshCwIcon,
  Settings2Icon,
  ShieldCheckIcon,
  Trash2Icon,
  UploadCloudIcon,
  XIcon,
  type LucideIcon,
} from "lucide-react";
import { toast } from "sonner";
import { api, backupJobActive, type BackupHealthState, type BackupJob, type BackupSet } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { DataTable, EmptyState, type Column } from "@/components/DataTable";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { ConfirmDialog, ErrorText } from "@/components/dialogs";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { BackupPolicyDialog, zonedTime } from "@/admin/storage/BackupPolicyDialog";
import { locationLabel } from "@/components/LocationSelect";
import { RestoreDialog } from "@/admin/storage/RestoreDialog";
import { DRIVE_ICON } from "@/lib/drives";
import { activeJob, BACKUP_JOB_KIND_LABEL, BACKUP_JOB_STATE_LABEL, backupSpaceLabel, completeSnapshot, useBackups } from "@/admin/storage/backups";
import { controlPanelItem, useSettingsSearch } from "@/admin/controlPanel";
import { t, tServer } from "@/lib/i18n";
import { invalidateFiles } from "@/lib/queries";
import { cn, formatBytes, formatDateTime } from "@/lib/utils";

export const HEALTH_LABEL: Record<BackupHealthState, string> = {
  protected: t("Protected"),
  catching_up: t("Changes waiting"),
  running: t("Backing up"),
  waiting: t("Waiting for the location"),
  failing: t("Failing"),
  overdue: t("Overdue"),
  paused: t("Paused"),
  never: t("No snapshot yet"),
};

const HEALTH_TONE: Record<BackupHealthState, string> = {
  protected: "text-emerald-600 dark:text-emerald-400",
  catching_up: "text-brand",
  running: "text-brand",
  waiting: "text-amber-600 dark:text-amber-400",
  failing: "text-destructive",
  overdue: "text-destructive",
  paused: "text-muted-foreground",
  never: "text-muted-foreground",
};

/** How long ago, roughly */
function ago(at: number) {
  const s = Math.max(0, Math.round(Date.now() / 1000 - at));
  if (s < 90) return t("{n} s|{n} s", { n: s });
  if (s < 5400) return t("{n} min|{n} min", { n: Math.round(s / 60) });
  if (s < 172_800) return t("{n} h|{n} h", { n: Math.round(s / 3600) });
  return t("{n} day|{n} days", { n: Math.round(s / 86400) });
}

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

/** What a copy or backup is doing, or how it is */
function SetState({ set, job }: { set: BackupSet; job: BackupJob | null }) {
  if (set.removing) return <span className="text-xs text-muted-foreground">{t("Being deleted")}</span>;
  const h = set.policy?.health;
  const head = h ? (
    <span className={cn("truncate text-xs", HEALTH_TONE[h.state])}>
      {HEALTH_LABEL[h.state]}
      {h.state === "catching_up" && h.behind_since ? ` · ${t("{time} behind", { time: ago(h.behind_since) })}` : ""}
    </span>
  ) : null;
  if (job && job.state !== "failed" && job.state !== "waiting") {
    return (
      <span className="grid min-w-0 gap-0.5">
        <span className={cn("truncate text-xs", job.state === "running" ? "text-brand" : "text-muted-foreground")}>
          {BACKUP_JOB_KIND_LABEL[job.kind]} · {BACKUP_JOB_STATE_LABEL[job.state]}
        </span>
        <JobProgress j={job} />
      </span>
    );
  }
  if (head) return head;
  if (job) return <span className="truncate text-xs text-destructive">{BACKUP_JOB_KIND_LABEL[job.kind]} · {BACKUP_JOB_STATE_LABEL[job.state]}</span>;
  return completeSnapshot(set) ? (
    <span className="text-xs text-emerald-600 dark:text-emerald-400">{t("Complete")}</span>
  ) : (
    <span className="text-xs text-muted-foreground">{t("Not complete")}</span>
  );
}

const KIND_ICON: Record<BackupSet["kind"], LucideIcon> = { copy: ArchiveIcon, policy: DatabaseBackupIcon, imported: UploadCloudIcon };

/** Control panel › Backups: backups made again and again, one-time copies, and restoring from them */
export function BackupsPage() {
  const qc = useQueryClient();
  const { title, icon } = controlPanelItem("backups");
  const searchSettings = useSettingsSearch();
  const [params, setParams] = useSearchParams();
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
  const [dialog, setDialog] = useState<{ t: "details" | "restore" | "delete" | "cancel" | "settings"; set: BackupSet } | null>(null);
  // "Back up…" on a storage location opens the settings of a new backup of it
  const newFor = params.get("new");
  const [making, setMaking] = useState<string | null>(null);
  const [finding, setFinding] = useState(false);
  const refresh = () => {
    qc.invalidateQueries({ queryKey: keys.backups() });
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
  const canResume = (s: BackupSet | null) => ["paused", "failed"].includes(jobOf(s)?.state ?? "") && s?.policy?.enabled !== false;
  const canCancel = (s: BackupSet | null) => !!jobOf(s) && jobOf(s)!.kind !== "remove";
  const idle = (s: BackupSet | null) => !!s && !jobOf(s) && !s.removing;
  const canRestore = (s: BackupSet | null) => !!s && !s.removing && !!completeSnapshot(s);
  const pause = (s: BackupSet) => act(() => api.pauseBackupJob(jobOf(s)!.id), t("\"{name}\" pauses after the item it is working on", { name: s.name }));
  const resume = (s: BackupSet) => act(() => api.resumeBackupJob(jobOf(s)!.id), t("\"{name}\" continues where it stopped", { name: s.name }));
  const verify = (s: BackupSet) => act(() => api.verifyBackup(s.id), t("\"{name}\" is being read back and checked", { name: s.name }));
  const runNow = (s: BackupSet) => act(() => api.runBackupPolicy(s.id), t("\"{name}\": a snapshot is being made", { name: s.name }));
  const setEnabled = (s: BackupSet, enabled: boolean) =>
    act(
      () => api.updateBackupPolicy(s.id, { enabled }),
      enabled ? t("\"{name}\" backs up again", { name: s.name }) : t("\"{name}\" is paused; what it made stays", { name: s.name }),
    );

  const toolbar = (
    <>
      <ToolButton icon={PlusIcon} label={t("New backup…")} showLabel className="h-9 px-2.5 text-[13px]" onClick={() => setMaking("")} />
      <ToolButton
        icon={ArchiveRestoreIcon}
        label={t("Restore…")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!canRestore(selected)}
        onClick={() => selected && setDialog({ t: "restore", set: selected })}
      />
      <ToolSeparator />
      {selected?.policy && (
        <>
          <ToolButton icon={DatabaseBackupIcon} label={t("Back up now")} disabled={!selected.policy.enabled || selected.removing} onClick={() => runNow(selected)} />
          <ToolButton icon={Settings2Icon} label={t("Backup settings")} disabled={selected.removing} onClick={() => setDialog({ t: "settings", set: selected })} />
        </>
      )}
      <ToolButton icon={PauseIcon} label={t("Pause")} disabled={!canPause(selected)} onClick={() => selected && pause(selected)} />
      <ToolButton icon={PlayIcon} label={t("Resume")} disabled={!canResume(selected)} onClick={() => selected && resume(selected)} />
      <ToolButton icon={XIcon} label={t("Cancel")} disabled={!canCancel(selected)} onClick={() => selected && setDialog({ t: "cancel", set: selected })} />
      <ToolSeparator />
      <ToolButton icon={ShieldCheckIcon} label={t("Check the copy")} disabled={!idle(selected) || !canRestore(selected)} onClick={() => selected && verify(selected)} />
      <ToolButton icon={Trash2Icon} label={t("Delete copy")} disabled={!idle(selected)} onClick={() => selected && setDialog({ t: "delete", set: selected })} />
      <ToolButton icon={InfoIcon} label={t("Details")} disabled={!selected} onClick={() => selected && setDialog({ t: "details", set: selected })} />
      <ToolButton icon={FolderSearchIcon} label={t("Find backups on a location…")} onClick={() => setFinding(true)} />
      <ToolButton icon={RefreshCwIcon} label={t("Refresh")} onClick={refresh} />
    </>
  );

  const columns: Column<BackupSet>[] = [
    {
      header: t("Name"),
      cell: (s) => {
        const Icon = KIND_ICON[s.kind];
        return (
          <span className="flex min-w-0 items-center gap-2">
            <Icon className="size-4 shrink-0 text-muted-foreground" />
            <span className="truncate">{s.name}</span>
          </span>
        );
      },
      title: (s) => `${s.name} · ${s.kind === "policy" ? t("Backup") : s.kind === "copy" ? t("Copy") : t("Found on a location")}`,
    },
    {
      header: t("From → to"),
      className: "w-[200px] max-md:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) => (
        <span className="flex min-w-0 items-center gap-1">
          <span className="truncate">{s.source_name || "—"}</span>
          <ArrowRightIcon className="size-3 shrink-0" aria-label={t("to")} />
          <span className="truncate">{s.dest_name}</span>
        </span>
      ),
      title: (s) => `${s.source_name} → ${s.dest_name}`,
    },
    {
      header: t("State"),
      className: "w-[230px]",
      cell: (s) => <SetState set={s} job={jobOf(s)} />,
      title: (s) => {
        const e = jobOf(s)?.error ?? s.policy?.health.error;
        return e ? tServer(e) : undefined;
      },
    },
    {
      header: t("Protected as of"),
      className: "w-[150px] max-lg:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) => {
        const snap = completeSnapshot(s);
        return snap?.cutoff ? formatDateTime(snap.cutoff) : "—";
      },
    },
    {
      header: t("Next"),
      className: "w-[150px] max-xl:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) =>
        s.policy?.enabled && s.policy.next_run_at ? zonedTime(s.policy.next_run_at, s.policy.tz) : s.policy?.enabled && s.policy.mode === "realtime" ? t("After changes") : "—",
    },
    {
      header: t("Size there"),
      className: "w-[100px] max-lg:hidden",
      cellClassName: "text-muted-foreground tabular-nums",
      cell: (s) => formatBytes(s.bytes),
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
        {s.policy && (
          <>
            <DropdownMenuItem disabled={!s.policy.enabled || s.removing} onClick={() => runNow(s)}>
              <DatabaseBackupIcon /> {t("Back up now")}
            </DropdownMenuItem>
            <DropdownMenuItem disabled={s.removing} onClick={() => setEnabled(s, !s.policy!.enabled)}>
              {s.policy.enabled ? <PauseIcon /> : <PlayIcon />} {s.policy.enabled ? t("Pause backing up") : t("Resume backing up")}
            </DropdownMenuItem>
            <DropdownMenuItem disabled={s.removing} onClick={() => setDialog({ t: "settings", set: s })}>
              <Settings2Icon /> {t("Backup settings")}
            </DropdownMenuItem>
          </>
        )}
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
        <DropdownMenuItem disabled={!idle(s) || !canRestore(s)} onClick={() => verify(s)}>
          <ShieldCheckIcon /> {t("Check the copy")}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" disabled={!idle(s)} onClick={() => setDialog({ t: "delete", set: s })}>
          <Trash2Icon /> {t("Delete copy")}
        </DropdownMenuItem>
      </>
    ) : (
      <>
        <DropdownMenuItem onClick={() => setMaking("")}>
          <PlusIcon /> {t("New backup…")}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={refresh}>
          <RefreshCwIcon /> {t("Refresh")}
        </DropdownMenuItem>
      </>
    );

  const running = jobs.filter(backupJobActive).length;
  const cancelling = dialog?.t === "cancel" ? jobOf(dialog.set) : null;
  const opening = making ?? newFor;
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
        empty={
          <EmptyState
            icon={DatabaseBackupIcon}
            title={t("No backups yet")}
            hint={t("Make a backup with New backup…, or a one-time copy from Control panel › Storage locations › Copy everything to….")}
          />
        }
      />
      <div className="sr-only" role="status" aria-live="polite" aria-atomic="true">
        {announcement}
      </div>
      {opening !== null && (
        <BackupPolicyDialog
          source={opening || undefined}
          onClose={() => {
            setMaking(null);
            if (newFor !== null) setParams({}, { replace: true });
          }}
          onDone={refresh}
        />
      )}
      {dialog?.t === "settings" && <BackupPolicyDialog set={sets.find((s) => s.id === dialog.set.id) ?? dialog.set} onClose={() => setDialog(null)} onDone={refresh} />}
      {finding && <FindBackupsDialog onClose={() => setFinding(false)} onDone={refresh} />}
      {dialog?.t === "details" && (
        <SetDetails set={sets.find((s) => s.id === dialog.set.id) ?? dialog.set} jobs={jobs.filter((j) => j.set_id === dialog.set.id)} onClose={() => setDialog(null)} />
      )}
      {dialog?.t === "restore" && completeSnapshot(dialog.set) && <RestoreDialog set={dialog.set} onClose={() => setDialog(null)} onDone={refresh} />}
      {dialog?.t === "delete" && (
        <ConfirmDialog
          title={t("Delete the copy \"{name}\"?", { name: dialog.set.name })}
          description={
            dialog.set.policy
              ? t("The backup stops, and its folder on {dest} is deleted with every snapshot in it: it can't be restored from any more. Nothing on {source} changes. To stop making snapshots and keep them, pause it instead.", {
                  dest: dialog.set.dest_name,
                  source: dialog.set.source_name,
                })
              : t("Its folder on {dest} is deleted, with everything in it; it can't be restored from any more. Nothing on {source} changes.", {
                  dest: dialog.set.dest_name,
                  source: dialog.set.source_name,
                })
          }
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

/** "Find backups on a location…": copies and backups kept there that this server doesn't list, made before its
 * database was lost or by another installation */
function FindBackupsDialog({ onClose, onDone }: { onClose(): void; onDone(): void }) {
  const locations = useQuery(queries.storageLocations);
  const [location, setLocation] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <form
          className="grid gap-4"
          onSubmit={async (e) => {
            e.preventDefault();
            setBusy(true);
            setError(null);
            try {
              const r = await api.importBackups(location);
              toast.success(
                r.added.length
                  ? t("{n} backup found and listed; it is being checked|{n} backups found and listed; they are being checked", { n: r.added.length })
                  : t("No backups that aren't listed yet were found there"),
              );
              onDone();
              onClose();
            } catch (err) {
              setError(err instanceof Error ? err.message : t("Operation failed"));
            } finally {
              setBusy(false);
            }
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("Find backups on a location")}</DialogTitle>
            <DialogDescription>
              {t("Lists copies and backups kept on a location that this server doesn't know, for example after its database was lost. Their complete snapshots can then be restored from; a personal space goes into the personal space of the user with the same user name.")}
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="find-location">{t("Location")}</Label>
            <NativeSelect id="find-location" size="lg" value={location} onChange={(e) => setLocation(e.target.value)}>
              <option value="" disabled>
                {t("Choose a location")}
              </option>
              {(locations.data ?? []).map((l) => (
                <option key={l.id} value={l.id} disabled={!l.connected}>
                  {locationLabel(l)}
                </option>
              ))}
            </NativeSelect>
          </div>
          <ErrorText>{error}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !location}>
              {t("Find")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Everything about a copy or backup: where from and to, what it holds, its settings and its jobs */
function SetDetails({ set: s, jobs, onClose }: { set: BackupSet; jobs: BackupJob[]; onClose(): void }) {
  const snap = completeSnapshot(s);
  const p = s.policy;
  const points = s.snapshots.filter((n) => n.state === "complete");
  const rows: [string, string][] = [
    [t("From"), s.source_name || "—"],
    [t("To"), s.dest_name],
    [t("Made"), `${formatDateTime(s.created_at)} · ${t("By {name}", { name: s.created_by_name })}`],
    ...(p
      ? ([
          [t("State"), HEALTH_LABEL[p.health.state] + (p.health.error ? ` · ${tServer(p.health.error)}` : "")],
          [
            t("When"),
            p.mode === "realtime" ? t("Soon after changes") : p.mode === "scheduled" ? t("On a schedule") : t("Soon after changes, and on a schedule"),
          ],
          ...(p.next_run_at ? ([[t("Next"), `${zonedTime(p.next_run_at, p.tz)} (${p.tz})`]] as [string, string][]) : []),
          [t("Kept"), t("{days} days, and always the newest {n}", { days: p.keep_days, n: p.keep_min })],
          ...(p.last_verify_at ? ([[t("Last checked"), formatDateTime(p.last_verify_at)]] as [string, string][]) : []),
        ] as [string, string][])
      : []),
    ...(snap
      ? ([
          [t("Protected as of"), snap.cutoff ? formatDateTime(snap.cutoff) : "—"],
          [t("Holds"), `${t("{n} file|{n} files", { n: snap.files })} · ${t("{n} folder|{n} folders", { n: snap.folders })} · ${t("{n} earlier version|{n} earlier versions", { n: snap.versions })}`],
          [t("Size"), t("{size} of files; {stored} kept there (each content once)", { size: formatBytes(snap.logical_bytes), stored: formatBytes(s.bytes) })],
        ] as [string, string][])
      : []),
  ];
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{s.name}</DialogTitle>
          <DialogDescription>
            {s.source_name || "—"} → {s.dest_name}
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
        {points.length > 1 && (
          <div className="grid gap-1">
            <p className="text-sm">{t("Restore points")}</p>
            <ul className="max-h-32 divide-y overflow-y-auto rounded-md border text-xs">
              {points.map((n) => (
                <li key={n.id} className="flex items-center gap-2 px-3 py-1.5">
                  <span className="flex-1">{n.cutoff ? formatDateTime(n.cutoff) : "—"}</span>
                  <span className="text-muted-foreground tabular-nums">
                    {t("{n} file|{n} files", { n: n.files })} · {formatBytes(n.logical_bytes)}
                  </span>
                </li>
              ))}
            </ul>
          </div>
        )}
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
                    {formatDateTime(j.created_at)}
                    {j.created_by_name ? ` · ${t("By {name}", { name: j.created_by_name })}` : ""}
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
