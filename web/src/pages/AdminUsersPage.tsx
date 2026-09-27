import { useEffect, useMemo, useState } from "react";
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { HistoryIcon, Loader2Icon, PencilIcon, RefreshCwIcon, Trash2Icon, TriangleAlertIcon, UserCheckIcon, UserPlusIcon, UsersIcon, UserXIcon } from "lucide-react";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { DataTable, type Column } from "@/components/DataTable";
import { toast } from "sonner";
import { api, type UserRow } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ConfirmDialog, ErrorText } from "@/components/dialogs";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { useMe } from "@/lib/session";
import { useSettingsSearch } from "@/lib/controlPanel";
import { t, tc } from "@/lib/i18n";
import { formatBytes, formatDate, formatDateTime } from "@/lib/utils";
import { LoginLogDialog } from "@/components/logs/LoginLog";
import { ProviderIcon, SSO_LABEL, type SsoProviderId } from "@/components/ProviderIcon";

const GB = 1024 ** 3;
const USERS_PAGE = 200;

export function AdminUsersPage() {
  const me = useMe();
  // Loaded a page at a time: there is one account per person, so the list can be long
  const q = useInfiniteQuery({
    queryKey: ["admin-users", "pages"],
    queryFn: ({ pageParam }) => api.usersPage(pageParam, USERS_PAGE),
    initialPageParam: 0,
    getNextPageParam: (last) => (last.length < USERS_PAGE ? undefined : last[last.length - 1].id),
  });
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [editing, setEditing] = useState<UserRow | "new" | null>(null);
  const [deleting, setDeleting] = useState<UserRow | null>(null);
  const [loginsOf, setLoginsOf] = useState<UserRow | null>(null);
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
    </>
  );

  const searchSettings = useSettingsSearch();
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
      cell: (u) => (
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
      searchPlaceholder={t("Search settings")}
      onSearch={searchSettings}
      icon={UsersIcon}
      footer={
        <span className="flex items-center gap-2">
          {t("{n} user|{n} users", { n: users.length })}
          {q.hasNextPage && (
            <Button variant="link" size="sm" className="h-auto p-0" disabled={q.isFetchingNextPage} onClick={() => q.fetchNextPage()}>
              {t("Show more")}
            </Button>
          )}
        </span>
      }
    >
      <DataTable
        rows={users}
        rowKey={(u) => String(u.id)}
        columns={columns}
        loading={q.isLoading}
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
              {selected.id !== me.id && (
                <DropdownMenuItem
                  onClick={async () => {
                    try {
                      await api.updateUser(selected.id, { disabled: !selected.disabled });
                      toast.success(selected.disabled ? t("Account enabled") : t("Account disabled"));
                      qc.invalidateQueries({ queryKey: ["admin-users"] });
                    } catch (e) {
                      toast.error(e instanceof Error ? e.message : t("Operation failed"));
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
              <DropdownMenuItem onClick={() => qc.invalidateQueries({ queryKey: ["admin-users"] })}>
                <RefreshCwIcon /> {t("Refresh")}
              </DropdownMenuItem>
            </>
          )
        }
      />
      {editing && (
        <UserDialog user={editing === "new" ? null : editing} self={editing !== "new" && editing.id === me.id} onClose={() => setEditing(null)} />
      )}
      {loginsOf && <LoginLogDialog title={t("Sign-in log for \"{name}\"", { name: loginsOf.username })} userId={loginsOf.id} onClose={() => setLoginsOf(null)} />}
      {deleting && (
        <ConfirmDialog
          title={t("Delete user \"{name}\"?", { name: deleting.username })}
          description={t("This permanently deletes all of this user's files ({size}) and share links. This can't be undone.", { size: formatBytes(deleting.used_bytes) })}
          confirmText={t("Delete user")}
          destructive
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            await api.deleteUser(deleting.id);
            toast.success(t("User deleted"));
            setDeleting(null);
            setSelectedId(null);
            qc.invalidateQueries({ queryKey: ["admin-users"] });
          }}
        />
      )}
    </Frame>
  );
}
function UserDialog({ user, self, onClose }: { user: UserRow | null; self: boolean; onClose(): void }) {
  const qc = useQueryClient();
  const [username, setUsername] = useState(user?.username ?? "");
  const [displayName, setDisplayName] = useState(user?.display_name ?? "");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState<"admin" | "user">(user?.role ?? "user");
  const [canWrite, setCanWrite] = useState(user?.can_write ?? true);
  const [canDelete, setCanDelete] = useState(user?.can_delete ?? true);
  const [canShare, setCanShare] = useState(user?.can_share ?? true);
  // When adding a user, prefill the default space size from system settings (Control panel › General)
  const system = useQuery({ queryKey: ["system"], queryFn: api.systemSettings, enabled: !user });
  const defaultQuota = !user ? system.data?.default_user_quota : undefined;
  const [quotaGb, setQuotaGb] = useState(user?.quota_bytes ? String(+(user.quota_bytes / GB).toFixed(2)) : "");
  const [quotaTouched, setQuotaTouched] = useState(false);
  useEffect(() => {
    if (defaultQuota !== undefined && !quotaTouched) setQuotaGb(defaultQuota ? String(+(defaultQuota / GB).toFixed(2)) : "");
  }, [defaultQuota, quotaTouched]);
  const [disabled, setDisabled] = useState(user?.disabled ?? false);

  const save = useMutation({
    mutationFn: async () => {
      const body = {
        display_name: displayName,
        role,
        can_write: canWrite,
        can_delete: canDelete,
        can_share: canShare,
        quota_bytes: quotaGb ? Math.round(Number(quotaGb) * GB) : 0,
      };
      if (user) return api.updateUser(user.id, { ...body, disabled, password: password || undefined });
      return api.createUser({ ...body, username, password });
    },
    onSuccess: () => {
      toast.success(user ? t("User updated") : t("User created"));
      qc.invalidateQueries({ queryKey: ["admin-users"] });
      qc.invalidateQueries({ queryKey: ["me"] });
      onClose();
    },
  });

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle>{user ? t("Edit \"{name}\"", { name: user.username }) : t("Add user")}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-3">
            {!user && (
              <div className="grid gap-1.5">
                <Label htmlFor="u-name">{t("Username")}</Label>
                <Input id="u-name" value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="off" />
              </div>
            )}
            <div className="grid gap-1.5">
              <Label htmlFor="u-display">{t("Display name (optional)")}</Label>
              <Input id="u-display" value={displayName} onChange={(e) => setDisplayName(e.target.value)} autoComplete="off" maxLength={80} />
              {user?.source !== "password" && user && (
                <p className="text-xs text-muted-foreground">{t("Follows the name from {provider} on each sign-in unless you set a different one here", { provider: SSO_LABEL[user.source as SsoProviderId] ?? user.source })}</p>
              )}
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="u-pw">{user ? t("Reset password (leave blank to keep current)") : t("Password (at least 6 characters)")}</Label>
              <Input id="u-pw" type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" />
            </div>
            <div className="grid gap-1.5">
              <Label>{t("Role")}</Label>
              <div className="flex gap-1.5">
                <Button type="button" size="sm" variant={role === "user" ? "default" : "outline"} disabled={self} onClick={() => setRole("user")}>
                  {t("Standard user")}
                </Button>
                <Button type="button" size="sm" variant={role === "admin" ? "default" : "outline"} onClick={() => setRole("admin")}>
                  {t("Administrator")}
                </Button>
              </div>
            </div>
            <div className="grid gap-2">
              <Label>{t("Permissions")}</Label>
              <div className="flex flex-wrap gap-x-5 gap-y-2">
                <Perm label={t("Upload and edit")} admin={role === "admin"} checked={canWrite} onChange={setCanWrite} />
                <Perm label={t("Delete")} admin={role === "admin"} checked={canDelete} onChange={setCanDelete} />
                <Perm label={t("Create share link")} admin={role === "admin"} checked={canShare} onChange={setCanShare} />
              </div>
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="u-quota">{t("Personal space size (GB, leave blank for unlimited)")}</Label>
              <Input
                id="u-quota"
                type="number"
                min={0}
                step="0.1"
                value={quotaGb}
                onChange={(e) => {
                  setQuotaGb(e.target.value);
                  setQuotaTouched(true);
                }}
                placeholder={t("Unlimited")}
              />
            </div>
            {user && !self && (
              <Label className="flex items-center gap-2 font-normal">
                <Checkbox checked={disabled} onCheckedChange={(v) => setDisabled(!!v)} />
                {t("Disable this account (can't sign in, and share links stop working)")}
              </Label>
            )}
            <ErrorText>{save.error?.message}</ErrorText>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={save.isPending || (!user && (!username || !password))}>
              {save.isPending && <Loader2Icon className="animate-spin" />}
              {user ? t("Save") : t("Create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Account permission checkboxes; admins always have every permission */
function Perm({ label, checked, admin, onChange }: { label: string; checked: boolean; admin: boolean; onChange(v: boolean): void }) {
  return (
    <Label className="flex items-center gap-2 font-normal">
      <Checkbox checked={admin || checked} disabled={admin} onCheckedChange={(v) => onChange(!!v)} />
      {label}
    </Label>
  );
}
