import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { DownloadIcon, Loader2Icon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, triggerDownload, type LogArchive, type LogSettings } from "@/api";
import { Skeleton } from "@/components/ui/skeleton";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ConfirmDialog } from "@/components/dialogs";
import { cn, formatBytes, formatDate } from "@/lib/utils";
import { locale, t, tServer } from "@/lib/i18n";
import { Toggle, Section, SettingsFrame } from "@/pages/SettingsFrame";

const KIND_LABEL: Record<LogArchive["kind"], string> = {
  activity: t("Activity log"),
  share_access: t("Share link access log"),
  login_log: t("Sign-in log"),
};

function days(n: number) {
  return n === 0 ? t("no cleanup") : t("{n} day|{n} days", { n });
}

/** Retention days input (at module level, so the input isn't recreated and doesn't lose focus on re-render) */
function DayInput({ label, hint, value, onChange }: { label: string; hint: string; value: number; onChange(v: number): void }) {
  return (
    <label className="grid gap-1.5">
      <span className="text-[13px] font-medium">{label}</span>
      <div className="flex items-center gap-2">
        <Input
          type="number"
          min={0}
          max={36500}
          className="h-8 w-28 text-right tabular-nums"
          value={value}
          onChange={(e) => onChange(Math.max(0, Math.floor(Number(e.target.value) || 0)))}
        />
        <span className="text-xs text-muted-foreground">{t("days")}</span>
      </div>
      <span className="text-xs text-muted-foreground">{hint}</span>
    </label>
  );
}

/** Log settings: retention days, archive or delete, archive retention, whether to record guest info; plus the archive list */
export function LogSettingsPage() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["log-status"], queryFn: api.logStatus });
  const [draft, setDraft] = useState<LogSettings | null>(null);
  const [deleting, setDeleting] = useState<LogArchive | null>(null);
  const cur = draft ?? q.data?.settings;
  const changed = !!draft && JSON.stringify(draft) !== JSON.stringify(q.data?.settings);

  const save = useMutation({
    mutationFn: (s: LogSettings) => api.updateLogSettings(s),
    onSuccess: (data) => {
      qc.setQueryData(["log-status"], data);
      setDraft(null);
      toast.success(t("Log settings updated"));
    },
    onError: (e) => toast.error(e.message),
  });
  const archive = useMutation({
    mutationFn: api.archiveLogsNow,
    onSuccess: (data) => {
      qc.setQueryData(["log-status"], data);
      qc.invalidateQueries({ queryKey: ["activity"] });
      qc.invalidateQueries({ queryKey: ["share-access"] });
      toast.success(data.summary ? tServer(data.summary) : t("Archiving complete"));
    },
    onError: (e) => toast.error(e.message),
  });

  const set = (patch: Partial<LogSettings>) => cur && setDraft({ ...cur, ...patch });
  const d = q.data;
  const modes: [boolean, string, string][] = [
    [true, t("Compress and archive"), t("Moved out of the database and saved as compressed files you can download when needed")],
    [false, t("Delete directly"), t("Not kept; frees space in both the database and on disk")],
  ];
  return (
    <SettingsFrame item="logs" onRefresh={() => q.refetch()}>
      {!d || !cur ? (
        <Skeleton className="h-40" />
      ) : (
        <>
          <Section title={t("Current status")}>
            <dl className="grid grid-cols-2 sm:grid-cols-5">
              {[
                [
                  t("Activity log"),
                  t("{n} entry|{n} entries", { n: d.activity.rows }),
                  d.activity.oldest ? t("Oldest: {date}", { date: formatDate(d.activity.oldest) }) : "—",
                ],
                [
                  t("Share link access"),
                  t("{n} entry|{n} entries", { n: d.share_access.rows }),
                  d.share_access.oldest ? t("Oldest: {date}", { date: formatDate(d.share_access.oldest) }) : "—",
                ],
                [
                  t("Sign-in log"),
                  t("{n} entry|{n} entries", { n: d.login_log.rows }),
                  d.login_log.oldest ? t("Oldest: {date}", { date: formatDate(d.login_log.oldest) }) : "—",
                ],
                [t("Archives"), t("{n}", { n: d.archives.length }), formatBytes(d.archive_bytes)],
                [t("Last cleanup"), d.last_run ? formatDate(d.last_run) : t("Not run yet"), t("Runs automatically every day")],
              ].map(([k, v, hint]) => (
                <div key={k} className="border-r border-b p-4 [&:nth-child(2n)]:max-sm:border-r-0 sm:[&:nth-child(5n)]:border-r-0">
                  <dt className="text-xs text-muted-foreground">{k}</dt>
                  <dd className="mt-1 text-lg font-medium tabular-nums">{v}</dd>
                  <dd className="text-[11px] text-muted-foreground">{hint}</dd>
                </div>
              ))}
            </dl>
          </Section>

          <Section title={t("Retention and archiving")}>
            <form
              className="grid gap-5 p-4"
              onSubmit={(e) => {
                e.preventDefault();
                if (changed && draft) save.mutate(draft);
              }}
            >
              <div className="grid gap-5 sm:grid-cols-2">
                <DayInput
                  label={t("Activity log retention (days)")}
                  hint={t("Days to keep in the database; older entries are cleaned up once a day. 0 = no cleanup")}
                  value={cur.activity_days}
                  onChange={(v) => set({ activity_days: v })}
                />
                <DayInput
                  label={t("Share link access log retention (days)")}
                  hint={t("Same as above, for access records of public share links")}
                  value={cur.share_days}
                  onChange={(v) => set({ share_days: v })}
                />
                <DayInput
                  label={t("Sign-in log retention (days)")}
                  hint={t("Successful or failed sign-ins, sign-outs, and password changes; security audits often require keeping these for a year or more")}
                  value={cur.login_days}
                  onChange={(v) => set({ login_days: v })}
                />
              </div>
              <div className="grid gap-2">
                <span className="text-[13px] font-medium">{t("Entries older than the retention period")}</span>
                <div className="flex flex-wrap gap-2">
                  {modes.map(([v, label, hint]) => (
                    <button
                      key={String(v)}
                      type="button"
                      aria-pressed={cur.archive === v}
                      onClick={() => set({ archive: v })}
                      className={cn(
                        "grid max-w-64 gap-0.5 rounded-lg border px-3 py-2 text-left",
                        cur.archive === v ? "border-brand bg-brand/5" : "hover:bg-muted/60",
                      )}
                    >
                      <span className="text-[13px] font-medium">{label}</span>
                      <span className="text-xs text-muted-foreground">{hint}</span>
                    </button>
                  ))}
                </div>
              </div>
              {cur.archive && (
                <DayInput
                  label={t("Archive retention (days)")}
                  hint={t("Archives older than this are deleted automatically. 0 = keep forever")}
                  value={cur.archive_keep_days}
                  onChange={(v) => set({ archive_keep_days: v })}
                />
              )}
              <div className="flex items-start gap-4">
                <div className="min-w-0 flex-1">
                  <div className="text-[13px] font-medium">{t("Record share link visitors' IP address and browser")}</div>
                  <p className="mt-1 text-xs text-muted-foreground">{t("Used to investigate unusual downloads; when off, only the time and event are recorded. Follow your company's privacy policy when deciding whether to turn this on.")}</p>
                </div>
                <Toggle label={t("Record visitors' IP address and browser")} checked={cur.record_visitor} onChange={(v) => set({ record_visitor: v })} />
              </div>
              <div className="flex flex-wrap items-center gap-2 border-t pt-4">
                <Button type="submit" size="sm" disabled={!changed || save.isPending}>
                  {t("Save settings")}
                </Button>
                {changed && (
                  <Button type="button" size="sm" variant="ghost" onClick={() => setDraft(null)}>
                    {t("Discard changes")}
                  </Button>
                )}
                <span className="flex-1" />
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  disabled={archive.isPending || changed}
                  onClick={() => archive.mutate()}
                  title={changed ? t("Save your settings first") : t("Clean up now using the current settings instead of waiting for the daily schedule")}
                >
                  {archive.isPending && <Loader2Icon className="animate-spin" />}
                  {t("Clean up now")}
                </Button>
              </div>
              <p className="-mt-2 text-xs text-muted-foreground">
                {(() => {
                  const vars = { activity: days(d.settings.activity_days), login: days(d.settings.login_days), share: days(d.settings.share_days) };
                  if (!d.settings.archive) return t("Current settings: activity log {activity}, sign-in log {login}, share access {share}. Older entries are deleted.", vars);
                  if (!d.settings.archive_keep_days)
                    return t("Current settings: activity log {activity}, sign-in log {login}, share access {share}. Older entries are compressed and archived, and archives are kept forever.", vars);
                  return t("Current settings: activity log {activity}, sign-in log {login}, share access {share}. Older entries are compressed and archived, and archives are kept for {n} day.|Current settings: activity log {activity}, sign-in log {login}, share access {share}. Older entries are compressed and archived, and archives are kept for {n} days.", {
                    ...vars,
                    n: d.settings.archive_keep_days,
                  });
                })()}
              </p>
            </form>
          </Section>

          <Section title={t("Archives ({n})", { n: d.archives.length })}>
            {d.archives.length === 0 ? (
              <p className="p-6 text-center text-sm text-muted-foreground">{t("No archives yet")}</p>
            ) : (
              <table className="w-full border-collapse text-xs">
                <thead>
                  <tr className="border-b text-left text-muted-foreground">
                    <th className="px-3 py-2 font-normal">{t("Log type")}</th>
                    <th className="px-3 py-2 font-normal">{t("Period")}</th>
                    <th className="px-3 py-2 text-right font-normal">{t("Entries")}</th>
                    <th className="px-3 py-2 text-right font-normal max-sm:hidden">{t("Size")}</th>
                    <th className="w-24 px-3 py-2" />
                  </tr>
                </thead>
                <tbody>
                  {d.archives.map((a) => (
                    <tr key={a.id} className="border-b border-border/40 last:border-0">
                      <td className="px-3 py-2">{KIND_LABEL[a.kind]}</td>
                      <td className="px-3 py-2 whitespace-nowrap text-muted-foreground">
                        {formatDate(a.from_at)} – {formatDate(a.to_at)}
                      </td>
                      <td className="px-3 py-2 text-right tabular-nums">{a.rows.toLocaleString(locale)}</td>
                      <td className="px-3 py-2 text-right text-muted-foreground tabular-nums max-sm:hidden">{formatBytes(a.bytes)}</td>
                      <td className="px-3 py-2 text-right whitespace-nowrap">
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          title={t("Download (.jsonl.gz, one JSON entry per line)")}
                          aria-label={t("Download")}
                          onClick={() => triggerDownload(api.logArchiveUrl(a.id))}
                        >
                          <DownloadIcon />
                        </Button>
                        <Button variant="ghost" size="icon-sm" title={t("Delete archive")} aria-label={t("Delete")} onClick={() => setDeleting(a)}>
                          <Trash2Icon />
                        </Button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Section>
        </>
      )}
      {deleting && (
        <ConfirmDialog
          title={t("Delete this archive?")}
          description={t("{kind} {from} – {to}, {n} entry. This can't be undone, so consider downloading it first.|{kind} {from} – {to}, {n} entries. This can't be undone, so consider downloading it first.", {
            kind: KIND_LABEL[deleting.kind],
            from: formatDate(deleting.from_at),
            to: formatDate(deleting.to_at),
            n: deleting.rows,
          })}
          confirmText={t("Delete permanently")}
          destructive
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            await api.deleteLogArchive(deleting.id);
            setDeleting(null);
            q.refetch();
          }}
        />
      )}
    </SettingsFrame>
  );
}
