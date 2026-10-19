import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type DatabaseBackup } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { ErrorText } from "@/components/dialogs";
import { locationLabel } from "@/components/LocationSelect";
import { SecretInput } from "@/components/SecretField";
import { ScheduleFields, localZone, useSchedule, zonedTime } from "@/admin/storage/ScheduleFields";
import { t, tServer } from "@/lib/i18n";
import { errorMessage, formatBytes, formatDateTime } from "@/lib/utils";
import { useSubmit } from "@/lib/useSubmit";

/** The least a passphrase has (backups/database.rs) */
const MIN_PASSPHRASE = 12;

/** What the database backups did last, for the Backups page's footer */
export function databaseBackupSummary(b: DatabaseBackup | undefined) {
  if (!b) return "";
  if (b.running) return t("Database: a copy is being made");
  if (!b.enabled) return t("Database backups are off");
  if (b.status.error) return t("Database: the last copy failed");
  return b.status.last_success_at ? t("Database: copied {time}", { time: formatDateTime(b.status.last_success_at) }) : t("Database: no copy yet");
}

/**
 * Database backups: a copy of the database (the folder tree, accounts and access, share links, tags, and the rest) on a
 * schedule, encrypted with a passphrase, on a storage location. File backups don't hold these.
 */
export function DatabaseBackupDialog({ onClose }: { onClose(): void }) {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: keys.databaseBackup(),
    queryFn: ({ signal }) => api.databaseBackup(signal),
    // Followed while a copy is being made
    refetchInterval: (query) => (query.state.data?.running ? 2000 : false),
  });
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        {q.data ? (
          <Settings
            current={q.data}
            onClose={onClose}
            onSaved={(b) => {
              qc.setQueryData(keys.databaseBackup(), b);
              void qc.invalidateQueries({ queryKey: keys.databaseCopies() });
            }}
          />
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>{t("Database backups")}</DialogTitle>
              <DialogDescription>{t("Loading…")}</DialogDescription>
            </DialogHeader>
            <ErrorText>{q.error ? errorMessage(q.error, t("Operation failed")) : null}</ErrorText>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

function Settings({ current: b, onClose, onSaved }: { current: DatabaseBackup; onClose(): void; onSaved(b: DatabaseBackup): void }) {
  const locations = useQuery(queries.storageLocations);
  const [enabled, setEnabled] = useState(b.enabled || !b.location);
  const [location, setLocation] = useState(b.location);
  const when = useSchedule(b.schedule, b.tz === "UTC" && !b.location ? localZone() : b.tz);
  const [keepLast, setKeepLast] = useState(b.keep_last);
  const [keepDaily, setKeepDaily] = useState(b.keep_daily);
  const [keepWeekly, setKeepWeekly] = useState(b.keep_weekly);
  const [keepMonthly, setKeepMonthly] = useState(b.keep_monthly);
  const [alertHours, setAlertHours] = useState(b.alert_hours);
  const [passphrase, setPassphrase] = useState("");
  const [again, setAgain] = useState("");
  // Asked for again after each new copy
  const copies = useQuery({
    queryKey: [...keys.databaseCopies(), b.status.last_success_at],
    queryFn: ({ signal }) => api.databaseCopies(signal),
    enabled: !!b.location,
    retry: false,
  });
  // A new passphrase is typed twice: a copy can't be opened without it
  const passphraseProblem =
    passphrase && passphrase.length < MIN_PASSPHRASE
      ? t("The passphrase needs at least {n} characters", { n: MIN_PASSPHRASE })
      : passphrase !== again
        ? t("The two passphrases aren't the same")
        : !passphrase && enabled && !b.has_passphrase
          ? t("Set a passphrase to encrypt the copies with")
          : null;
  const ready = !!location && when.valid && !passphraseProblem && keepLast >= 1;
  const { busy, error, run } = useSubmit(async () => {
    if (!ready) return;
    const saved = await api.saveDatabaseBackup({
      enabled,
      location,
      schedule: when.schedule,
      tz: when.tz,
      keep_last: keepLast,
      keep_daily: keepDaily,
      keep_weekly: keepWeekly,
      keep_monthly: keepMonthly,
      alert_hours: alertHours,
      passphrase: passphrase || undefined,
    });
    onSaved(saved);
    toast.success(enabled ? t("Database backups are on") : t("Database backups are off"));
    onClose();
  }, t("Couldn't make the change"));
  const now = useSubmit(async () => {
    onSaved(await api.runDatabaseBackup());
    toast.success(t("A copy of the database is being made"));
  }, t("Operation failed"));
  const s = b.status;
  const number = (value: number, set: (n: number) => void, max: number, label: string) => (
    <Input type="number" min={0} max={max} aria-label={label} className="w-16" value={value} onChange={(e) => set(Number(e.target.value))} />
  );
  return (
    <form className="grid gap-4" onSubmit={run}>
      <DialogHeader>
        <DialogTitle>{t("Database backups")}</DialogTitle>
        <DialogDescription>
          {t(
            "The database holds the folder tree, accounts and groups, access, share links, tags, S3 buckets, FTP receive folders, versions and the activity log. File backups don't: restoring a server needs a copy of the database too.",
          )}
        </DialogDescription>
      </DialogHeader>
      <div className="grid gap-1 rounded-md border p-3 text-sm" role="status">
        {b.running ? (
          <span className="flex items-center gap-2">
            <Loader2Icon className="size-4 animate-spin" /> {t("A copy of the database is being made")}
          </span>
        ) : s.last_success_at ? (
          <span>
            {t("Last copy: {time}, {size}, made in {seconds} s", {
              time: formatDateTime(s.last_success_at),
              size: formatBytes(s.last_size ?? 0),
              seconds: Math.max(1, Math.round((s.last_duration_ms ?? 0) / 1000)),
            })}
          </span>
        ) : (
          <span className="text-muted-foreground">{t("No copy yet")}</span>
        )}
        {s.error && !b.running && <span className="text-destructive">{t("The last copy failed: {error}", { error: tServer(s.error) })}</span>}
        {b.enabled && s.next_run_at && <span className="text-muted-foreground">{t("Next: {times}", { times: zonedTime(s.next_run_at, b.tz) })}</span>}
      </div>
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" className="size-4 accent-brand" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
        {t("Make copies of the database on a schedule")}
      </label>
      <div className="grid gap-2">
        <Label htmlFor="dbb-location">{t("Keep the copies on")}</Label>
        <NativeSelect id="dbb-location" value={location} onChange={(e) => setLocation(e.target.value)}>
          <option value="">{t("Choose a storage location")}</option>
          {(locations.data ?? []).map((l) => (
            <option key={l.id} value={l.id}>
              {locationLabel(l)}
            </option>
          ))}
        </NativeSelect>
        <p className="text-xs text-muted-foreground">{t("Best on another disk or server than the data folder. They are kept in its folder .thirtyfile-backups/database.")}</p>
      </div>
      <fieldset className="grid gap-2">
        <legend className="mb-1 text-sm font-medium">{t("Passphrase")}</legend>
        <SecretInput
          id="dbb-passphrase"
          aria-label={b.has_passphrase ? t("New passphrase (leave blank to keep the current one)") : t("Passphrase")}
          placeholder={b.has_passphrase ? t("New passphrase (leave blank to keep the current one)") : undefined}
          revealLabel={t("Show passphrase")}
          autoComplete="new-password"
          value={passphrase}
          onChange={(e) => setPassphrase(e.target.value)}
        />
        {passphrase && (
          <SecretInput
            id="dbb-passphrase-again"
            aria-label={t("Passphrase again")}
            placeholder={t("Passphrase again")}
            revealLabel={t("Show passphrase")}
            autoComplete="new-password"
            value={again}
            onChange={(e) => setAgain(e.target.value)}
          />
        )}
        <p className="text-xs text-muted-foreground">
          {t(
            "The copies are encrypted with it, not with the server's secret.key. Keep it somewhere safe, apart from the server: without it a copy can't be restored, and nobody can recover it. A new passphrase applies to the next copies.",
          )}
        </p>
      </fieldset>
      <fieldset className="grid gap-2">
        <legend className="mb-1 text-sm font-medium">{t("When")}</legend>
        <ScheduleFields
          s={when}
          idPrefix="dbb"
          every={[60, 120, 240, 360, 720]}
          note={t("A time clocks skip when summer time starts isn't run that day; one that happens twice when it ends is run once. After the server was off, one copy catches up.")}
        />
      </fieldset>
      <fieldset className="grid gap-2 text-sm">
        <legend className="mb-1 font-medium">{t("Keeping")}</legend>
        <p className="text-xs text-muted-foreground">{t("The newest copies are kept, and the newest copy of each of the last days, weeks and months:")}</p>
        <span className="grid grid-cols-2 gap-2 sm:grid-cols-4">
          {(
            [
              [t("Newest copies"), keepLast, setKeepLast],
              [t("Days"), keepDaily, setKeepDaily],
              [t("Weeks"), keepWeekly, setKeepWeekly],
              [t("Months"), keepMonthly, setKeepMonthly],
            ] as const
          ).map(([label, value, set]) => (
            <label key={label} className="grid gap-1">
              {label}
              {number(value, set, 1000, label)}
            </label>
          ))}
        </span>
        <span className="flex flex-wrap items-center gap-2">
          {t("Tell administrators when there is no new copy for")}
          <Input type="number" min={0} max={8760} className="w-20" aria-label={t("Hours without a copy")} value={alertHours} onChange={(e) => setAlertHours(Number(e.target.value))} />
          {t("hours (0: never)")}
        </span>
        <p className="text-xs text-muted-foreground">{t("Older copies are deleted only after a new one is stored.")}</p>
      </fieldset>
      {copies.data && copies.data.length > 0 && (
        <details className="text-sm">
          <summary className="cursor-default">{t("{n} copy on the location|{n} copies on the location", { n: copies.data.length })}</summary>
          <ul className="mt-2 grid gap-1 text-xs text-muted-foreground">
            {copies.data.map((c) => (
              <li key={c.name} className="flex justify-between gap-2">
                <span className="truncate font-mono">{c.name}</span>
                <span className="shrink-0 tabular-nums">
                  {formatDateTime(c.at)} · {formatBytes(c.size)}
                </span>
              </li>
            ))}
          </ul>
          <p className="mt-2 text-xs text-muted-foreground">{t("To restore one, copy it to the new server and run thirtyfile restore-database: see the backup guide.")}</p>
        </details>
      )}
      <ErrorText>{passphraseProblem && (passphrase || again) ? passphraseProblem : (error ?? now.error)}</ErrorText>
      <DialogFooter>
        <Button type="button" variant="outline" disabled={now.busy || b.running || !b.location || !b.has_passphrase} onClick={() => void now.run()}>
          {t("Back up now")}
        </Button>
        <Button type="button" variant="outline" onClick={onClose}>
          {t("Cancel")}
        </Button>
        <Button type="submit" disabled={busy || !ready}>
          {busy && <Loader2Icon className="animate-spin" />}
          {t("Save")}
        </Button>
      </DialogFooter>
    </form>
  );
}
