// Control panel › Storage usage: what the server's samples say (GET /admin/usage and /admin/usage/history), and the
// arithmetic the page and its charts do with them. Unknown values stay null: a missing disk size is never 0, and a
// period without operations has no latency.

export type UsageOp = "read" | "write" | "delete" | "list" | "check";
export type UsageWork = "foreground" | "background" | "probe";
export type UsageRange = "6h" | "2d" | "30d" | "1y";

/** A capacity sample (server: usage_capacity) */
export interface UsageCapacity {
  /** '' for everything together */
  location_id: string;
  sampled_at: number;
  /** Files as people see them (not in the trash), the trash and earlier versions: identical content counts every time */
  live_bytes: number;
  trash_bytes: number;
  version_bytes: number;
  /** Physical content ThirtyFile keeps: the content store (identical content once), folder spaces' files and versions */
  store_bytes: number;
  folder_bytes: number;
  folder_version_bytes: number;
  /** Contents waiting to be deleted (their size isn't known) */
  pending_deletes: number;
  /** Uploads in progress, in the data folder */
  temp_bytes: number;
  /** Reserved until ThirtyFile makes backups and replicas */
  backup_bytes: number | null;
  replica_bytes: number | null;
  /** The whole disk as the system reports it; null when it can't be measured */
  disk_free: number | null;
  disk_total: number | null;
  disk_id: string | null;
  online: boolean;
  /** How long taking the sample took (ms) */
  took_ms: number;
}

export interface SampledCapacity extends UsageCapacity {
  /** Older than the sampler should have left it */
  stale: boolean;
}

/** Operations summed over a period; durations are null without any operation */
export interface OpFigures {
  count: number;
  errors: number;
  timeouts: number;
  bytes: number;
  p50_ms: number | null;
  p95_ms: number | null;
  mean_ms: number | null;
  max_ms: number | null;
}

export interface OpSummary extends OpFigures {
  op: UsageOp;
  work: UsageWork;
}

export interface LocationUsage {
  id: string;
  name: string;
  kind: "local" | "s3" | "sftp" | "ftp";
  online: boolean;
  error: string | null;
  checked_at: number | null;
  capacity: SampledCapacity | null;
  same_disk_as: string[];
  active: { calls: number; transfers: number };
  /** The last `recent_secs` */
  recent: OpSummary[];
}

export interface UsageThresholds {
  /** Percent; 0 is off */
  disk_percent: number;
  error_percent: number;
}

export interface UsageAlert {
  kind: "disk" | "errors" | "offline" | "stale";
  /** '' for the data folder */
  location: string;
  value: number;
  threshold: number;
}

export interface UsageOverview {
  now: number;
  ops_span: number;
  capacity_span: number;
  recent_secs: number;
  thresholds: UsageThresholds;
  total: SampledCapacity | null;
  locations: LocationUsage[];
  /** Operations on folder spaces that are on no storage location */
  unplaced: OpSummary[];
  queue: { moves_queued: number; moves_running: number; moves_paused: number; jobs_running: number; pending_deletes: number };
  alerts: UsageAlert[];
}

export interface CapacityPoint extends UsageCapacity {
  at: number;
}

export interface OpsPoint extends OpFigures {
  at: number;
}

export interface UsageForecast {
  days: number | null;
  reason: "ok" | "unknown_capacity" | "short" | "not_growing" | "unsteady";
  window_days: number;
  points: number;
}

export interface UsageHistory {
  location: string;
  range: UsageRange;
  work: UsageWork | "all";
  from: number;
  to: number;
  capacity_span: number;
  ops_span: number;
  capacity: CapacityPoint[];
  ops: { op: UsageOp; points: OpsPoint[] }[];
  forecast: UsageForecast;
}

/** Physical content ThirtyFile accounts for on a location */
export function storedBytes(c: UsageCapacity): number {
  return c.store_bytes + c.folder_bytes + c.folder_version_bytes;
}

/** Bytes used on the disk by everything on it; null when the disk can't be measured */
export function diskUsed(c: UsageCapacity): number | null {
  return c.disk_free === null || c.disk_total === null ? null : Math.max(0, c.disk_total - c.disk_free);
}

/** Percent of the disk used; null when unknown (never computed from a missing total) */
export function usedPercent(c: UsageCapacity): number | null {
  const used = diskUsed(c);
  return used === null || !c.disk_total ? null : (used * 100) / c.disk_total;
}

/** Share of operations that failed or timed out (percent); null without operations */
export function errorRate(f: OpFigures): number | null {
  return f.count > 0 ? ((f.errors + f.timeouts) * 100) / f.count : null;
}

/** Average bytes per second over a period of `span` seconds */
export function perSecond(bytes: number, span: number): number {
  return span > 0 ? bytes / span : 0;
}

/** Summaries of several kinds of work added up (their durations can't be: the largest percentile is kept) */
export function sumFigures(list: OpFigures[]): OpFigures | null {
  if (!list.length) return null;
  const max = (k: "p50_ms" | "p95_ms" | "max_ms") => list.reduce<number | null>((m, f) => (f[k] === null ? m : Math.max(m ?? 0, f[k])), null);
  const count = list.reduce((n, f) => n + f.count, 0);
  const timed = list.filter((f) => f.mean_ms !== null);
  return {
    count,
    errors: list.reduce((n, f) => n + f.errors, 0),
    timeouts: list.reduce((n, f) => n + f.timeouts, 0),
    bytes: list.reduce((n, f) => n + f.bytes, 0),
    p50_ms: max("p50_ms"),
    p95_ms: max("p95_ms"),
    max_ms: max("max_ms"),
    mean_ms: timed.length ? timed.reduce((s, f) => s + f.mean_ms! * f.count, 0) / Math.max(1, timed.reduce((n, f) => n + f.count, 0)) : null,
  };
}

/** Round numbers for an axis from 0 to at least `max` (1, 2 or 5 times a power of ten), at most `count` steps */
export function niceTicks(max: number, count = 4): number[] {
  if (!(max > 0)) return [0, 1];
  const rough = max / count;
  const power = 10 ** Math.floor(Math.log10(rough));
  const step = [1, 2, 5, 10].map((m) => m * power).find((s) => s >= rough) ?? 10 * power;
  const ticks = [];
  for (let v = 0; v < max + step * 0.999; v += step) ticks.push(Number(v.toPrecision(12)));
  return ticks;
}

/** Whole numbers for an axis of counts */
export function countTicks(max: number, count = 4): number[] {
  return niceTicks(Math.max(1, max), count).filter(Number.isInteger);
}

/** Round numbers of bytes for an axis (steps of 1, 2 or 5 KB, MB…, as formatBytes shows them) */
export function byteTicks(max: number, count = 4): number[] {
  if (!(max > 0)) return [0, 1024];
  let unit = 1;
  while (max / unit >= 1024 && unit < 1024 ** 4) unit *= 1024;
  return niceTicks(max / unit, count).map((v) => v * unit);
}

/**
 * Values of each series at the times of all of them, with null where a series has no point. A time more than one period
 * after the one before gets a null row first, so lines don't run across idle periods.
 */
export function alignSeries<P extends { at: number }>(series: P[][], span: number): { at: number; points: (P | null)[] }[] {
  const times = [...new Set(series.flatMap((s) => s.map((p) => p.at)))].sort((a, b) => a - b);
  const byTime = series.map((s) => new Map(s.map((p) => [p.at, p])));
  const rows: { at: number; points: (P | null)[] }[] = [];
  let last: number | null = null;
  for (const at of times) {
    if (last !== null && at - last > span) rows.push({ at: last + span, points: series.map(() => null) });
    rows.push({ at, points: byTime.map((m) => m.get(at) ?? null) });
    last = at;
  }
  return rows;
}

/** A history as CSV (one row per period; operations by operation), for spreadsheets */
export function historyCsv(h: UsageHistory): string {
  const iso = (ts: number) => new Date(ts * 1000).toISOString();
  const cell = (v: number | string | boolean | null) => (v === null ? "" : typeof v === "string" && /[",\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : String(v));
  const lines = [
    [
      "period_start_utc",
      "period_seconds",
      "live_bytes",
      "trash_bytes",
      "version_bytes",
      "stored_bytes",
      "disk_used_bytes",
      "disk_total_bytes",
      "pending_deletes",
      "online",
    ].join(","),
    ...h.capacity.map((c) =>
      [iso(c.at), h.capacity_span, c.live_bytes, c.trash_bytes, c.version_bytes, storedBytes(c), diskUsed(c), c.disk_total, c.pending_deletes, c.online]
        .map(cell)
        .join(","),
    ),
    "",
    ["period_start_utc", "period_seconds", "operation", "work", "count", "errors", "timeouts", "bytes", "p50_ms", "p95_ms", "max_ms"].join(","),
    ...h.ops.flatMap((s) =>
      s.points.map((p) => [iso(p.at), h.ops_span, s.op, h.work, p.count, p.errors, p.timeouts, p.bytes, p.p50_ms, p.p95_ms, p.max_ms].map(cell).join(",")),
    ),
  ];
  return lines.join("\n") + "\n";
}
