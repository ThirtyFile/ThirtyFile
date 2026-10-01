import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArchiveRestoreIcon,
  BellIcon,
  CalendarClockIcon,
  CheckCheckIcon,
  CopyCheckIcon,
  HardDriveIcon,
  InboxIcon,
  KeySquareIcon,
  Link2Icon,
  SettingsIcon,
  Trash2Icon,
  UsersRoundIcon,
} from "lucide-react";
import { toast } from "sonner";
import { api, type AppNotification } from "@/api";
import { affected, invalidate, keys } from "@/api/queryKeys";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { NotificationSettingsDialog } from "@/components/NotificationSettingsDialog";
import { notificationLink, notificationText, unreadBadge } from "@/lib/notifications";
import { cn, formatTime } from "@/lib/utils";
import { t } from "@/lib/i18n";

const ICON = {
  shared: UsersRoundIcon,
  space_full: HardDriveIcon,
  access_expiring: CalendarClockIcon,
  app_password: KeySquareIcon,
  sign_in_method: Link2Icon,
  link_upload: InboxIcon,
  backup: ArchiveRestoreIcon,
  replica: CopyCheckIcon,
};

/** How often the bell asks for new notifications */
const POLL_MS = 60_000;

function Item({ n, onOpen }: { n: AppNotification; onOpen(n: AppNotification): void }) {
  const { title, detail } = notificationText(n);
  const Icon = ICON[n.kind] ?? BellIcon;
  return (
    <DropdownMenuItem onClick={() => onOpen(n)} className="items-start gap-2.5 py-2">
      <Icon className={cn("mt-0.5", (n.kind === "space_full" || ((n.kind === "backup" || n.kind === "replica") && n.data.state !== "recovered")) && "text-destructive")} />
      <span className="grid min-w-0 flex-1 gap-0.5">
        <span className={cn("text-[13px] leading-snug whitespace-normal", !n.read && "font-medium")}>{title}</span>
        {detail && <span className="text-xs whitespace-normal text-muted-foreground">{detail}</span>}
        <span className="text-[11px] text-muted-foreground">{formatTime(n.created_at)}</span>
      </span>
      {!n.read && <span className="mt-1.5 size-2 shrink-0 rounded-full bg-brand" aria-label={t("Unread")} />}
    </DropdownMenuItem>
  );
}

/** The bell next to the tabs: what was shared with you, spaces filling up and access ending soon */
export function NotificationBell() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [settings, setSettings] = useState(false);
  const q = useQuery({ queryKey: keys.notifications(), queryFn: api.notifications, refetchInterval: POLL_MS });
  const unread = q.data?.unread ?? 0;
  const items = useMemo(() => q.data?.items ?? [], [q.data]);

  // Something new was shared: the space list and "Shared with me" show it without a manual refresh
  const newest = items[0]?.id ?? 0;
  const seen = useRef<number | null>(null);
  useEffect(() => {
    if (seen.current !== null && newest > seen.current && items.some((n) => n.id > seen.current! && n.kind === "shared")) {
      void invalidate(qc, ...affected.myAccess());
    }
    seen.current = newest;
  }, [newest, items, qc]);

  const refresh = () => qc.invalidateQueries({ queryKey: keys.notifications() });
  const read = useMutation({ mutationFn: (ids?: number[]) => api.markNotificationsRead(ids), onSettled: refresh });
  const clear = useMutation({
    mutationFn: api.clearNotifications,
    onSettled: refresh,
    onError: (e) => toast.error(e.message),
  });
  const open = (n: AppNotification) => {
    if (!n.read) read.mutate([n.id]);
    const to = notificationLink(n);
    if (to) navigate(to);
  };

  return (
    <>
      <DropdownMenu onOpenChange={(o) => o && refresh()}>
        <DropdownMenuTrigger
          render={
            <button
              type="button"
              aria-label={unread ? t("Notifications ({n} unread)", { n: unread }) : t("Notifications")}
              title={t("Notifications")}
              className="relative mt-2 mr-2 mb-1 flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground"
            />
          }
        >
          <BellIcon className="size-4" />
          {unread > 0 && (
            <span className="absolute -top-1 -right-1 flex h-4 min-w-4 items-center justify-center rounded-full bg-destructive px-1 text-[10px] leading-none font-medium text-white tabular-nums">
              {unreadBadge(unread)}
            </span>
          )}
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-[min(360px,calc(100vw-1rem))]">
          <div className="px-2 py-1.5 text-xs font-medium text-muted-foreground">{t("Notifications")}</div>
          {items.length === 0 ? (
            <p className="px-2 pt-2 pb-4 text-center text-sm text-muted-foreground">{t("No notifications")}</p>
          ) : (
            <div className="max-h-[min(60vh,480px)] overflow-y-auto">
              {items.map((n) => (
                <Item key={n.id} n={n} onOpen={open} />
              ))}
            </div>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem disabled={!unread} closeOnClick={false} onClick={() => read.mutate(undefined)}>
            <CheckCheckIcon /> {t("Mark all as read")}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={!items.length} onClick={() => clear.mutate()}>
            <Trash2Icon /> {t("Clear all")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => setSettings(true)}>
            <SettingsIcon /> {t("Notification settings")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {settings && <NotificationSettingsDialog onClose={() => setSettings(false)} />}
    </>
  );
}
