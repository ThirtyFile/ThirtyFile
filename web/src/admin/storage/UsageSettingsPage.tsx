import { Fragment, useState, type FormEvent } from "react";
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CircleCheckIcon, CircleXIcon, ClockIcon, DownloadIcon, TriangleAlertIcon } from "lucide-react";
import { Link } from "react-router";
import { toast } from "sonner";
import { api } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { Skeleton } from "@/components/ui/skeleton";
import { Pending } from "@/components/ErrorState";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect } from "@/components/ui/native-select";
import { UsageChart } from "@/admin/storage/UsageChart";
import { cn, formatBytes, formatDateTime, formatTime } from "@/lib/utils";
import { locale, t, tServer } from "@/lib/i18n";
import {
  alignSeries,
  byteTicks,
  countTicks,
  diskUsed,
  errorRate,
  historyCsv,
  niceTicks,
  perSecond,
  storedBytes,
  sumFigures,
  usedPercent,
  type LocationUsage,
  type OpFigures,
  type OpSummary,
  type SampledCapacity,
  type UsageAlert,
  type UsageForecast,
  type UsageHistory,
  type UsageOp,
  type UsageOverview,
  type UsageRange,
  type UsageThresholds,
  type UsageWork,
} from "@/lib/usage";
import { Section, SettingsFrame } from "@/admin/SettingsFrame";

/** The overview is asked for again this often while the page is shown (not while the tab is hidden) */
const OVERVIEW_MS = 60_000;
/** The history changes every five minutes at most */
const HISTORY_MS = 300_000;

const percent = (p: number) => `${p.toLocaleString(locale, { maximumFractionDigits: p < 10 ? 1 : 0 })}%`;
const ms = (v: number) => `${v.toLocaleString(locale, { maximumFractionDigits: v < 10 ? 1 : 0 })} ms`;
const rate = (v: number) => `${formatBytes(Math.round(v))}/s`;
const count = (v: number) => v.toLocaleString(locale, { maximumFractionDigits: 0 });

/** The period each point of a chart stands for */
function periodLabel(span: number): string {
  if (span >= 86400) return t("day (UTC)");
  if (span >= 3600) return t("hour");
  return t("{n} minutes", { n: span / 60 });
}

const OPS: { op: UsageOp; label: () => string }[] = [
  { op: "read", label: () => t("Reads") },
  { op: "write", label: () => t("Writes") },
  { op: "delete", label: () => t("Deletes") },
  { op: "list", label: () => t("Listing and lookups") },
  { op: "check", label: () => t("Connection checks") },
];
const opLabel = (op: UsageOp) => OPS.find((o) => o.op === op)!.label();

const KIND: Record<LocationUsage["kind"], () => string> = {
  local: () => t("Local folder"),
  s3: () => "S3",
  sftp: () => "SFTP",
  ftp: () => "FTP",
};

export function UsageSettingsPage() {
  const q = useQuery(queries.system);
  const usage = useQuery({ queryKey: keys.usage(), queryFn: ({ signal }) => api.usageOverview(signal), refetchInterval: OVERVIEW_MS });
  const [filter, setFilter] = useState<{ location: string; range: UsageRange; work: UsageWork | "all"; op: UsageOp }>({
    location: "",
    range: "2d",
    work: "foreground",
    op: "read",
  });
  const history = useQuery({
    queryKey: keys.usageHistory(filter.location, filter.range, filter.work),
    queryFn: ({ signal }) => api.usageHistory(filter, signal),
    refetchInterval: HISTORY_MS,
    placeholderData: keepPreviousData,
  });
  const s = q.data?.stats;
  const stats: [string, string, string?][] = s
    ? [
        [t("Users"), t("{n} user|{n} users", { n: s.users }), t("{n} group|{n} groups", { n: s.groups })],
        [t("Personal space"), formatBytes(s.personal_bytes), t("{n} file|{n} files", { n: s.personal_files })],
        [t("All files (company)"), formatBytes(s.shared_bytes), t("{n} file|{n} files", { n: s.shared_files })],
        [t("Team space"), formatBytes(s.team_bytes), `${t("{n} space|{n} spaces", { n: s.team_drives })} · ${t("{n} file|{n} files", { n: s.team_files })}`],
        [t("Trash"), formatBytes(s.trash_bytes)],
        [t("Earlier versions of files"), formatBytes(s.version_bytes), t("Not counted toward the spaces' sizes")],
        [
          t("Actual storage used"),
          formatBytes(s.stored_bytes),
          `${t("Folder spaces count their files; the content store keeps identical content once")} · ${t("{n} share link|{n} share links", { n: s.share_links })}`,
        ],
      ]
    : [];
  const refresh = () => {
    q.refetch();
    usage.refetch();
    history.refetch();
  };
  return (
    <SettingsFrame item="usage" onRefresh={refresh} footer={<span>{t("Includes all spaces and the trash")}</span>}>
      {usage.data && usage.data.alerts.length > 0 && <Alerts overview={usage.data} />}
      {!q.data ? (
        <Pending query={q} loading={<Skeleton className="h-40" />} />
      ) : (
        <Section title={t("Storage usage")}>
          <dl className="grid grid-cols-2 sm:grid-cols-3">
            {stats.map(([label, value, hint]) => (
              <div key={label} className="border-r border-b p-4 [&:nth-child(2n)]:max-sm:border-r-0 sm:[&:nth-child(3n)]:border-r-0">
                <dt className="text-xs text-muted-foreground">{label}</dt>
                <dd className="mt-1 text-lg font-medium tabular-nums">{value}</dd>
                {hint && <dd className="text-[11px] text-muted-foreground">{hint}</dd>}
              </div>
            ))}
          </dl>
        </Section>
      )}
      {!usage.data ? (
        <Pending query={usage} loading={<Skeleton className="h-60" />} />
      ) : (
        <>
          <Locations overview={usage.data} />
          <History overview={usage.data} filter={filter} onFilter={(f) => setFilter((old) => ({ ...old, ...f }))} history={history.data} loading={history.isFetching} error={history.error} />
          <Thresholds key={JSON.stringify(usage.data.thresholds)} value={usage.data.thresholds} />
        </>
      )}
    </SettingsFrame>
  );
}

/** A location's name, or "Data folder" for the '' of everything together */
function nameOf(o: UsageOverview, id: string) {
  return id === "" ? t("Data folder") : (o.locations.find((l) => l.id === id)?.name ?? id);
}

function Alerts({ overview }: { overview: UsageOverview }) {
  const text = (a: UsageAlert) => {
    const name = nameOf(overview, a.location);
    switch (a.kind) {
      case "disk":
        return t("{name}: the disk is {percent} full (alert at {threshold})", { name, percent: percent(a.value), threshold: percent(a.threshold) });
      case "errors":
        return t("{name}: {percent} of operations failed or timed out in the last hour (alert at {threshold})", {
          name,
          percent: percent(a.value),
          threshold: percent(a.threshold),
        });
      case "offline":
        return t("{name} is offline", { name });
      case "stale":
        return t("{name}: no sample since {time}", { name, time: formatDateTime(a.value) });
    }
  };
  return (
    <section aria-labelledby="usage-alerts" className="grid gap-2 rounded-lg border border-destructive/40 bg-destructive/5 p-4">
      <h2 id="usage-alerts" className="flex items-center gap-2 text-sm font-medium">
        <TriangleAlertIcon className="size-4 text-destructive" aria-hidden />
        {t("{n} alert|{n} alerts", { n: overview.alerts.length })}
      </h2>
      <ul className="grid gap-1 text-sm">
        {overview.alerts.map((a) => (
          <li key={`${a.kind}-${a.location}`}>{text(a)}</li>
        ))}
      </ul>
      <p className="text-xs text-muted-foreground">
        {t("Look further in:")}{" "}
        <Link className="underline underline-offset-2" to="/admin/activity">
          {t("Activity log")}
        </Link>
        {" · "}
        <Link className="underline underline-offset-2" to="/admin/moves">
          {t("Moves")}
        </Link>
      </p>
    </section>
  );
}

/** How full a disk is: a bar and its numbers, or why they aren't known */
function Disk({ c, remote, threshold }: { c: SampledCapacity; remote: boolean; threshold: number }) {
  const pct = usedPercent(c);
  if (pct === null || c.disk_total === null || c.disk_free === null) {
    return (
      <>
        <dd className="mt-1 text-sm font-medium">{t("Capacity unknown")}</dd>
        <dd className="text-[11px] text-muted-foreground">{remote ? t("S3, SFTP and FTP don't report how much room is left") : t("The disk didn't answer in time")}</dd>
      </>
    );
  }
  const over = threshold > 0 && pct >= threshold;
  return (
    <>
      <dd className="mt-1 flex items-center gap-1.5 text-sm font-medium tabular-nums">
        {over && <TriangleAlertIcon className="size-3.5 text-destructive" aria-hidden />}
        {t("{used} used of {total}", { used: formatBytes(diskUsed(c)!), total: formatBytes(c.disk_total) })}
      </dd>
      <dd>
        <div
          role="meter"
          aria-label={t("Disk used")}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(pct)}
          aria-valuetext={percent(pct)}
          className="mt-1 h-1.5 overflow-hidden rounded-full bg-muted"
        >
          <div className={cn("h-full rounded-full", over ? "bg-destructive" : "bg-brand")} style={{ width: `${Math.min(100, pct)}%` }} />
        </div>
      </dd>
      <dd className="mt-0.5 text-[11px] text-muted-foreground tabular-nums">{t("{free} free · {percent} used", { free: formatBytes(c.disk_free), percent: percent(pct) })}</dd>
    </>
  );
}

/** The last hour's operations of one kind of work, as one line per operation */
function Recent({ list, span }: { list: OpSummary[]; span: number }) {
  const people = list.filter((s) => s.work === "foreground" && (s.op === "read" || s.op === "write"));
  const others = sumFigures(list.filter((s) => s.work !== "foreground"));
  if (!list.length) return <dd className="mt-1 text-sm text-muted-foreground">{t("No operations in the last hour")}</dd>;
  const line = (label: string, f: OpFigures, bytes: boolean) => {
    const failed = errorRate(f) ?? 0;
    return (
      <dd className="text-xs tabular-nums">
        <span className="font-medium">{label}</span>: {t("{n} operation|{n} operations", { n: f.count })}
        {bytes && ` · ${rate(perSecond(f.bytes, span))}`}
        {f.p95_ms !== null && ` · p95 ${ms(f.p95_ms)}`}
        {failed > 0 && (
          <span className="text-destructive">
            {" · "}
            {t("{percent} failed", { percent: percent(failed) })}
          </span>
        )}
      </dd>
    );
  };
  return (
    <>
      {people.map((s) => (
        <Fragment key={s.op}>{line(opLabel(s.op), s, true)}</Fragment>
      ))}
      {others && line(t("Background work and checks"), others, false)}
    </>
  );
}

function LocationCard({ l, overview }: { l: LocationUsage; overview: UsageOverview }) {
  const c = l.capacity;
  return (
    <article className="grid gap-3 border-b p-4 last:border-b-0" aria-labelledby={`usage-${l.id}`}>
      <header className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <h3 id={`usage-${l.id}`} className="text-sm font-medium">
          {l.name}
        </h3>
        <span className="text-xs text-muted-foreground">{KIND[l.kind]()}</span>
        <span className={cn("flex items-center gap-1 text-xs", l.online ? "text-muted-foreground" : "text-destructive")}>
          {l.online ? <CircleCheckIcon className="size-3.5" aria-hidden /> : <CircleXIcon className="size-3.5" aria-hidden />}
          {l.online ? t("Online") : t("Offline")}
        </span>
        {!l.online && l.error && <span className="w-full text-xs text-destructive">{tServer(l.error)}</span>}
      </header>
      {!c ? (
        <p className="text-sm text-muted-foreground">{t("No samples yet. The first is taken shortly after ThirtyFile starts.")}</p>
      ) : (
        <dl className="grid grid-cols-1 gap-x-6 gap-y-3 sm:grid-cols-2">
          <div>
            <dt className="text-xs text-muted-foreground">{t("Files")}</dt>
            <dd className="mt-1 text-sm font-medium tabular-nums">{formatBytes(c.live_bytes)}</dd>
            <dd className="text-[11px] text-muted-foreground">
              {t("Trash {trash} · earlier versions {versions}", { trash: formatBytes(c.trash_bytes), versions: formatBytes(c.version_bytes) })}
            </dd>
          </div>
          <div>
            <dt className="text-xs text-muted-foreground">{t("Stored by ThirtyFile")}</dt>
            <dd className="mt-1 text-sm font-medium tabular-nums">{formatBytes(storedBytes(c))}</dd>
            <dd className="text-[11px] text-muted-foreground">
              {c.folder_bytes + c.folder_version_bytes > 0
                ? t("Content store {store} · folder spaces {folders}", { store: formatBytes(c.store_bytes), folders: formatBytes(c.folder_bytes + c.folder_version_bytes) })
                : t("Identical content is stored once")}
            </dd>
          </div>
          <div>
            <dt className="text-xs text-muted-foreground">{t("Disk")}</dt>
            <Disk c={c} remote={l.kind !== "local"} threshold={overview.thresholds.disk_percent} />
            {l.same_disk_as.length > 0 && (
              <dd className="text-[11px] text-muted-foreground">
                {t("Same disk as {names}: its size is counted once", { names: l.same_disk_as.map((id) => nameOf(overview, id)).join(", ") })}
              </dd>
            )}
          </div>
          <div>
            <dt className="text-xs text-muted-foreground">{t("Last hour")}</dt>
            <Recent list={l.recent} span={overview.recent_secs} />
            {l.active.transfers + l.active.calls > 0 && (
              <dd className="text-[11px] text-muted-foreground">{t("Running now: {n} transfer|Running now: {n} transfers", { n: l.active.transfers + l.active.calls })}</dd>
            )}
          </div>
          {(c.pending_deletes > 0 || c.backup_bytes !== null || c.replica_bytes !== null) && (
            <div>
              {c.pending_deletes > 0 && (
                <>
                  <dt className="text-xs text-muted-foreground">{t("Waiting to be deleted")}</dt>
                  <dd className="mt-1 text-sm tabular-nums">{t("{n} content|{n} contents", { n: c.pending_deletes })}</dd>
                </>
              )}
              {c.backup_bytes !== null && (
                <>
                  <dt className="text-xs text-muted-foreground">{t("Backups")}</dt>
                  <dd className="mt-1 text-sm tabular-nums">{formatBytes(c.backup_bytes)}</dd>
                </>
              )}
              {c.replica_bytes !== null && (
                <>
                  <dt className="text-xs text-muted-foreground">{t("Replicas")}</dt>
                  <dd className="mt-1 text-sm tabular-nums">{formatBytes(c.replica_bytes)}</dd>
                </>
              )}
            </div>
          )}
        </dl>
      )}
      {c && <Sampled c={c} />}
    </article>
  );
}

function Sampled({ c }: { c: SampledCapacity }) {
  return (
    <p className={cn("flex items-center gap-1 text-[11px]", c.stale ? "text-destructive" : "text-muted-foreground")}>
      <ClockIcon className="size-3" aria-hidden />
      {c.stale ? t("Out of date: sampled {time}", { time: formatDateTime(c.sampled_at) }) : t("Sampled {time}", { time: formatTime(c.sampled_at) })}
    </p>
  );
}

function Locations({ overview }: { overview: UsageOverview }) {
  const all = overview.total;
  const qd = overview.queue;
  return (
    <Section title={t("Storage locations")}>
      {overview.locations.map((l) => (
        <LocationCard key={l.id} l={l} overview={overview} />
      ))}
      <article className="grid gap-3 p-4" aria-labelledby="usage-data">
        <h3 id="usage-data" className="text-sm font-medium">
          {t("Data folder")}
        </h3>
        <p className="-mt-2 text-xs text-muted-foreground">{t("The database, thumbnails and uploads in progress")}</p>
        {all && (
          <dl className="grid grid-cols-1 gap-x-6 gap-y-3 sm:grid-cols-2">
            <div>
              <dt className="text-xs text-muted-foreground">{t("Disk")}</dt>
              <Disk c={all} remote={false} threshold={overview.thresholds.disk_percent} />
            </div>
            <div>
              <dt className="text-xs text-muted-foreground">{t("Uploads in progress")}</dt>
              <dd className="mt-1 text-sm font-medium tabular-nums">{formatBytes(all.temp_bytes)}</dd>
              <dd className="text-[11px] text-muted-foreground">{t("Taking the sample took {n} ms", { n: all.took_ms })}</dd>
            </div>
          </dl>
        )}
        <dl className="grid grid-cols-1 gap-x-6 gap-y-3 sm:grid-cols-2">
          <div>
            <dt className="text-xs text-muted-foreground">{t("Waiting and running")}</dt>
            <dd className="mt-1 text-xs tabular-nums">
              {t("Moves: {queued} waiting, {running} running, {paused} paused", { queued: qd.moves_queued, running: qd.moves_running, paused: qd.moves_paused })}
            </dd>
            <dd className="text-xs tabular-nums">{t("{n} task running|{n} tasks running", { n: qd.jobs_running })}</dd>
            <dd className="text-xs tabular-nums">{t("{n} content waiting to be deleted|{n} contents waiting to be deleted", { n: qd.pending_deletes })}</dd>
            <dd className="text-[11px]">
              <Link className="underline underline-offset-2" to="/admin/moves">
                {t("Moves")}
              </Link>
              {" · "}
              <Link className="underline underline-offset-2" to="/admin/activity">
                {t("Activity log")}
              </Link>
            </dd>
          </div>
          {overview.unplaced.length > 0 && (
            <div>
              <dt className="text-xs text-muted-foreground">{t("Folders on no storage location, last hour")}</dt>
              <Recent list={overview.unplaced} span={overview.recent_secs} />
            </div>
          )}
        </dl>
        {all && <Sampled c={all} />}
      </article>
    </Section>
  );
}

function forecastText(f: UsageForecast): string {
  const days = f.window_days.toLocaleString(locale, { maximumFractionDigits: 1 });
  switch (f.reason) {
    case "ok":
      return f.days! > 3650
        ? t("Time to full: more than ten years at the pace of the last {days} days", { days })
        : t("Time to full: about {n} days at the pace of the last {days} days ({points} hourly samples)", {
            n: Math.round(f.days!).toLocaleString(locale),
            days,
            points: f.points,
          });
    case "short":
      return t("Time to full: needs at least 3 days of samples");
    case "not_growing":
      return t("Time to full: the disk didn't fill up over the last {days} days", { days });
    case "unsteady":
      return t("Time to full: use went up and down too much over the last {days} days to tell", { days });
    case "unknown_capacity":
      return t("Time to full: the size of this disk isn't known");
  }
}

function History({
  overview,
  filter,
  onFilter,
  history: h,
  loading,
  error,
}: {
  overview: UsageOverview;
  filter: { location: string; range: UsageRange; work: UsageWork | "all"; op: UsageOp };
  onFilter(f: Partial<{ location: string; range: UsageRange; work: UsageWork | "all"; op: UsageOp }>): void;
  history: UsageHistory | undefined;
  loading: boolean;
  error: Error | null;
}) {
  const download = () => {
    if (!h) return;
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([historyCsv(h)], { type: "text/csv" }));
    a.download = `storage-usage-${h.location || "all"}-${h.range}.csv`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
  };
  const field = "grid gap-1 text-xs text-muted-foreground";
  return (
    <Section title={t("History")}>
      <div className="grid gap-6 p-4">
        <div className="flex flex-wrap items-end gap-3">
          <label className={field}>
            {t("Storage location")}
            <NativeSelect size="sm" value={filter.location} onChange={(e) => onFilter({ location: e.target.value })}>
              <option value="">{t("All locations")}</option>
              {overview.locations.map((l) => (
                <option key={l.id} value={l.id}>
                  {l.name}
                </option>
              ))}
            </NativeSelect>
          </label>
          <label className={field}>
            {t("Time range")}
            <NativeSelect size="sm" value={filter.range} onChange={(e) => onFilter({ range: e.target.value as UsageRange })}>
              <option value="6h">{t("Last 6 hours")}</option>
              <option value="2d">{t("Last 2 days")}</option>
              <option value="30d">{t("Last 30 days")}</option>
              <option value="1y">{t("Last year")}</option>
            </NativeSelect>
          </label>
          <label className={field}>
            {t("Operations of")}
            <NativeSelect size="sm" value={filter.work} onChange={(e) => onFilter({ work: e.target.value as UsageWork | "all" })}>
              <option value="foreground">{t("People using files")}</option>
              <option value="background">{t("Background work")}</option>
              <option value="probe">{t("Checks and tests")}</option>
              <option value="all">{t("Everything")}</option>
            </NativeSelect>
          </label>
          <Button variant="outline" size="sm" onClick={download} disabled={!h}>
            <DownloadIcon aria-hidden />
            {t("Download CSV")}
          </Button>
        </div>
        {error ? (
          <p className="text-sm text-destructive">{error.message}</p>
        ) : !h ? (
          <Skeleton className="h-60" />
        ) : (
          <div className={cn("grid gap-8", loading && "opacity-70")} aria-busy={loading}>
            <Charts h={h} overview={overview} op={filter.op} onOp={(op) => onFilter({ op })} />
          </div>
        )}
        <p className="text-[11px] text-muted-foreground">
          {t(
            "Times are in your time zone; daily points are UTC days. Operations are counted for the content store, and for files of folder spaces that are read or uploaded; renaming, deleting and scanning in folder spaces aren't measured. Kept: 5-minute points for 2 days, hours for 45 days, days for 400 days.",
          )}
        </p>
      </div>
    </Section>
  );
}

function Charts({ h, overview, op, onOp }: { h: UsageHistory; overview: UsageOverview; op: UsageOp; onOp(op: UsageOp): void }) {
  const period = periodLabel(h.capacity_span);
  const opsPeriod = periodLabel(h.ops_span);
  const cap = h.capacity;
  const times = cap.map((c) => c.at);
  const hasDisk = cap.some((c) => c.disk_total !== null);
  const lastTotal = [...cap].reverse().find((c) => c.disk_total !== null)?.disk_total ?? null;
  const threshold = overview.thresholds.disk_percent;
  const opsOf = (o: UsageOp) => h.ops.find((s) => s.op === o)?.points ?? [];
  const rw = alignSeries([opsOf("read"), opsOf("write")], h.ops_span);
  const chosen = opsOf(op);
  const lat = alignSeries([chosen], h.ops_span);
  const failed = alignSeries(
    [
      Array.from(
        h.ops
          .flatMap((s) => s.points)
          .reduce<Map<number, { at: number; n: number }>>((m, p) => {
            const e = m.get(p.at) ?? { at: p.at, n: 0 };
            e.n += p.errors + p.timeouts;
            return m.set(p.at, e);
          }, new Map())
          .values(),
      ).sort((a, b) => a.at - b.at),
    ],
    h.ops_span,
  );
  const total = (points: OpFigures[]) => points.reduce((n, p) => n + p.count, 0);
  const common = { from: h.from, to: h.to };
  return (
    <>
      <UsageChart
        title={t("Content")}
        unit={t("The last sample of each {period}", { period })}
        times={times}
        span={h.capacity_span}
        {...common}
        series={[
          { label: t("Files"), values: cap.map((c) => c.live_bytes) },
          { label: t("Stored by ThirtyFile"), values: cap.map(storedBytes) },
          { label: t("Trash and earlier versions"), values: cap.map((c) => c.trash_bytes + c.version_bytes) },
        ]}
        format={formatBytes}
        ticks={byteTicks}
        empty={t("No samples in this range")}
      />
      <div className="grid gap-2">
        <UsageChart
          title={t("Disk")}
          unit={hasDisk ? t("Used by everything on the disk; the last sample of each {period}", { period }) : t("The size of this disk isn't known")}
          times={times}
          span={h.capacity_span}
          {...common}
          series={[{ label: t("Disk used"), values: cap.map(diskUsed) }]}
          format={formatBytes}
          ticks={(max) => byteTicks(Math.max(max, lastTotal ?? 0))}
          reference={lastTotal !== null && threshold > 0 ? { value: (lastTotal * threshold) / 100, label: t("Alert at {percent}", { percent: percent(threshold) }) } : undefined}
          empty={hasDisk || !cap.length ? t("No samples in this range") : t("Capacity unknown")}
        />
        <p className="text-xs text-muted-foreground">{forecastText(h.forecast)}</p>
      </div>
      <UsageChart
        title={t("Throughput")}
        unit={t("Bytes per second, averaged over each {period}; {n} reads and writes in this range", { period: opsPeriod, n: count(total(opsOf("read")) + total(opsOf("write"))) })}
        times={rw.map((r) => r.at)}
        span={h.ops_span}
        {...common}
        series={[
          { label: t("Read"), values: rw.map((r) => (r.points[0] ? perSecond(r.points[0].bytes, h.ops_span) : null)) },
          { label: t("Written"), values: rw.map((r) => (r.points[1] ? perSecond(r.points[1].bytes, h.ops_span) : null)) },
        ]}
        format={rate}
        ticks={byteTicks}
        empty={t("No reads or writes in this range")}
      />
      <div className="grid gap-2">
        <label className="flex items-center gap-2 text-xs text-muted-foreground">
          {t("Time of")}
          <NativeSelect size="xs" value={op} onChange={(e) => onOp(e.target.value as UsageOp)}>
            {OPS.map((o) => (
              <option key={o.op} value={o.op}>
                {o.label()}
              </option>
            ))}
          </NativeSelect>
        </label>
        <UsageChart
          title={t("Time per operation: {op}", { op: opLabel(op) })}
          unit={t("Milliseconds per {period}; {n} operations in this range. Reads are timed until they can start, writes until stored.", {
            period: opsPeriod,
            n: count(total(chosen)),
          })}
          times={lat.map((r) => r.at)}
          span={h.ops_span}
          {...common}
          series={[
            { label: t("Median (p50)"), values: lat.map((r) => r.points[0]?.p50_ms ?? null) },
            { label: t("95th percentile (p95)"), values: lat.map((r) => r.points[0]?.p95_ms ?? null) },
          ]}
          format={ms}
          ticks={niceTicks}
          empty={t("No operations in this range")}
        />
      </div>
      <UsageChart
        title={t("Failed or timed out")}
        unit={t("Operations per {period}, all kinds together", { period: opsPeriod })}
        times={failed.map((r) => r.at)}
        span={h.ops_span}
        {...common}
        series={[{ label: t("Failed or timed out"), values: failed.map((r) => r.points[0]?.n ?? null) }]}
        format={count}
        ticks={countTicks}
        bars
        empty={t("No operations in this range")}
      />
    </>
  );
}

function Thresholds({ value }: { value: UsageThresholds }) {
  const qc = useQueryClient();
  const [disk, setDisk] = useState(String(value.disk_percent));
  const [errors, setErrors] = useState(String(value.error_percent));
  const valid = (s: string) => s.trim() !== "" && Number.isFinite(Number(s)) && Number(s) >= 0 && Number(s) <= 100;
  const save = useMutation({
    mutationFn: () => api.setUsageThresholds({ disk_percent: Number(disk), error_percent: Number(errors) }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: keys.usage() });
      toast.success(t("Alert thresholds saved"));
    },
    onError: (e) => toast.error(e.message),
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (valid(disk) && valid(errors)) save.mutate();
  };
  const row = (id: string, label: string, v: string, set: (s: string) => void) => (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <label htmlFor={id} className="text-sm">
        {label}
      </label>
      <div className="relative">
        <Input id={id} inputMode="decimal" className="h-8 w-24 pr-7 text-right tabular-nums" value={v} aria-invalid={!valid(v)} onChange={(e) => set(e.target.value)} />
        <span className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-xs text-muted-foreground">%</span>
      </div>
    </div>
  );
  return (
    <Section title={t("Alert thresholds")}>
      <form className="grid gap-3 p-4" onSubmit={submit}>
        {row("usage-disk", t("Alert when a disk is fuller than"), disk, setDisk)}
        {row("usage-errors", t("Alert when more operations fail in an hour than"), errors, setErrors)}
        <div className="flex flex-wrap items-center justify-between gap-2">
          <p className="text-xs text-muted-foreground">{t("0 turns an alert off. Alerts are shown at the top of this page.")}</p>
          <Button type="submit" size="sm" disabled={save.isPending || !valid(disk) || !valid(errors)}>
            {t("Save")}
          </Button>
        </div>
      </form>
    </Section>
  );
}
