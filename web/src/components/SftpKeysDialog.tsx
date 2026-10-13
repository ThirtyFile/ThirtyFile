import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { api } from "@/api";
import { queries } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Pending } from "@/components/ErrorState";
import { Skeleton } from "@/components/ui/skeleton";
import { ErrorText } from "@/components/dialogs";
import { formatDateTime } from "@/lib/utils";
import { t } from "@/lib/i18n";

export function SftpKeysDialog({ onClose }: { onClose(): void }) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["sftp-keys"], queryFn: api.sftpKeys });
  const security = useQuery(queries.twoFactor);
  const [name, setName] = useState("");
  const [publicKey, setPublicKey] = useState("");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const add = useMutation({
    mutationFn: () => api.addSftpKey({ name: name.trim(), public_key: publicKey.trim(), password: password || undefined, code: code || undefined }),
    onSuccess: () => {
      setName("");
      setPublicKey("");
      setPassword("");
      setCode("");
      qc.invalidateQueries({ queryKey: ["sftp-keys"] });
      toast.success(t("SSH key added"));
    },
  });
  const remove = useMutation({
    mutationFn: api.removeSftpKey,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["sftp-keys"] });
      toast.success(t("SSH key removed"));
    },
    onError: (e) => toast.error(e.message),
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!add.isPending && name.trim() && publicKey.trim()) add.mutate();
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
        {!security.data && <Pending query={security} loading={<Skeleton className="h-8" />} />}
        <form onSubmit={submit} className="grid gap-2 rounded border p-3">
          <Label htmlFor="sftp-name">{t("Name")}</Label>
          <Input id="sftp-name" maxLength={80} value={name} onChange={(e) => setName(e.target.value)} />
          <Label htmlFor="sftp-public">{t("SSH public key")}</Label>
          <Textarea id="sftp-public" maxLength={16384} value={publicKey} onChange={(e) => setPublicKey(e.target.value)} className="font-mono text-xs" placeholder="ssh-ed25519 AAAA…" />
          {security.data?.has_password !== false && (
            <>
              <Label htmlFor="sftp-password">{t("Your current password")}</Label>
              <Input id="sftp-password" type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" />
            </>
          )}
          {security.data?.enabled && (
            <>
              <Label htmlFor="sftp-code">{t("Code from your app")}</Label>
              <Input id="sftp-code" value={code} onChange={(e) => setCode(e.target.value)} autoComplete="one-time-code" inputMode="numeric" />
            </>
          )}
          <ErrorText>{add.error?.message}</ErrorText>
          <Button type="submit" disabled={add.isPending || !security.data || !name.trim() || !publicKey.trim()}>
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
                  <Button size="sm" variant="outline" disabled={remove.isPending} onClick={() => remove.mutate(key.id)} aria-label={t("Remove SSH key {name}", { name: key.name })}>
                    {t("Remove")}
                  </Button>
                </div>
                <p className="break-all font-mono text-xs">{key.fingerprint}</p>
                <p className="text-xs text-muted-foreground">{key.last_used_at ? t("Last used {time}", { time: formatDateTime(key.last_used_at) }) : t("Never used")}</p>
              </div>
            ))}
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
