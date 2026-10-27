import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type BackupSchedule } from "@/api";
import { keys } from "@/api/queryKeys";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { ErrorText } from "@/components/dialogs";
import { locale, t } from "@/lib/i18n";
import { cn, errorMessage, formatDateTime } from "@/lib/utils";

/** The browser's time zone, the default of a new policy */
export const localZone = () => {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
};

/** Every time zone the browser knows */
export const zones = (): string[] => {
  try {
    return (Intl as unknown as { supportedValuesOf(k: string): string[] }).supportedValuesOf("timeZone");
  } catch {
    return ["UTC"];
  }
};

/** A time in a time zone, as the settings show the next snapshots; in the browser's own zone when that one is unknown */
export function zonedTime(t: number, tz: string) {
  try {
    return formatDateTime(t, tz);
  } catch {
    return formatDateTime(t);
  }
}

const DAYS = [1, 2, 3, 4, 5, 6, 7];
/** Monday is 1: named in the interface's language (1 January 2024 was a Monday) */
const dayName = (d: number) => new Intl.DateTimeFormat(locale, { weekday: "short" }).format(new Date(2024, 0, d));

type Kind = "every" | "daily" | "weekly";

/** A schedule being chosen: every so many minutes, every day at a time, or on some days at a time, in a time zone */
export function useSchedule(initial: BackupSchedule, initialTz: string) {
  const [kind, setKind] = useState<Kind>("every" in initial ? "every" : "weekly" in initial ? "weekly" : "daily");
  const [every, setEvery] = useState("every" in initial ? initial.every : 60);
  const [time, setTime] = useState("daily" in initial ? initial.daily : "weekly" in initial ? initial.weekly : "03:00");
  const [days, setDays] = useState<number[]>("weekly" in initial ? initial.days : [1, 2, 3, 4, 5]);
  const [tz, setTz] = useState(initialTz);
  const schedule: BackupSchedule = kind === "every" ? { every } : kind === "daily" ? { daily: time } : { weekly: time, days };
  return { kind, setKind, every, setEvery, time, setTime, days, setDays, tz, setTz, schedule, valid: kind !== "weekly" || days.length > 0 };
}

/**
 * The fields of a schedule (`useSchedule`), with the next times it runs. `every`: the choices of "Every", in minutes;
 * `note`: what happens around summer time and after the server was off
 */
export function ScheduleFields({ s, idPrefix, every = [15, 30, 60, 120, 240, 360, 720], note }: { s: ReturnType<typeof useSchedule>; idPrefix: string; every?: number[]; note: string }) {
  const next = useQuery({
    queryKey: keys.backupNextRuns(JSON.stringify(s.schedule), s.tz),
    queryFn: () => api.backupNextRuns(s.schedule, s.tz),
    enabled: s.valid,
    retry: false,
  });
  const allZones = useMemo(() => zones(), []);
  return (
    <div className="grid gap-2">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <NativeSelect aria-label={t("Schedule")} value={s.kind} onChange={(e) => s.setKind(e.target.value as Kind)}>
          <option value="every">{t("Every")}</option>
          <option value="daily">{t("Every day at")}</option>
          <option value="weekly">{t("On these days at")}</option>
        </NativeSelect>
        {s.kind === "every" ? (
          <NativeSelect aria-label={t("How often")} value={s.every} onChange={(e) => s.setEvery(Number(e.target.value))}>
            {every.map((m) => (
              <option key={m} value={m}>
                {m < 60 ? t("{n} minutes", { n: m }) : t("{n} hour|{n} hours", { n: m / 60 })}
              </option>
            ))}
          </NativeSelect>
        ) : (
          <Input type="time" aria-label={t("Time")} className="w-32" value={s.time} onChange={(e) => s.setTime(e.target.value)} />
        )}
      </div>
      {s.kind === "weekly" && (
        <div className="flex flex-wrap gap-1">
          {DAYS.map((d) => (
            <button
              key={d}
              type="button"
              aria-pressed={s.days.includes(d)}
              className={cn("rounded-md border px-2 py-1 text-xs", s.days.includes(d) ? "border-brand bg-brand/10 text-brand" : "text-muted-foreground")}
              onClick={() => s.setDays((cur) => (cur.includes(d) ? cur.filter((x) => x !== d) : [...cur, d].sort((a, b) => a - b)))}
            >
              {dayName(d)}
            </button>
          ))}
        </div>
      )}
      <div className="grid gap-1">
        <Label htmlFor={`${idPrefix}-tz`}>{t("Time zone")}</Label>
        <NativeSelect id={`${idPrefix}-tz`} value={s.tz} onChange={(e) => s.setTz(e.target.value)}>
          {(allZones.includes(s.tz) ? allZones : [s.tz, ...allZones]).map((z) => (
            <option key={z} value={z}>
              {z}
            </option>
          ))}
        </NativeSelect>
      </div>
      {next.data && next.data.length > 0 && <p className="text-xs text-muted-foreground">{t("Next: {times}", { times: next.data.map((n) => zonedTime(n, s.tz)).join(" · ") })}</p>}
      {next.error && <ErrorText>{errorMessage(next.error, t("Operation failed"))}</ErrorText>}
      <p className="text-xs text-muted-foreground">{note}</p>
    </div>
  );
}
