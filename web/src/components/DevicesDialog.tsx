import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon, LogOutIcon, MonitorIcon, SmartphoneIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type Device } from "@/api";
import { keys } from "@/api/queryKeys";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { describeAgent } from "@/components/logs/ShareAccessLog";
import { SSO_LABEL, type SsoProviderId } from "@/components/ProviderIcon";
import { formatDateTime } from "@/lib/utils";
import { t } from "@/lib/i18n";

/**
 * Signed-in devices: account menu › Devices (my own), or Control panel › Users › Devices (with `user`, for administrators).
 * Each device can be signed out on its own, or every other one at once.
 */
export function DevicesDialog({ user, onClose }: { user?: { id: number; username: string }; onClose(): void }) {
  const qc = useQueryClient();
  const key = keys.devices(user?.id ?? "me");
  const q = useQuery({ queryKey: key, queryFn: () => (user ? api.userDevices(user.id) : api.devices()) });
  const devices = q.data ?? [];
  /** An administrator looking at someone else's devices signs out all of them */
  const everywhere = !devices.some((d) => d.current);
  const done = (message: string) => {
    qc.invalidateQueries({ queryKey: key });
    toast.success(message);
  };
  const signOut = useMutation({
    mutationFn: (d: Device) => (user ? api.signOutUserDevice(user.id, d.id) : api.signOutDevice(d.id)),
    onSuccess: () => done(t("Device signed out")),
    onError: (e) => toast.error(e.message),
  });
  const signOutAll = useMutation({
    mutationFn: () => (user ? api.signOutUserDevices(user.id) : api.signOutOtherDevices()),
    onSuccess: () => done(everywhere ? t("Signed out on all devices") : t("Signed out on all other devices")),
    onError: (e) => toast.error(e.message),
  });
  const others = devices.filter((d) => !d.current).length;
  const busy = signOut.isPending || signOutAll.isPending;

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{user ? t('Devices of "{name}"', { name: user.username }) : t("Devices")}</DialogTitle>
          <DialogDescription>
            {user
              ? t("Browsers where this user is signed in. Signing a device out ends its session right away; app passwords keep working.")
              : t("Browsers where you're signed in. If you don't recognize one, sign it out here. If you sign in with a password, change it too.")}
          </DialogDescription>
        </DialogHeader>
        {q.isLoading ? (
          <Loader2Icon className="mx-auto size-5 animate-spin text-muted-foreground" />
        ) : devices.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("Not signed in on any device.")}</p>
        ) : (
          <div className="max-h-[50vh] divide-y overflow-y-auto rounded-lg border">
            {devices.map((d) => {
              const Icon = /iPhone|iPad|Android|Mobile/.test(d.user_agent) ? SmartphoneIcon : MonitorIcon;
              const via = SSO_LABEL[d.method as SsoProviderId];
              return (
                <div key={d.id} className="flex items-center gap-3 px-3 py-2.5">
                  <Icon className="size-5 shrink-0 text-muted-foreground" />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2 text-sm">
                      <span className="truncate" title={d.user_agent}>
                        {describeAgent(d.user_agent)}
                      </span>
                      {d.current && <Badge className="h-4 px-1.5 text-[10px]">{t("This device")}</Badge>}
                    </div>
                    <div className="truncate text-xs text-muted-foreground">
                      {[
                        d.ip,
                        via ? t("Signed in with {provider} {time}", { provider: via, time: formatDateTime(d.created_at) }) : t("Signed in {time}", { time: formatDateTime(d.created_at) }),
                        d.last_used_at && t("Last used {time}", { time: formatDateTime(d.last_used_at) }),
                      ]
                        .filter(Boolean)
                        .join(" · ")}
                    </div>
                  </div>
                  {!d.current && (
                    <Button variant="ghost" size="sm" disabled={busy} onClick={() => signOut.mutate(d)}>
                      <LogOutIcon /> {t("Sign out")}
                    </Button>
                  )}
                </div>
              );
            })}
          </div>
        )}
        {others > 0 && (
          <div className="flex justify-end">
            <Button variant="outline" disabled={busy} onClick={() => signOutAll.mutate()}>
              {signOutAll.isPending ? <Loader2Icon className="animate-spin" /> : <LogOutIcon />}
              {everywhere ? t("Sign out everywhere") : t("Sign out everywhere else")}
            </Button>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
