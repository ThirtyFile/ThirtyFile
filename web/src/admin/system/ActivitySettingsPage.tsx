import { useQueryClient, type QueryKey } from "@tanstack/react-query";
import { ActivityIcon, CircleAlertIcon, Link2Icon, LogInIcon, RefreshCwIcon, SettingsIcon, type LucideIcon } from "lucide-react";
import { Link } from "react-router";
import { Tabs } from "@base-ui/react/tabs";
import { keys } from "@/api/queryKeys";
import { ActivityLog } from "@/components/logs/ActivityLog";
import { ShareAccessLog } from "@/components/logs/ShareAccessLog";
import { LoginLog } from "@/components/logs/LoginLog";
import { ErrorLog } from "@/components/logs/ErrorLog";
import { Frame, ToolButton } from "@/components/Frame";
import { controlPanelItem, useSettingsSearch } from "@/admin/controlPanel";
import { cn } from "@/lib/utils";
import { usePersisted } from "@/lib/session";
import { t } from "@/lib/i18n";

type LogTab = "activity" | "login" | "share" | "errors";
const LOG_TABS: Record<LogTab, { query: QueryKey; footer: string }> = {
  activity: { query: keys.activity(), footer: t("Actions users performed in the system") },
  login: { query: keys.loginLog(), footer: t("Records of successful and failed sign-ins, sign-outs, and password changes") },
  share: {
    query: keys.shareAccess(),
    footer: t("Records of public share links being opened, previewed, and downloaded"),
  },
  errors: { query: keys.errors(), footer: t("Errors people ran into, reported by the server and by the web page") },
};

export function ActivitySettingsPage() {
  const qc = useQueryClient();
  const [tab, setTab] = usePersisted<LogTab>("tf-log-tab", "activity");
  const { title, icon } = controlPanelItem("activity");
  const searchSettings = useSettingsSearch();
  // Tabs (Base UI): the arrow keys, Home and End move between them, and each is tied to the log it shows
  const tabBtn = (key: LogTab, label: string, Icon: LucideIcon) => (
    <Tabs.Tab
      value={key}
      className={cn(
        "flex h-9 shrink-0 items-center gap-1.5 border-b-2 px-3 text-[13px] whitespace-nowrap outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset max-sm:px-2",
        tab === key ? "border-brand font-medium text-foreground" : "border-transparent text-muted-foreground hover:text-foreground",
      )}
    >
      <Icon className="size-4" /> {label}
    </Tabs.Tab>
  );
  const panel = "flex min-h-0 flex-1 flex-col outline-none";
  return (
    <Frame
      toolbar={
        <>
          <ToolButton icon={RefreshCwIcon} label={t("Refresh")} showLabel onClick={() => qc.invalidateQueries({ queryKey: LOG_TABS[tab].query })} />
          <span className="flex-1" />
          <Link to="/admin/logs" className="flex h-8 items-center gap-1.5 rounded-md px-2.5 text-xs text-muted-foreground hover:bg-muted hover:text-foreground">
            <SettingsIcon className="size-4" /> {t("Log settings")}
          </Link>
        </>
      }
      crumbs={[{ label: t("Control panel"), to: "/admin" }, { label: title }]}
      icon={icon as LucideIcon}
      upTo="/admin"
      searchPlaceholder={t("Search settings")}
      onSearch={searchSettings}
      footer={<span>{LOG_TABS[tab].footer}</span>}
    >
      <Tabs.Root value={tab} onValueChange={(v) => setTab(v as LogTab)} className="flex min-h-0 flex-1 flex-col">
        <Tabs.List activateOnFocus aria-label={title} className="flex shrink-0 gap-1 overflow-x-auto border-b px-3 max-sm:px-1">
          {tabBtn("activity", t("Activity log"), ActivityIcon)}
          {tabBtn("login", t("Sign-in log"), LogInIcon)}
          {tabBtn("share", t("Share link access"), Link2Icon)}
          {tabBtn("errors", t("Errors"), CircleAlertIcon)}
        </Tabs.List>
        <Tabs.Panel value="activity" className={panel}>
          <ActivityLog className="min-h-0 flex-1" />
        </Tabs.Panel>
        <Tabs.Panel value="login" className={panel}>
          <LoginLog admin className="min-h-0 flex-1" />
        </Tabs.Panel>
        <Tabs.Panel value="share" className={panel}>
          <ShareAccessLog admin className="min-h-0 flex-1" />
        </Tabs.Panel>
        <Tabs.Panel value="errors" className={panel}>
          <ErrorLog className="min-h-0 flex-1" />
        </Tabs.Panel>
      </Tabs.Root>
    </Frame>
  );
}
