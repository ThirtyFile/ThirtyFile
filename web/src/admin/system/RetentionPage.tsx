import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { DownloadIcon, Loader2Icon, LockIcon, PlusIcon } from "lucide-react";
import { toast } from "sonner";
import { api, driveName, type Drive, type HeldItem, type Hold } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { Section, SettingsFrame } from "@/admin/SettingsFrame";
import { ConfirmDialog } from "@/components/dialogs";
import { ErrorState } from "@/components/ErrorState";
import { HoldReasonField, scopeLabel } from "@/components/RetentionSection";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { t } from "@/lib/i18n";
import { formatBytes, formatDate, formatDateTime } from "@/lib/utils";

/** The longest period the server accepts: a hundred years */
const MAX_DAYS = 36_500;

type State = "active" | "released" | "all";

/** Control panel › Retention and holds: legal holds, and how long each space keeps its files */
export function RetentionPage() {
  const [state, setState] = useState<State>("active");
  const [search, setSearch] = useState("");
  const q = useQuery({ queryKey: keys.holds(state, search), queryFn: ({ signal }) => api.holds(state, search, signal) });
  const drives = useQuery(queries.adminDrives);
  const [placing, setPlacing] = useState(false);
  return (
    <SettingsFrame item="retention" onRefresh={() => void Promise.all([q.refetch(), drives.refetch()])} searchPlaceholder={t("Search holds")} onSearch={setSearch}>
      <div className="grid gap-6">
        <Section title={t("Legal holds")}>
          <div className="grid gap-3 p-4">
            <p className="text-sm text-muted-foreground">
              {t(
                "A hold keeps everything it covers from being deleted for good, whatever people do: earlier versions are kept, and a space or an account that holds something held can't be deleted. Deleting still moves items to the trash. Place a hold on a folder or file from its details pane.",
              )}
            </p>
            <div className="flex flex-wrap items-center gap-2">
              <Button type="button" size="sm" onClick={() => setPlacing(true)}>
                <PlusIcon /> {t("Place a hold")}
              </Button>
              <NativeSelect aria-label={t("Show")} size="sm" value={state} onChange={(e) => setState(e.target.value as State)}>
                <option value="active">{t("Holds in force")}</option>
                <option value="released">{t("Released holds")}</option>
                <option value="all">{t("All holds")}</option>
              </NativeSelect>
            </div>
          </div>
          {q.error ? (
            <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
          ) : !q.data ? (
            <p role="status" className="border-t p-4 text-sm text-muted-foreground">
              {t("Loading…")}
            </p>
          ) : !q.data.holds.length ? (
            <p className="border-t p-4 text-sm text-muted-foreground">{search ? t("No hold matches.") : state === "released" ? t("No hold was released.") : t("No holds.")}</p>
          ) : (
            <ul className="divide-y border-t" aria-label={t("Legal holds")}>
              {q.data.holds.map((h) => (
                <HoldRow key={h.id} hold={h} />
              ))}
            </ul>
          )}
        </Section>

        <Section title={t("Retention periods")}>
          <p className="p-4 pb-2 text-sm text-muted-foreground">
            {t(
              "In a space with a period, files, and their earlier versions, can't be deleted for good until that many days after their last change. Deleting still moves them to the trash, where they wait, marked, until the period has passed.",
            )}
          </p>
          {drives.error ? (
            <ErrorState message={drives.error.message} onRetry={() => drives.refetch()} />
          ) : !drives.data ? (
            <p role="status" className="p-4 text-sm text-muted-foreground">
              {t("Loading…")}
            </p>
          ) : (
            <ul className="divide-y" aria-label={t("Retention periods")}>
              {drives.data.map((d) => (
                <PeriodRow key={d.id} drive={d} />
              ))}
            </ul>
          )}
        </Section>
      </div>
      {placing && <PlaceDialog onClose={() => setPlacing(false)} />}
    </SettingsFrame>
  );
}

function HoldRow({ hold: h }: { hold: Hold }) {
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const [releasing, setReleasing] = useState(false);
  return (
    <li className="grid gap-2 p-4">
      <div className="flex flex-wrap items-start gap-3">
        <LockIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-1 text-sm">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium [overflow-wrap:anywhere]">{h.label}</span>
            <Badge variant="outline" className="h-4 px-1.5 text-[10px]">
              {scopeLabel(h.scope)}
            </Badge>
            {h.released_at && (
              <Badge variant="secondary" className="h-4 px-1.5 text-[10px]">
                {t("Released")}
              </Badge>
            )}
            {!h.present && (
              <Badge variant="outline" className="h-4 px-1.5 text-[10px]" title={t("What it was placed on is gone")}>
                {t("Gone")}
              </Badge>
            )}
          </div>
          <p className="[overflow-wrap:anywhere]">{h.reason}</p>
          <p className="text-xs text-muted-foreground">
            {t("Placed by {name} on {date}", { name: h.created_by_name, date: formatDateTime(h.created_at) })}
            {h.released_at && ` · ${t("Released by {name} on {date}", { name: h.released_by_name ?? "—", date: formatDateTime(h.released_at) })}`}
          </p>
        </div>
        <div className="flex gap-2">
          <Button type="button" variant="outline" size="sm" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
            {open ? t("Hide files") : t("Files")}
          </Button>
          {!h.released_at && (
            <Button type="button" variant="outline" size="sm" onClick={() => setReleasing(true)}>
              {t("Release")}
            </Button>
          )}
        </div>
      </div>
      {open && <HeldFiles hold={h} />}
      {releasing && (
        <ConfirmDialog
          title={t('Release the hold on "{label}"?', { label: h.label })}
          description={t("What it covered can be deleted for good again, unless something else keeps it.")}
          confirmText={t("Release")}
          onClose={() => setReleasing(false)}
          onConfirm={async () => {
            await api.releaseHold(h.id);
            setReleasing(false);
            toast.success(t("Hold released"));
            await qc.invalidateQueries({ queryKey: ["holds"] });
          }}
        />
      )}
    </li>
  );
}

/** The files a hold covers: the first of them, and an export of the list */
function HeldFiles({ hold }: { hold: Hold }) {
  const q = useQuery({ queryKey: keys.holdItems(hold.id), queryFn: ({ signal }) => api.holdItems(hold.id, signal) });
  if (q.error) return <ErrorState compact message={q.error.message} onRetry={() => q.refetch()} />;
  if (!q.data)
    return (
      <p role="status" className="text-xs text-muted-foreground">
        {t("Loading…")}
      </p>
    );
  const { items, more } = q.data;
  return (
    <div className="grid gap-2 rounded-md border bg-muted/30 p-3 text-xs">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span>{items.length ? t("{n} file shown|{n} files shown", { n: items.length }) : t("No files are covered now.")}</span>
        <a href={api.holdExportUrl(hold.id)} download className="inline-flex items-center gap-1 text-brand underline-offset-4 hover:underline">
          <DownloadIcon className="size-3.5" /> {t("Export as CSV")}
        </a>
      </div>
      {!!items.length && (
        <ul className="max-h-60 divide-y overflow-y-auto rounded border bg-background" aria-label={t("Files covered")}>
          {items.map((i: HeldItem) => (
            <li key={i.id} className="flex flex-wrap justify-between gap-x-3 px-2 py-1">
              <span className="[overflow-wrap:anywhere]">{[i.space, i.path].filter(Boolean).join("/")}</span>
              <span className="text-muted-foreground">
                {formatBytes(i.size)} · {formatDate(i.updated_at)}
              </span>
            </li>
          ))}
        </ul>
      )}
      {more && <p className="text-muted-foreground">{t("More files are covered than are shown. The export lists them.")}</p>}
    </div>
  );
}

/** A space's retention period, in days */
function PeriodRow({ drive: d }: { drive: Drive }) {
  const qc = useQueryClient();
  const [days, setDays] = useState(String(d.retention_days));
  const n = Number(days);
  const valid = days.trim() !== "" && Number.isInteger(n) && n >= 0 && n <= MAX_DAYS;
  const save = useMutation({
    mutationFn: () => api.updateDrive(d.id, { retention_days: n }),
    onSuccess: () => {
      toast.success(n ? t("Retention period saved") : t("Retention period removed"));
      void qc.invalidateQueries({ queryKey: keys.adminDrives() });
      void qc.invalidateQueries({ queryKey: ["holds"] });
    },
    onError: (e) => toast.error(e.message),
  });
  const id = `retention-${d.id}`;
  return (
    <li className="flex flex-wrap items-center gap-3 px-4 py-2.5">
      <Label htmlFor={id} className="min-w-0 flex-1 font-normal [overflow-wrap:anywhere]">
        {driveName(d)}
      </Label>
      <Input
        id={id}
        className="w-28"
        type="number"
        inputMode="numeric"
        min={0}
        max={MAX_DAYS}
        value={days}
        onChange={(e) => setDays(e.target.value)}
        aria-label={t("Days kept after the last change")}
      />
      <span className="text-xs text-muted-foreground">{t("days (0 = no period)")}</span>
      <Button type="button" size="sm" disabled={!valid || n === d.retention_days || save.isPending} onClick={() => save.mutate()}>
        {save.isPending && <Loader2Icon className="animate-spin" />}
        {t("Save")}
      </Button>
    </li>
  );
}

/** Placing a hold on a whole space, or on the files of one person (a folder or file is held from its details pane) */
function PlaceDialog({ onClose }: { onClose(): void }) {
  const qc = useQueryClient();
  const drives = useQuery(queries.adminDrives);
  const users = useQuery(queries.adminUsers);
  const [scope, setScope] = useState<"space" | "user">("space");
  const [target, setTarget] = useState("");
  const [reason, setReason] = useState("");
  return (
    <ConfirmDialog
      title={t("Place a legal hold")}
      description={t("Nothing it covers can be deleted for good, and its earlier versions are kept, until you release the hold.")}
      confirmText={t("Place hold")}
      ready={!!target && !!reason.trim()}
      onClose={onClose}
      onConfirm={async () => {
        await api.placeHold(scope === "space" ? { scope, drive_id: target, reason: reason.trim() } : { scope, user_id: Number(target), reason: reason.trim() });
        toast.success(t("Hold placed"));
        await qc.invalidateQueries({ queryKey: ["holds"] });
        onClose();
      }}
    >
      <div className="grid gap-1.5">
        <Label htmlFor="hold-scope">{t("What to hold")}</Label>
        <NativeSelect
          id="hold-scope"
          size="lg"
          value={scope}
          onChange={(e) => {
            setScope(e.target.value as "space" | "user");
            setTarget("");
          }}
        >
          <option value="space">{t("Everything in a space")}</option>
          <option value="user">{t("The files a person owns")}</option>
        </NativeSelect>
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor="hold-target">{scope === "space" ? t("Space") : t("Person")}</Label>
        <NativeSelect id="hold-target" size="lg" value={target} onChange={(e) => setTarget(e.target.value)}>
          <option value="">{t("Choose…")}</option>
          {scope === "space"
            ? (drives.data ?? []).map((d) => (
                <option key={d.id} value={d.id}>
                  {driveName(d)}
                </option>
              ))
            : (users.data ?? []).map((u) => (
                <option key={u.id} value={u.id}>
                  {u.username}
                </option>
              ))}
        </NativeSelect>
      </div>
      <HoldReasonField value={reason} onChange={setReason} />
    </ConfirmDialog>
  );
}
