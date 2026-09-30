import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type NotificationKind, type NotificationPrefs, type NotificationSettings } from "@/api";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ErrorText } from "@/components/dialogs";
import { useConfirmIdentity } from "@/components/ConfirmIdentity";
import { ADMIN_NOTIFICATION_KINDS, NOTIFICATION_KINDS } from "@/lib/notifications";
import { useMe } from "@/lib/session";
import { t } from "@/lib/i18n";

/** Account menu (or the bell) › Notification settings: the email address, and each kind on or off in the app and by email */
export function NotificationSettingsDialog({ onClose }: { onClose(): void }) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["notification-settings"], queryFn: api.notificationSettings });
  const admin = useMe().role === "admin";
  const [email, setEmail] = useState("");
  const [kinds, setKinds] = useState<NotificationSettings["kinds"] | null>(null);
  useEffect(() => {
    if (q.data && !kinds) {
      setEmail(q.data.email);
      setKinds(q.data.kinds);
    }
  }, [q.data, kinds]);

  // Reset links and every notice go to the address, so changing it asks for the password again
  const identity = useConfirmIdentity("notify");
  const emailChanged = !!q.data && email.trim() !== q.data.email;
  const save = useMutation({
    mutationFn: () => api.updateNotificationSettings({ email: email.trim(), kinds: kinds ?? undefined, ...(emailChanged ? identity.values : {}) }),
    onSuccess: (data) => {
      qc.setQueryData(["notification-settings"], data);
      toast.success(t("Notification settings saved"));
      onClose();
    },
  });
  const set = (kind: NotificationKind, change: Partial<NotificationPrefs>) => setKinds((k) => k && { ...k, [kind]: { ...k[kind], ...change } });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!save.isPending && (!emailChanged || identity.ready)) save.mutate();
  };
  const emailReady = !!q.data?.email_ready;

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <form onSubmit={submit} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{t("Notification settings")}</DialogTitle>
            <DialogDescription>{t("Choose what you're told about under the bell, and what is also sent to you by email.")}</DialogDescription>
          </DialogHeader>
          {!kinds ? (
            <Loader2Icon className="mx-auto size-5 animate-spin text-muted-foreground" />
          ) : (
            <>
              <div className="grid gap-1.5">
                <Label htmlFor="notify-email">{t("Email address")}</Label>
                <Input
                  id="notify-email"
                  type="email"
                  value={email}
                  autoComplete="email"
                  placeholder={t("name@example.com")}
                  onChange={(e) => setEmail(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  {emailReady
                    ? t("Emails are sent to this address. Leave it blank to get no emails.")
                    : t("Emails aren't sent yet: an administrator hasn't set up an email server. You can still enter your address now.")}
                </p>
                {emailChanged && (
                  <div className="mt-1 grid gap-1.5 rounded-lg border p-3">
                    <p className="text-xs text-muted-foreground">
                      {t("Password reset links go to this address too, so changing it asks who you are. The previous address is told about the change.")}
                    </p>
                    {identity.fields(t("For your security, your email address can only be changed within 10 minutes of signing in."))}
                  </div>
                )}
              </div>
              <div className="overflow-hidden rounded-lg border">
                <div className="grid grid-cols-[1fr_4rem_4rem] items-center gap-2 border-b bg-muted/40 px-3 py-1.5 text-xs text-muted-foreground">
                  <span>{t("Notify me when")}</span>
                  <span className="text-center">{t("In the app")}</span>
                  <span className="text-center">{t("By email")}</span>
                </div>
                {NOTIFICATION_KINDS.filter((k) => admin || !ADMIN_NOTIFICATION_KINDS.includes(k.kind)).map(({ kind, label, desc }) => (
                  <div key={kind} className="grid grid-cols-[1fr_4rem_4rem] items-center gap-2 border-b px-3 py-2.5 last:border-b-0">
                    <div className="min-w-0">
                      <div className="text-sm">{label}</div>
                      <div className="text-xs text-muted-foreground">{desc}</div>
                    </div>
                    <span className="flex justify-center">
                      <Checkbox
                        aria-label={t("{kind}: in the app", { kind: label })}
                        checked={kinds[kind].in_app}
                        onCheckedChange={(v) => set(kind, { in_app: !!v })}
                      />
                    </span>
                    <span className="flex justify-center">
                      <Checkbox
                        aria-label={t("{kind}: by email", { kind: label })}
                        checked={kinds[kind].email}
                        onCheckedChange={(v) => set(kind, { email: !!v })}
                      />
                    </span>
                  </div>
                ))}
              </div>
            </>
          )}
          <ErrorText>{save.error?.message ?? q.error?.message}</ErrorText>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("Cancel")}
            </Button>
            <Button type="submit" disabled={!kinds || save.isPending || (emailChanged && !identity.ready)}>
              {save.isPending && <Loader2Icon className="animate-spin" />}
              {t("Save")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
