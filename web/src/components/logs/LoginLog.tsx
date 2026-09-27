import { useState } from "react";
import { useInfiniteQuery } from "@tanstack/react-query";
import { DownloadIcon, Loader2Icon } from "lucide-react";
import { api, triggerDownload, type LoginFilter } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn, formatWinDate } from "@/lib/utils";
import { DateRangeFilter, FilterBar, MultiSelect, SearchBox, rangeToUnix, type DateRange } from "./filters";
import { describeAgent } from "./ShareAccessLog";
import { ProviderIcon } from "@/components/ProviderIcon";
import { t, tc } from "@/lib/i18n";
import { shownCount } from "@/components/logs/shown";

const PAGE = 100;

export const LOGIN_EVENTS: Record<string, { label: string; tone?: string }> = {
  login: { label: t("Signed in"), tone: "text-emerald-600 dark:text-emerald-400" },
  bad_password: { label: t("Wrong password"), tone: "text-destructive" },
  unknown_user: { label: t("Account doesn't exist"), tone: "text-destructive" },
  disabled: { label: tc("event", "Account disabled"), tone: "text-amber-600 dark:text-amber-400" },
  locked: { label: t("Temporarily locked"), tone: "text-destructive font-medium" },
  logout: { label: t("Signed out") },
  password_change: { label: t("Password changed"), tone: "text-brand" },
  sso_denied: { label: t("Third-party sign-in denied"), tone: "text-destructive" },
  sso_provisioned: { label: t("Account created by third-party sign-in"), tone: "text-brand" },
  sso_link: { label: t("External account linked"), tone: "text-brand" },
  sso_unlink: { label: t("External account unlinked") },
};

const METHOD_LABEL: Record<string, string> = { password: tc("method", "Password"), microsoft: "Microsoft", google: "Google", github: "GitHub" };

const EVENT_GROUPS = [
  { label: t("Sign-ins"), options: ["login", "logout", "password_change"].map((v) => ({ value: v, label: LOGIN_EVENTS[v].label })) },
  { label: t("Failed"), options: ["bad_password", "unknown_user", "disabled", "locked", "sso_denied"].map((v) => ({ value: v, label: LOGIN_EVENTS[v].label })) },
  { label: t("External accounts"), options: ["sso_provisioned", "sso_link", "sso_unlink"].map((v) => ({ value: v, label: LOGIN_EVENTS[v].label })) },
];

/**
 * Login log: successful or failed logins, logouts, password changes.
 * Admins see everything (filterable by account); with userId only that user; regular users only ever get their own records.
 */
export function LoginLog({ userId, admin, className }: { userId?: number; admin?: boolean; className?: string }) {
  const [user, setUser] = useState("");
  const [ip, setIp] = useState("");
  const [events, setEvents] = useState<string[]>([]);
  const [range, setRange] = useState<DateRange>({ key: "all" });
  const showUser = admin && userId === undefined;
  const filter: LoginFilter = { user_id: userId, user: showUser ? user : "", ip, event: events.join(","), ...rangeToUnix(range) };

  const list = useInfiniteQuery({
    queryKey: ["login-log", filter],
    queryFn: ({ pageParam }) => api.loginLog({ ...filter, before: pageParam, limit: PAGE }),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (last) => last.next ?? undefined,
  });
  const rows = list.data?.pages.flatMap((p) => p.items) ?? [];
  const filtered = !!((showUser && user) || ip || events.length || range.key !== "all");
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
            onClick={() => triggerDownload(api.loginLogExportUrl(filter))}
          >
            <DownloadIcon /> {t("Export CSV")}
          </Button>
        }
      >
        {showUser && <SearchBox value={user} onChange={setUser} placeholder={t("Account")} className="w-32" />}
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
          <p className="p-6 text-center text-sm text-muted-foreground">{filtered ? t("No matching records") : t("No sign-in records yet")}</p>
        ) : (
          <table className="w-full min-w-[480px] border-collapse text-xs">
            <thead>
              <tr className="border-b">
                <th className={cn(th, "w-[150px]")}>{t("Time")}</th>
                {showUser && <th className={cn(th, "w-[120px]")}>{t("Account")}</th>}
                <th className={cn(th, "w-[96px]")}>{t("Event")}</th>
                <th className={cn(th, "w-[96px]")}>{t("Method")}</th>
                <th className={cn(th, "w-[130px]")}>IP</th>
                <th className={cn(th, "max-md:hidden")}>{t("Browser")}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => {
                const ev = LOGIN_EVENTS[r.event];
                return (
                  <tr key={r.id} className="border-b border-border/40 hover:bg-muted/50">
                    <td className="px-2.5 py-1.5 whitespace-nowrap text-muted-foreground">{formatWinDate(r.at)}</td>
                    {showUser && (
                      <td className="max-w-0 truncate px-2.5 py-1.5">
                        <button
                          type="button"
                          className={cn("hover:text-brand hover:underline", r.user_id === null && "text-muted-foreground italic")}
                          title={r.user_id === null ? t("This account doesn't exist; click to show only this account") : t("Show only this account")}
                          onClick={() => setUser(r.username)}
                        >
                          {r.username || t("(blank)")}
                        </button>
                      </td>
                    )}
                    <td className={cn("px-2.5 py-1.5 whitespace-nowrap", ev?.tone)}>{ev?.label ?? r.event}</td>
                    <td className="px-2.5 py-1.5 whitespace-nowrap">
                      <span className="inline-flex items-center gap-1.5">
                        {r.method !== "password" && <ProviderIcon provider={r.method} className="size-3.5" />}
                        {METHOD_LABEL[r.method] ?? r.method}
                      </span>
                    </td>
                    <td className="px-2.5 py-1.5 font-mono whitespace-nowrap">
                      <button type="button" className="hover:text-brand hover:underline" title={t("Show only this IP")} onClick={() => setIp(r.ip)}>
                        {r.ip}
                      </button>
                    </td>
                    <td className="truncate px-2.5 py-1.5 text-muted-foreground max-md:hidden" title={r.user_agent}>
                      {describeAgent(r.user_agent)}
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

/** Login log dialog: users see their own; admins see a user's from the user list */
export function LoginLogDialog({ title, description, userId, onClose }: { title: string; description?: string; userId?: number; onClose(): void }) {
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="flex h-[75vh] flex-col gap-0 p-0 sm:max-w-3xl">
        <DialogHeader className="border-b px-4 py-3">
          <DialogTitle>{title}</DialogTitle>
          {description && <DialogDescription>{description}</DialogDescription>}
        </DialogHeader>
        <LoginLog userId={userId} className="min-h-0 flex-1" />
      </DialogContent>
    </Dialog>
  );
}
