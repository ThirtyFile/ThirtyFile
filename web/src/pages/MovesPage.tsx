import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowRightIcon, InfoIcon, PauseIcon, PlayIcon, RefreshCwIcon, TruckIcon, XIcon, type LucideIcon } from "lucide-react";
import { toast } from "sonner";
import { api, moveActive, type MoveState, type SpaceMove } from "@/api";
import { DataTable, EmptyState, type Column } from "@/components/DataTable";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { ConfirmDialog } from "@/components/dialogs";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DRIVE_ICON } from "@/lib/drives";
import { controlPanelItem, useSettingsSearch } from "@/lib/controlPanel";
import { t, tServer } from "@/lib/i18n";
import { invalidateFiles } from "@/lib/queries";
import { cn, formatBytes, formatDateTime } from "@/lib/utils";

export const MOVE_STATE_LABEL: Record<MoveState, string> = {
  queued: t("Waiting"),
  running: t("Moving"),
  paused: t("Paused"),
  failed: t("Stopped by an error"),
  done: t("Done"),
  cancelled: t("Cancelled"),
};

/** How a moved space is named: personal spaces by their owner, as they are all called "My files" */
export const moveSpaceLabel = (m: Pick<SpaceMove, "space_kind" | "space_name" | "owner_name">) =>
  m.space_kind === "personal" && m.owner_name ? `${m.space_name} · ${m.owner_name}` : m.space_name;

/** "Local disk → S3" */
export const moveRoute = (m: SpaceMove) => `${m.from_name || t("A folder on the server")} → ${m.to_name}`;

/** Time left at the current speed */
function timeLeft(m: SpaceMove) {
  if (!m.speed || m.speed < 1 || m.bytes_total <= m.bytes_done) return null;
  const s = Math.round((m.bytes_total - m.bytes_done) / m.speed);
  if (s < 60) return t("{s} s left", { s });
  if (s < 3600) return t("{m} min {s} s left", { m: Math.floor(s / 60), s: s % 60 });
  return t("{h} h {m} min left", { h: Math.floor(s / 3600), m: Math.floor((s % 3600) / 60) });
}

/** A move's progress: a bar with what is copied, in files and bytes */
export function MoveProgress({ m, compact }: { m: SpaceMove; compact?: boolean }) {
  const pct = m.bytes_total > 0 ? Math.min(100, (m.bytes_done / m.bytes_total) * 100) : m.state === "done" ? 100 : 0;
  const label = t("{done} of {total} files · {bytes} of {size}", {
    done: m.files_done,
    total: m.files_total,
    bytes: formatBytes(m.bytes_done),
    size: formatBytes(m.bytes_total),
  });
  return (
    <span className="grid min-w-0 gap-0.5">
      <span
        role="progressbar"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(pct)}
        className="h-1.5 overflow-hidden rounded bg-muted"
      >
        <span className={cn("block h-full transition-[width]", m.state === "failed" ? "bg-destructive" : "bg-brand")} style={{ width: `${pct}%` }} />
      </span>
      {!compact && <span className="truncate text-[11px] text-muted-foreground tabular-nums">{label}</span>}
    </span>
  );
}

/** Control panel › Moves: spaces being moved to another storage location, those waiting, and the history */
export function MovesPage() {
  const qc = useQueryClient();
  const { title, icon } = controlPanelItem("moves");
  const searchSettings = useSettingsSearch();
  // Refreshed every second and a half while a move runs or waits, and now and then while one is paused or stopped
  // (it may be resumed elsewhere)
  const q = useQuery({
    queryKey: ["moves"],
    queryFn: api.moves,
    refetchInterval: (query) => {
      const moves = query.state.data?.moves ?? [];
      return moves.some((m) => m.state === "running" || m.state === "queued") ? 1500 : moves.some(moveActive) ? 10_000 : false;
    },
  });
  const moves = q.data?.moves ?? [];
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected = moves.find((m) => m.id === selectedId) ?? null;
  const [dialog, setDialog] = useState<{ t: "details" | "cancel"; move: SpaceMove } | null>(null);
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["moves"] });
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
  const canPause = (m: SpaceMove | null) => m?.state === "running" || m?.state === "queued";
  const canResume = (m: SpaceMove | null) => m?.state === "paused" || m?.state === "failed";
  const canCancel = (m: SpaceMove | null) => !!m && moveActive(m);
  const pause = (m: SpaceMove) => act(() => api.pauseMove(m.id), t("\"{name}\" will pause after the file it is copying", { name: moveSpaceLabel(m) }));
  const resume = (m: SpaceMove) => act(() => api.resumeMove(m.id), t("\"{name}\" continues where it stopped", { name: moveSpaceLabel(m) }));
  const setConcurrency = (n: number) => act(() => api.setMoveConcurrency(n), t("Moves at the same time: {n}", { n }));

  const toolbar = (
    <>
      <ToolButton icon={PauseIcon} label={t("Pause")} showLabel className="h-9 px-2.5 text-[13px]" disabled={!canPause(selected)} onClick={() => selected && pause(selected)} />
      <ToolButton icon={PlayIcon} label={t("Resume")} showLabel className="h-9 px-2.5 text-[13px]" disabled={!canResume(selected)} onClick={() => selected && resume(selected)} />
      <ToolButton
        icon={XIcon}
        label={t("Cancel move")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!canCancel(selected)}
        onClick={() => selected && setDialog({ t: "cancel", move: selected })}
      />
      <ToolSeparator />
      <ToolButton icon={InfoIcon} label={t("Details")} disabled={!selected} onClick={() => selected && setDialog({ t: "details", move: selected })} />
      <ToolButton icon={RefreshCwIcon} label={t("Refresh")} onClick={refresh} />
      <div className="flex-1" />
      <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <span className="max-sm:hidden">{t("At the same time")}</span>
        <select
          className="h-7 rounded-md border bg-background px-1.5 text-xs text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
          aria-label={t("Moves at the same time")}
          value={q.data?.concurrency ?? 1}
          disabled={!q.data}
          onChange={(e) => setConcurrency(Number(e.target.value))}
        >
          {[1, 2, 3, 4, 5, 6, 7, 8].map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </select>
      </label>
    </>
  );

  const columns: Column<SpaceMove>[] = [
    {
      header: t("Space"),
      cell: (m) => {
        const Icon = DRIVE_ICON[m.space_kind];
        return (
          <span className="flex min-w-0 items-center gap-2">
            <Icon className="size-4 shrink-0" />
            <span className="truncate">{moveSpaceLabel(m)}</span>
          </span>
        );
      },
      title: (m) => moveSpaceLabel(m),
    },
    {
      header: t("From → to"),
      className: "w-[190px] max-md:hidden",
      cellClassName: "text-muted-foreground",
      cell: (m) => (
        <span className="flex min-w-0 items-center gap-1">
          <span className="truncate">{m.from_name || t("A folder on the server")}</span>
          <ArrowRightIcon className="size-3 shrink-0" aria-label={t("to")} />
          <span className="truncate">{m.to_name}</span>
        </span>
      ),
      title: (m) => moveRoute(m),
    },
    { header: t("Progress"), className: "w-[220px]", cell: (m) => <MoveProgress m={m} /> },
    {
      header: t("Speed"),
      className: "w-[150px] max-lg:hidden",
      cellClassName: "text-muted-foreground tabular-nums",
      cell: (m) =>
        m.state === "running" && m.speed ? (
          <span className="grid text-[11px]">
            <span>{t("{size}/s", { size: formatBytes(m.speed) })}</span>
            <span>{timeLeft(m)}</span>
          </span>
        ) : (
          "—"
        ),
    },
    {
      header: t("State"),
      className: "w-[130px]",
      cell: (m) => (
        <span className={cn("text-xs", m.state === "failed" ? "text-destructive" : m.state === "running" ? "text-brand" : m.state === "done" ? "text-emerald-600 dark:text-emerald-400" : "text-muted-foreground")}>
          {MOVE_STATE_LABEL[m.state]}
          {m.failed_items > 0 && m.state !== "done" && ` · ${t("{n} file failed|{n} files failed", { n: m.failed_items })}`}
        </span>
      ),
      title: (m) => (m.error ? tServer(m.error) : undefined),
    },
    {
      header: t("Asked for"),
      className: "w-[150px] max-xl:hidden",
      cellClassName: "text-muted-foreground",
      cell: (m) => formatDateTime(m.created_at),
      title: (m) => t("By {name}", { name: m.created_by_name }),
    },
  ];

  const menu = (m: SpaceMove | null) =>
    m ? (
      <>
        <DropdownMenuItem onClick={() => setDialog({ t: "details", move: m })}>
          <InfoIcon /> {t("Details")}
        </DropdownMenuItem>
        {canPause(m) && (
          <DropdownMenuItem onClick={() => pause(m)}>
            <PauseIcon /> {t("Pause")}
          </DropdownMenuItem>
        )}
        {canResume(m) && (
          <DropdownMenuItem onClick={() => resume(m)}>
            <PlayIcon /> {t("Resume")}
          </DropdownMenuItem>
        )}
        {canCancel(m) && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive" onClick={() => setDialog({ t: "cancel", move: m })}>
              <XIcon /> {t("Cancel move")}
            </DropdownMenuItem>
          </>
        )}
      </>
    ) : (
      <DropdownMenuItem onClick={refresh}>
        <RefreshCwIcon /> {t("Refresh")}
      </DropdownMenuItem>
    );

  const active = moves.filter(moveActive).length;
  return (
    <Frame
      toolbar={toolbar}
      icon={icon as LucideIcon}
      crumbs={[{ label: t("Control panel"), to: "/admin" }, { label: title }]}
      upTo="/admin"
      searchPlaceholder={t("Search settings")}
      onSearch={searchSettings}
      footer={<span>{t("{n} move not finished|{n} moves not finished", { n: active })}</span>}
    >
      <DataTable
        rows={moves}
        rowKey={(m) => m.id}
        columns={columns}
        fixed
        loading={q.isLoading}
        selectedKey={selectedId}
        onSelect={setSelectedId}
        onOpen={(m) => setDialog({ t: "details", move: m })}
        menu={menu}
        empty={<EmptyState icon={TruckIcon} title={t("No moves yet")} hint={t("Move a space from Control panel › Spaces.")} />}
      />
      {dialog?.t === "details" && <MoveDetails move={moves.find((m) => m.id === dialog.move.id) ?? dialog.move} onClose={() => setDialog(null)} />}
      {dialog?.t === "cancel" && (
        <ConfirmDialog
          title={t("Cancel moving \"{name}\"?", { name: moveSpaceLabel(dialog.move) })}
          description={t("The space stays on {from}, and what was already copied to {to} is removed.", {
            from: dialog.move.from_name || t("its folder on the server"),
            to: dialog.move.to_name,
          })}
          confirmText={t("Cancel move")}
          destructive
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            await api.cancelMove(dialog.move.id);
            toast.success(t("Move cancelled"));
            setDialog(null);
            refresh();
          }}
        />
      )}
    </Frame>
  );
}

/** Everything about one move: where from and to, when, by whom, and what went wrong */
function MoveDetails({ move: m, onClose }: { move: SpaceMove; onClose(): void }) {
  const rows: [string, string][] = [
    [t("From"), m.from_name || t("A folder on the server")],
    [t("To"), m.to_name],
    [t("State"), MOVE_STATE_LABEL[m.state]],
    [t("Asked for"), `${formatDateTime(m.created_at)} · ${t("By {name}", { name: m.created_by_name })}`],
    ...(m.started_at ? [[t("Started"), formatDateTime(m.started_at)] as [string, string]] : []),
    ...(m.finished_at ? [[t("Finished"), formatDateTime(m.finished_at)] as [string, string]] : []),
  ];
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("Moving \"{name}\"", { name: moveSpaceLabel(m) })}</DialogTitle>
          <DialogDescription>{moveRoute(m)}</DialogDescription>
        </DialogHeader>
        <MoveProgress m={m} />
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          {rows.map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="text-muted-foreground">{k}</dt>
              <dd className="min-w-0 break-words">{v}</dd>
            </div>
          ))}
        </dl>
        {m.error && <p className="rounded-md bg-destructive/10 px-3 py-2 text-sm text-destructive">{tServer(m.error)}</p>}
        {m.note && <p className="rounded-md bg-muted px-3 py-2 text-sm break-words whitespace-pre-line">{m.note.split("\n").map((l) => tServer(l)).join("\n")}</p>}
        {m.failures.length > 0 && (
          <div className="grid gap-1.5">
            <p className="text-sm">{t("{n} file couldn't be copied:|{n} files couldn't be copied:", { n: m.failed_items })}</p>
            <ul className="max-h-48 divide-y overflow-y-auto rounded-md border text-xs">
              {m.failures.map((f, i) => (
                <li key={i} className="grid gap-0.5 px-3 py-1.5">
                  <span className="truncate">{f.item ?? t("A file of a personal space")}</span>
                  <span className="text-muted-foreground">{tServer(f.error)}</span>
                </li>
              ))}
            </ul>
            <p className="text-xs text-muted-foreground">{t("Resume the move to try them again, or cancel it to keep the space where it is.")}</p>
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
