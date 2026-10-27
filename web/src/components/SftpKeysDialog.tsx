import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { api } from "@/api";
import { keys } from "@/api/queryKeys";
import type { SftpKey } from "@/api/sftp";
import { useConfirmIdentity } from "@/components/ConfirmIdentity";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Pending } from "@/components/ErrorState";
import { Skeleton } from "@/components/ui/skeleton";
import { ConfirmDialog, ErrorText } from "@/components/dialogs";
import { formatDateTime } from "@/lib/utils";
import { t } from "@/lib/i18n";

export function SftpKeysDialog({ onClose }: { onClose(): void }) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: keys.sftpKeys(), queryFn: api.sftpKeys });
  const identity = useConfirmIdentity("sftp");
  const [name, setName] = useState("");
  const [publicKey, setPublicKey] = useState("");
  const [removing, setRemoving] = useState<SftpKey | null>(null);
  const add = useMutation({
    mutationFn: () => api.addSftpKey({ name: name.trim(), public_key: publicKey.trim(), ...identity.values }),
    onSuccess: () => {
      setName("");
      setPublicKey("");
      identity.clear();
      void qc.invalidateQueries({ queryKey: keys.sftpKeys() });
      toast.success(t("SSH key added"));
    },
  });
  const ready = !add.isPending && !!name.trim() && !!publicKey.trim() && identity.ready;
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (ready) add.mutate();
  };
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("SFTP keys")}</DialogTitle>
          <DialogDescription>
            {t("Add an SSH public key to access your files over SFTP. Keep the private key on your device. Administrators must enable SFTP and SSH keys first.")}
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-2 rounded border p-3">
          <Label htmlFor="sftp-name">{t("Name")}</Label>
          <Input id="sftp-name" maxLength={80} value={name} onChange={(e) => setName(e.target.value)} />
          <Label htmlFor="sftp-public">{t("SSH public key")}</Label>
          <Textarea id="sftp-public" maxLength={16384} value={publicKey} onChange={(e) => setPublicKey(e.target.value)} className="font-mono text-xs" placeholder="ssh-ed25519 AAAA…" />
          {identity.fields(t("Sign in again before adding an SSH key if your sign-in is older than 10 minutes."), add.error, "sftp-error")}
          <ErrorText id="sftp-error">{add.error?.message}</ErrorText>
          <Button type="submit" disabled={!ready}>
            {t("Add SSH key")}
          </Button>
        </form>
        {!q.data ? (
          <Pending query={q} loading={<Skeleton className="h-20" />} />
        ) : (
          <div className="max-h-64 space-y-2 overflow-auto">
            {q.data.length === 0 && <p className="text-sm text-muted-foreground">{t("No SSH keys yet")}</p>}
            {q.data.map((key) => (
              <div key={key.id} className="rounded border p-3">
                <div className="flex items-center justify-between gap-2">
                  <span className="truncate font-medium">{key.name}</span>
                  <Button size="sm" variant="outline" onClick={() => setRemoving(key)} aria-label={t("Remove SSH key {name}", { name: key.name })}>
                    {t("Remove")}
                  </Button>
                </div>
                <p className="break-all font-mono text-xs">{key.fingerprint}</p>
                <p className="text-xs text-muted-foreground">{key.last_used_at ? t("Last used {time}", { time: formatDateTime(key.last_used_at) }) : t("Never used")}</p>
              </div>
            ))}
          </div>
        )}
        {removing && (
          <ConfirmDialog
            title={t('Remove the SSH key "{name}"?', { name: removing.name })}
            description={t("Programs signing in with it lose access over SFTP. This can't be undone.")}
            confirmText={t("Remove")}
            destructive
            onClose={() => setRemoving(null)}
            onConfirm={async () => {
              await api.removeSftpKey(removing.id);
              await qc.invalidateQueries({ queryKey: keys.sftpKeys() });
              toast.success(t("SSH key removed"));
              setRemoving(null);
            }}
          />
        )}
      </DialogContent>
    </Dialog>
  );
}
