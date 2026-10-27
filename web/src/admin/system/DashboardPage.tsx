import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { CircleAlertIcon, CircleCheckIcon, TriangleAlertIcon } from "lucide-react";
import { Link } from "react-router";
import { api, type DashboardInfo, type DashboardSeries } from "@/api";
import { keys } from "@/api/queryKeys";
import { SettingsFrame } from "@/admin/SettingsFrame";
import { UsageChart } from "@/admin/storage/UsageChart";
import { ErrorState } from "@/components/ErrorState";
import { Button } from "@/components/ui/button";
import { alertText } from "@/lib/alerts";
import { t, tServer } from "@/lib/i18n";
import { countTicks, niceTicks } from "@/lib/usage";
import { cn, formatBytes, formatDateTime } from "@/lib/utils";

const DAY = 86400;

/** Control panel › Dashboard: what is wrong now, a few numbers, the last day or week, and the latest errors */
export function DashboardPage() {
  const [span, setSpan] = useState(DAY);
  const q = useQuery({ queryKey: keys.dashboard(span), queryFn: ({ signal }) => api.dashboard(span, signal), refetchInterval: 30_000, placeholderData: (previous) => previous });
  return (
    <SettingsFrame item="dashboard" onRefresh={() => q.refetch()}>
      {q.error && !q.data ? (
        <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
      ) : !q.data ? (
        <p role="status">{t("Loading…")}</p>
      ) : (
        <Dashboard info={q.data} span={span} setSpan={setSpan} />
      )}
    </SettingsFrame>
  );
}

/** One figure for each period from `from`, null where the server wrote none (it wasn't running) */
export function periods(info: DashboardInfo): number[] {
  const first = Math.floor(info.from / info.step) * info.step;
  const times: number[] = [];
  for (let at = first; at <= info.now; at += info.step) times.push(at);
  return times;
}

export function values(info: DashboardInfo, times: number[], name: DashboardSeries): (number | null)[] {
  const byTime = new Map(info.series[name]);
  return times.map((at) => byTime.get(at) ?? null);
}

function Banner({ info }: { info: DashboardInfo }) {
  const ok = info.status === "ok";
  const Icon = ok ? CircleCheckIcon : info.status === "error" ? CircleAlertIcon : TriangleAlertIcon;
  return (
    <div
      role="status"
      className={cn(
        "flex items-start gap-3 rounded-lg border p-3 text-sm",
        ok ? "border-emerald-500/40 bg-emerald-500/10" : info.status === "error" ? "border-red-500/40 bg-red-500/10" : "border-amber-500/40 bg-amber-500/10",
      )}
    >
      <Icon className="mt-0.5 size-5 shrink-0" />
      <div className="grid gap-1">
        <span className="font-medium">{ok ? t("Everything is working") : info.status === "error" ? t("The database can't be read") : t("Something needs a look")}</span>
        {info.warnings.map((w) => (
          <span key={w} className="text-xs">
            {tServer(w)}
          </span>
        ))}
      </div>
    </div>
  );
}

function Figure({ label, value, note }: { label: string; value: string; note?: string }) {
  return (
    <div className="grid gap-0.5 rounded-lg border bg-card p-3">
      <span className="text-xs text-muted-foreground">{label}</span>
      <span className="text-xl font-semibold tabular-nums">{value}</span>
      {note && <span className="text-xs text-muted-foreground">{note}</span>}
    </div>
  );
}

const count = (v: number) => new Intl.NumberFormat().format(Math.round(v));
const percent = (v: number) => `${Math.round(v)}%`;
const millis = (v: number) => (v >= 1000 ? t("{n} s", { n: new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(v / 1000) }) : t("{n} ms", { n: count(v) }));

function Dashboard({ info, span, setSpan }: { info: DashboardInfo; span: number; setSpan(s: number): void }) {
  const times = periods(info);
  const v = (name: DashboardSeries) => values(info, times, name);
  const common = { times, span: info.step, from: times[0] ?? info.from, to: info.now };
  const n = info.numbers;
  const empty = t("No figures in this range");
  return (
    <>
      <Banner info={info} />

      {info.alerts.length > 0 && (
        <section className="grid gap-2" aria-label={t("Alerts firing now")}>
          <h2 className="text-xs font-medium text-muted-foreground">{t("Alerts firing now")}</h2>
          <ul className="grid gap-1 rounded-lg border bg-card p-3 text-sm">
            {info.alerts.map((a) => (
              <li key={`${a.kind}-${a.since}-${JSON.stringify(a.params)}`} className="flex flex-wrap items-baseline justify-between gap-x-3">
                <span>{alertText(a.kind, a.params)}</span>
                <span className="text-xs text-muted-foreground">{t("since {time}", { time: formatDateTime(a.since) })}</span>
              </li>
            ))}
          </ul>
          <Link to="/admin/alerts" className="w-fit text-xs underline underline-offset-2">
            {t("Administrator alerts")}
          </Link>
        </section>
      )}

      <div className="grid grid-cols-[repeat(auto-fill,minmax(160px,1fr))] gap-2">
        <Figure label={t("Signed in now")} value={count(n.sessions)} note={t("Sessions that haven't ended")} />
        <Figure label={t("Active in 24 hours")} value={count(n.active_accounts)} note={t("People whose session was used")} />
        <Figure label={t("Tasks running")} value={n.tasks_running === null ? "—" : count(n.tasks_running)} />
        <Figure label={t("Uploads in progress")} value={n.uploads === null ? "—" : count(n.uploads)} />
        <Figure
          label={t("Free disk space")}
          value={n.disk_free_percent === null ? "—" : percent(n.disk_free_percent)}
          note={n.disk_free_bytes === null ? undefined : formatBytes(n.disk_free_bytes)}
        />
      </div>

      <div className="flex flex-wrap items-center gap-2" role="group" aria-label={t("Period")}>
        {[
          [DAY, t("Last 24 hours")],
          [7 * DAY, t("Last 7 days")],
        ].map(([s, label]) => (
          <Button key={s} type="button" size="sm" variant={span === s ? "default" : "outline"} aria-pressed={span === s} onClick={() => setSpan(Number(s))}>
            {label}
          </Button>
        ))}
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        <UsageChart
          title={t("Requests")}
          unit={t("Requests answered in each {minutes} minutes, and those that failed with a server error", { minutes: info.step / 60 })}
          {...common}
          series={[
            { label: t("Requests"), values: v("requests") },
            { label: t("Server errors"), values: v("errors") },
          ]}
          format={count}
          ticks={countTicks}
          empty={empty}
        />
        <UsageChart
          title={t("Time to answer")}
          unit={t("The time 95 of every 100 requests took to start their answer")}
          {...common}
          series={[{ label: t("95th percentile"), values: v("latency_p95_ms") }]}
          format={millis}
          ticks={niceTicks}
          empty={empty}
        />
        <UsageChart
          title={t("People and work")}
          unit={t("The most in each period")}
          {...common}
          series={[
            { label: t("Signed in"), values: v("sessions") },
            { label: t("Tasks running"), values: v("tasks_running") },
            { label: t("Uploads in progress"), values: v("uploads") },
          ]}
          format={count}
          ticks={countTicks}
          empty={empty}
        />
        <UsageChart
          title={t("Storage operations")}
          unit={t("Calls to the storage locations in each period, and those that failed")}
          {...common}
          series={[
            { label: t("Operations"), values: v("storage_ops") },
            { label: t("Failed"), values: v("storage_errors") },
          ]}
          format={count}
          ticks={countTicks}
          empty={empty}
        />
        <UsageChart
          title={t("Failed sign-ins")}
          unit={t("Wrong passwords and codes, locked accounts and unknown users in each period")}
          {...common}
          series={[{ label: t("Failed sign-ins"), values: v("failed_sign_ins") }]}
          format={count}
          ticks={countTicks}
          bars
          empty={empty}
        />
        <UsageChart
          title={t("Free disk space")}
          unit={t("The disk of the data and storage folders with the least room, in percent")}
          {...common}
          series={[{ label: t("Free"), values: v("disk_free_percent") }]}
          format={percent}
          ticks={() => [0, 25, 50, 75, 100]}
          empty={empty}
        />
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        <section className="grid content-start gap-2" aria-label={t("Latest server errors")}>
          <h2 className="text-xs font-medium text-muted-foreground">{t("Latest server errors")}</h2>
          <div className="rounded-lg border bg-card">
            {info.errors.length === 0 ? (
              <p className="p-3 text-sm text-muted-foreground">{t("None")}</p>
            ) : (
              <ul className="divide-y text-sm">
                {info.errors.map((e) => (
                  <li key={`${e.at}-${e.route}-${e.message}`} className="grid gap-0.5 px-3 py-2">
                    <span className="flex flex-wrap items-baseline justify-between gap-x-3">
                      <span className="truncate font-medium">{e.message ? tServer(e.message) : e.kind}</span>
                      <span className="text-xs text-muted-foreground">{formatDateTime(e.at)}</span>
                    </span>
                    <span className="truncate text-xs text-muted-foreground">
                      {e.route || "—"}
                      {e.count > 1 ? ` · ${t("{n} times", { n: e.count })}` : ""}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </div>
          <Link to="/admin/activity" className="w-fit text-xs underline underline-offset-2">
            {t("Activity log")}
          </Link>
        </section>
        <section className="grid content-start gap-2" aria-label={t("Latest failed sign-ins")}>
          <h2 className="text-xs font-medium text-muted-foreground">{t("Latest failed sign-ins")}</h2>
          <div className="rounded-lg border bg-card">
            {info.failed_sign_ins.length === 0 ? (
              <p className="p-3 text-sm text-muted-foreground">{t("None")}</p>
            ) : (
              <ul className="divide-y text-sm">
                {info.failed_sign_ins.map((f) => (
                  <li key={`${f.at}-${f.username}-${f.event}`} className="grid gap-0.5 px-3 py-2">
                    <span className="flex flex-wrap items-baseline justify-between gap-x-3">
                      <span className="truncate font-medium">{f.username || "—"}</span>
                      <span className="text-xs text-muted-foreground">{formatDateTime(f.at)}</span>
                    </span>
                    <span className="truncate text-xs text-muted-foreground">
                      {f.event} · {f.ip || "—"}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </div>
          <Link to="/admin/activity" className="w-fit text-xs underline underline-offset-2">
            {t("Activity log")}
          </Link>
        </section>
      </div>

      <p className="text-xs text-muted-foreground">
        {info.monitoring ? t("The same figures, and more, are at /metrics for Prometheus.") : t("To read these figures in Prometheus or Grafana, turn on monitoring.")}{" "}
        <Link to="/admin/monitoring" className="underline underline-offset-2">
          {t("Monitoring")}
        </Link>
      </p>
    </>
  );
}
