import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { api } from "@/api";
import { keys } from "@/api/queryKeys";
import type { SftpSettings as Settings } from "@/api/sftp";
import { Pending } from "@/components/ErrorState";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { t, tServer } from "@/lib/i18n";

/** Its options are saved together with Save, as S3's and FTP's are */
function Form({ value }: { value: Settings }) {
  const qc = useQueryClient();
  const [draft, setDraft] = useState({ enabled: value.enabled, keys_enabled: value.keys_enabled });
  const save = useMutation({
    mutationFn: () => api.updateSftpSettings(draft),
    onSuccess: (data) => {
      qc.setQueryData(keys.sftpSettings(), data);
      void qc.invalidateQueries({ queryKey: keys.me() });
      toast.success(t("SFTP settings updated"));
    },
    onError: (e) => toast.error(e.message),
  });
  return (
    <form
      className="grid gap-3 p-4"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <h3 className="text-sm font-medium">SFTP</h3>
      {value.error && (
        <p role="alert" className="text-xs text-destructive">
          {t("SFTP is unavailable: {reason}", { reason: tServer(value.error) })}
        </p>
      )}
      <Label className="flex gap-2">
        <input type="checkbox" className="size-4 accent-brand" checked={draft.enabled} onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })} />
        {t("Allow SFTP access")}
      </Label>
      <p className="text-xs text-muted-foreground">{t("Connect with an app password. SFTP uses its own port and does not provide a shell or run commands.")}</p>
      <Label className="flex gap-2">
        <input type="checkbox" className="size-4 accent-brand" checked={draft.keys_enabled} onChange={(e) => setDraft({ ...draft, keys_enabled: e.target.checked })} />
        {t("Allow SSH keys")}
      </Label>
      <p className="text-xs text-muted-foreground">{t("People can sign in with SSH keys added under their account menu.")}</p>
      <p className="text-xs text-muted-foreground">{t("Listen address: {address}", { address: value.address || "—" })}</p>
      <p className="text-xs text-muted-foreground">{t("Publish this port in Docker and use your server hostname to connect.")}</p>
      <p className="text-xs">{t("Host key fingerprint")}</p>
      <p className="break-all font-mono text-xs">{value.fingerprint || "—"}</p>
      <p className="text-xs text-muted-foreground">{t("Check this fingerprint before trusting the server on your first connection.")}</p>
      <Button className="justify-self-start" type="submit" disabled={save.isPending}>
        {save.isPending ? t("Saving…") : t("Save")}
      </Button>
    </form>
  );
}

export function SftpSettings() {
  const q = useQuery({ queryKey: keys.sftpSettings(), queryFn: api.sftpSettings, refetchInterval: 5000 });
  return q.data ? (
    <Form key={JSON.stringify([q.data.enabled, q.data.keys_enabled])} value={q.data} />
  ) : (
    <div className="p-4">
      <Pending query={q} loading={<Skeleton className="h-20" />} />
    </div>
  );
}
