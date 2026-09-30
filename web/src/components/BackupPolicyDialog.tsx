import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangleIcon, Loader2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type BackupPolicySettings, type BackupSchedule, type BackupSet } from "@/api";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { ErrorText } from "@/components/dialogs";
import { locationLabel } from "@/components/LocationSelect";
import { DRIVE_ICON } from "@/lib/drives";
import { t, tServer } from "@/lib/i18n";
import { cn } from "@/lib/utils";

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

/** A time in a time zone, as the settings show the next snapshots */
export function zonedTime(t: number, tz: string) {
  try {
    return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short", timeZone: tz }).format(new Date(t * 1000));
  } catch {
    return new Date(t * 1000).toLocaleString();
  }
}

const DAYS = [1, 2, 3, 4, 5, 6, 7];
/** Monday is 1: named in the browser's language (1 January 2024 was a Monday) */
const dayName = (d: number) => new Intl.DateTimeFormat(undefined, { weekday: "short" }).format(new Date(2024, 0, d));

type Kind = "every" | "daily" | "weekly";

/**
 * A backup policy: the spaces of a location, backed up to another location soon after changes, on a schedule, or
 * both, and kept for a number of days. Made new (`source` may be given), or changed (`set`).
 */
export function BackupPolicyDialog({ set, source: preset, onClose, onDone }: { set?: BackupSet; source?: string; onClose(): void; onDone(): void }) {
  const p = set?.policy ?? null;
  const locations = useQuery({ queryKey: ["storage-locations"], queryFn: api.storageLocations });
  const [name, setName] = useState(set?.name ?? "");
  const [source, setSource] = useState(set?.source_location ?? preset ?? "");
  const [dest, setDest] = useState(set?.dest_location ?? "");
  const [allSpaces, setAllSpaces] = useState(p?.all_spaces ?? true);
  const [spaces, setSpaces] = useState<string[]>(p?.spaces ?? []);
  const [mode, setMode] = useState<BackupPolicySettings["mode"]>(p?.mode ?? "both");
  const initial = p?.schedule ?? { daily: "03:00" };
  const [kind, setKind] = useState<Kind>("every" in initial ? "every" : "weekly" in initial ? "weekly" : "daily");
  const [every, setEvery] = useState("every" in initial ? initial.every : 60);
  const [time, setTime] = useState("daily" in initial ? initial.daily : "weekly" in initial ? initial.weekly : "03:00");
  const [days, setDays] = useState<number[]>("weekly" in initial ? initial.days : [1, 2, 3, 4, 5]);
  const [tz, setTz] = useState(p?.tz ?? localZone());
  const [versions, setVersions] = useState(p?.versions ?? true);
  const [trash, setTrash] = useState(p?.trash ?? true);
  const [keepDays, setKeepDays] = useState(p?.keep_days ?? 30);
  const [keepMin, setKeepMin] = useState(p?.keep_min ?? 1);
  const [rate, setRate] = useState(p ? p.rate_limit / 1_000_000 : 0);
  const [alertHours, setAlertHours] = useState(p?.alert_hours ?? 48);
  const [verifyDays, setVerifyDays] = useState(p?.verify_days ?? 7);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const list = locations.data ?? [];
  const sources = list.filter((l) => l.drive_count > 0 || l.id === source);
  const dests = list.filter((l) => l.id !== source);
  const sourceName = list.find((l) => l.id === source)?.name ?? set?.source_name ?? "";
  const on = useQuery({ queryKey: ["storage-location-spaces", source], queryFn: () => api.storageLocationSpaces(source), enabled: !!source });
  const preview = useQuery({ queryKey: ["copy-preview", source, dest], queryFn: () => api.copyPreview(source, dest), enabled: !set && !!source && !!dest, retry: false });
  const schedule: BackupSchedule = kind === "every" ? { every } : kind === "daily" ? { daily: time } : { weekly: time, days };
  const next = useQuery({
    queryKey: ["backup-next-runs", JSON.stringify(schedule), tz],
    queryFn: () => api.backupNextRuns(schedule, tz),
    enabled: mode !== "realtime" && (kind !== "weekly" || days.length > 0),
    retry: false,
  });
  const allZones = useMemo(() => zones(), []);
  const defaultName = sourceName ? t("Backup of {name}", { name: sourceName }) : "";
  const pv = preview.data;
  const ready = !!source && !!dest && (allSpaces || spaces.length > 0) && (mode === "realtime" || kind !== "weekly" || days.length > 0);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <form
          className="grid gap-4"
          onSubmit={async (e) => {
            e.preventDefault();
            if (!ready) return;
            setBusy(true);
            setError(null);
            const settings: Partial<BackupPolicySettings> = {
              mode,
              schedule,
              tz,
              all_spaces: allSpaces,
              spaces,
              versions,
              trash,
              keep_days: keepDays,
              keep_min: keepMin,
              rate_limit: Math.round(rate * 1_000_000),
              alert_hours: alertHours,
              verify_days: verifyDays,
            };
            try {
              if (set) {
                await api.updateBackupPolicy(set.id, { ...settings, name: name.trim() || undefined });
                toast.success(t("\"{name}\" was changed", { name: name.trim() || set.name }));
              } else {
                await api.createBackupPolicy({ ...settings, name: name.trim() || defaultName, source, dest });
                toast.success(t("\"{name}\" was made; its first snapshot is being made in the background", { name: name.trim() || defaultName }));
              }
              onDone();
              onClose();
            } catch (err) {
              setError(err instanceof Error ? err.message : t("Couldn't make the change"));
            } finally {
              setBusy(false);
            }
          }}
        >
          <DialogHeader>
            <DialogTitle>{set ? t("Backup settings") : t("New backup")}</DialogTitle>
            <DialogDescription>
              {t("Snapshots of the spaces of a location are kept on another location, and older ones for as long as you choose. The spaces stay where they are and keep working meanwhile.")}
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="backup-name">{t("Name")}</Label>
            <Input id="backup-name" value={name} placeholder={defaultName} maxLength={200} onChange={(e) => setName(e.target.value)} />
          </div>
          <div className="grid gap-2 sm:grid-cols-2">
            <div className="grid gap-2">
              <Label htmlFor="backup-source">{t("Back up")}</Label>
              <NativeSelect id="backup-source" size="lg" value={source} disabled={!!set} onChange={(e) => setSource(e.target.value)}>
                <option value="" disabled>
                  {t("Choose a location")}
                </option>
                {sources.map((l) => (
                  <option key={l.id} value={l.id}>
                    {locationLabel(l)}
                  </option>
                ))}
              </NativeSelect>
            </div>
            <div className="grid gap-2">
              <Label htmlFor="backup-dest">{t("To")}</Label>
              <NativeSelect id="backup-dest" size="lg" value={dest} disabled={!!set} onChange={(e) => setDest(e.target.value)}>
                <option value="" disabled>
                  {t("Choose a location")}
                </option>
                {dests.map((l) => (
                  <option key={l.id} value={l.id} disabled={!l.connected}>
                    {locationLabel(l)}
                  </option>
                ))}
              </NativeSelect>
            </div>
          </div>
          {preview.error && <ErrorText>{preview.error instanceof Error ? preview.error.message : t("Operation failed")}</ErrorText>}
          {pv && (pv.shared || pv.unencrypted || pv.problem) && (
            <div className="grid gap-1 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-900 dark:text-amber-100" role="note">
              {pv.problem && (
                <p className="flex gap-1.5">
                  <AlertTriangleIcon className="mt-px size-3.5 shrink-0" /> {t("{name} can't be reached now: {error}", { name: pv.dest_name, error: tServer(pv.problem) })}
                </p>
              )}
              {pv.shared && <p>{t("{name} is on the same disk or storage service as {source}: the copy doesn't survive a failure of it.", { name: pv.dest_name, source: pv.source_name })}</p>}
              {pv.unencrypted && <p>{t("{name} is FTP without encryption: the files travel unencrypted.", { name: pv.dest_name })}</p>}
            </div>
          )}
          <fieldset className="grid gap-2">
            <legend className="mb-1 text-sm font-medium">{t("Spaces")}</legend>
            <label className="flex items-center gap-2 text-sm">
              <input type="radio" name="backup-spaces" checked={allSpaces} onChange={() => setAllSpaces(true)} />
              {t("Every space on the location, also those added later")}
            </label>
            <label className="flex items-center gap-2 text-sm">
              <input type="radio" name="backup-spaces" checked={!allSpaces} onChange={() => setAllSpaces(false)} />
              {t("These spaces:")}
            </label>
            {!allSpaces && (
              <ul className="ml-6 max-h-40 divide-y overflow-y-auto rounded-md border text-xs">
                {(on.data ?? []).map((s) => {
                  const Icon = DRIVE_ICON[s.kind];
                  return (
                    <li key={s.id}>
                      <label className="flex items-center gap-2 px-2.5 py-1.5">
                        <Checkbox
                          checked={spaces.includes(s.id)}
                          onCheckedChange={(v) => setSpaces((cur) => (v === true ? [...cur, s.id] : cur.filter((x) => x !== s.id)))}
                        />
                        <Icon className="size-3.5 shrink-0 text-muted-foreground" />
                        <span className="truncate">{s.kind === "personal" && s.owner_name ? `${s.name} · ${s.owner_name}` : s.name}</span>
                      </label>
                    </li>
                  );
                })}
              </ul>
            )}
            <div className="flex flex-wrap gap-x-4 gap-y-1 text-sm">
              <label className="flex items-center gap-2">
                <Checkbox checked={versions} onCheckedChange={(v) => setVersions(v === true)} /> {t("Earlier versions")}
              </label>
              <label className="flex items-center gap-2">
                <Checkbox checked={trash} onCheckedChange={(v) => setTrash(v === true)} /> {t("What is in the trash")}
              </label>
            </div>
          </fieldset>
          <fieldset className="grid gap-2">
            <legend className="mb-1 text-sm font-medium">{t("When")}</legend>
            <NativeSelect size="lg" aria-label={t("When")} value={mode} onChange={(e) => setMode(e.target.value as BackupPolicySettings["mode"])}>
              <option value="both">{t("Soon after changes, and on a schedule")}</option>
              <option value="realtime">{t("Soon after changes")}</option>
              <option value="scheduled">{t("On a schedule")}</option>
            </NativeSelect>
            {mode !== "scheduled" && (
              <p className="text-xs text-muted-foreground">
                {t("Changes are gathered for a few seconds, then copied in the background: files are protected a little after they change, not at the same moment. Changes other programs make in folder spaces are seen at the next check for changes.")}
              </p>
            )}
            {mode !== "realtime" && (
              <div className="grid gap-2">
                <div className="flex flex-wrap items-center gap-2 text-sm">
                  <NativeSelect aria-label={t("Schedule")} value={kind} onChange={(e) => setKind(e.target.value as Kind)}>
                    <option value="every">{t("Every")}</option>
                    <option value="daily">{t("Every day at")}</option>
                    <option value="weekly">{t("On these days at")}</option>
                  </NativeSelect>
                  {kind === "every" ? (
                    <NativeSelect aria-label={t("How often")} value={every} onChange={(e) => setEvery(Number(e.target.value))}>
                      {[15, 30, 60, 120, 240, 360, 720].map((m) => (
                        <option key={m} value={m}>
                          {m < 60 ? t("{n} minutes", { n: m }) : t("{n} hour|{n} hours", { n: m / 60 })}
                        </option>
                      ))}
                    </NativeSelect>
                  ) : (
                    <Input type="time" aria-label={t("Time")} className="w-32" value={time} onChange={(e) => setTime(e.target.value)} />
                  )}
                </div>
                {kind === "weekly" && (
                  <div className="flex flex-wrap gap-1">
                    {DAYS.map((d) => (
                      <button
                        key={d}
                        type="button"
                        aria-pressed={days.includes(d)}
                        className={cn("rounded-md border px-2 py-1 text-xs", days.includes(d) ? "border-brand bg-brand/10 text-brand" : "text-muted-foreground")}
                        onClick={() => setDays((cur) => (cur.includes(d) ? cur.filter((x) => x !== d) : [...cur, d].sort()))}
                      >
                        {dayName(d)}
                      </button>
                    ))}
                  </div>
                )}
                <div className="grid gap-1">
                  <Label htmlFor="backup-tz">{t("Time zone")}</Label>
                  <NativeSelect id="backup-tz" value={tz} onChange={(e) => setTz(e.target.value)}>
                    {(allZones.includes(tz) ? allZones : [tz, ...allZones]).map((z) => (
                      <option key={z} value={z}>
                        {z}
                      </option>
                    ))}
                  </NativeSelect>
                </div>
                {next.data && next.data.length > 0 && (
                  <p className="text-xs text-muted-foreground">{t("Next: {times}", { times: next.data.map((n) => zonedTime(n, tz)).join(" · ") })}</p>
                )}
                {next.error && <ErrorText>{next.error instanceof Error ? next.error.message : t("Operation failed")}</ErrorText>}
                <p className="text-xs text-muted-foreground">
                  {t("A time clocks skip when summer time starts isn't run that day; one that happens twice when it ends is run once. After the server was off, one snapshot catches up.")}
                </p>
              </div>
            )}
          </fieldset>
          <fieldset className="grid gap-2 text-sm">
            <legend className="mb-1 font-medium">{t("Keeping and checking")}</legend>
            <label className="flex flex-wrap items-center gap-2">
              {t("Keep snapshots for")}
              <Input type="number" min={1} max={3650} className="w-20" value={keepDays} onChange={(e) => setKeepDays(Number(e.target.value))} />
              {t("days, and always the newest")}
              <Input type="number" min={1} max={1000} className="w-16" value={keepMin} onChange={(e) => setKeepMin(Number(e.target.value))} />
            </label>
            <label className="flex flex-wrap items-center gap-2">
              {t("Tell administrators when there is no new snapshot for")}
              <Input type="number" min={0} max={8760} className="w-20" value={alertHours} onChange={(e) => setAlertHours(Number(e.target.value))} />
              {t("hours (0: never)")}
            </label>
            <label className="flex flex-wrap items-center gap-2">
              {t("Read the backup back and check it every")}
              <Input type="number" min={0} max={365} className="w-16" value={verifyDays} onChange={(e) => setVerifyDays(Number(e.target.value))} />
              {t("days (0: only when asked)")}
            </label>
            <label className="flex flex-wrap items-center gap-2">
              {t("Copy at most")}
              <Input type="number" min={0} step={0.5} className="w-20" value={rate} onChange={(e) => setRate(Number(e.target.value))} />
              {t("MB/s (0: no limit)")}
            </label>
            <p className="text-xs text-muted-foreground">
              {t("The newest complete snapshot is never deleted, and a snapshot that fails deletes nothing. Content several snapshots hold is stored once.")}
            </p>
          </fieldset>
          <ErrorText>{error}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !ready || (!set && !!pv?.problem)}>
              {busy && <Loader2Icon className="animate-spin" />}
              {set ? t("Save") : t("Start backing up")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
