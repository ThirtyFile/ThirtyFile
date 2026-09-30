import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { cn } from "@/lib/utils";
import { locale, t } from "@/lib/i18n";

/**
 * A small chart of values over time, drawn as inline SVG (Control panel › Storage usage). Up to three series, told
 * apart by colour, line pattern and marker shape; null values leave a gap. Pointing at it, or moving along it with the
 * arrow keys when it has the focus, shows the values of one period; the same values are in a table below it.
 */

export interface ChartSeries {
  label: string;
  /** One value per row of `times`; null where there is none */
  values: (number | null)[];
}

const HEIGHT = 170;
const PAD = { left: 60, right: 12, top: 10, bottom: 24 };
/** Line pattern and marker of each series, besides its colour */
const STYLES = [
  { color: "var(--series-1)", dash: undefined, marker: "circle" },
  { color: "var(--series-2)", dash: "6 3", marker: "square" },
  { color: "var(--series-3)", dash: "2 3", marker: "triangle" },
] as const;

function Marker({ kind, x, y, color }: { kind: (typeof STYLES)[number]["marker"]; x: number; y: number; color: string }) {
  const common = { fill: color, stroke: "var(--card)", strokeWidth: 1.5 };
  if (kind === "square") return <rect x={x - 3.5} y={y - 3.5} width={7} height={7} {...common} />;
  if (kind === "triangle") return <path d={`M${x},${y - 4.5}L${x + 4.5},${y + 3.5}L${x - 4.5},${y + 3.5}Z`} {...common} />;
  return <circle cx={x} cy={y} r={4} {...common} />;
}

/** The swatch of a series in the legend: its line pattern and marker */
export function SeriesSwatch({ index }: { index: number }) {
  const s = STYLES[index];
  return (
    <svg width="26" height="10" aria-hidden="true" className="shrink-0">
      <line x1="1" x2="25" y1="5" y2="5" stroke={s.color} strokeWidth="2" strokeDasharray={s.dash} />
      <Marker kind={s.marker} x={13} y={5} color={s.color} />
    </svg>
  );
}

/** Width of an element, followed as it changes */
function useWidth<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setWidth(el.clientWidth);
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, width] as const;
}

/** A time as the axis and the table show it: the time of day for short ranges, the date for long ones */
export function timeLabel(ts: number, span: number, withDate = false): string {
  const d = new Date(ts * 1000);
  if (span >= 86400) return new Intl.DateTimeFormat(locale, { month: "2-digit", day: "2-digit" }).format(d);
  const opts: Intl.DateTimeFormatOptions = { hour: "2-digit", minute: "2-digit", hour12: false };
  if (withDate || span >= 3600) Object.assign(opts, { month: "2-digit", day: "2-digit" });
  return new Intl.DateTimeFormat(locale, opts).format(d);
}

export function UsageChart({
  title,
  unit,
  times,
  span,
  from,
  to,
  series,
  format,
  ticks,
  bars = false,
  reference,
  empty,
}: {
  title: string;
  /** What the values are and over what period, e.g. "Bytes per second, averaged over 5 minutes" */
  unit: string;
  /** Start of each period (Unix seconds) */
  times: number[];
  /** Seconds each period stands for */
  span: number;
  from: number;
  to: number;
  series: ChartSeries[];
  format(v: number): string;
  /** Axis values from 0 to the largest value */
  ticks(max: number): number[];
  /** Bars instead of lines (one series) */
  bars?: boolean;
  /** A line across the chart, e.g. a threshold */
  reference?: { value: number; label: string };
  /** Said instead of the chart when there is nothing to show */
  empty: string;
}) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const [at, setAt] = useState<number | null>(null);
  const id = useId();
  const values = series.flatMap((s) => s.values.filter((v): v is number => v !== null));
  const hasData = values.length > 0;
  useEffect(() => setAt(null), [times.length]);

  const yTicks = ticks(Math.max(...values, reference?.value ?? 0, 0));
  const yMax = yTicks[yTicks.length - 1] || 1;
  const plotW = Math.max(10, width - PAD.left - PAD.right);
  const plotH = HEIGHT - PAD.top - PAD.bottom;
  const x = (ts: number) => PAD.left + ((ts + span / 2 - from) / Math.max(1, to - from)) * plotW;
  const y = (v: number) => PAD.top + plotH - (v / yMax) * plotH;
  const xTicks = Array.from({ length: width < 420 ? 3 : 5 }, (_, i) => from + ((to - from) * i) / (width < 420 ? 2 : 4));

  const summary = hasData
    ? `${title}: ${series
        .map((s) => {
          const known = s.values.filter((v): v is number => v !== null);
          if (!known.length) return `${s.label}: ${empty}`;
          return t("{label} from {min} to {max}, latest {last}", {
            label: s.label,
            min: format(Math.min(...known)),
            max: format(Math.max(...known)),
            last: format(known[known.length - 1]),
          });
        })
        .join("; ")}`
    : `${title}: ${empty}`;

  const nearest = (px: number) => {
    let best = -1;
    let dist = Infinity;
    times.forEach((ts, i) => {
      if (series.every((s) => s.values[i] === null)) return;
      const d = Math.abs(x(ts) - px);
      if (d < dist) [best, dist] = [i, d];
    });
    return best < 0 ? null : best;
  };
  const withValues = times.map((_, i) => i).filter((i) => series.some((s) => s.values[i] !== null));
  const step = (dir: number) => {
    if (!withValues.length) return;
    const pos = at === null ? (dir > 0 ? -1 : withValues.length) : withValues.indexOf(at);
    setAt(withValues[Math.min(withValues.length - 1, Math.max(0, pos + dir))]);
  };

  return (
    <figure className="grid gap-2">
      <figcaption className="grid gap-0.5">
        <span className="text-sm font-medium">{title}</span>
        <span className="text-xs text-muted-foreground">{unit}</span>
      </figcaption>
      {series.length > 1 && (
        <ul className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground" aria-label={t("Legend")}>
          {series.map((s, i) => (
            <li key={s.label} className="flex items-center gap-1.5">
              <SeriesSwatch index={i} />
              {s.label}
            </li>
          ))}
        </ul>
      )}
      <div ref={ref} className="relative">
        {!hasData ? (
          <p className="grid h-24 place-items-center rounded-md border border-dashed text-xs text-muted-foreground">{empty}</p>
        ) : (
          width > 0 && (
            <svg
              width={width}
              height={HEIGHT}
              role="img"
              aria-label={summary}
              aria-describedby={at !== null ? `${id}-tip` : undefined}
              tabIndex={0}
              className="block rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
              onPointerMove={(e) => setAt(nearest(e.clientX - e.currentTarget.getBoundingClientRect().left))}
              onPointerLeave={() => setAt(null)}
              onBlur={() => setAt(null)}
              onKeyDown={(e) => {
                if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
                  e.preventDefault();
                  step(e.key === "ArrowRight" ? 1 : -1);
                } else if (e.key === "Escape") setAt(null);
              }}
            >
              {yTicks.map((v) => (
                <g key={v}>
                  <line x1={PAD.left} x2={PAD.left + plotW} y1={y(v)} y2={y(v)} stroke="var(--border)" strokeWidth={1} />
                  <text x={PAD.left - 6} y={y(v)} dy="0.32em" textAnchor="end" className="fill-muted-foreground text-[10px] tabular-nums">
                    {format(v)}
                  </text>
                </g>
              ))}
              {xTicks.map((ts, i) => (
                <text
                  key={ts}
                  x={PAD.left + ((ts - from) / Math.max(1, to - from)) * plotW}
                  y={HEIGHT - 6}
                  textAnchor={i === 0 ? "start" : i === xTicks.length - 1 ? "end" : "middle"}
                  className="fill-muted-foreground text-[10px] tabular-nums"
                >
                  {timeLabel(ts, span, to - from > 86400)}
                </text>
              ))}
              {reference && (
                <g>
                  <line x1={PAD.left} x2={PAD.left + plotW} y1={y(reference.value)} y2={y(reference.value)} stroke="var(--muted-foreground)" strokeDasharray="4 4" />
                  <text x={PAD.left + plotW} y={y(reference.value) - 4} textAnchor="end" className="fill-muted-foreground text-[10px]">
                    {reference.label}
                  </text>
                </g>
              )}
              {bars
                ? series[0].values.map((v, i) => {
                    if (v === null || v === 0) return null;
                    const w = Math.max(2, (span / Math.max(1, to - from)) * plotW - 2);
                    return <rect key={times[i]} x={x(times[i]) - w / 2} y={y(v)} width={w} height={Math.max(1, y(0) - y(v))} rx={1} fill={STYLES[0].color} />;
                  })
                : series.map((s, si) => {
                    const style = STYLES[si];
                    // Runs of consecutive values: a null ends a line
                    const runs: [number, number][][] = [[]];
                    s.values.forEach((v, i) => {
                      if (v === null) runs.push([]);
                      else runs[runs.length - 1].push([x(times[i]), y(v)]);
                    });
                    const few = s.values.filter((v) => v !== null).length <= 48;
                    return (
                      <g key={s.label}>
                        {runs
                          .filter((r) => r.length > 1)
                          .map((r) => (
                            <polyline
                              key={r[0].join()}
                              points={r.map((p) => p.join(",")).join(" ")}
                              fill="none"
                              stroke={style.color}
                              strokeWidth={2}
                              strokeDasharray={style.dash}
                              strokeLinejoin="round"
                            />
                          ))}
                        {runs
                          .filter((r) => few || r.length === 1)
                          .flat()
                          .map(([px, py]) => (
                            <Marker key={`${px},${py}`} kind={style.marker} x={px} y={py} color={style.color} />
                          ))}
                      </g>
                    );
                  })}
              {at !== null && <line x1={x(times[at])} x2={x(times[at])} y1={PAD.top} y2={PAD.top + plotH} stroke="var(--muted-foreground)" strokeWidth={1} />}
            </svg>
          )
        )}
        {at !== null && hasData && (
          <div
            id={`${id}-tip`}
            role="status"
            className={cn(
              "pointer-events-none absolute top-1 z-10 grid gap-0.5 rounded-md border bg-popover px-2 py-1.5 text-xs text-popover-foreground shadow-md",
              x(times[at]) > width / 2 ? "left-16" : "right-2",
            )}
          >
            <span className="font-medium tabular-nums">{timeLabel(times[at], span, true)}</span>
            {series.map((s, i) => (
              <span key={s.label} className="flex items-center gap-1.5 tabular-nums">
                {series.length > 1 && <SeriesSwatch index={i} />}
                {s.label}: {s.values[at] === null ? t("No operations") : format(s.values[at]!)}
              </span>
            ))}
          </div>
        )}
      </div>
      {hasData && (
        <details className="text-xs">
          <summary className="cursor-pointer text-muted-foreground hover:text-foreground">{t("Show as a table")}</summary>
          <div className="mt-2 max-h-64 overflow-auto rounded-md border">
            <table className="w-full text-left tabular-nums">
              <caption className="sr-only">{title}</caption>
              <thead className="sticky top-0 bg-muted">
                <tr>
                  <th scope="col" className="px-2 py-1 font-medium">
                    {t("Period starting")}
                  </th>
                  {series.map((s) => (
                    <th key={s.label} scope="col" className="px-2 py-1 font-medium">
                      {s.label}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {withValues.map((i) => (
                  <tr key={times[i]} className="border-t">
                    <th scope="row" className="px-2 py-1 font-normal">
                      {timeLabel(times[i], span, true)}
                    </th>
                    {series.map((s) => (
                      <td key={s.label} className="px-2 py-1">
                        {s.values[i] === null ? "—" : format(s.values[i]!)}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </details>
      )}
    </figure>
  );
}
