import { Fragment, useState } from "react";
import { useInfiniteQuery } from "@tanstack/react-query";
import { ChevronRightIcon, CircleAlertIcon, Loader2Icon, TriangleAlertIcon } from "lucide-react";
import { api, type ErrorEntry, type ErrorFilter } from "@/api";
import { keys } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { ErrorState } from "@/components/ErrorState";
import { cn, formatWinDate } from "@/lib/utils";
import { t, tServer } from "@/lib/i18n";
import { shownCount } from "@/components/logs/shown";
import { ExportCsvButton, csvTime, type CsvColumn } from "@/components/logs/exportCsv";
import { DateRangeFilter, FilterBar, MultiSelect, SearchBox, rangeToUnix, type DateRange } from "./filters";

const PAGE = 100;

const SOURCE_LABEL: Record<ErrorEntry["source"], string> = { backend: t("Server"), frontend: t("Web page") };
const SEVERITY_LABEL: Record<ErrorEntry["severity"], string> = { error: t("Error"), warning: t("Warning") };

/** The columns of an exported error log, with the names the list and a record's details show */
const ERROR_COLUMNS: CsvColumn<ErrorEntry>[] = [
  { header: t("Time"), value: (r) => csvTime(r.at) },
  { header: t("First seen"), value: (r) => csvTime(r.first_at) },
  { header: t("Occurrences"), value: (r) => r.count },
  { header: t("User"), value: (r) => (r.user_id === null && !r.username ? t("Not signed in") : r.username) },
  { header: t("Source"), value: (r) => SOURCE_LABEL[r.source] ?? r.source },
  { header: t("Severity"), value: (r) => SEVERITY_LABEL[r.severity] ?? r.severity },
  { header: t("Kind"), value: (r) => ERROR_KINDS[r.kind] ?? r.kind },
  { header: t("Operation"), value: (r) => r.operation },
  { header: t("Status"), value: (r) => r.status },
  { header: t("Message"), value: (r) => (r.source === "backend" ? tServer(r.message) : r.message) },
  { header: t("Diagnostic details"), value: (r) => r.detail },
  { header: t("Reported by the page"), value: (r) => r.client },
  { header: t("Request ID"), value: (r) => r.request_id },
  { header: t("Version"), value: (r) => r.version },
];

/** What kind of failure it was */
export const ERROR_KINDS: Record<string, string> = {
  server: t("Server error"),
  storage: t("Storage unavailable"),
  timeout: t("Timed out"),
  validation: t("Invalid request"),
  permission: t("No permission"),
  not_found: t("Not found"),
  conflict: t("Conflict"),
  limit: t("Over a limit"),
  render: t("Page error"),
  uncaught: t("Script error"),
  rejection: t("Unhandled error"),
  handled: t("Shown to the user"),
};

/** What was being done, for the operations the page reports */
const OPERATIONS: Record<string, string> = {
  upload: t("Upload"),
  preview: t("Preview"),
  save: t("Save"),
  rename: t("Rename"),
  move: t("Move"),
  copy: t("Copy"),
  delete: t("Delete permanently"),
  create: t("New"),
  favorite: t("Favorites"),
  compress: t("Compress to ZIP"),
  extract: t("Extract"),
  job: t("Background task"),
};

const FILTER_GROUPS = {
  source: [{ label: t("Source"), options: (["backend", "frontend"] as const).map((v) => ({ value: v, label: SOURCE_LABEL[v] })) }],
  severity: [{ label: t("Severity"), options: (["error", "warning"] as const).map((v) => ({ value: v, label: SEVERITY_LABEL[v] })) }],
};

/**
 * Error log (administrators): errors people ran into, reported by the server and by the web page. Filters by keyword,
 * user, source, severity and date; a row opens to show its details. Everything shown is text (never markup).
 */
export function ErrorLog({ className }: { className?: string }) {
  const [q, setQ] = useState("");
  const [user, setUser] = useState("");
  const [sources, setSources] = useState<string[]>([]);
  const [severities, setSeverities] = useState<string[]>([]);
  const [range, setRange] = useState<DateRange>({ key: "all" });
  const [open, setOpen] = useState<number | null>(null);
  const filter: ErrorFilter = { q, user, source: sources.join(","), severity: severities.join(","), ...rangeToUnix(range) };

  const list = useInfiniteQuery({
    queryKey: keys.errors(filter),
    queryFn: ({ pageParam }) => api.errorLog({ ...filter, before: pageParam, limit: PAGE }),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (last) => last.next ?? undefined,
  });
  const rows = list.data?.pages.flatMap((p) => p.items) ?? [];
  const filtered = !!(q || user || sources.length || severities.length || range.key !== "all");
  const th = "sticky top-0 z-[1] bg-background px-2.5 py-1.5 text-left font-normal text-muted-foreground";

  return (
    <div className={cn("flex min-h-0 flex-col", className)}>
      <FilterBar right={<ExportCsvButton name={t("error-log")} load={() => api.errorLogExport(filter)} columns={ERROR_COLUMNS} disabled={!rows.length} />}>
        <SearchBox value={q} onChange={setQ} placeholder={t("Search messages or request IDs")} className="w-52 max-sm:w-full" />
        <SearchBox value={user} onChange={setUser} placeholder={t("User")} className="w-32" />
        <MultiSelect label={t("Source")} groups={FILTER_GROUPS.source} value={sources} onChange={setSources} />
        <MultiSelect label={t("Severity")} groups={FILTER_GROUPS.severity} value={severities} onChange={setSeverities} />
        <DateRangeFilter value={range} onChange={setRange} />
      </FilterBar>
      <div className="min-h-0 flex-1 overflow-auto">
        {list.isLoading ? (
          <div className="flex h-24 items-center justify-center text-muted-foreground">
            <Loader2Icon className="size-5 animate-spin" />
          </div>
        ) : list.error ? (
          <ErrorState message={list.error.message} onRetry={() => list.refetch()} />
        ) : !rows.length ? (
          <p className="p-6 text-center text-sm text-muted-foreground">{filtered ? t("No matching records") : t("No errors recorded")}</p>
        ) : (
          <table className="w-full min-w-[600px] border-collapse text-xs">
            <thead>
              <tr className="border-b">
                <th className={cn(th, "w-[170px]")}>{t("Time")}</th>
                <th className={cn(th, "w-[100px]")}>{t("User")}</th>
                <th className={cn(th, "w-[80px]")}>{t("Source")}</th>
                <th className={cn(th, "w-[160px] max-md:hidden")}>{t("Operation")}</th>
                <th className={th}>{t("Message")}</th>
                <th className={cn(th, "w-[56px] text-right")}>{t("Count")}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => {
                const expanded = open === r.id;
                const message = r.source === "backend" ? tServer(r.message) : r.message;
                const Icon = r.severity === "error" ? CircleAlertIcon : TriangleAlertIcon;
                return (
                  <Fragment key={r.id}>
                    <tr className={cn("border-b border-border/40 hover:bg-muted/50", expanded && "bg-muted/40")}>
                      <td className="px-2.5 py-1.5 whitespace-nowrap text-muted-foreground">
                        <button
                          type="button"
                          aria-expanded={expanded}
                          aria-controls={`error-${r.id}`}
                          title={expanded ? t("Hide details") : t("Show details")}
                          className="inline-flex items-center gap-1 rounded hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                          onClick={() => setOpen(expanded ? null : r.id)}
                        >
                          <ChevronRightIcon className={cn("size-3.5 transition-transform", expanded && "rotate-90")} />
                          {formatWinDate(r.at)}
                        </button>
                      </td>
                      <td className="max-w-0 truncate px-2.5 py-1.5">
                        {r.user_id === null && !r.username ? (
                          <span className="text-muted-foreground italic">{t("Not signed in")}</span>
                        ) : (
                          <button type="button" className="hover:text-brand hover:underline" title={t("Show only this user")} onClick={() => setUser(r.username)}>
                            {r.username}
                          </button>
                        )}
                      </td>
                      <td className="px-2.5 py-1.5 whitespace-nowrap">{SOURCE_LABEL[r.source] ?? r.source}</td>
                      <td className="max-w-0 truncate px-2.5 py-1.5 font-mono text-[11px] max-md:hidden" title={r.operation}>
                        {OPERATIONS[r.operation] ?? r.operation}
                      </td>
                      <td className="max-w-0 truncate px-2.5 py-1.5" title={message}>
                        <span className={cn("inline-flex max-w-full items-center gap-1.5", r.severity === "error" ? "text-destructive" : "text-amber-700 dark:text-amber-400")}>
                          <Icon className="size-3.5 shrink-0" aria-label={SEVERITY_LABEL[r.severity]} />
                          <span className="truncate text-foreground">{message || ERROR_KINDS[r.kind] || r.kind}</span>
                        </span>
                      </td>
                      <td className="px-2.5 py-1.5 text-right tabular-nums">{r.count}</td>
                    </tr>
                    {expanded && (
                      <tr id={`error-${r.id}`} className="border-b border-border/40 bg-muted/20">
                        <td colSpan={6} className="px-4 py-3">
                          <ErrorDetails entry={r} message={message} />
                        </td>
                      </tr>
                    )}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        )}
        {list.hasNextPage && (
          <div className="flex justify-center p-3">
            <Button variant="outline" size="sm" disabled={list.isFetchingNextPage} onClick={() => list.fetchNextPage()}>
              {list.isFetchingNextPage && <Loader2Icon className="animate-spin" />}
              {t("Load more")}
            </Button>
          </div>
        )}
      </div>
      <div className="border-t px-3 py-1.5 text-[11px] text-muted-foreground">
        {shownCount(rows.length, !!list.hasNextPage, filtered)} · {t("Names of files and folders are left out of error records.")}
      </div>
    </div>
  );
}

/** A record's details, as plain text */
function ErrorDetails({ entry: r, message }: { entry: ErrorEntry; message: string }) {
  const fields: [string, string][] = [
    [t("Severity"), SEVERITY_LABEL[r.severity] ?? r.severity],
    [t("Kind"), ERROR_KINDS[r.kind] ?? r.kind],
    [t("Message"), message],
    [t("Operation"), r.operation],
    [t("Route"), r.route],
    [t("Item ID"), r.resource],
    [t("Status"), r.status === null ? "" : String(r.status)],
    [t("Error code"), r.code],
    [t("Request ID"), r.request_id ?? ""],
    [t("First seen"), formatWinDate(r.first_at)],
    [t("Last seen"), r.count > 1 ? formatWinDate(r.at) : ""],
    [t("Occurrences"), String(r.count)],
    [t("Version"), r.version],
    [t("Reported by the page"), r.client],
  ];
  return (
    <div className="space-y-3 text-xs">
      <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1">
        {fields
          .filter(([, v]) => v)
          .map(([k, v]) => (
            <Fragment key={k}>
              <dt className="text-muted-foreground">{k}</dt>
              <dd className="min-w-0 break-all">{v}</dd>
            </Fragment>
          ))}
      </dl>
      {r.detail && (
        <div>
          <div className="mb-1 text-muted-foreground">{t("Diagnostic details")}</div>
          <pre className="max-h-64 overflow-auto rounded border bg-background p-2 font-mono text-[11px] whitespace-pre-wrap break-all">{r.detail}</pre>
        </div>
      )}
    </div>
  );
}
