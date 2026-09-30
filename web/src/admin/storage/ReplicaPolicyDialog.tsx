import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ArrowUpIcon, Loader2Icon, PlusIcon, XIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type BackupSchedule, type ReplicaPolicy, type ReplicaPolicyRequest } from "@/api";
import { queries } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { ErrorText } from "@/components/dialogs";
import { locationLabel } from "@/components/LocationSelect";
import { localZone, zones } from "@/admin/storage/BackupPolicyDialog";
import { DRIVE_ICON } from "@/lib/drives";
import { t } from "@/lib/i18n";
import { useSubmit } from "@/lib/useSubmit";

/** When a target is brought up to date: soon after changes, every few hours, or once a day */
type When = "realtime" | "every" | "daily";

interface Row {
  location: string;
  when: When;
  every: number;
  daily: string;
}

const rowOf = (x: ReplicaPolicy["targets"][number]): Row => ({
  location: x.location_id,
  when: x.mode === "realtime" ? "realtime" : "every" in x.schedule ? "every" : "daily",
  every: "every" in x.schedule ? x.schedule.every : 60,
  daily: "daily" in x.schedule ? x.schedule.daily : "weekly" in x.schedule ? x.schedule.weekly : "03:00",
});

/**
 * A replica policy: the spaces of a location, kept as copies on other locations (its targets, in order of priority),
 * checked by reading them back, and read from when the location fails. Made new (`source` may be given), or changed.
 */
export function ReplicaPolicyDialog({ policy: p, source: preset, onClose, onDone }: { policy?: ReplicaPolicy; source?: string; onClose(): void; onDone(): void }) {
  const locations = useQuery(queries.storageLocations);
  const [name, setName] = useState(p?.name ?? "");
  const [source, setSource] = useState(p?.source_location ?? preset ?? "");
  const [rows, setRows] = useState<Row[]>(p ? p.targets.filter((x) => x.state === "active").map(rowOf) : [{ location: "", when: "realtime", every: 60, daily: "03:00" }]);
  const stale = p?.targets.filter((x) => x.state === "stale") ?? [];
  const [copies, setCopies] = useState(p?.copies ?? 1);
  const [allSpaces, setAllSpaces] = useState(p?.all_spaces ?? true);
  const [spaces, setSpaces] = useState<string[]>(p?.spaces ?? []);
  const [fallback, setFallback] = useState(p?.read_fallback ?? true);
  const [tz, setTz] = useState(p?.targets[0]?.tz ?? localZone());
  const [verifyDays, setVerifyDays] = useState(p?.verify_days ?? 1);
  const [alertHours, setAlertHours] = useState(p?.alert_hours ?? 24);
  const [rate, setRate] = useState(p ? p.rate_limit / 1_000_000 : 0);
  const list = locations.data ?? [];
  const sources = list.filter((l) => l.drive_count > 0 || l.id === source);
  const sourceName = list.find((l) => l.id === source)?.name ?? p?.source_name ?? "";
  const on = useQuery({ ...queries.storageLocationSpaces(source), enabled: !!source });
  const folders = (on.data ?? []).filter((s) => s.mode === "folder");
  const allZones = useMemo(() => zones(), []);
  const defaultName = sourceName ? t("Replicas of {name}", { name: sourceName }) : "";
  const chosen = rows.map((r) => r.location).filter(Boolean);
  const ready = !!source && rows.length > 0 && rows.every((r) => r.location) && new Set(chosen).size === chosen.length && (allSpaces || spaces.length > 0);
  const scheduled = rows.some((r) => r.when !== "realtime");
  const setRow = (i: number, change: Partial<Row>) => setRows((cur) => cur.map((r, j) => (j === i ? { ...r, ...change } : r)));
  const { busy, error, run } = useSubmit(async () => {
    if (!ready) return;
    const settings: ReplicaPolicyRequest = {
      // An old primary being checked stays a target, after the others
      targets: [
        ...rows.map((r) => {
          const schedule: BackupSchedule = r.when === "every" ? { every: r.every } : { daily: r.daily };
          return { location: r.location, mode: r.when === "realtime" ? ("realtime" as const) : ("scheduled" as const), schedule, tz };
        }),
        ...stale.map((x) => ({ location: x.location_id, mode: x.mode, schedule: x.schedule, tz: x.tz })),
      ],
      copies: Math.min(Math.max(1, copies), 16),
      all_spaces: allSpaces,
      spaces,
      read_fallback: fallback,
      verify_days: verifyDays,
      alert_hours: alertHours,
      rate_limit: Math.round(rate * 1_000_000),
    };
    if (p) {
      await api.updateReplicaPolicy(p.id, { ...settings, name: name.trim() || undefined });
      toast.success(t("\"{name}\" was changed", { name: name.trim() || p.name }));
    } else {
      await api.createReplicaPolicy({ ...settings, name: name.trim() || defaultName, source });
      toast.success(t("\"{name}\" was made; the first copies are being made in the background", { name: name.trim() || defaultName }));
    }
    onDone();
    onClose();
  }, t("Couldn't make the change"));
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <form className="grid gap-4" onSubmit={run}>
          <DialogHeader>
            <DialogTitle>{p ? t("Replica settings") : t("New replicas")}</DialogTitle>
            <DialogDescription>
              {t("The files of the spaces of a location are kept on other locations too, checked, and read from there when the location fails. A replica holds the files as they are now, not earlier states: for those, make a backup.")}
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="replica-name">{t("Name")}</Label>
            <Input id="replica-name" value={name} placeholder={defaultName} maxLength={200} onChange={(e) => setName(e.target.value)} />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="replica-source">{t("Replicate")}</Label>
            <NativeSelect id="replica-source" size="lg" value={source} disabled={!!p} onChange={(e) => setSource(e.target.value)}>
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
          <fieldset className="grid gap-2">
            <legend className="mb-1 text-sm font-medium">{t("Keep copies on")}</legend>
            <ol className="grid gap-2">
              {rows.map((r, i) => (
                <li key={i} className="grid gap-2 rounded-md border p-2">
                  <div className="flex items-center gap-2">
                    <span className="w-5 shrink-0 text-center text-xs text-muted-foreground tabular-nums">{i + 1}</span>
                    <NativeSelect aria-label={t("Location {n}", { n: i + 1 })} className="min-w-0 flex-1" value={r.location} onChange={(e) => setRow(i, { location: e.target.value })}>
                      <option value="" disabled>
                        {t("Choose a location")}
                      </option>
                      {list
                        .filter((l) => l.id !== source && !stale.some((x) => x.location_id === l.id))
                        .map((l) => (
                          <option key={l.id} value={l.id} disabled={chosen.includes(l.id) && l.id !== r.location}>
                            {locationLabel(l)}
                          </option>
                        ))}
                    </NativeSelect>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      aria-label={t("Move up")}
                      disabled={i === 0}
                      onClick={() => setRows((cur) => cur.map((x, j) => (j === i - 1 ? cur[i] : j === i ? cur[i - 1] : x)))}
                    >
                      <ArrowUpIcon />
                    </Button>
                    <Button type="button" variant="ghost" size="icon" aria-label={t("Remove")} disabled={rows.length === 1} onClick={() => setRows((cur) => cur.filter((_, j) => j !== i))}>
                      <XIcon />
                    </Button>
                  </div>
                  <div className="flex flex-wrap items-center gap-2 pl-7 text-sm">
                    <NativeSelect aria-label={t("When copies are made")} value={r.when} onChange={(e) => setRow(i, { when: e.target.value as When })}>
                      <option value="realtime">{t("Soon after changes")}</option>
                      <option value="every">{t("Every")}</option>
                      <option value="daily">{t("Every day at")}</option>
                    </NativeSelect>
                    {r.when === "every" && (
                      <NativeSelect aria-label={t("How often")} value={r.every} onChange={(e) => setRow(i, { every: Number(e.target.value) })}>
                        {[15, 30, 60, 120, 240, 360, 720].map((m) => (
                          <option key={m} value={m}>
                            {m < 60 ? t("{n} minutes", { n: m }) : t("{n} hour|{n} hours", { n: m / 60 })}
                          </option>
                        ))}
                      </NativeSelect>
                    )}
                    {r.when === "daily" && <Input type="time" aria-label={t("Time")} className="w-32" value={r.daily} onChange={(e) => setRow(i, { daily: e.target.value })} />}
                  </div>
                </li>
              ))}
            </ol>
            <div>
              <Button type="button" variant="outline" size="sm" onClick={() => setRows((cur) => [...cur, { location: "", when: "realtime", every: 60, daily: "03:00" }])}>
                <PlusIcon /> {t("Add a location")}
              </Button>
            </div>
            {stale.length > 0 && (
              <p className="text-xs text-muted-foreground">
                {t("{names} held the spaces before a promotion: they are checked, and count as a copy again once they are current.", { names: stale.map((x) => x.name).join(", ") })}
              </p>
            )}
            <label className="flex flex-wrap items-center gap-2 text-sm">
              {t("Keep")}
              <Input type="number" min={1} max={16} className="w-16" value={copies} onChange={(e) => setCopies(Number(e.target.value))} />
              {t("copies besides the original, on the first locations of the list that work")}
            </label>
            {copies > rows.length && <p className="text-xs text-amber-700 dark:text-amber-300">{t("There are fewer locations than copies: add locations, or keep fewer copies.")}</p>}
            <p className="text-xs text-muted-foreground">
              {t("Copies are made in the background a little after files change, not at the same moment: a file changed just before a location fails may not be on the others yet.")}
            </p>
            {scheduled && (
              <div className="grid gap-1">
                <Label htmlFor="replica-tz">{t("Time zone")}</Label>
                <NativeSelect id="replica-tz" value={tz} onChange={(e) => setTz(e.target.value)}>
                  {(allZones.includes(tz) ? allZones : [tz, ...allZones]).map((z) => (
                    <option key={z} value={z}>
                      {z}
                    </option>
                  ))}
                </NativeSelect>
              </div>
            )}
          </fieldset>
          <fieldset className="grid gap-2">
            <legend className="mb-1 text-sm font-medium">{t("Spaces")}</legend>
            <label className="flex items-center gap-2 text-sm">
              <input type="radio" name="replica-spaces" checked={allSpaces} onChange={() => setAllSpaces(true)} />
              {t("Every space on the location, also those added later")}
            </label>
            <label className="flex items-center gap-2 text-sm">
              <input type="radio" name="replica-spaces" checked={!allSpaces} onChange={() => setAllSpaces(false)} />
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
            {folders.length > 0 && (
              <p className="text-xs text-muted-foreground">
                {t("Folder spaces are read from their folder: what other programs change there is copied once the check for changes has seen it.")}
              </p>
            )}
          </fieldset>
          <fieldset className="grid gap-2 text-sm">
            <legend className="mb-1 font-medium">{t("Reading and checking")}</legend>
            <label className="flex items-center gap-2">
              <Checkbox checked={fallback} onCheckedChange={(v) => setFallback(v === true)} />
              {t("When the location can't be read, read files from a checked copy")}
            </label>
            <label className="flex flex-wrap items-center gap-2">
              {t("Check the copies every")}
              <Input type="number" min={0} max={365} className="w-16" value={verifyDays} onChange={(e) => setVerifyDays(Number(e.target.value))} />
              {t("days by reading them back (0: only when asked)")}
            </label>
            <label className="flex flex-wrap items-center gap-2">
              {t("Tell administrators when changes have waited to be copied for more than")}
              <Input type="number" min={0} max={8760} className="w-20" value={alertHours} onChange={(e) => setAlertHours(Number(e.target.value))} />
              {t("hours (0: never tell)")}
            </label>
            <p className="text-xs text-muted-foreground">{t("A location that can't be reached, fails or holds damaged copies is told at once, and again when it works.")}</p>
            <label className="flex flex-wrap items-center gap-2">
              {t("Copy at most")}
              <Input type="number" min={0} step={0.5} className="w-20" value={rate} onChange={(e) => setRate(Number(e.target.value))} />
              {t("MB/s (0: no limit)")}
            </label>
          </fieldset>
          <ErrorText>{error}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !ready}>
              {busy && <Loader2Icon className="animate-spin" />}
              {p ? t("Save") : t("Start replicating")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
