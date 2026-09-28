import { useQueryClient } from "@tanstack/react-query";
import { ActivityIcon, Link2Icon, LogInIcon, RefreshCwIcon, SettingsIcon, type LucideIcon } from "lucide-react";
import { Link } from "react-router";
import { ActivityLog } from "@/components/logs/ActivityLog";
import { ShareAccessLog } from "@/components/logs/ShareAccessLog";
import { LoginLog } from "@/components/logs/LoginLog";
import { Frame, ToolButton } from "@/components/Frame";
import { controlPanelItem, useSettingsSearch } from "@/lib/controlPanel";
import { cn } from "@/lib/utils";
import { usePersisted } from "@/lib/session";
import { t } from "@/lib/i18n";

type LogTab = "activity" | "login" | "share";
const LOG_TABS: Record<LogTab, { query: string; footer: string }> = {
  activity: { query: "activity", footer: t("Actions users performed in the system") },
  login: { query: "login-log", footer: t("Records of successful and failed sign-ins, sign-outs, and password changes") },
  share: {
    query: "share-access",
    footer: t("Records of public share links being opened, previewed, and downloaded"),
  },
};

export function ActivitySettingsPage() {
  const qc = useQueryClient();
  const [tab, setTab] = usePersisted<LogTab>("tf-log-tab", "activity");
  const { title, icon } = controlPanelItem("activity");
  const searchSettings = useSettingsSearch();
  const tabBtn = (key: LogTab, label: string, Icon: LucideIcon) => (
    <button
      type="button"
      role="tab"
      aria-selected={tab === key}
      onClick={() => setTab(key)}
      className={cn(
        "flex h-9 items-center gap-1.5 border-b-2 px-3 text-[13px]",
        tab === key ? "border-brand font-medium text-foreground" : "border-transparent text-muted-foreground hover:text-foreground",
      )}
    >
      <Icon className="size-4" /> {label}
    </button>
  );
  return (
    <Frame
      toolbar={
        <>
          <ToolButton icon={RefreshCwIcon} label={t("Refresh")} showLabel onClick={() => qc.invalidateQueries({ queryKey: [LOG_TABS[tab].query] })} />
          <span className="flex-1" />
          <Link
            to="/admin/logs"
            className="flex h-8 items-center gap-1.5 rounded-md px-2.5 text-xs text-muted-foreground hover:bg-muted hover:text-foreground"
          >
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
      <div role="tablist" className="flex shrink-0 gap-1 border-b px-3">
        {tabBtn("activity", t("Activity log"), ActivityIcon)}
        {tabBtn("login", t("Sign-in log"), LogInIcon)}
        {tabBtn("share", t("Share link access"), Link2Icon)}
      </div>
      {tab === "activity" ? (
        <ActivityLog className="min-h-0 flex-1" />
      ) : tab === "login" ? (
        <LoginLog admin className="min-h-0 flex-1" />
      ) : (
        <ShareAccessLog admin className="min-h-0 flex-1" />
      )}
    </Frame>
  );
}
