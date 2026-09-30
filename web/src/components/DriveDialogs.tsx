import { useState } from "react";
import { toast } from "sonner";
import { api, type Drive } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ActivityLog } from "@/components/logs/ActivityLog";
import { ErrorText, errorProps } from "@/components/dialogs";
import { LocationSelect } from "@/components/LocationSelect";
import { DRIVE_ICON, DRIVE_KIND_LABEL, ROLE_LABEL, atLeast } from "@/lib/drives";
import { useMe } from "@/lib/session";
import { formatBytes } from "@/lib/utils";
import { t, tc } from "@/lib/i18n";

const GB = 1024 ** 3;

/** Create a team space */
export function CreateDriveDialog({ onClose, onCreated }: { onClose(): void; onCreated(d: Drive): void }) {
  const me = useMe();
  const [name, setName] = useState("");
  const [quota, setQuota] = useState("");
  // Administrators choose the storage location ("" = the default location)
  const [location, setLocation] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <form
          className="grid gap-4"
          onSubmit={async (e) => {
            e.preventDefault();
            setBusy(true);
            setError(null);
            try {
              const d = await api.createDrive(name.trim(), quota ? Math.round(Number(quota) * GB) : 0, undefined, undefined, location || undefined);
              toast.success(t("Space created. You can now invite members"));
              onCreated(d);
              onClose();
            } catch (err) {
              setError(err instanceof Error ? err.message : t("Couldn't create"));
            } finally {
              setBusy(false);
            }
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("New team space")}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="drive-name">{t("Name")}</Label>
            <Input
              id="drive-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={tc("example", "Marketing")}
              autoFocus
              {...errorProps(error, "drive-error")}
            />
            {me.role === "admin" && (
              <>
                <Label htmlFor="drive-quota">{t("Quota (GB, leave blank for unlimited)")}</Label>
                <Input
                  id="drive-quota"
                  type="number"
                  min={0}
                  step="0.1"
                  value={quota}
                  onChange={(e) => setQuota(e.target.value)}
                  placeholder={t("Unlimited")}
                />
                <Label htmlFor="drive-location">{t("Storage location")}</Label>
                <LocationSelect id="drive-location" value={location} onChange={setLocation} blank="default" />
              </>
            )}
            <p className="text-xs text-muted-foreground">{t("You'll be the owner of this space. After creating it, you can invite users or groups.")}</p>
            <ErrorText id="drive-error">{error}</ErrorText>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={busy || !name.trim()}>
              {t("Create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Properties of a space, with its recent activity */
export function DrivePropsDialog({ drive, onClose }: { drive: Drive; onClose(): void }) {
  const me = useMe();
  const Icon = DRIVE_ICON[drive.kind];
  const canSeeActivity = atLeast(drive.role, "manager") || (me.role === "admin" && drive.kind !== "personal") || drive.kind === "personal";
  const rows: [string, string][] = [
    [t("Type"), DRIVE_KIND_LABEL[drive.kind]],
    [t("Owner"), drive.kind === "company" ? t("Company") : drive.owner_name],
    [t("Used"), formatBytes(drive.used_bytes)],
    [t("Quota"), drive.quota_bytes ? formatBytes(drive.quota_bytes) : t("Unlimited")],
    [t("Members"), drive.kind === "personal" ? t("Owner only") : t("{n} permission|{n} permissions", { n: drive.member_count })],
    [t("My role"), drive.role ? ROLE_LABEL[drive.role] : "—"],
  ];
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2 pr-8">
            <Icon className="size-5" /> {drive.name}
          </DialogTitle>
        </DialogHeader>
        <dl className="grid grid-cols-[auto_minmax(0,1fr)_auto_minmax(0,1fr)] gap-x-4 gap-y-2 text-[13px]">
          {rows.map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="text-muted-foreground">{k}</dt>
              <dd className="truncate">{v}</dd>
            </div>
          ))}
        </dl>
        {canSeeActivity && (
          <div className="grid gap-2">
            <div className="text-xs font-medium text-muted-foreground">{t("Recent activity")}</div>
            <ActivityLog driveId={drive.id} compact className="h-80 rounded-md border" />
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
