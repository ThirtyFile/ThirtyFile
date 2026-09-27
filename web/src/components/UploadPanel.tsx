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
import { cn, formatBytes } from "@/lib/utils";
import { cancel, cancelAll, clearFinished, pause, resume, useUploads } from "@/uploads";
import { t } from "@/lib/i18n";

export function UploadPanel() {
  const tasks = useUploads();
  const navigate = useNavigate();
  const [collapsed, setCollapsed] = useState(false);
  if (tasks.length === 0) return null;

  const active = tasks.filter((x) => x.status === "uploading" || x.status === "queued");
  const failed = tasks.filter((x) => x.status === "error").length;
  const total = tasks.reduce((s, x) => s + x.size, 0);
  const sent = tasks.reduce((s, x) => s + x.sent, 0);
  const pct = total ? Math.round((sent / total) * 100) : 100;
  const title = active.length
    ? t("Uploading {n} file · {pct}%|Uploading {n} files · {pct}%", { n: active.length, pct })
    : failed
      ? t("{n} file failed to upload|{n} files failed to upload", { n: failed })
      : t("{n} upload complete|{n} uploads complete", { n: tasks.length });

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
          onClick={() => (active.length ? window.confirm(t("Cancel all uploads in progress?")) && cancelAll() : clearFinished())}
        >
          <XIcon />
        </Button>
      </div>
      {/* Read out by screen readers once everything has finished (not on every percent) */}
      <div role="status" className="sr-only">
        {active.length ? "" : title}
      </div>
      {active.length > 0 && (
        <div role="progressbar" aria-label={t("Upload progress")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} className="h-0.5 bg-muted">
          <div className="h-full bg-brand transition-[width]" style={{ width: `${pct}%` }} />
        </div>
      )}
      {!collapsed && (
        <div className="max-h-72 overflow-y-auto">
          {tasks.map((task) => {
            const p = task.size ? Math.round((task.sent / task.size) * 100) : 100;
            return (
              <ContextMenu key={task.id}>
                <ContextMenuTrigger className="flex items-center gap-2.5 border-b border-border/50 px-3 py-2 last:border-0">
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
                </ContextMenuTrigger>
                <ContextMenuContent>
                  {task.status === "uploading" && (
                    <DropdownMenuItem onClick={() => pause(task.id)}>
                      <PauseIcon /> {t("Pause")}
                    </DropdownMenuItem>
                  )}
                  {task.status === "paused" && (
                    <DropdownMenuItem onClick={() => resume(task.id)}>
                      <PlayIcon /> {t("Resume")}
                    </DropdownMenuItem>
                  )}
                  {task.status === "error" && (
                    <DropdownMenuItem onClick={() => resume(task.id)}>
                      <RotateCwIcon /> {t("Retry")}
                    </DropdownMenuItem>
                  )}
                  <DropdownMenuItem onClick={() => navigate(`/files/${task.parentId}`)}>
                    <FolderOpenIcon /> {t("Open destination folder")}
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  {task.status !== "done" && (
                    <DropdownMenuItem variant="destructive" onClick={() => cancel(task.id)}>
                      <XIcon /> {t("Cancel upload")}
                    </DropdownMenuItem>
                  )}
                  <DropdownMenuItem onClick={clearFinished}>
                    <CheckCircle2Icon /> {t("Clear completed")}
                  </DropdownMenuItem>
                </ContextMenuContent>
              </ContextMenu>
            );
          })}
        </div>
      )}
    </div>
  );
}
