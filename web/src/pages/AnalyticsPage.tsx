import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChartPieIcon, Loader2Icon, RefreshCwIcon } from "lucide-react";
import { Link, useParams } from "react-router";
import { toast } from "sonner";
import { api, type AnalyticsFile, type AnalyticsSnapshot } from "@/api";
import { keys } from "@/api/queryKeys";
import { Treemap } from "@/components/analytics/Treemap";
import { ErrorState } from "@/components/ErrorState";
import { Frame, ToolButton } from "@/components/Frame";
import { Button } from "@/components/ui/button";
import { NativeSelect } from "@/components/ui/native-select";
import { waitForJob } from "@/lib/jobs";
import { t, tServer } from "@/lib/i18n";
import { cn, errorMessage, formatBytes, formatDate, formatDateTime } from "@/lib/utils";

type Tab = "overview" | "largest" | "duplicates" | "old" | "treemap";

const YEAR = 365 * 86400;
const count = (n: number) => new Intl.NumberFormat().format(n);

const KIND_LABEL = {
  pictures: () => t("Pictures"),
  video: () => t("Video"),
  audio: () => t("Audio"),
  documents: () => t("Documents"),
  archives: () => t("Archives"),
  other: () => t("Other"),
} as const;

/** What takes the room of a space: the largest files, copies, folders nobody has touched, and a treemap of it all */
export function AnalyticsPage() {
  const { drive = "" } = useParams();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: keys.analytics(drive), queryFn: ({ signal }) => api.analytics(drive, signal) });
  const [tab, setTab] = useState<Tab>("overview");
  const [working, setWorking] = useState(false);
  const refresh = async () => {
    setWorking(true);
    try {
      await waitForJob(await api.refreshAnalytics(drive));
      await qc.invalidateQueries({ queryKey: keys.analytics(drive) });
    } catch (e) {
      toast.error(tServer(errorMessage(e, t("Couldn't work out what takes the room"))));
    } finally {
      setWorking(false);
    }
  };
  const name = q.data?.drive.name ?? "";
  const s = q.data?.snapshot ?? null;
  return (
    <Frame
      toolbar={<ToolButton icon={RefreshCwIcon} label={t("Refresh")} showLabel disabled={working} onClick={() => void refresh()} />}
      icon={ChartPieIcon}
      crumbs={[{ label: t("Storage analytics") }, ...(name ? [{ label: name }] : [])]}
      upTo="/drives"
    >
      <div className="min-h-0 flex-1 overflow-auto px-4 pb-6">
        {q.error && !q.data ? (
          <ErrorState message={tServer(q.error.message)} onRetry={() => q.refetch()} />
        ) : !q.data ? (
          <p role="status" className="pt-4">
            {t("Loading…")}
          </p>
        ) : (
          <div className="grid gap-4 pt-3">
            <div className="flex flex-wrap items-center gap-3 text-sm" role="status">
              {working ? (
                <span className="flex items-center gap-2">
                  <Loader2Icon className="size-4 animate-spin" />
                  {t("Reading {name}…", { name })}
                </span>
              ) : s ? (
                <span className="text-muted-foreground">{t("As of {time}", { time: formatDateTime(s.taken_at) })}</span>
              ) : (
                <span className="text-muted-foreground">{t("Nothing has been worked out for {name} yet.", { name })}</span>
              )}
              <Button type="button" size="sm" variant={s ? "outline" : "default"} disabled={working} onClick={() => void refresh()}>
                {s ? t("Refresh") : t("Work it out")}
              </Button>
              {!s && !working && <span className="text-xs text-muted-foreground">{t("A large space takes a while; the page can be left meanwhile.")}</span>}
            </div>
            {s && <Tabs tab={tab} setTab={setTab} s={s} />}
            {s && tab === "overview" && <Overview s={s} setTab={setTab} />}
            {s && tab === "largest" && <Largest s={s} onChange={() => qc.invalidateQueries({ queryKey: keys.analytics(drive) })} />}
            {s && tab === "duplicates" && <Duplicates s={s} onChange={() => qc.invalidateQueries({ queryKey: keys.analytics(drive) })} />}
            {s && tab === "old" && <OldFolders s={s} />}
            {s && tab === "treemap" && <Treemap root={s.tree} />}
          </div>
        )}
      </div>
    </Frame>
  );
}

function Tabs({ tab, setTab, s }: { tab: Tab; setTab(t: Tab): void; s: AnalyticsSnapshot }) {
  const items: [Tab, string][] = [
    ["overview", t("Overview")],
    ["largest", t("Largest files")],
    ["duplicates", s.duplicates.groups.length ? t("Duplicates ({n})", { n: s.duplicates.groups.length }) : t("Duplicates")],
    ["old", t("Old folders")],
    ["treemap", t("Treemap")],
  ];
  return (
    <div role="tablist" aria-label={t("Storage analytics")} className="flex flex-wrap gap-1 border-b">
      {items.map(([id, label]) => (
        <button
          key={id}
          type="button"
          role="tab"
          aria-selected={tab === id}
          onClick={() => setTab(id)}
          className={cn("-mb-px border-b-2 px-3 py-1.5 text-sm", tab === id ? "border-brand font-medium" : "border-transparent text-muted-foreground hover:text-foreground")}
        >
          {label}
        </button>
      ))}
    </div>
  );
}

function Overview({ s, setTab }: { s: AnalyticsSnapshot; setTab(t: Tab): void }) {
  const widest = Math.max(1, ...s.kinds.map((k) => k.bytes));
  return (
    <div className="grid gap-5">
      <div className="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-2">
        <Figure label={t("Files")} value={formatBytes(s.live_bytes)} note={t("{n} file|{n} files", { n: s.files })} />
        <Figure label={t("Folders")} value={count(s.folders)} />
        <Figure label={t("In the trash")} value={formatBytes(s.trash_bytes)} note={t("{n} file|{n} files", { n: s.trash_files })} />
        <Figure label={t("Earlier versions")} value={formatBytes(s.version_bytes)} />
      </div>

      <section className="grid gap-2" aria-label={t("By kind of file")}>
        <h2 className="text-xs font-medium text-muted-foreground">{t("By kind of file")}</h2>
        <ul className="grid gap-1.5 rounded-lg border bg-card p-3 text-sm">
          {s.kinds.map((k) => (
            <li key={k.kind} className="grid grid-cols-[110px_minmax(0,1fr)_150px] items-center gap-3">
              <span>{KIND_LABEL[k.kind]()}</span>
              <span className="h-2 rounded bg-muted" aria-hidden="true">
                <span className="block h-2 rounded bg-brand" style={{ width: `${Math.round((k.bytes / widest) * 100)}%` }} />
              </span>
              <span className="text-right text-xs text-muted-foreground tabular-nums">
                {formatBytes(k.bytes)} · {t("{n} file|{n} files", { n: k.files })}
              </span>
            </li>
          ))}
        </ul>
      </section>

      <section className="grid gap-2" aria-label={t("What could be cleaned up")}>
        <h2 className="text-xs font-medium text-muted-foreground">{t("What could be cleaned up")}</h2>
        <ul className="grid gap-2 text-sm">
          {s.trash_bytes > 0 && (
            <Suggestion>
              <span>{t("Empty the trash ({size})", { size: formatBytes(s.trash_bytes) })}</span>
              <Link to="/trash" className="underline underline-offset-2">
                {t("Open the trash")}
              </Link>
            </Suggestion>
          )}
          {s.old_version_bytes > 0 && (
            <Suggestion>
              <span>{t("Earlier versions replaced more than a year ago ({size})", { size: formatBytes(s.old_version_bytes) })}</span>
              <span className="text-xs text-muted-foreground">{t("How long versions are kept is set in Control panel › General")}</span>
            </Suggestion>
          )}
          {s.stale_bytes > 0 && (
            <Suggestion>
              <span>{t("Files not changed for a year ({size})", { size: formatBytes(s.stale_bytes) })}</span>
              <button type="button" className="underline underline-offset-2" onClick={() => setTab("old")}>
                {t("Look at the old folders")}
              </button>
            </Suggestion>
          )}
          {s.duplicates.wasted_bytes > 0 && (
            <Suggestion>
              <span>{t("Copies of the same file ({size})", { size: formatBytes(s.duplicates.wasted_bytes) })}</span>
              <button type="button" className="underline underline-offset-2" onClick={() => setTab("duplicates")}>
                {t("Look at the copies")}
              </button>
            </Suggestion>
          )}
          {s.trash_bytes === 0 && s.old_version_bytes === 0 && s.stale_bytes === 0 && s.duplicates.wasted_bytes === 0 && (
            <li className="text-muted-foreground">{t("Nothing stands out.")}</li>
          )}
        </ul>
      </section>
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

function Suggestion({ children }: { children: React.ReactNode }) {
  return <li className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 rounded-lg border bg-card px-3 py-2">{children}</li>;
}

/** A file's place and what can be done with it, in a row */
function FileLine({ f, children }: { f: AnalyticsFile; children?: React.ReactNode }) {
  return (
    <div className="grid min-w-0 gap-0.5">
      <span className="flex flex-wrap items-baseline gap-x-3">
        <span className="truncate font-medium">{f.name}</span>
        <span className="text-xs text-muted-foreground tabular-nums">{formatBytes(f.size)}</span>
      </span>
      <span className="flex flex-wrap items-center gap-x-3 text-xs text-muted-foreground">
        <span className="truncate">{f.folder}</span>
        {f.owner && <span>{f.owner}</span>}
        <span>{formatDate(f.updated_at)}</span>
        {f.folder_id && (
          <Link to={`/files/${f.folder_id}`} className="underline underline-offset-2">
            {t("Open location")}
          </Link>
        )}
        {children}
      </span>
    </div>
  );
}

async function trash(ids: string[]) {
  await api.trash(ids);
  toast.success(t("{n} item moved to the trash|{n} items moved to the trash", { n: ids.length }));
}

function Largest({ s, onChange }: { s: AnalyticsSnapshot; onChange(): void }) {
  const [gone, setGone] = useState<string[]>([]);
  const files = s.largest.filter((f) => !gone.includes(f.id));
  if (!s.largest.length) return <p className="text-sm text-muted-foreground">{t("There are no files.")}</p>;
  return (
    <ul className="divide-y rounded-lg border bg-card text-sm" aria-label={t("Largest files")}>
      {files.map((f) => (
        <li key={f.id} className="flex items-center justify-between gap-3 px-3 py-2">
          <FileLine f={f} />
          <Button
            type="button"
            size="xs"
            variant="outline"
            onClick={() =>
              void trash([f.id])
                .then(() => {
                  setGone((g) => [...g, f.id]);
                  onChange();
                })
                .catch((e) => toast.error(tServer(errorMessage(e, t("Couldn't make the change")))))
            }
          >
            {t("Move to the trash")}
          </Button>
        </li>
      ))}
    </ul>
  );
}

function Duplicates({ s, onChange }: { s: AnalyticsSnapshot; onChange(): void }) {
  const d = s.duplicates;
  const [picked, setPicked] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const toggle = (id: string) => setPicked((p) => (p.includes(id) ? p.filter((x) => x !== id) : [...p, id]));
  if (!d.groups.length) {
    return <p className="text-sm text-muted-foreground">{d.partial ? t("No copies were found in the files that were read.") : t("No two files have the same content.")}</p>;
  }
  return (
    <div className="grid gap-3">
      <p className="text-xs leading-relaxed text-muted-foreground">
        {s.folder_space
          ? t("Each copy is a file on the disk, so moving one to the trash frees its room once the trash is emptied. Files that share a size were read to compare them.")
          : t("ThirtyFile stores identical content once, so copies take quota but not disk space: moving them to the trash gives the quota back.")}{" "}
        {t("Nothing is chosen or removed for you.")}
      </p>
      {d.partial && <p className="text-xs text-amber-700 dark:text-amber-300">{t("Only some of the files with the same size were read, so there may be more copies.")}</p>}
      <div className="flex flex-wrap items-center gap-3 text-sm">
        <span>{t("{size} in copies", { size: formatBytes(d.wasted_bytes) })}</span>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={!picked.length || busy}
          onClick={() => {
            setBusy(true);
            void trash(picked)
              .then(() => {
                setPicked([]);
                onChange();
              })
              .catch((e) => toast.error(tServer(errorMessage(e, t("Couldn't make the change")))))
              .finally(() => setBusy(false));
          }}
        >
          {busy && <Loader2Icon className="animate-spin" />}
          {t("Move selected to the trash")}
        </Button>
      </div>
      <ul className="grid gap-3">
        {d.groups.map((g, i) => {
          const ids = g.files.map((f) => f.id);
          return (
            <li key={`${g.size}-${i}-${ids[0]}`} className="rounded-lg border bg-card">
              <p className="border-b px-3 py-2 text-xs text-muted-foreground">
                {t("{n} copies of {size}", { n: g.copies, size: formatBytes(g.size) })} · {t("{size} in copies", { size: formatBytes(g.wasted) })}
                {g.copies > g.files.length ? ` · ${t("{n} shown", { n: g.files.length })}` : ""}
              </p>
              <ul className="divide-y text-sm">
                {g.files.map((f) => {
                  const on = picked.includes(f.id);
                  // One copy always stays: the last one of a group can't be chosen
                  const last = !on && ids.filter((x) => x !== f.id).every((x) => picked.includes(x)) && g.files.length === g.copies;
                  return (
                    <li key={f.id} className="flex items-center gap-3 px-3 py-2">
                      <input type="checkbox" className="size-4 accent-brand" checked={on} disabled={last} aria-label={t("Select {name}", { name: f.name })} onChange={() => toggle(f.id)} />
                      <FileLine f={f} />
                    </li>
                  );
                })}
              </ul>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function OldFolders({ s }: { s: AnalyticsSnapshot }) {
  const [years, setYears] = useState(1);
  const cutoff = s.taken_at - years * YEAR;
  const rows = s.folders_by_size.filter((f) => f.newest > 0 && f.newest < cutoff);
  return (
    <div className="grid gap-3">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <label htmlFor="old-years">{t("Folders where nothing changed for")}</label>
        <NativeSelect id="old-years" value={String(years)} onChange={(e) => setYears(Number(e.target.value))}>
          <option value="1">{t("1 year")}</option>
          <option value="2">{t("2 years")}</option>
          <option value="5">{t("5 years")}</option>
        </NativeSelect>
      </div>
      <p className="text-xs text-muted-foreground">
        {t("Not changed means the newest change of a file in the folder, in ThirtyFile or on the disk. ThirtyFile doesn't know when a file was last opened.")}
      </p>
      {rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("No folder is that old among the {n} largest.", { n: s.folders_by_size.length })}</p>
      ) : (
        <ul className="divide-y rounded-lg border bg-card text-sm" aria-label={t("Old folders")}>
          {rows.map((f) => (
            <li key={f.id} className="flex items-center justify-between gap-3 px-3 py-2">
              <span className="grid min-w-0 gap-0.5">
                <span className="truncate font-medium">{f.path}</span>
                <span className="text-xs text-muted-foreground">
                  {t("{n} file|{n} files", { n: f.files })} · {t("Last change {date}", { date: formatDate(f.newest) })}
                </span>
              </span>
              <span className="flex shrink-0 items-center gap-3">
                <span className="text-xs tabular-nums">{formatBytes(f.bytes)}</span>
                <Link to={`/files/${f.id}`} className="text-xs underline underline-offset-2">
                  {t("Open location")}
                </Link>
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
