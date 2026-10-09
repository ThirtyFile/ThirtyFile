import { useState } from "react";
import { CheckCircle2Icon, ChevronDownIcon, ChevronUpIcon, FolderOpenIcon, PauseIcon, PlayIcon, RotateCwIcon, XIcon, AlertCircleIcon } from "lucide-react";
import { useNavigate } from "react-router";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Button } from "@/components/ui/button";
import { FileIcon } from "@/components/FileIcon";
import { confirm } from "@/lib/confirm";
import { cn, formatBytes } from "@/lib/utils";
import { RecoveredUploads } from "@/components/RecoveredUploads";
import { cancel, cancelAll, clearFinished, pause, resume, retryFailed, useInterrupted, useUploads } from "@/uploads";
import { t } from "@/lib/i18n";

/** Above this many uploads, only those in progress, paused or failed get a row; the rest are counted in a summary */
const ROW_LIMIT = 100;

/**
 * Upload progress, and uploads to `endpoint` that a reload or a closed browser interrupted; `visitor`: on a share
 * link's page, where the destination folder can't be opened
 */
export function UploadPanel({ visitor = false, endpoint = "/api/uploads" }: { visitor?: boolean; endpoint?: string }) {
  const { tasks, totals } = useUploads();
  const recovered = useInterrupted(endpoint);
  const navigate = useNavigate();
  const [collapsed, setCollapsed] = useState(false);
  // One context menu for every row: remembers which row it was opened on
  const [menuId, setMenuId] = useState<string | null>(null);
  if (tasks.length === 0 && recovered.length === 0) return null;

  const active = totals.uploading + totals.queued;
  const failed = totals.error;
  const pct = totals.size ? Math.round((totals.sent / totals.size) * 100) : 100;
  const waiting = tasks.some((task) => task.status === "uploading" && (task.phase === "preparing" || task.phase === "finishing"));
  const title = active
    ? waiting
      ? t("Uploading {n} file|Uploading {n} files", { n: active })
      : t("Uploading {n} file · {pct}%|Uploading {n} files · {pct}%", { n: active, pct })
    : failed
      ? t("{n} file failed to upload|{n} files failed to upload", { n: failed })
      : totals.paused
        ? t("Paused · {pct}%", { pct })
        : tasks.length
          ? t("{n} upload complete|{n} uploads complete", { n: tasks.length })
          : t("{n} interrupted upload|{n} interrupted uploads", { n: recovered.length });

  const condensed = tasks.length > ROW_LIMIT;
  const shown = condensed ? tasks.filter((x) => x.status === "uploading" || x.status === "paused" || x.status === "error") : tasks;
  const rows = shown.slice(0, ROW_LIMIT);
  const summary = condensed
    ? [
        totals.queued > 0 && t("{n} waiting", { n: totals.queued }),
        totals.done > 0 && t("{n} complete", { n: totals.done }),
        shown.length > rows.length && t("{n} more not shown", { n: shown.length - rows.length }),
      ].filter(Boolean)
    : [];
  const menuTask = menuId ? tasks.find((x) => x.id === menuId) : undefined;

  return (
    <div className="overflow-hidden rounded-xl border bg-popover text-popover-foreground shadow-xl">
      <div className="flex items-center gap-2 border-b px-3 py-2">
        <span className="flex-1 truncate text-sm font-medium">{title}</span>
        <Button size="icon-xs" variant="ghost" aria-label={collapsed ? t("Expand") : t("Collapse")} title={collapsed ? t("Expand") : t("Collapse")} onClick={() => setCollapsed(!collapsed)}>
          {collapsed ? <ChevronUpIcon /> : <ChevronDownIcon />}
        </Button>
        <Button
          size="icon-xs"
          variant="ghost"
          aria-label={t("Close")}
          title={t("Close")}
          disabled={tasks.length === 0}
          onClick={async () => {
            if (!active) return clearFinished();
            if (await confirm({ title: t("Cancel all uploads in progress?"), confirmText: t("Cancel uploads"), destructive: true })) cancelAll();
          }}
        >
          <XIcon />
        </Button>
      </div>
      {/* Read out by screen readers once everything has finished (not on every percent) */}
      <div role="status" className="sr-only">
        {active ? (waiting ? (tasks.some((task) => task.status === "uploading" && task.phase === "preparing") ? t("Preparing upload…") : t("Finishing upload…")) : "") : title}
      </div>
      {active > 0 && (
        <div role="progressbar" aria-label={t("Upload progress")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={waiting ? undefined : pct} className="h-0.5 bg-muted">
          <div className={cn("h-full bg-brand transition-[width]", waiting && "w-1/3 animate-pulse motion-reduce:animate-none")} style={waiting ? undefined : { width: `${pct}%` }} />
        </div>
      )}
      {!collapsed && recovered.length > 0 && <RecoveredUploads endpoint={endpoint} batches={recovered} />}
      {!collapsed && tasks.length > 0 && (
        <ContextMenu>
          <ContextMenuTrigger className="block max-h-[min(18rem,calc(100dvh-16rem))] overflow-y-auto" onContextMenuCapture={() => setMenuId(null)}>
            {rows.map((task) => {
              const p = task.size ? Math.round((task.sent / task.size) * 100) : 100;
              const pending = task.status === "uploading" && (task.phase === "preparing" || task.phase === "finishing");
              return (
                <div key={task.id} className="flex items-center gap-2.5 border-b border-border/50 px-3 py-2 last:border-0" onContextMenu={() => setMenuId(task.id)}>
                  <FileIcon node={{ kind: "file", name: task.relativePath ? `${task.relativePath}/${task.name}` : task.name, mime: task.file.type, size: task.size }} className="size-5" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm" title={task.relativePath ? `${task.relativePath}/${task.name}` : task.name}>
                      {task.name}
                    </div>
                    <div className={cn("truncate text-xs text-muted-foreground", task.status === "error" && "text-destructive")}>
                      {task.status === "error"
                        ? task.error
                        : task.status === "done"
                          ? // Both files were kept: say which name the new one got
                            task.savedAs
                            ? t('Saved as "{name}"', { name: task.savedAs })
                            : formatBytes(task.size)
                          : task.status === "queued"
                            ? t("Waiting")
                            : task.status === "paused"
                              ? t("Paused · {pct}%", { pct: p })
                              : task.phase === "preparing"
                                ? t("Preparing upload…")
                                : task.phase === "finishing"
                                  ? t("Finishing upload…")
                                  : `${formatBytes(task.sent)} / ${formatBytes(task.size)} · ${p}%`}
                    </div>
                    {(task.status === "uploading" || task.status === "paused") && (
                      <div
                        role="progressbar"
                        aria-label={task.name}
                        aria-valuemin={0}
                        aria-valuemax={100}
                        aria-valuenow={pending ? undefined : p}
                        className="mt-1 h-1 overflow-hidden rounded bg-muted"
                      >
                        <div
                          className={cn("h-full bg-brand transition-[width]", pending && "w-1/3 animate-pulse motion-reduce:animate-none")}
                          style={pending ? undefined : { width: `${p}%` }}
                        />
                      </div>
                    )}
                  </div>
                  {task.status === "done" && <CheckCircle2Icon className="size-4 text-emerald-500" />}
                  {task.status === "error" && <AlertCircleIcon className="size-4 text-destructive" />}
                  {task.status === "uploading" && (
                    <Button size="icon-xs" variant="ghost" aria-label={t("Pause")} title={t("Pause")} onClick={() => pause(task.id)}>
                      <PauseIcon />
                    </Button>
                  )}
                  {task.status === "paused" && (
                    <Button size="icon-xs" variant="ghost" aria-label={t("Resume")} title={t("Resume")} onClick={() => resume(task.id)}>
                      <PlayIcon />
                    </Button>
                  )}
                  {task.status === "error" && (
                    <Button size="icon-xs" variant="ghost" aria-label={t("Retry")} title={t("Retry")} onClick={() => resume(task.id)}>
                      <RotateCwIcon />
                    </Button>
                  )}
                  {task.status !== "done" && (
                    <Button size="icon-xs" variant="ghost" aria-label={t("Cancel")} title={t("Cancel")} onClick={() => cancel(task.id)}>
                      <XIcon />
                    </Button>
                  )}
                </div>
              );
            })}
            {summary.length > 0 && <div className="px-3 py-2 text-xs text-muted-foreground">{summary.join(" · ")}</div>}
          </ContextMenuTrigger>
          <ContextMenuContent>
            {menuTask?.status === "uploading" && (
              <DropdownMenuItem onClick={() => pause(menuTask.id)}>
                <PauseIcon /> {t("Pause")}
              </DropdownMenuItem>
            )}
            {menuTask?.status === "paused" && (
              <DropdownMenuItem onClick={() => resume(menuTask.id)}>
                <PlayIcon /> {t("Resume")}
              </DropdownMenuItem>
            )}
            {menuTask?.status === "error" && (
              <DropdownMenuItem onClick={() => resume(menuTask.id)}>
                <RotateCwIcon /> {t("Retry")}
              </DropdownMenuItem>
            )}
            {failed > 1 && (
              <DropdownMenuItem onClick={retryFailed}>
                <RotateCwIcon /> {t("Retry all failed")}
              </DropdownMenuItem>
            )}
            {menuTask && !visitor && (
              <DropdownMenuItem onClick={() => navigate(`/files/${menuTask.parentId}`)}>
                <FolderOpenIcon /> {t("Open destination folder")}
              </DropdownMenuItem>
            )}
            <DropdownMenuSeparator />
            {menuTask && menuTask.status !== "done" && (
              <DropdownMenuItem variant="destructive" onClick={() => cancel(menuTask.id)}>
                <XIcon /> {t("Cancel upload")}
              </DropdownMenuItem>
            )}
            <DropdownMenuItem onClick={clearFinished}>
              <CheckCircle2Icon /> {t("Clear completed")}
            </DropdownMenuItem>
          </ContextMenuContent>
        </ContextMenu>
      )}
    </div>
  );
}
