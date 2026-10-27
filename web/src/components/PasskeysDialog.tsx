import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckIcon, FingerprintIcon, Loader2Icon, PencilIcon, PlusIcon, Trash2Icon, XIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type Passkey } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ErrorText } from "@/components/dialogs";
import { useConfirmIdentity } from "@/components/ConfirmIdentity";
import { createPasskey, PasskeyCancelled, passkeysSupported } from "@/lib/passkeys";
import { formatDate, formatDateTime } from "@/lib/utils";
import { t } from "@/lib/i18n";

function describe(p: Passkey) {
  return [t("Created {date}", { date: formatDate(p.created_at) }), p.last_used_at ? t("Last used {time}", { time: formatDateTime(p.last_used_at) }) : t("Never used")].join(" · ");
}

/** Account menu › Passkeys: sign in with a fingerprint, a face, a screen lock or a security key instead of a password */
export function PasskeysDialog({ onClose }: { onClose(): void }) {
  const qc = useQueryClient();
  const q = useQuery(queries.passkeys);
  // Changing the list asks who it is again, as other things that outlast this browser session do
  const identity = useConfirmIdentity("pk");
  const [name, setName] = useState("");
  const [renaming, setRenaming] = useState<{ id: number; name: string } | null>(null);
  const supported = passkeysSupported();
  const data = q.data;
  const refresh = () => qc.invalidateQueries({ queryKey: keys.passkeys() });

  const add = useMutation({
    mutationFn: async () => {
      const options = await api.passkeyAddOptions(identity.values.password, identity.values.code);
      try {
        await api.addPasskey(name.trim(), await createPasskey(options));
      } catch (err) {
        if (err instanceof PasskeyCancelled) throw new Error(t("The passkey was not added. Try again."), { cause: err });
        if (err instanceof DOMException && err.name === "InvalidStateError") throw new Error(t("This device already has a passkey for this site."), { cause: err });
        throw err;
      }
    },
    onSuccess: () => {
      setName("");
      identity.clear();
      toast.success(t("Passkey added"));
      void refresh();
    },
  });
  const rename = useMutation({
    mutationFn: (r: { id: number; name: string }) => api.renamePasskey(r.id, r.name.trim()),
    onSuccess: () => {
      setRenaming(null);
      void refresh();
    },
    onError: (e) => toast.error(e.message),
  });
  const remove = useMutation({
    mutationFn: (p: Passkey) => api.removePasskey(p.id, identity.values.password, identity.values.code),
    onSuccess: () => {
      identity.clear();
      toast.success(t("Passkey removed"));
      void refresh();
    },
  });

  const full = !!data && data.passkeys.length >= data.max;
  const ready = !!name.trim() && identity.ready && !full;
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (ready && !add.isPending) add.mutate();
  };
  const error = add.error ?? remove.error;

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("Passkeys")}</DialogTitle>
          <DialogDescription>
            {t(
              "Sign in with your fingerprint, face, screen lock or a security key instead of a password. A passkey stays on your device, so there is nothing to type or steal. After a password, it also serves as the second step.",
            )}
          </DialogDescription>
        </DialogHeader>

        {!data ? (
          <Loader2Icon className="mx-auto size-5 animate-spin text-muted-foreground" />
        ) : !supported || !data.available ? (
          <div className="grid gap-2">
            <p className="text-sm text-muted-foreground">
              {!supported
                ? t("This browser can't use passkeys here. Passkeys need a secure (https) page.")
                : t("Passkeys need the site address to be an https address with a host name. Ask your administrator to set it in Control panel › General.")}
            </p>
            {/* Passkeys that can't be used can still be removed */}
            {!!data.passkeys.length && identity.fields(t("For your security, passkeys can only be added or removed within 10 minutes of signing in."), error, "pk-error")}
            <ErrorText id="pk-error">{error?.message}</ErrorText>
          </div>
        ) : (
          <form onSubmit={submit} className="grid gap-2 rounded-lg border p-3">
            <div className="grid gap-1.5">
              <Label htmlFor="pk-name">{t("Name")}</Label>
              <Input id="pk-name" value={name} maxLength={60} placeholder={t("For example: Work laptop")} onChange={(e) => setName(e.target.value)} autoComplete="off" />
            </div>
            {identity.fields(t("For your security, passkeys can only be added or removed within 10 minutes of signing in."), error, "pk-error")}
            <div className="flex justify-end">
              <Button type="submit" disabled={!ready || add.isPending}>
                {add.isPending ? <Loader2Icon className="animate-spin" /> : <PlusIcon />}
                {t("Add a passkey")}
              </Button>
            </div>
            <ErrorText id="pk-error">{error?.message}</ErrorText>
            {full && <p className="text-xs text-muted-foreground">{t("An account can have {n} passkeys at most.", { n: data.max })}</p>}
          </form>
        )}

        {data &&
          (!data.passkeys.length ? (
            <p className="text-sm text-muted-foreground">{t("You have no passkeys.")}</p>
          ) : (
            <ul className="max-h-[40vh] divide-y overflow-y-auto rounded-lg border" aria-label={t("Your passkeys")}>
              {data.passkeys.map((p) => (
                <li key={p.id} className="flex items-center gap-3 px-3 py-2.5">
                  <FingerprintIcon className="size-5 shrink-0 text-muted-foreground" />
                  {renaming?.id === p.id ? (
                    <form
                      className="flex min-w-0 flex-1 items-center gap-1.5"
                      onSubmit={(e) => {
                        e.preventDefault();
                        if (renaming.name.trim() && !rename.isPending) rename.mutate(renaming);
                      }}
                    >
                      <Input aria-label={t("Name")} value={renaming.name} maxLength={60} autoFocus onChange={(e) => setRenaming({ id: p.id, name: e.target.value })} />
                      <Button type="submit" size="icon-sm" variant="ghost" aria-label={t("Save")} title={t("Save")} disabled={!renaming.name.trim() || rename.isPending}>
                        <CheckIcon />
                      </Button>
                      <Button type="button" size="icon-sm" variant="ghost" aria-label={t("Cancel")} title={t("Cancel")} onClick={() => setRenaming(null)}>
                        <XIcon />
                      </Button>
                    </form>
                  ) : (
                    <>
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2 text-sm">
                          <span className="truncate">{p.name}</span>
                          {p.backup_eligible && (
                            <Badge variant="outline" className="h-4 px-1.5 text-[10px]" title={t("Kept in a phone's keychain or a password manager, so it is on more than one device")}>
                              {t("Synced")}
                            </Badge>
                          )}
                        </div>
                        <div className="truncate text-xs text-muted-foreground" title={describe(p)}>
                          {describe(p)}
                        </div>
                      </div>
                      <Button variant="ghost" size="icon-sm" aria-label={t("Rename")} title={t("Rename")} onClick={() => setRenaming({ id: p.id, name: p.name })}>
                        <PencilIcon />
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={remove.isPending || !identity.ready}
                        onClick={() => remove.mutate(p)}
                        title={identity.ready ? t("Signing in with this passkey stops working right away") : t("Enter your password above first")}
                      >
                        <Trash2Icon /> {t("Remove")}
                      </Button>
                    </>
                  )}
                </li>
              ))}
            </ul>
          ))}
      </DialogContent>
    </Dialog>
  );
}
