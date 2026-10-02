//! Adding an account, or changing one: its name, password, permissions and space size

import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type UserRow } from "@/api";
import { affected, invalidate, queries } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ErrorText, errorProps } from "@/components/dialogs";
import { LocationSelect, useDefaultLocationId, useLocationName } from "@/components/LocationSelect";
import { useMe } from "@/lib/session";
import { t } from "@/lib/i18n";
import { formatBytes } from "@/lib/utils";
import { SSO_LABEL, type SsoProviderId } from "@/components/ProviderIcon";
import { PERMISSION_LABEL, PERMISSIONS_HINT } from "@/admin/users/permissions";

const GB = 1024 ** 3;
/** `onPersonal`: an existing user's "My files" is to be created or removed (in its own dialog) */
export function UserDialog({ user, self, onClose, onPersonal }: { user: UserRow | null; self: boolean; onClose(): void; onPersonal(what: "add" | "remove", user: UserRow): void }) {
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
      if (row.personal_pending) toast.warning(t('User created. Their "My files" is created once its storage location is available.'));
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
            <DialogTitle>{user ? t('Edit "{name}"', { name: user.username }) : t("Add user")}</DialogTitle>
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
                <p className="text-xs text-muted-foreground">
                  {t("Follows the name from {provider} on each sign-in unless you set a different one here", { provider: SSO_LABEL[user.source as SsoProviderId] ?? user.source })}
                </p>
              )}
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="u-pw">{user ? t("Reset password (leave blank to keep current)") : t("Password (at least {n} characters)", { n: me.min_password_length })}</Label>
              <Input id="u-pw" type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" {...errorProps(save.error, "u-error")} />
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
                <Perm label={PERMISSION_LABEL.can_write} admin={role === "admin"} checked={canWrite} onChange={setCanWrite} />
                <Perm label={PERMISSION_LABEL.can_delete} admin={role === "admin"} checked={canDelete} onChange={setCanDelete} />
                <Perm label={PERMISSION_LABEL.can_share} admin={role === "admin"} checked={canShare} onChange={setCanShare} />
              </div>
              <p className="text-xs text-muted-foreground">{PERMISSIONS_HINT}</p>
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
                  {t('Create "My files" (a private space only they can see)')}
                </Label>
                {withPersonal && <LocationSelect aria-label={t('Storage location of "My files"')} value={personalLocation} onChange={setLocation} blank="default" disabled={!system.data} />}
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
                        : t('No "My files"')}
                  </span>
                  {!user.personal_space && (
                    <Button type="button" variant="link" className="h-auto p-0" onClick={() => onPersonal("add", user)}>
                      {t('Create "My files"…')}
                    </Button>
                  )}
                  {(user.personal_space || user.personal_pending) && (
                    <Button type="button" variant="link" className="h-auto p-0 text-destructive" onClick={() => onPersonal("remove", user)}>
                      {t('Remove "My files"…')}
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
