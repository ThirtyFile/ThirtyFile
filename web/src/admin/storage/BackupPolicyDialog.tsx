import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangleIcon, Loader2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type BackupPolicySettings, type BackupSet } from "@/api";
import { ScheduleFields, localZone, useSchedule } from "@/admin/storage/ScheduleFields";
import { keys, queries } from "@/api/queryKeys";
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
import { errorMessage } from "@/lib/utils";
import { useSubmit } from "@/lib/useSubmit";

export { localZone, zonedTime, zones } from "@/admin/storage/ScheduleFields";

/**
 * A backup policy: the spaces of a location, backed up to another location soon after changes, on a schedule, or
 * both, and kept for a number of days. Made new (`source` may be given), or changed (`set`).
 */
export function BackupPolicyDialog({ set, source: preset, onClose, onDone }: { set?: BackupSet; source?: string; onClose(): void; onDone(): void }) {
  const p = set?.policy ?? null;
  const locations = useQuery(queries.storageLocations);
  const [name, setName] = useState(set?.name ?? "");
  const [source, setSource] = useState(set?.source_location ?? preset ?? "");
  const [dest, setDest] = useState(set?.dest_location ?? "");
  const [allSpaces, setAllSpaces] = useState(p?.all_spaces ?? true);
  const [spaces, setSpaces] = useState<string[]>(p?.spaces ?? []);
  const [mode, setMode] = useState<BackupPolicySettings["mode"]>(p?.mode ?? "both");
  const when = useSchedule(p?.schedule ?? { daily: "03:00" }, p?.tz ?? localZone());
  const [versions, setVersions] = useState(p?.versions ?? true);
  const [trash, setTrash] = useState(p?.trash ?? true);
  const [keepDays, setKeepDays] = useState(p?.keep_days ?? 30);
  const [keepMin, setKeepMin] = useState(p?.keep_min ?? 1);
  const [rate, setRate] = useState(p ? p.rate_limit / 1_000_000 : 0);
  const [alertHours, setAlertHours] = useState(p?.alert_hours ?? 48);
  const [verifyDays, setVerifyDays] = useState(p?.verify_days ?? 7);
  const list = locations.data ?? [];
  const sources = list.filter((l) => l.drive_count > 0 || l.id === source);
  const dests = list.filter((l) => l.id !== source);
  const sourceName = list.find((l) => l.id === source)?.name ?? set?.source_name ?? "";
  const on = useQuery({ ...queries.storageLocationSpaces(source), enabled: !!source });
  const preview = useQuery({ queryKey: keys.copyPreview(source, dest), queryFn: () => api.copyPreview(source, dest), enabled: !set && !!source && !!dest, retry: false });
  const defaultName = sourceName ? t("Backup of {name}", { name: sourceName }) : "";
  const pv = preview.data;
  const ready = !!source && !!dest && (allSpaces || spaces.length > 0) && (mode === "realtime" || when.valid);
  const { busy, error, run } = useSubmit(async () => {
    if (!ready) return;
    const settings: Partial<BackupPolicySettings> = {
      mode,
      schedule: when.schedule,
      tz: when.tz,
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
    if (set) {
      // The name shown may be a translation of the one ThirtyFile gave it: it is sent only when changed
      await api.updateBackupPolicy(set.id, { ...settings, name: name.trim() && name.trim() !== set.name ? name.trim() : undefined });
      toast.success(t('"{name}" was changed', { name: name.trim() || set.name }));
    } else {
      // Left empty, the server names it "Backup of <location>", shown in each person's language
      await api.createBackupPolicy({ ...settings, name: name.trim() || undefined, source, dest });
      toast.success(t('"{name}" was made; its first snapshot is being made in the background', { name: name.trim() || defaultName }));
    }
    onDone();
    onClose();
  }, t("Couldn't make the change"));
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <form className="grid gap-4" onSubmit={run}>
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
          {preview.error && <ErrorText>{errorMessage(preview.error, t("Operation failed"))}</ErrorText>}
          {pv && (pv.shared || pv.unencrypted || pv.problem) && (
            <div className="grid gap-1 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-900 dark:text-amber-100" role="note">
              {pv.problem && (
                <p className="flex gap-1.5">
                  <AlertTriangleIcon className="mt-px size-3.5 shrink-0" /> {t("{name} can't be reached now: {error}", { name: pv.dest_name, error: tServer(pv.problem) })}
                </p>
              )}
              {pv.shared && (
                <p>{t("{name} is on the same disk or storage service as {source}: the copy doesn't survive a failure of it.", { name: pv.dest_name, source: pv.source_name })}</p>
              )}
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
                        <Checkbox checked={spaces.includes(s.id)} onCheckedChange={(v) => setSpaces((cur) => (v === true ? [...cur, s.id] : cur.filter((x) => x !== s.id)))} />
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
                {t(
                  "Changes are gathered for a few seconds, then copied in the background: files are protected a little after they change, not at the same moment. Changes other programs make in folder spaces are seen at the next check for changes.",
                )}
              </p>
            )}
            {mode !== "realtime" && (
              <ScheduleFields
                s={when}
                idPrefix="backup"
                note={t("A time clocks skip when summer time starts isn't run that day; one that happens twice when it ends is run once. After the server was off, one snapshot catches up.")}
              />
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
