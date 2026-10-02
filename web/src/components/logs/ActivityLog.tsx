import { useState } from "react";
import { useInfiniteQuery } from "@tanstack/react-query";
import { DownloadIcon, Loader2Icon } from "lucide-react";
import { api, type ActivityFilter } from "@/api";
import { keys } from "@/api/queryKeys";
import { nativeDownload } from "@/downloads";
import { Button } from "@/components/ui/button";
import { ErrorState } from "@/components/ErrorState";
import { ACTION_GROUPS, actionLabel } from "@/components/logs/actions";
import { cn, formatDateTime } from "@/lib/utils";
import { t, tServer } from "@/lib/i18n";
import { shownCount } from "@/components/logs/shown";
import { DateRangeFilter, FilterBar, MultiSelect, SearchBox, rangeToUnix, type DateRange } from "./filters";

const PAGE = 100;
const GROUPS = ACTION_GROUPS.map((g) => ({ label: g.label, options: g.actions.map((a) => ({ value: a, label: actionLabel(a) })) }));

/**
 * Activity log: keyword, user, action and date filters, loads more when scrolled to the bottom, exportable to CSV.
 * With driveId, only that space (space managers); otherwise everything (admins).
 */
/** Space names are user data; only the default names created by the system are translated */
const defaultName = (name: string | null) => (name === "My files" ? t("My files") : name === "All files" ? t("All files") : (name ?? ""));

export function ActivityLog({ driveId, className, compact }: { driveId?: string; className?: string; compact?: boolean }) {
  const [q, setQ] = useState("");
  const [user, setUser] = useState("");
  const [actions, setActions] = useState<string[]>([]);
  const [range, setRange] = useState<DateRange>({ key: "all" });
  const filter: ActivityFilter = { drive_id: driveId, q, user, action: actions.join(","), ...rangeToUnix(range) };

  const list = useInfiniteQuery({
    queryKey: keys.activity(filter),
    queryFn: ({ pageParam }) => api.activity({ ...filter, before: pageParam, limit: PAGE }),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (last) => last.next ?? undefined,
  });
  const rows = list.data?.pages.flatMap((p) => p.items) ?? [];
  const filtered = !!(q || user || actions.length || range.key !== "all");
  const th = "sticky top-0 z-[1] bg-background px-2.5 py-1.5 text-left font-normal text-muted-foreground";

  return (
    <div className={cn("flex min-h-0 flex-col", className)}>
      <FilterBar
        right={
          <Button
            variant="outline"
            size="sm"
            className="h-8 text-xs"
            disabled={!rows.length}
            title={t("Export records that match the filters (up to 100,000)")}
            onClick={() => nativeDownload(api.activityExportUrl(filter))}
          >
            <DownloadIcon /> {t("Export CSV")}
          </Button>
        }
      >
        <SearchBox value={q} onChange={setQ} placeholder={t("Search items or details")} className="w-48 max-sm:w-full" />
        {!compact && <SearchBox value={user} onChange={setUser} placeholder={t("User")} className="w-32" />}
        <MultiSelect label={t("Action")} groups={GROUPS} value={actions} onChange={setActions} />
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
          <p className="p-6 text-center text-sm text-muted-foreground">{filtered ? t("No matching records") : t("No activity yet")}</p>
        ) : (
          <table className="w-full min-w-[560px] border-collapse text-xs">
            <thead>
              <tr className="border-b">
                <th className={cn(th, "w-[150px]")}>{t("Time")}</th>
                <th className={cn(th, "w-[100px]")}>{t("User")}</th>
                <th className={cn(th, "w-[110px]")}>{t("Action")}</th>
                <th className={th}>{t("Item")}</th>
                {!driveId && <th className={cn(th, "w-[130px] max-md:hidden")}>{t("Spaces")}</th>}
              </tr>
            </thead>
            <tbody>
              {rows.map((a) => {
                // Translated once per row (the cell and its tooltip show the same text)
                const detail = tServer(a.detail);
                return (
                  <tr key={a.id} className="border-b border-border/40 hover:bg-muted/50">
                    <td className="px-2.5 py-1.5 whitespace-nowrap text-muted-foreground">{formatDateTime(a.at)}</td>
                    <td className="truncate px-2.5 py-1.5">
                      <button type="button" className="hover:text-brand hover:underline" title={t("Show only this user")} onClick={() => setUser(a.username)}>
                        {a.username}
                      </button>
                    </td>
                    <td className="px-2.5 py-1.5 whitespace-nowrap">{actionLabel(a.action)}</td>
                    {a.private ? (
                      <td className="max-w-0 truncate px-2.5 py-1.5 text-muted-foreground italic">{t("In someone else's personal space")}</td>
                    ) : (
                      <td className="max-w-0 truncate px-2.5 py-1.5" title={`${a.node_name} ${detail}`}>
                        {a.node_name || (a.node_id ? defaultName(a.drive_name) : "")}
                        {detail && <span className="ml-1.5 text-muted-foreground">{detail}</span>}
                      </td>
                    )}
                    {!driveId && <td className="truncate px-2.5 py-1.5 text-muted-foreground max-md:hidden">{a.drive_name === null ? "—" : defaultName(a.drive_name)}</td>}
                  </tr>
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
        {shownCount(rows.length, !!list.hasNextPage, filtered)}
        {!driveId && <> · {t("Older records are archived according to Log settings and can be downloaded there.")}</>}
      </div>
    </div>
  );
}
