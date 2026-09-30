//! How a move of a space is shown: its state, the space, where it goes, and its progress

import type { MoveState, SpaceMove } from "@/api";
import { t } from "@/lib/i18n";
import { cn, formatBytes } from "@/lib/utils";

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
