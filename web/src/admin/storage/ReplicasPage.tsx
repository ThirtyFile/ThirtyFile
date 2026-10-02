import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useSearchParams } from "react-router";
import {
  AlertTriangleIcon,
  ArrowRightIcon,
  CopyCheckIcon,
  CrownIcon,
  InfoIcon,
  Loader2Icon,
  PauseIcon,
  PlayIcon,
  PlusIcon,
  RefreshCwIcon,
  Settings2Icon,
  ShieldCheckIcon,
  Trash2Icon,
  XIcon,
  type LucideIcon,
} from "lucide-react";
import { toast } from "sonner";
import { api, type ReplicaJob, type ReplicaPolicy } from "@/api";
import { keys } from "@/api/queryKeys";
import { DataTable, EmptyState, type Column } from "@/components/DataTable";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { ConfirmDialog, ErrorText } from "@/components/dialogs";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { ReplicaPolicyDialog } from "@/admin/storage/ReplicaPolicyDialog";
import { BACKUP_JOB_STATE_LABEL } from "@/admin/storage/backups";
import { controlPanelItem, useSettingsSearch } from "@/admin/controlPanel";
import { t, tServer } from "@/lib/i18n";
import { invalidateFiles } from "@/lib/queries";
import {
  REPLICA_HEALTH_LABEL,
  REPLICA_HEALTH_TONE,
  REPLICA_JOB_KIND_LABEL,
  REPLICA_STATE_LABEL,
  REPLICA_STATE_TONE,
  replicaJobActive,
  targetJob,
  useReplicas,
} from "@/admin/storage/replicas";
import { cn, formatBytes, formatDateTime, errorMessage } from "@/lib/utils";

/** How long ago, roughly */
function ago(at: number) {
  const s = Math.max(0, Math.round(Date.now() / 1000 - at));
  if (s < 90) return t("{n} s|{n} s", { n: s });
  if (s < 5400) return t("{n} min|{n} min", { n: Math.round(s / 60) });
  if (s < 172_800) return t("{n} h|{n} h", { n: Math.round(s / 3600) });
  return t("{n} day|{n} days", { n: Math.round(s / 86400) });
}

/** A job's progress: a bar with what is done, in files and bytes */
function JobProgress({ j }: { j: ReplicaJob }) {
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

/** How a policy is doing: its worst state, and how many targets are current of the copies wanted */
function PolicyState({ p, jobs }: { p: ReplicaPolicy; jobs: ReplicaJob[] }) {
  const running = jobs.find((j) => j.policy_id === p.id && j.state === "running");
  const h = p.health;
  return (
    <span className="grid min-w-0 gap-0.5">
      <span className={cn("truncate text-xs", running ? "text-brand" : REPLICA_HEALTH_TONE[h.state])}>
        {running ? REPLICA_JOB_KIND_LABEL[running.kind] : REPLICA_HEALTH_LABEL[h.state]} · {t("{current} of {wanted} copies current", { current: h.current, wanted: h.wanted })}
      </span>
      {running && <JobProgress j={running} />}
      {!running && h.source_offline && <span className="truncate text-[11px] text-destructive">{t("{reason}: files are read from copies", { reason: tServer(h.source_offline) })}</span>}
    </span>
  );
}

/** Control panel › Replicas: the spaces of a location kept on other locations too, and promoting one of them */
export function ReplicasPage() {
  const qc = useQueryClient();
  const { title, icon } = controlPanelItem("replicas");
  const searchSettings = useSettingsSearch();
  const [params, setParams] = useSearchParams();
  const q = useReplicas(1500);
  const policies = q.data?.policies ?? [];
  const jobs = q.data?.jobs ?? [];
  const unneeded = q.data?.unneeded ?? [];
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected = policies.find((p) => p.id === selectedId) ?? null;
  const [dialog, setDialog] = useState<{ t: "details" | "settings" | "delete" | "promote"; id: string; target?: string } | null>(null);
  const [purging, setPurging] = useState<[string, string, number, number] | null>(null);
  // "Replicate…" on a storage location opens the settings of new replicas of it
  const newFor = params.get("new");
  const [making, setMaking] = useState<string | null>(null);
  const refresh = () => {
    qc.invalidateQueries({ queryKey: keys.replicas() });
    invalidateFiles(qc, "admin-drives", "storage-locations");
  };
  const act = async (what: () => Promise<unknown>, done: string) => {
    try {
      await what();
      toast.success(done);
    } catch (e) {
      toast.error(errorMessage(e, t("Operation failed")));
    } finally {
      refresh();
    }
  };
  const syncNow = (p: ReplicaPolicy) => act(() => api.syncReplicas(p.id), t('"{name}": copies are being brought up to date', { name: p.name }));
  const verify = (p: ReplicaPolicy) => act(() => api.verifyReplicas(p.id), t('"{name}": copies are being read back and checked', { name: p.name }));
  const setEnabled = (p: ReplicaPolicy, enabled: boolean) =>
    act(() => api.updateReplicaPolicy(p.id, { enabled }), enabled ? t('"{name}" replicates again', { name: p.name }) : t('"{name}" is paused; its copies stay', { name: p.name }));
  const open = (t_: "details" | "settings" | "delete" | "promote", p: ReplicaPolicy | null) => p && setDialog({ t: t_, id: p.id });
  const current = dialog ? policies.find((p) => p.id === dialog.id) : null;

  const toolbar = (
    <>
      <ToolButton icon={PlusIcon} label={t("New replicas…")} showLabel className="h-9 px-2.5 text-[13px]" onClick={() => setMaking("")} />
      <ToolButton icon={CrownIcon} label={t("Promote…")} showLabel className="h-9 px-2.5 text-[13px]" disabled={!selected} onClick={() => open("promote", selected)} />
      <ToolSeparator />
      <ToolButton icon={RefreshCwIcon} label={t("Sync now")} disabled={!selected?.enabled} onClick={() => selected && syncNow(selected)} />
      <ToolButton icon={ShieldCheckIcon} label={t("Check the copies")} disabled={!selected} onClick={() => selected && verify(selected)} />
      <ToolButton
        icon={selected?.enabled === false ? PlayIcon : PauseIcon}
        label={selected?.enabled === false ? t("Resume replicating") : t("Pause replicating")}
        disabled={!selected}
        onClick={() => selected && setEnabled(selected, !selected.enabled)}
      />
      <ToolButton icon={Settings2Icon} label={t("Replica settings")} disabled={!selected} onClick={() => open("settings", selected)} />
      <ToolSeparator />
      <ToolButton icon={Trash2Icon} label={t("Delete replica policy")} disabled={!selected} onClick={() => open("delete", selected)} />
      <ToolButton icon={InfoIcon} label={t("Details")} disabled={!selected} onClick={() => open("details", selected)} />
      <ToolButton icon={RefreshCwIcon} label={t("Refresh")} onClick={refresh} />
    </>
  );

  const columns: Column<ReplicaPolicy>[] = [
    {
      header: t("Name"),
      cell: (p) => (
        <span className="flex min-w-0 items-center gap-2">
          <CopyCheckIcon className="size-4 shrink-0 text-muted-foreground" />
          <span className="truncate">{p.name}</span>
        </span>
      ),
    },
    {
      header: t("From → to"),
      className: "w-[260px] max-md:hidden",
      cellClassName: "text-muted-foreground",
      cell: (p) => (
        <span className="flex min-w-0 items-center gap-1">
          <span className="truncate">{p.source_name}</span>
          <ArrowRightIcon className="size-3 shrink-0" aria-label={t("to")} />
          <span className="truncate">{p.targets.map((x) => x.name).join(", ")}</span>
        </span>
      ),
      title: (p) => `${p.source_name} → ${p.targets.map((x) => x.name).join(", ")}`,
    },
    {
      header: t("State"),
      className: "w-[250px]",
      cell: (p) => <PolicyState p={p} jobs={jobs} />,
      title: (p) => {
        const e = p.health.targets.find((x) => x.error)?.error;
        return e ? tServer(e) : undefined;
      },
    },
    {
      header: t("Spaces"),
      className: "w-[90px] max-lg:hidden",
      cellClassName: "text-muted-foreground tabular-nums",
      cell: (p) => p.replicated,
    },
    {
      header: t("Last synced"),
      className: "w-[150px] max-xl:hidden",
      cellClassName: "text-muted-foreground",
      cell: (p) => {
        const last = Math.max(0, ...p.targets.map((x) => x.synced_at ?? 0));
        return last ? formatDateTime(last) : "—";
      },
    },
  ];

  const menu = (p: ReplicaPolicy | null) =>
    p ? (
      <>
        <DropdownMenuItem onClick={() => open("details", p)}>
          <InfoIcon /> {t("Details")}
        </DropdownMenuItem>
        <DropdownMenuItem disabled={!p.enabled} onClick={() => syncNow(p)}>
          <RefreshCwIcon /> {t("Sync now")}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => verify(p)}>
          <ShieldCheckIcon /> {t("Check the copies")}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => setEnabled(p, !p.enabled)}>
          {p.enabled ? <PauseIcon /> : <PlayIcon />} {p.enabled ? t("Pause replicating") : t("Resume replicating")}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => open("settings", p)}>
          <Settings2Icon /> {t("Replica settings")}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => open("promote", p)}>
          <CrownIcon /> {t("Promote…")}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" onClick={() => open("delete", p)}>
          <Trash2Icon /> {t("Delete replica policy")}
        </DropdownMenuItem>
      </>
    ) : (
      <>
        <DropdownMenuItem onClick={() => setMaking("")}>
          <PlusIcon /> {t("New replicas…")}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={refresh}>
          <RefreshCwIcon /> {t("Refresh")}
        </DropdownMenuItem>
      </>
    );

  const running = jobs.filter(replicaJobActive).length;
  const opening = making ?? newFor;
  return (
    <Frame
      toolbar={toolbar}
      icon={icon as LucideIcon}
      crumbs={[{ label: t("Control panel"), to: "/admin" }, { label: title }]}
      upTo="/admin"
      searchPlaceholder={t("Search settings")}
      onSearch={searchSettings}
      footer={
        <span>
          {t("{n} replica policy|{n} replica policies", { n: policies.length })} · {t("{n} job not finished|{n} jobs not finished", { n: running })}
        </span>
      }
    >
      {unneeded.length > 0 && (
        <div className="mx-3 mt-3 grid gap-1 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-900 dark:text-amber-100" role="note">
          {unneeded.map((u) => (
            <div key={u[0]} className="flex flex-wrap items-center gap-2">
              <AlertTriangleIcon className="size-3.5 shrink-0" />
              <span className="min-w-0 flex-1">{t("{location} keeps {n} copies no replica policy wants any more ({size}).", { location: u[1], n: u[2], size: formatBytes(u[3]) })}</span>
              <Button size="sm" variant="outline" className="h-7" onClick={() => setPurging(u)}>
                {t("Remove them…")}
              </Button>
            </div>
          ))}
        </div>
      )}
      <DataTable
        label={title}
        rows={policies}
        rowKey={(p) => p.id}
        columns={columns}
        fixed
        loading={q.isLoading}
        error={q.error}
        onRetry={() => q.refetch()}
        selectedKey={selectedId}
        onSelect={setSelectedId}
        onOpen={(p) => open("details", p)}
        menu={menu}
        empty={
          <EmptyState
            icon={CopyCheckIcon}
            title={t("No replicas yet")}
            hint={t("Keep the files of a location on other locations too with New replicas…, so they can still be read when it fails.")}
          />
        }
      />
      {opening !== null && (
        <ReplicaPolicyDialog
          source={opening || undefined}
          onClose={() => {
            setMaking(null);
            if (newFor !== null) setParams({}, { replace: true });
          }}
          onDone={refresh}
        />
      )}
      {dialog?.t === "settings" && current && <ReplicaPolicyDialog policy={current} onClose={() => setDialog(null)} onDone={refresh} />}
      {dialog?.t === "details" && current && (
        <PolicyDetails
          p={current}
          jobs={jobs.filter((j) => j.policy_id === current.id)}
          onClose={() => setDialog(null)}
          onPromote={(target) => setDialog({ t: "promote", id: current.id, target })}
          onChanged={refresh}
        />
      )}
      {dialog?.t === "promote" && current && <PromoteDialog p={current} target={dialog.target} onClose={() => setDialog(null)} onDone={refresh} />}
      {dialog?.t === "delete" && current && (
        <ConfirmDialog
          title={t('Delete the replica policy "{name}"?', { name: current.name })}
          description={t(
            "No more copies are made. The copies already made stay on {targets}, kept from deletion, until you remove them here as copies no policy wants. The spaces on {source} don't change.",
            { targets: current.targets.map((x) => x.name).join(", "), source: current.source_name },
          )}
          confirmText={t("Delete replica policy")}
          destructive
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            await api.deleteReplicaPolicy(current.id);
            toast.success(t('"{name}" was deleted', { name: current.name }));
            setDialog(null);
            refresh();
          }}
        />
      )}
      {purging && (
        <ConfirmDialog
          title={t("Remove the copies on {location} no policy wants?", { location: purging[1] })}
          description={t(
            "{n} copies ({size}) are deleted from {location}. Each is checked again first: a copy a policy wants, content that is the original there, and copies of folder spaces that can't be reached now stay. The files on the other locations don't change.",
            { n: purging[2], size: formatBytes(purging[3]), location: purging[1] },
          )}
          confirmText={t("Remove copies")}
          destructive
          onClose={() => setPurging(null)}
          onConfirm={async () => {
            const r = await api.purgeReplicas(purging[0]);
            toast.success(t("{n} copy was removed|{n} copies were removed", { n: r.removed }));
            setPurging(null);
            refresh();
          }}
        />
      )}
    </Frame>
  );
}

/** A policy's targets one by one, with what they hold and their jobs */
function PolicyDetails({ p, jobs, onClose, onPromote, onChanged }: { p: ReplicaPolicy; jobs: ReplicaJob[]; onClose(): void; onPromote(target: string): void; onChanged(): void }) {
  const act = async (what: () => Promise<unknown>, done: string) => {
    try {
      await what();
      toast.success(done);
    } catch (e) {
      toast.error(errorMessage(e, t("Operation failed")));
    } finally {
      onChanged();
    }
  };
  const rows: [string, string][] = [
    [t("Replicates"), p.source_name],
    [t("Made"), `${formatDateTime(p.created_at)} · ${t("By {name}", { name: p.created_by_name })}`],
    [t("Copies"), t("{n} besides the original", { n: p.copies })],
    [t("Spaces"), p.all_spaces ? t("Every space on the location ({n} now)", { n: p.replicated }) : t("{n} chosen space|{n} chosen spaces", { n: p.replicated })],
    [t("Reading from copies"), p.read_fallback ? t("When the location can't be read") : t("Off")],
    [t("Checked"), p.verify_days ? t("Every {n} day|Every {n} days", { n: p.verify_days }) : t("Only when asked")],
  ];
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{p.name}</DialogTitle>
          <DialogDescription>
            {REPLICA_HEALTH_LABEL[p.health.state]} · {t("{current} of {wanted} copies current", { current: p.health.current, wanted: p.health.wanted })}
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
        {p.health.source_offline && (
          <ErrorText>{t("{reason}: files are read from copies. If it is lost for good, promote a location that holds them.", { reason: tServer(p.health.source_offline) })}</ErrorText>
        )}
        {p.health.shortfall && <ErrorText>{t("There are fewer working locations than copies wanted: add locations, or keep fewer copies.")}</ErrorText>}
        <div className="grid gap-1">
          <p className="text-sm">{t("Locations with copies")}</p>
          <ul className="divide-y rounded-md border text-xs">
            {p.targets.map((x, i) => {
              const h = p.health.targets.find((y) => y.location_id === x.location_id);
              const job = targetJob(p, x.location_id, jobs);
              return (
                <li key={x.location_id} className="grid gap-1 px-3 py-2">
                  <span className="flex flex-wrap items-center gap-2">
                    <span className="w-4 text-muted-foreground tabular-nums">{i + 1}</span>
                    <span className="font-medium">{x.name}</span>
                    {h && (
                      <span className={cn(REPLICA_STATE_TONE[h.state])}>
                        {REPLICA_STATE_LABEL[h.state]}
                        {h.behind_since ? ` · ${t("{time} behind", { time: ago(h.behind_since) })}` : ""}
                      </span>
                    )}
                    <span className="ml-auto flex gap-1">
                      <Button
                        size="sm"
                        variant="ghost"
                        className="h-7"
                        disabled={!p.enabled}
                        onClick={() => act(() => api.syncReplicas(p.id, x.location_id), t("{name} is being brought up to date", { name: x.name }))}
                      >
                        {t("Sync")}
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        className="h-7"
                        onClick={() => act(() => api.verifyReplicas(p.id, x.location_id), t("The copies on {name} are being checked", { name: x.name }))}
                      >
                        {t("Check")}
                      </Button>
                      <Button size="sm" variant="ghost" className="h-7" onClick={() => onPromote(x.location_id)}>
                        {t("Promote…")}
                      </Button>
                    </span>
                  </span>
                  {h && (
                    <span className="text-muted-foreground">
                      {t("Holds {held} of {wanted} contents", { held: h.held, wanted: h.wanted })}
                      {h.damaged > 0 ? ` · ${t("{n} damaged", { n: h.damaged })}` : ""}
                      {" · "}
                      {x.mode === "realtime" ? t("Soon after changes") : t("On a schedule")}
                      {" · "}
                      {x.synced_at ? t("Synced {time}", { time: formatDateTime(x.synced_at) }) : t("Not synced yet")}
                      {x.last_verify_at ? ` · ${t("Checked {time}", { time: formatDateTime(x.last_verify_at) })}` : ""}
                    </span>
                  )}
                  {h?.error && <span className="text-destructive">{tServer(h.error)}</span>}
                  {job && (
                    <span className="flex items-center gap-2">
                      <span className="min-w-0 flex-1">
                        <span className="text-muted-foreground">
                          {REPLICA_JOB_KIND_LABEL[job.kind]} · {BACKUP_JOB_STATE_LABEL[job.state]}
                        </span>
                        <JobProgress j={job} />
                      </span>
                      {["running", "queued", "waiting"].includes(job.state) && (
                        <Button
                          size="icon"
                          variant="ghost"
                          className="size-7"
                          aria-label={t("Pause")}
                          onClick={() => act(() => api.pauseReplicaJob(job.id), t("Pauses after the item it is copying"))}
                        >
                          <PauseIcon />
                        </Button>
                      )}
                      {["paused", "failed", "waiting"].includes(job.state) && (
                        <Button size="icon" variant="ghost" className="size-7" aria-label={t("Resume")} onClick={() => act(() => api.resumeReplicaJob(job.id), t("Continues"))}>
                          <PlayIcon />
                        </Button>
                      )}
                      <Button size="icon" variant="ghost" className="size-7" aria-label={t("Cancel")} onClick={() => act(() => api.cancelReplicaJob(job.id), t("Cancelled"))}>
                        <XIcon />
                      </Button>
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
        </div>
        {jobs.length > 0 && (
          <div className="grid gap-1">
            <p className="text-sm">{t("Jobs")}</p>
            <ul className="max-h-64 divide-y overflow-y-auto rounded-md border text-xs">
              {jobs.map((j) => (
                <li key={j.id} className="grid gap-1 px-3 py-2">
                  <span className="flex items-center gap-2">
                    <span className="font-medium">{REPLICA_JOB_KIND_LABEL[j.kind]}</span>
                    <span className="min-w-0 truncate text-muted-foreground">{p.targets.find((x) => x.location_id === j.location_id)?.name ?? j.label}</span>
                    <span
                      className={cn("ml-auto shrink-0", j.state === "failed" ? "text-destructive" : j.state === "done" ? "text-emerald-600 dark:text-emerald-400" : "text-muted-foreground")}
                    >
                      {BACKUP_JOB_STATE_LABEL[j.state]}
                    </span>
                  </span>
                  <span className="text-muted-foreground">
                    {formatDateTime(j.created_at)}
                    {j.created_by_name ? ` · ${t("By {name}", { name: j.created_by_name })}` : ""}
                    {j.finished_at ? ` · ${t("Finished {time}", { time: formatDateTime(j.finished_at) })}` : ""}
                  </span>
                  {j.error && <span className="text-destructive">{tServer(j.error)}</span>}
                  {j.note && (
                    <span className="whitespace-pre-line">
                      {j.note
                        .split("\n")
                        .map((l) => tServer(l))
                        .join("\n")}
                    </span>
                  )}
                  {j.failures.length > 0 && (
                    <ul className="grid gap-0.5 rounded bg-muted/50 px-2 py-1">
                      {j.failures.map((f, i) => (
                        <li key={i} className="grid">
                          {f.item && <span className="truncate">{f.item}</span>}
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
 * Promoting a target: its copies become the originals of the policy's spaces, which then read and write there; the
 * location they were on becomes a target, checked before it counts as a copy again. Said in full before it is done.
 */
function PromoteDialog({ p, target: preset, onClose, onDone }: { p: ReplicaPolicy; target?: string; onClose(): void; onDone(): void }) {
  const active = p.targets.filter((x) => x.state === "active");
  const [target, setTarget] = useState(preset ?? active[0]?.location_id ?? "");
  const [accept, setAccept] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pre = useQuery({ queryKey: keys.promotePreflight(p.id, target), queryFn: () => api.promotePreflight(p.id, target), enabled: !!target, retry: false });
  const pf = pre.data;
  const allowed = !!pf && !pf.problem && (!pf.needs_accept || accept);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t("Promote a location")}</DialogTitle>
          <DialogDescription>
            {t(
              "The spaces of {source} then read and write the copies on the location you choose. Do this when {source} has failed or is being retired. Nothing is deleted: {source} keeps what it has, and becomes a location with copies once it is checked.",
              {
                source: p.source_name,
              },
            )}
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-2">
          <Label htmlFor="promote-target">{t("Promote")}</Label>
          <NativeSelect
            id="promote-target"
            size="lg"
            value={target}
            onChange={(e) => {
              setTarget(e.target.value);
              setAccept(false);
            }}
          >
            {p.targets.map((x) => (
              <option key={x.location_id} value={x.location_id} disabled={x.state !== "active"}>
                {x.name}
              </option>
            ))}
          </NativeSelect>
        </div>
        {pre.isLoading && (
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2Icon className="size-4 animate-spin" /> {t("Checking both locations…")}
          </p>
        )}
        {pre.error && <ErrorText>{errorMessage(pre.error, t("Operation failed"))}</ErrorText>}
        {pf && (
          <div className="grid gap-2 text-sm">
            <ul className="grid gap-1">
              <li>
                {pf.source_reachable ? (
                  t("{name} can be reached.", { name: pf.source_name })
                ) : (
                  <span className="text-destructive">{t("{name} can't be reached.", { name: pf.source_name })}</span>
                )}
              </li>
              <li>
                {pf.target_reachable ? (
                  t("{name} can be reached.", { name: pf.target_name })
                ) : (
                  <span className="text-destructive">{t("{name} can't be reached.", { name: pf.target_name })}</span>
                )}
                {pf.target_state !== "unknown" && ` ${REPLICA_STATE_LABEL[pf.target_state]}`}
                {pf.behind_since ? ` · ${t("{time} behind", { time: ago(pf.behind_since) })}` : ""}
              </li>
              <li>{t("{n} content moves to {name} ({size}).|{n} contents move to {name} ({size}).", { n: pf.moved, name: pf.target_name, size: formatBytes(pf.moved_bytes) })}</li>
              {pf.missing > 0 && (
                <li className="text-destructive">
                  {t("{n} contents ({size}) aren't on {name}: their files can't be read until {source} is back.", {
                    n: pf.missing,
                    size: formatBytes(pf.missing_bytes),
                    name: pf.target_name,
                    source: pf.source_name,
                  })}
                </li>
              )}
            </ul>
            {pf.spaces.length > 0 && (
              <div className="grid gap-1">
                <p>{t("Spaces that move")}</p>
                <ul className="max-h-32 divide-y overflow-y-auto rounded-md border text-xs">
                  {pf.spaces.map((s) => (
                    <li key={s} className="px-3 py-1.5">
                      {s}
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {pf.folder_spaces.length > 0 && (
              <p className="text-xs">
                {t("Folder spaces not wholly on {name} stay on {source}, as they are: {names}", { name: pf.target_name, source: pf.source_name, names: pf.folder_spaces.join(", ") })}
              </p>
            )}
            {pf.problem && <ErrorText>{tServer(pf.problem)}</ErrorText>}
            {pf.needs_accept && !pf.problem && (
              <label className="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2">
                <Checkbox className="mt-0.5" checked={accept} onCheckedChange={(v) => setAccept(v === true)} />
                <span>{t("I understand that files whose content isn't on {name} can't be read until {source} is back.", { name: pf.target_name, source: pf.source_name })}</span>
              </label>
            )}
            <p className="text-xs text-muted-foreground">
              {t(
                "Running copies stop. {source} isn't written to by this server any more, and files are never moved back on their own: to go back, promote {source} later, once it is current again.",
                {
                  source: pf.source_name,
                },
              )}
            </p>
          </div>
        )}
        <ErrorText>{error}</ErrorText>
        <DialogFooter>
          <Button type="button" variant="outline" onClick={onClose}>
            {t("Cancel")}
          </Button>
          <Button
            variant="destructive"
            disabled={busy || !allowed}
            onClick={async () => {
              setBusy(true);
              setError(null);
              try {
                const r = await api.promoteReplica(p.id, target, accept);
                toast.success(t("{name} holds the spaces now ({n} contents)", { name: pf!.target_name, n: r.moved }));
                onDone();
                onClose();
              } catch (e) {
                setError(errorMessage(e, t("Operation failed")));
                void pre.refetch();
              } finally {
                setBusy(false);
              }
            }}
          >
            {busy && <Loader2Icon className="animate-spin" />}
            {t("Promote")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
