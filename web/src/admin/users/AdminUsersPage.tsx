import { useEffect, useMemo, useState } from "react";
import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { ClockIcon, FolderMinusIcon, SearchXIcon, FolderPlusIcon, HistoryIcon, MonitorSmartphoneIcon, ShieldCheckIcon, ShieldOffIcon, PencilIcon, RefreshCwIcon, Trash2Icon, TriangleAlertIcon, UserCheckIcon, UserPlusIcon, UsersIcon, UserXIcon } from "lucide-react";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { DataTable, EmptyState, type Column } from "@/components/DataTable";
import { toast } from "sonner";
import { api, type UserRow } from "@/api";
import { keys } from "@/api/queryKeys";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/dialogs";
import { useLocationName } from "@/components/LocationSelect";
import { confirm } from "@/lib/confirm";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { useMe } from "@/lib/session";
import { t, tc } from "@/lib/i18n";
import { formatBytes, formatDate, formatDateTime, errorMessage } from "@/lib/utils";
import { LoginLogDialog } from "@/components/logs/LoginLog";
import { DevicesDialog } from "@/components/DevicesDialog";
import { ProviderIcon, SSO_LABEL, type SsoProviderId } from "@/components/ProviderIcon";
import { AddPersonalDialog, RemovePersonalDialog, DeleteUserDialog } from "@/admin/users/AccountDialogs";
import { UserDialog } from "@/admin/users/UserDialog";

const USERS_PAGE = 200;

export function AdminUsersPage() {
  const me = useMe();
  // The search box finds accounts by username, display name or email, on the server (the list comes in pages)
  const [typed, setTyped] = useState("");
  const [search, setSearch] = useState("");
  useEffect(() => {
    const timer = setTimeout(() => setSearch(typed.trim()), 250);
    return () => clearTimeout(timer);
  }, [typed]);
  // Loaded a page at a time: there is one account per person, so the list can be long
  const q = useInfiniteQuery({
    queryKey: keys.adminUserPages(search),
    queryFn: ({ pageParam, signal }) => api.usersPage(pageParam, USERS_PAGE, search, signal),
    initialPageParam: 0,
    getNextPageParam: (last) => (last.length < USERS_PAGE ? undefined : last[last.length - 1].id),
  });
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [editing, setEditing] = useState<UserRow | "new" | null>(null);
  const [deleting, setDeleting] = useState<UserRow | null>(null);
  const [loginsOf, setLoginsOf] = useState<UserRow | null>(null);
  const [devicesOf, setDevicesOf] = useState<UserRow | null>(null);
  const [resetting, setResetting] = useState<UserRow | null>(null);
  // Creating or removing someone's "My files"
  const [personalOf, setPersonalOf] = useState<{ t: "add" | "remove"; user: UserRow } | null>(null);
  const locationName = useLocationName();
  const qc = useQueryClient();
  const users = useMemo(() => q.data?.pages.flat() ?? [], [q.data]);
  const selected = users.find((u) => u.id === selectedId) ?? null;

  const toolbar = (
    <>
      <ToolButton icon={UserPlusIcon} label={t("Add user")} showLabel onClick={() => setEditing("new")} />
      <ToolSeparator />
      <ToolButton icon={PencilIcon} label={t("Edit")} showLabel disabled={!selected} onClick={() => selected && setEditing(selected)} />
      <ToolButton
        icon={Trash2Icon}
        label={t("Delete")}
        showLabel
        disabled={!selected || selected.id === me.id}
        onClick={() => selected && setDeleting(selected)}
      />
      <ToolSeparator />
      <ToolButton icon={HistoryIcon} label={t("Sign-in log")} showLabel disabled={!selected} onClick={() => selected && setLoginsOf(selected)} />
      <ToolButton icon={MonitorSmartphoneIcon} label={t("Devices")} showLabel disabled={!selected} onClick={() => selected && setDevicesOf(selected)} />
    </>
  );

  const columns: Column<UserRow>[] = [
    {
      header: t("Account"),
      cell: (u) => (
        <div className="flex items-center gap-2">
          <span className="flex size-5 items-center justify-center rounded-full bg-brand/80 text-[10px] text-brand-foreground uppercase">
            {u.username.slice(0, 1)}
          </span>
          <span>{u.username}</span>
          {u.display_name && <span className="truncate text-muted-foreground">{u.display_name}</span>}
          {u.sso_email && u.sso_email.toLowerCase() !== u.username.toLowerCase() && (
            <span className="text-amber-600 dark:text-amber-400" title={t("The email of the linked sign-in ({email}) differs from the username", { email: u.sso_email })}>
              <TriangleAlertIcon className="size-3.5" />
            </span>
          )}
          {u.sso &&
            u.sso.split(",").map((p) => (
              <span key={p} title={t("Linked to {provider} sign-in", { provider: SSO_LABEL[p as SsoProviderId] ?? p })}>
                <ProviderIcon provider={p} className="size-3.5" />
              </span>
            ))}
          {u.source !== "password" && (
            <Badge variant="outline" className="h-4 px-1.5 text-[10px]" title={t("Created automatically by {provider} sign-in", { provider: SSO_LABEL[u.source as SsoProviderId] ?? u.source })}>
              {t("Auto-created")}
            </Badge>
          )}
          {u.two_factor && (
            <span className="text-emerald-600 dark:text-emerald-400" title={t("Two-factor sign-in is on")}>
              <ShieldCheckIcon className="size-3.5" />
            </span>
          )}
          {u.role === "admin" && <Badge className="h-4 px-1.5 text-[10px]">{t("Administrator")}</Badge>}
          {u.disabled && (
            <Badge variant="outline" className="h-4 px-1.5 text-[10px]">
              {t("Disabled")}
            </Badge>
          )}
        </div>
      ),
    },
    {
      header: t("Permissions"),
      cellClassName: "text-muted-foreground",
      cell: (u) =>
        u.role === "admin"
          ? t("All")
          : [u.can_write && t("Edit"), u.can_delete && t("Delete"), u.can_share && t("Share")].filter(Boolean).join(t(", ")) || t("View only"),
    },
    {
      header: tc("space", "Used"),
      cell: (u) =>
        !u.personal_space ? (
          // No "My files": say so, or that it waits for its storage location
          u.personal_pending ? (
            <Badge
              variant="outline"
              className="h-5 gap-1 border-amber-500/50 px-1.5 text-[11px] text-amber-700 dark:text-amber-300"
              title={t("Waiting for {location}: it's created once the location is available", { location: locationName(u.personal_pending) })}
            >
              <ClockIcon className="size-3" />
              {t("My files pending (location unavailable)")}
            </Badge>
          ) : (
            <span className="text-muted-foreground">{t("No \"My files\"")}</span>
          )
        ) : (
          <div className="flex items-center gap-2" title={t("Includes items in the trash")}>
            <span className="tabular-nums">
              {formatBytes(u.used_bytes)}
              <span className="text-muted-foreground"> / {u.quota_bytes ? formatBytes(u.quota_bytes) : tc("short", "Unlimited")}</span>
            </span>
            {u.quota_bytes > 0 && (
              <span className="h-1 w-20 overflow-hidden rounded bg-muted">
                <span className="block h-full bg-brand" style={{ width: `${Math.min(100, (u.used_bytes / u.quota_bytes) * 100)}%` }} />
              </span>
            )}
          </div>
        ),
    },
    {
      header: t("Last sign-in"),
      className: "max-md:hidden",
      cellClassName: "text-muted-foreground",
      // Logins from before the login log was enabled have no time to show
      cell: (u) => (u.last_login_at ? formatDateTime(u.last_login_at) : <span title={t("Hasn't signed in since the sign-in log was enabled")}>—</span>),
    },
    { header: t("Date created"), className: "max-lg:hidden", cellClassName: "text-muted-foreground", cell: (u) => formatDate(u.created_at) },
  ];

  return (
    <Frame
      toolbar={toolbar}
      crumbs={[{ label: t("Control panel"), to: "/admin" }, { label: t("Users") }]}
      upTo="/admin"
      searchPlaceholder={t("Search users")}
      onSearch={setTyped}
      icon={UsersIcon}
      footer={
        <span className="flex items-center gap-2">
          {search ? t("{n} user found|{n} users found", { n: users.length }) : t("{n} user|{n} users", { n: users.length })}
          {q.hasNextPage && (
            <Button variant="link" size="sm" className="h-auto p-0" disabled={q.isFetchingNextPage} onClick={() => q.fetchNextPage()}>
              {t("Show more")}
            </Button>
          )}
        </span>
      }
    >
      <DataTable
        label={t("Users")}
        rows={users}
        rowKey={(u) => String(u.id)}
        columns={columns}
        loading={q.isLoading}
        error={q.error}
        onRetry={() => q.refetch()}
        empty={search ? <EmptyState icon={SearchXIcon} title={t("No users match \"{query}\"", { query: search })} /> : undefined}
        selectedKey={selectedId === null ? null : String(selectedId)}
        onSelect={(k) => setSelectedId(k ? Number(k) : null)}
        onOpen={(u) => setEditing(u)}
        rowClassName={(u) => u.disabled && "text-muted-foreground"}
        menu={() =>
          selected ? (
            <>
              <DropdownMenuItem onClick={() => setEditing(selected)}>
                <PencilIcon /> {t("Edit")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => setLoginsOf(selected)}>
                <HistoryIcon /> {t("Sign-in log")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => setDevicesOf(selected)}>
                <MonitorSmartphoneIcon /> {t("Devices")}
              </DropdownMenuItem>
              {selected.two_factor && (
                <DropdownMenuItem onClick={() => setResetting(selected)}>
                  <ShieldOffIcon /> {t("Reset two-factor sign-in")}
                </DropdownMenuItem>
              )}
              <DropdownMenuSeparator />
              {!selected.personal_space && (
                <DropdownMenuItem onClick={() => setPersonalOf({ t: "add", user: selected })}>
                  <FolderPlusIcon /> {t("Create \"My files\"…")}
                </DropdownMenuItem>
              )}
              {(selected.personal_space || selected.personal_pending) && (
                <DropdownMenuItem onClick={() => setPersonalOf({ t: "remove", user: selected })}>
                  <FolderMinusIcon /> {t("Remove \"My files\"…")}
                </DropdownMenuItem>
              )}
              {selected.id !== me.id && (
                <DropdownMenuItem
                  onClick={async () => {
                    if (
                      !selected.disabled &&
                      !(await confirm({
                        title: t("Disable account \"{name}\"?", { name: selected.username }),
                        description: t("They can no longer sign in, and their share links stop working until the account is enabled again."),
                        confirmText: t("Disable account"),
                        destructive: true,
                      }))
                    )
                      return;
                    try {
                      await api.updateUser(selected.id, { disabled: !selected.disabled });
                      toast.success(selected.disabled ? t("Account enabled") : t("Account disabled"));
                      qc.invalidateQueries({ queryKey: keys.adminUsers() });
                    } catch (e) {
                      toast.error(errorMessage(e, t("Operation failed")));
                    }
                  }}
                >
                  {selected.disabled ? <UserCheckIcon /> : <UserXIcon />} {selected.disabled ? t("Enable account") : t("Disable account")}
                </DropdownMenuItem>
              )}
              {selected.id !== me.id && (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem variant="destructive" onClick={() => setDeleting(selected)}>
                    <Trash2Icon /> {t("Delete user")}
                  </DropdownMenuItem>
                </>
              )}
            </>
          ) : (
            <>
              <DropdownMenuItem onClick={() => setEditing("new")}>
                <UserPlusIcon /> {t("Add user")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => qc.invalidateQueries({ queryKey: keys.adminUsers() })}>
                <RefreshCwIcon /> {t("Refresh")}
              </DropdownMenuItem>
            </>
          )
        }
      />
      {editing && (
        <UserDialog
          user={editing === "new" ? null : editing}
          self={editing !== "new" && editing.id === me.id}
          onClose={() => setEditing(null)}
          onPersonal={(what, user) => {
            setEditing(null);
            setPersonalOf({ t: what, user });
          }}
        />
      )}
      {loginsOf && <LoginLogDialog title={t("Sign-in log for \"{name}\"", { name: loginsOf.username })} userId={loginsOf.id} onClose={() => setLoginsOf(null)} />}
      {devicesOf && <DevicesDialog user={devicesOf} onClose={() => setDevicesOf(null)} />}
      {resetting && (
        <ConfirmDialog
          title={t("Reset two-factor sign-in for \"{name}\"?", { name: resetting.username })}
          description={t("For someone who lost their phone and recovery codes. Their authenticator app and recovery codes stop working, and they sign in with just their password until they set it up again (right away, if two-factor sign-in is required).")}
          confirmText={t("Reset")}
          destructive
          onClose={() => setResetting(null)}
          onConfirm={async () => {
            await api.resetTwoFactor(resetting.id);
            toast.success(t("Two-factor sign-in reset"));
            setResetting(null);
            qc.invalidateQueries({ queryKey: keys.adminUsers() });
          }}
        />
      )}
      {personalOf?.t === "add" && <AddPersonalDialog user={personalOf.user} onClose={() => setPersonalOf(null)} />}
      {personalOf?.t === "remove" && <RemovePersonalDialog user={personalOf.user} onClose={() => setPersonalOf(null)} />}
      {deleting && (
        <DeleteUserDialog
          user={deleting}
          onClose={() => setDeleting(null)}
          onDeleted={() => {
            setDeleting(null);
            setSelectedId(null);
          }}
        />
      )}
    </Frame>
  );
}
