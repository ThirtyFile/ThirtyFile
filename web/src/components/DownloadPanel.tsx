import { useState } from "react";
import { AlertCircleIcon, CheckCircle2Icon, ChevronDownIcon, ChevronUpIcon, FileArchiveIcon, FileDownIcon, RotateCwIcon, XIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { confirm } from "@/components/confirm";
import { cn, formatBytes } from "@/lib/utils";
import { cancelDownload, clearDownloads, removeDownload, retryDownload, useDownloads, type DownloadTask } from "@/downloads";
import { t } from "@/lib/i18n";

function eta(task: DownloadTask) {
  if (!task.total || !task.rate) return "";
  const s = Math.max(0, Math.round((task.total - task.received) / task.rate));
  return s < 60 ? t("{s} s left", { s }) : t("{m} min {s} s left", { m: Math.floor(s / 60), s: s % 60 });
}

function detail(task: DownloadTask) {
  if (task.status === "done") return t("Completed · {size}", { size: formatBytes(task.received) });
  if (task.status === "canceled") return t("Canceled");
  if (task.status === "error") return task.error ?? t("Download failed");
  const size = task.total ? `${formatBytes(task.received)} / ${formatBytes(task.total)}` : formatBytes(task.received);
  const speed = task.rate ? ` · ${formatBytes(task.rate)}/s` : "";
  const left = eta(task);
  return `${task.zip ? `${t("ZIP download")} · ` : ""}${size}${speed}${left ? ` · ${left}` : ""}`;
}

/** Download progress (bottom right of the page, stacked with upload progress) */
export function DownloadPanel() {
  const tasks = useDownloads();
  const [collapsed, setCollapsed] = useState(false);
  if (tasks.length === 0) return null;

  const active = tasks.filter((task) => task.status === "downloading");
  const total = active.reduce((s, task) => s + (task.total ?? task.received), 0);
  const received = active.reduce((s, task) => s + task.received, 0);
  const pct = total ? Math.round((received / total) * 100) : 0;
  const failed = tasks.filter((task) => task.status === "error").length;
  const done = tasks.filter((task) => task.status === "done").length;
  const title = active.length
    ? t("Downloading {n} item · {pct}%|Downloading {n} items · {pct}%", { n: active.length, pct })
    : failed
      ? t("{n} download failed|{n} downloads failed", { n: failed })
      : done
        ? t("{n} download complete|{n} downloads complete", { n: done })
        : t("Download canceled");

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
            if (active.length && !(await confirm({ title: t("Cancel all downloads in progress?"), confirmText: t("Cancel downloads"), destructive: true }))) return;
            clearDownloads();
          }}
        >
          <XIcon />
        </Button>
      </div>
      {/* Read out by screen readers once everything has finished (not on every percent) */}
      <div role="status" className="sr-only">
        {active.length ? "" : title}
      </div>
      {active.length > 0 && (
        <div role="progressbar" aria-label={t("Download progress")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} className="h-0.5 bg-muted">
          <div className="h-full bg-brand transition-[width]" style={{ width: `${pct}%` }} />
        </div>
      )}
      {!collapsed && (
        <div className="max-h-60 overflow-y-auto">
          {tasks.map((task) => {
            const p = task.total ? Math.min(100, Math.round((task.received / task.total) * 100)) : 0;
            const Icon = task.zip ? FileArchiveIcon : FileDownIcon;
            return (
              <div key={task.id} className="flex items-center gap-2.5 px-3 py-2 text-xs">
                <Icon className="size-5 shrink-0 text-muted-foreground" />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate" title={task.name}>
                      {task.name}
                    </span>
                    {task.status === "downloading" && task.total && <span className="ml-auto shrink-0 tabular-nums text-muted-foreground">{p}%</span>}
                  </div>
                  {task.status === "downloading" && (
                    <div
                      role="progressbar"
                      aria-label={task.name}
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-valuenow={task.total ? p : undefined}
                      className="mt-1 h-1 overflow-hidden rounded-full bg-muted"
                    >
                      <div
                        className={cn("h-full rounded-full bg-brand transition-[width]", !task.total && "w-1/3 animate-pulse")}
                        style={task.total ? { width: `${p}%` } : undefined}
                      />
                    </div>
                  )}
                  <div className={cn("mt-0.5 truncate text-[11px] text-muted-foreground tabular-nums", task.status === "error" && "text-destructive")} title={detail(task)}>
                    {detail(task)}
                  </div>
                </div>
                {task.status === "downloading" ? (
                  <Button size="icon-xs" variant="ghost" aria-label={t("Cancel download")} title={t("Cancel download")} onClick={() => cancelDownload(task.id)}>
                    <XIcon />
                  </Button>
                ) : task.status === "done" ? (
                  <CheckCircle2Icon className="size-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
                ) : (
                  <span className="flex shrink-0 items-center">
                    {task.status === "error" && <AlertCircleIcon className="size-4 text-destructive" />}
                    <Button size="icon-xs" variant="ghost" aria-label={t("Download again")} title={t("Download again")} onClick={() => retryDownload(task.id)}>
                      <RotateCwIcon />
                    </Button>
                    <Button size="icon-xs" variant="ghost" aria-label={t("Remove")} title={t("Remove")} onClick={() => removeDownload(task.id)}>
                      <XIcon />
                    </Button>
                  </span>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
