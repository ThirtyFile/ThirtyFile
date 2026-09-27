import { useState } from "react";
import { useInfiniteQuery } from "@tanstack/react-query";
import { Loader2Icon } from "lucide-react";
import { api, type ShareAccessFilter } from "@/api";
import { Button } from "@/components/ui/button";
import { cn, formatWinDate } from "@/lib/utils";
import { t } from "@/lib/i18n";
import { shownCount } from "@/components/logs/shown";
import { DateRangeFilter, FilterBar, MultiSelect, SearchBox, rangeToUnix, type DateRange } from "./filters";

const PAGE = 100;

export const ACCESS_EVENTS: Record<string, { label: string; tone?: string }> = {
  view: { label: t("Open share page") },
  unlock: { label: t("Correct password"), tone: "text-emerald-600 dark:text-emerald-400" },
  password_fail: { label: t("Wrong password"), tone: "text-destructive" },
  preview: { label: t("Preview") },
  download: { label: t("Download"), tone: "text-brand" },
  zip: { label: t("ZIP download"), tone: "text-brand" },
  upload: { label: t("Uploaded a file"), tone: "text-violet-600 dark:text-violet-400" },
};

const EVENT_GROUPS = [{ options: Object.entries(ACCESS_EVENTS).map(([value, e]) => ({ value, label: e.label })) }];

/** Work out the browser and OS from the User-Agent (only common cases; the full string is in the tooltip) */
export function describeAgent(ua: string) {
  if (!ua) return "—";
  const browser = /Edg\//.test(ua)
    ? "Edge"
    : /OPR\//.test(ua)
      ? "Opera"
      : /Chrome\//.test(ua)
        ? "Chrome"
        : /Firefox\//.test(ua)
          ? "Firefox"
          : /Safari\//.test(ua)
            ? "Safari"
            : /curl|wget|python|Go-http/i.test(ua)
              ? t("Script")
              : t("Other");
  const os = /Windows/.test(ua)
    ? "Windows"
    : /iPhone|iPad/.test(ua)
      ? "iOS"
      : /Android/.test(ua)
        ? "Android"
        : /Mac OS X/.test(ua)
          ? "macOS"
          : /Linux/.test(ua)
            ? "Linux"
            : "";
  return os ? `${browser} · ${os}` : browser;
}

/**
 * Share link access log. With shareId, only that link (for the sharer);
 * without it, lists records for all of your own links, or everything for admins, filterable by sharer.
 */
export function ShareAccessLog({ shareId, admin, className }: { shareId?: string; admin?: boolean; className?: string }) {
  const [q, setQ] = useState("");
  const [owner, setOwner] = useState("");
  const [ip, setIp] = useState("");
  const [events, setEvents] = useState<string[]>([]);
  const [range, setRange] = useState<DateRange>({ key: "all" });
  const filter: ShareAccessFilter = { share_id: shareId, q, owner, ip, event: events.join(","), ...rangeToUnix(range) };

  const list = useInfiniteQuery({
    queryKey: ["share-access", filter],
    queryFn: ({ pageParam }) => api.shareAccess({ ...filter, before: pageParam, limit: PAGE }),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (last) => last.next ?? undefined,
  });
  const rows = list.data?.pages.flatMap((p) => p.items) ?? [];
  const filtered = !!(q || owner || ip || events.length || range.key !== "all");
  const th = "sticky top-0 z-[1] bg-background px-2.5 py-1.5 text-left font-normal text-muted-foreground";

  return (
    <div className={cn("flex min-h-0 flex-col", className)}>
      <FilterBar>
        {!shareId && <SearchBox value={q} onChange={setQ} placeholder={t("Search items or link codes")} className="w-48 max-sm:w-full" />}
        {!shareId && admin && <SearchBox value={owner} onChange={setOwner} placeholder={t("Shared by")} className="w-28" />}
        <SearchBox value={ip} onChange={setIp} placeholder="IP" className="w-32" />
        <MultiSelect label={t("Event")} groups={EVENT_GROUPS} value={events} onChange={setEvents} />
        <DateRangeFilter value={range} onChange={setRange} />
      </FilterBar>
      <div className="min-h-0 flex-1 overflow-auto">
        {list.isLoading ? (
          <div className="flex h-24 items-center justify-center text-muted-foreground">
            <Loader2Icon className="size-5 animate-spin" />
          </div>
        ) : list.error ? (
          <p className="p-4 text-sm text-destructive">{list.error.message}</p>
        ) : !rows.length ? (
          <p className="p-6 text-center text-sm text-muted-foreground">{filtered ? t("No matching records") : t("No one has accessed this yet")}</p>
        ) : (
          <table className="w-full min-w-[560px] border-collapse text-xs">
            <thead>
              <tr className="border-b">
                <th className={cn(th, "w-[150px]")}>{t("Time")}</th>
                <th className={cn(th, "w-[96px]")}>{t("Event")}</th>
                <th className={th}>{t("Item")}</th>
                {!shareId && <th className={cn(th, "w-[130px] max-md:hidden")}>{t("Link")}</th>}
                {!shareId && admin && <th className={cn(th, "w-[90px] max-lg:hidden")}>{t("Shared by")}</th>}
                <th className={cn(th, "w-[120px]")}>IP</th>
                <th className={cn(th, "w-[130px] max-md:hidden")}>{t("Browser")}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((a) => {
                const ev = ACCESS_EVENTS[a.event];
                return (
                  <tr key={a.id} className="border-b border-border/40 hover:bg-muted/50">
                    <td className="px-2.5 py-1.5 whitespace-nowrap text-muted-foreground">{formatWinDate(a.at)}</td>
                    <td className={cn("px-2.5 py-1.5 whitespace-nowrap", ev?.tone)}>{ev?.label ?? a.event}</td>
                    <td className="max-w-0 truncate px-2.5 py-1.5" title={a.node_name}>
                      {a.node_name || <span className="text-muted-foreground">—</span>}
                    </td>
                    {!shareId && <td className="truncate px-2.5 py-1.5 font-mono text-muted-foreground max-md:hidden">/share/{a.share_id}</td>}
                    {!shareId && admin && <td className="truncate px-2.5 py-1.5 text-muted-foreground max-lg:hidden">{a.owner_name ?? "—"}</td>}
                    <td className="px-2.5 py-1.5 font-mono whitespace-nowrap">
                      {a.ip ? (
                        <button type="button" className="hover:text-brand hover:underline" title={t("Show only this IP")} onClick={() => setIp(a.ip)}>
                          {a.ip}
                        </button>
                      ) : (
                        <span className="text-muted-foreground" title={t("Visitor IP logging is turned off in Log settings")}>
                          {t("Not recorded")}
                        </span>
                      )}
                    </td>
                    <td className="truncate px-2.5 py-1.5 text-muted-foreground max-md:hidden" title={a.user_agent}>
                      {describeAgent(a.user_agent)}
                    </td>
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
      </div>
    </div>
  );
}
