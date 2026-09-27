import { useState } from "react";
import {
  CheckCircle2Icon,
  ChevronDownIcon,
  ChevronUpIcon,
  FolderOpenIcon,
  PauseIcon,
  PlayIcon,
  RotateCwIcon,
  XIcon,
  AlertCircleIcon,
} from "lucide-react";
import { useNavigate } from "react-router";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Button } from "@/components/ui/button";
import { FileIcon } from "@/components/FileIcon";
import { confirm } from "@/components/confirm";
import { cn, formatBytes } from "@/lib/utils";
import { cancel, cancelAll, clearFinished, pause, resume, retryFailed, useUploads } from "@/uploads";
import { t } from "@/lib/i18n";

/** Above this many uploads, only those in progress, paused or failed get a row; the rest are counted in a summary */
const ROW_LIMIT = 100;

export function UploadPanel() {
  const { tasks, totals } = useUploads();
  const navigate = useNavigate();
  const [collapsed, setCollapsed] = useState(false);
  // One context menu for every row: remembers which row it was opened on
  const [menuId, setMenuId] = useState<string | null>(null);
  if (tasks.length === 0) return null;

  const active = totals.uploading + totals.queued;
  const failed = totals.error;
  const pct = totals.size ? Math.round((totals.sent / totals.size) * 100) : 100;
  const title = active
    ? t("Uploading {n} file · {pct}%|Uploading {n} files · {pct}%", { n: active, pct })
    : failed
      ? t("{n} file failed to upload|{n} files failed to upload", { n: failed })
      : t("{n} upload complete|{n} uploads complete", { n: tasks.length });

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
        <Button
          size="icon-xs"
          variant="ghost"
          aria-label={collapsed ? t("Expand") : t("Collapse")}
          title={collapsed ? t("Expand") : t("Collapse")}
          onClick={() => setCollapsed(!collapsed)}
        >
          {collapsed ? <ChevronUpIcon /> : <ChevronDownIcon />}
        </Button>
        <Button
          size="icon-xs"
          variant="ghost"
          aria-label={t("Close")}
          title={t("Close")}
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
        {active ? "" : title}
      </div>
      {active > 0 && (
        <div role="progressbar" aria-label={t("Upload progress")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} className="h-0.5 bg-muted">
          <div className="h-full bg-brand transition-[width]" style={{ width: `${pct}%` }} />
        </div>
      )}
      {!collapsed && (
        <ContextMenu>
          <ContextMenuTrigger className="block max-h-72 overflow-y-auto" onContextMenuCapture={() => setMenuId(null)}>
            {rows.map((task) => {
              const p = task.size ? Math.round((task.sent / task.size) * 100) : 100;
              return (
                <div
                  key={task.id}
                  className="flex items-center gap-2.5 border-b border-border/50 px-3 py-2 last:border-0"
                  onContextMenu={() => setMenuId(task.id)}
                >
                  <FileIcon node={{ kind: "file", name: task.name, mime: task.file.type }} className="size-5" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm" title={task.relativePath ? `${task.relativePath}/${task.name}` : task.name}>
                      {task.name}
                    </div>
                    <div className={cn("truncate text-xs text-muted-foreground", task.status === "error" && "text-destructive")}>
                      {task.status === "error"
                        ? task.error
                        : task.status === "done"
                          ? formatBytes(task.size)
                          : task.status === "queued"
                            ? t("Waiting")
                            : task.status === "paused"
                              ? t("Paused · {pct}%", { pct: p })
                              : `${formatBytes(task.sent)} / ${formatBytes(task.size)} · ${p}%`}
                    </div>
                    {(task.status === "uploading" || task.status === "paused") && (
                      <div role="progressbar" aria-label={task.name} aria-valuemin={0} aria-valuemax={100} aria-valuenow={p} className="mt-1 h-1 overflow-hidden rounded bg-muted">
                        <div className="h-full bg-brand transition-[width]" style={{ width: `${p}%` }} />
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
            {menuTask && (
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
