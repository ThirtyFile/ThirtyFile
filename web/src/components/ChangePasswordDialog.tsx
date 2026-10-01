import { useId, useState } from "react";
import { Loader2Icon } from "lucide-react";
import { api } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useMe } from "@/lib/session";
import { t } from "@/lib/i18n";
import { useSubmit } from "@/lib/useSubmit";
import { ErrorText, errorProps } from "@/components/dialogs";

/** `required`: the password an administrator chose must be replaced; the dialog can't be closed without it */
export function ChangePasswordDialog({ onClose, required = false }: { onClose(): void; required?: boolean }) {
  const me = useMe();
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const { busy, error, run } = useSubmit(async () => {
    if (next !== confirm) throw new Error(t("The new passwords don't match"));
    await api.changePassword(current, next);
    onClose();
  });
  const errorId = useId();
  return (
    <Dialog open onOpenChange={(o) => !o && !required && onClose()}>
      <DialogContent showCloseButton={!required}>
        <form onSubmit={run} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{required ? t("Choose your own password") : t("Change password")}</DialogTitle>
            <DialogDescription>
              {required
                ? t("Your administrator chose your current password. Choose one only you know before you continue.")
                : t("After you change it, you'll be signed out on other devices.")}
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="pw-cur">{t("Current password")}</Label>
            <Input id="pw-cur" type="password" value={current} onChange={(e) => setCurrent(e.target.value)} autoComplete="current-password" />
            <Label htmlFor="pw-new">{t("New password (at least {n} characters)", { n: me.min_password_length })}</Label>
            <Input id="pw-new" type="password" value={next} onChange={(e) => setNext(e.target.value)} autoComplete="new-password" />
            <Label htmlFor="pw-cfm">{t("Confirm new password")}</Label>
            <Input id="pw-cfm" type="password" value={confirm} onChange={(e) => setConfirm(e.target.value)} autoComplete="new-password" {...errorProps(error, errorId)} />
            <ErrorText id={errorId}>{error}</ErrorText>
          </div>
          <DialogFooter>
            {required ? (
              <Button type="button" variant="outline" onClick={() => void api.logout().then(() => window.location.assign("/login"))}>
                {t("Sign out")}
              </Button>
            ) : (
              <Button type="button" variant="outline" onClick={onClose}>
                {t("Cancel")}
              </Button>
            )}
            <Button type="submit" disabled={busy || !current || !next}>
              {busy && <Loader2Icon className="animate-spin" />}
              {t("Change password")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
