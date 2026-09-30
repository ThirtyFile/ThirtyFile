import { useEffect, useMemo, useState } from "react";
import { type QueryClient, useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ClockIcon, FolderMinusIcon, SearchXIcon, FolderPlusIcon, HistoryIcon, Loader2Icon, MonitorSmartphoneIcon, ShieldCheckIcon, ShieldOffIcon, PencilIcon, RefreshCwIcon, Trash2Icon, TriangleAlertIcon, UserCheckIcon, UserPlusIcon, UsersIcon, UserXIcon } from "lucide-react";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { DataTable, EmptyState, type Column } from "@/components/DataTable";
import { toast } from "sonner";
import { api, type Drive, type UserRow } from "@/api";
import { affected, invalidate, keys, queries } from "@/api/queryKeys";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ConfirmDialog, ErrorText, errorProps } from "@/components/dialogs";
import { LocationSelect, useDefaultLocationId, useLocationName } from "@/components/LocationSelect";
import { confirm } from "@/lib/confirm";
import { Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { followJob } from "@/lib/jobs";
import { useMe } from "@/lib/session";
import { t, tc } from "@/lib/i18n";
import { formatBytes, formatDate, formatDateTime, errorMessage } from "@/lib/utils";
import { LoginLogDialog } from "@/components/logs/LoginLog";
import { DevicesDialog } from "@/components/DevicesDialog";
import { ProviderIcon, SSO_LABEL, type SsoProviderId } from "@/components/ProviderIcon";
import { NativeSelect } from "@/components/ui/native-select";

const GB = 1024 ** 3;
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
/**
 * What happens to the files in someone's "My files" when it goes (the user is deleted, or only their space): moved
 * into a folder in another space (the default), or deleted. Only its size is shown, never what is in it.
 */
function usePersonalFiles(user: UserRow) {
  const me = useMe();
  const drives = useQuery(queries.adminDrives);
  // Their own space ("My files": a folder on the server in new installs, whose folder is kept when it is removed)
  const own = drives.data?.find((d) => d.kind === "personal" && d.owner_name === user.username);
  // Spaces that can take the files: not the user's own, not turned off
  const targets = (drives.data ?? []).filter((d) => !d.disabled && d !== own);
  const mine = targets.find((d) => d.kind === "personal" && d.owner_name === me.username);
  const [choice, setChoice] = useState<"move" | "delete">("move");
  const [target, setTarget] = useState("");
  const moveTo = target || mine?.id || targets[0]?.id || "";
  return {
    user,
    own,
    targets,
    choice,
    setChoice,
    moveTo,
    setTarget,
    /** The query for the server */
    files: choice === "move" ? { move_to: moveTo } : { delete_files: true },
    /** A choice is complete (a space to move to was found) */
    ready: !user.personal_space || choice === "delete" || !!moveTo,
  };
}

function PersonalFilesChoice({ c }: { c: ReturnType<typeof usePersonalFiles> }) {
  const spaceLabel = (d: Drive) => (d.kind === "personal" ? t("My files of {name}", { name: d.owner_name }) : d.name);
  const { own, user } = c;
  return (
    <div className="grid gap-3" role="radiogroup" aria-label={t("Their files")}>
      <Label className="flex items-start gap-2 font-normal">
        <input type="radio" name="personal-files" className="mt-1 accent-brand" checked={c.choice === "move"} onChange={() => c.setChoice("move")} />
        <span className="grid min-w-0 flex-1 gap-1.5">
          <span>{t("Move their files to:")}</span>
          <NativeSelect
            aria-label={t("Move their files to:")}
            value={c.moveTo}
            disabled={c.choice !== "move"}
            onChange={(e) => c.setTarget(e.target.value)}
          >
            {c.targets.map((d) => (
              <option key={d.id} value={d.id}>
                {spaceLabel(d)}
              </option>
            ))}
          </NativeSelect>
          <span className="text-xs text-muted-foreground">
            {own?.mode === "folder"
              ? t("They go into a new folder named \"Files of {name}\" at the top of that space, and count toward its size. Their trash stays in their folder on the server.", { name: user.username })
              : t("They go into a new folder named \"Files of {name}\" at the top of that space, and count toward its size. Their trash is emptied.", { name: user.username })}
          </span>
        </span>
      </Label>
      <Label className="flex items-start gap-2 font-normal">
        <input type="radio" name="personal-files" className="mt-1 accent-brand" checked={c.choice === "delete"} onChange={() => c.setChoice("delete")} />
        {own?.mode === "folder" ? (
          <span className="min-w-0">
            {t("Remove their files from ThirtyFile")}
            <span className="block text-xs break-words text-muted-foreground">
              {t("Their folder on the server, {path}, is kept with the files in it: delete it there when it's no longer needed.", { path: own.source_path ?? "" })}
            </span>
          </span>
        ) : (
          <span>
            {t("Delete their files permanently")}
            <span className="block text-xs text-muted-foreground">{t("This can't be undone.")}</span>
          </span>
        )}
      </Label>
    </div>
  );
}

/**
 * After someone's "My files" is created or removed: the lists, and the administrator's own session and spaces (the
 * navigation pane), in case it was theirs
 */
function invalidatePersonal(qc: QueryClient) {
  void invalidate(qc, ...affected.personalSpace());
}

/** Creating someone's "My files" later, on a storage location */
function AddPersonalDialog({ user, onClose }: { user: UserRow; onClose(): void }) {
  const qc = useQueryClient();
  const system = useQuery(queries.system);
  const locationName = useLocationName();
  // Preset from the system setting (Control panel › General); "" = the default location
  const [location, setLocation] = useState<string | null>(null);
  const value = location ?? system.data?.personal_location ?? "";
  // "Default location" is sent as the location it names: left out, the system setting would apply instead
  const defaultLocation = useDefaultLocationId();
  const add = useMutation({
    mutationFn: () => api.addPersonalSpace(user.id, value || defaultLocation),
    onSuccess: () => {
      toast.success(t("\"My files\" created"));
      invalidatePersonal(qc);
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
            add.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("Create \"My files\" for \"{name}\"", { name: user.username })}</DialogTitle>
            <DialogDescription>{t("A private space that only they can see.")}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-1.5">
            <Label htmlFor="personal-add-location">{t("Storage location")}</Label>
            <LocationSelect id="personal-add-location" value={value} onChange={setLocation} blank="default" disabled={!system.data} />
            {user.personal_pending && (
              <p className="text-xs text-muted-foreground">
                {t("It's waiting for {location} to be available. Creating it now replaces the wait.", { location: locationName(user.personal_pending) })}
              </p>
            )}
          </div>
          <ErrorText>{add.error?.message}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={add.isPending || !system.data}>
              {add.isPending && <Loader2Icon className="animate-spin" />}
              {t("Create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Removing someone's "My files" (or stopping the wait for one): they keep their account */
function RemovePersonalDialog({ user, onClose }: { user: UserRow; onClose(): void }) {
  const qc = useQueryClient();
  const c = usePersonalFiles(user);
  const locationName = useLocationName();
  const remove = useMutation({
    mutationFn: () => api.removePersonalSpace(user.id, user.personal_space ? c.files : {}),
    // Moving the files to or from a folder on the server can take a while: the dialog closes, and a message follows it
    onSuccess: (job) => {
      onClose();
      void followJob(job, user.personal_space ? t("\"My files\" removed") : t("Stopped waiting to create \"My files\""), () => invalidatePersonal(qc));
    },
  });
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            remove.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("Remove \"My files\" of \"{name}\"?", { name: user.username })}</DialogTitle>
            <DialogDescription>
              {user.personal_space
                ? t("Their personal space ({size}) is removed. They keep their account and their access to other spaces, and start in the first space they can use.", { size: formatBytes(user.used_bytes) })
                : t("Their \"My files\" is still waiting for {location} to be available. It won't be created.", { location: locationName(user.personal_pending) })}
            </DialogDescription>
          </DialogHeader>
          {user.personal_space && <PersonalFilesChoice c={c} />}
          <ErrorText>{remove.error?.message}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" variant="destructive" disabled={remove.isPending || !c.ready}>
              {remove.isPending && <Loader2Icon className="animate-spin" />}
              {t("Remove \"My files\"")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Deleting a user: their personal space is moved into a folder in another space (the default) or deleted */
function DeleteUserDialog({ user, onClose, onDeleted }: { user: UserRow; onClose(): void; onDeleted(): void }) {
  const qc = useQueryClient();
  const c = usePersonalFiles(user);

  const remove = useMutation({
    mutationFn: () => api.deleteUser(user.id, user.personal_space ? c.files : {}),
    // Moving the files to or from a folder on the server can take a while: the dialog closes, and a message follows it
    onSuccess: (job) => {
      onDeleted();
      void followJob(job, t("User deleted"), () => {
        void invalidate(qc, ...affected.accountDeleted());
      });
    },
  });
  const disable = useMutation({
    mutationFn: () => api.updateUser(user.id, { disabled: true }),
    onSuccess: () => {
      toast.success(t("Account disabled"));
      qc.invalidateQueries({ queryKey: keys.adminUsers() });
      onClose();
    },
  });

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            remove.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("Delete user \"{name}\"?", { name: user.username })}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-2 text-sm text-muted-foreground">
            <p>
              {user.personal_space
                ? t("Their personal space \"My files\" ({size}) is removed. Files they added to other spaces, and team spaces they own, are transferred to you. Their share links are deleted.", {
                    size: formatBytes(user.used_bytes),
                  })
                : t("Files they added to spaces, and team spaces they own, are transferred to you. Their share links are deleted.")}
            </p>
            {!user.disabled && (
              <p>
                {t("To stop them signing in and keep everything as it is, disable the account instead.")}{" "}
                <Button type="button" variant="link" className="h-auto p-0" disabled={disable.isPending} onClick={() => disable.mutate()}>
                  {t("Disable account")}
                </Button>
              </p>
            )}
          </div>
          {user.personal_space && <PersonalFilesChoice c={c} />}
          <ErrorText>{remove.error?.message ?? disable.error?.message}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" variant="destructive" disabled={remove.isPending || !c.ready}>
              {remove.isPending && <Loader2Icon className="animate-spin" />}
              {t("Delete user")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** `onPersonal`: an existing user's "My files" is to be created or removed (in its own dialog) */
function UserDialog({ user, self, onClose, onPersonal }: { user: UserRow | null; self: boolean; onClose(): void; onPersonal(what: "add" | "remove", user: UserRow): void }) {
  const me = useMe();
  const qc = useQueryClient();
  const locationName = useLocationName();
  const [username, setUsername] = useState(user?.username ?? "");
  const [displayName, setDisplayName] = useState(user?.display_name ?? "");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState<"admin" | "user">(user?.role ?? "user");
  const [canWrite, setCanWrite] = useState(user?.can_write ?? true);
  const [canDelete, setCanDelete] = useState(user?.can_delete ?? true);
  const [canShare, setCanShare] = useState(user?.can_share ?? true);
  // When adding a user, prefill the default space size from system settings (Control panel › General)
  const system = useQuery({ ...queries.system, enabled: !user });
  const defaultQuota = !user ? system.data?.default_user_quota : undefined;
  const [quotaGb, setQuotaGb] = useState(user?.quota_bytes ? String(+(user.quota_bytes / GB).toFixed(2)) : "");
  const [quotaTouched, setQuotaTouched] = useState(false);
  useEffect(() => {
    if (defaultQuota !== undefined && !quotaTouched) setQuotaGb(defaultQuota ? String(+(defaultQuota / GB).toFixed(2)) : "");
  }, [defaultQuota, quotaTouched]);
  const [disabled, setDisabled] = useState(user?.disabled ?? false);
  // New users: "My files" and its location, preset from the system settings until changed here
  const [personal, setPersonal] = useState<boolean | null>(null);
  const [location, setLocation] = useState<string | null>(null);
  const withPersonal = personal ?? system.data?.personal_spaces ?? true;
  const personalLocation = location ?? system.data?.personal_location ?? "";
  // "Default location" is sent as the location it names: left out, the system setting would apply instead
  const defaultLocation = useDefaultLocationId();

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
      return api.createUser({ ...body, username, password, personal_space: withPersonal, personal_location: withPersonal ? personalLocation || defaultLocation : undefined });
    },
    onSuccess: (row) => {
      if (row.personal_pending) toast.warning(t("User created. Their \"My files\" is created once its storage location is available."));
      else toast.success(user ? t("User updated") : t("User created"));
      void invalidate(qc, ...affected.account());
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
              <Label htmlFor="u-pw">{user ? t("Reset password (leave blank to keep current)") : t("Password (at least {n} characters)", { n: me.min_password_length })}</Label>
              <Input
                id="u-pw"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                autoComplete="new-password"
                {...errorProps(save.error, "u-error")}
              />
            </div>
            <div className="grid gap-1.5">
              <Label id="u-role">{t("Role")}</Label>
              <div role="group" aria-labelledby="u-role" className="flex gap-1.5">
                <Button type="button" size="sm" variant={role === "user" ? "default" : "outline"} aria-pressed={role === "user"} disabled={self} onClick={() => setRole("user")}>
                  {t("Standard user")}
                </Button>
                <Button type="button" size="sm" variant={role === "admin" ? "default" : "outline"} aria-pressed={role === "admin"} onClick={() => setRole("admin")}>
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
            {!user && (
              <div className="grid gap-1.5">
                <Label className="flex items-center gap-2 font-normal">
                  <Checkbox checked={withPersonal} disabled={!system.data} onCheckedChange={(v) => setPersonal(!!v)} />
                  {t("Create \"My files\" (a private space only they can see)")}
                </Label>
                {withPersonal && (
                  <LocationSelect
                    aria-label={t("Storage location of \"My files\"")}
                    value={personalLocation}
                    onChange={setLocation}
                    blank="default"
                    disabled={!system.data}
                  />
                )}
              </div>
            )}
            {user && (
              <div className="grid gap-1.5">
                <div className="text-sm font-medium">{t("My files")}</div>
                <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
                  <span className="min-w-0 text-muted-foreground">
                    {user.personal_space
                      ? t("On {location} · {size} used", { location: locationName(user.personal_location), size: formatBytes(user.used_bytes) })
                      : user.personal_pending
                        ? t("My files pending (location unavailable)")
                        : t("No \"My files\"")}
                  </span>
                  {!user.personal_space && (
                    <Button type="button" variant="link" className="h-auto p-0" onClick={() => onPersonal("add", user)}>
                      {t("Create \"My files\"…")}
                    </Button>
                  )}
                  {(user.personal_space || user.personal_pending) && (
                    <Button type="button" variant="link" className="h-auto p-0 text-destructive" onClick={() => onPersonal("remove", user)}>
                      {t("Remove \"My files\"…")}
                    </Button>
                  )}
                </div>
              </div>
            )}
            {user && !self && (
              <Label className="flex items-center gap-2 font-normal">
                <Checkbox checked={disabled} onCheckedChange={(v) => setDisabled(!!v)} />
                {t("Disable this account (can't sign in, and share links stop working)")}
              </Label>
            )}
            <ErrorText id="u-error">{save.error?.message}</ErrorText>
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
