import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { api } from "@/api";
import { Section, Toggle } from "@/admin/SettingsFrame";
import { Pending } from "@/components/ErrorState";
import { Skeleton } from "@/components/ui/skeleton";
import { t } from "@/lib/i18n";

export function SftpSettings() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["sftp-settings"], queryFn: api.sftpSettings, refetchInterval: 5000 });
  const save = useMutation({ mutationFn: api.updateSftpSettings, onSuccess: (data) => qc.setQueryData(["sftp-settings"], data), onError: (e) => toast.error(e.message) });
  return (
    <Section title={t("Apps and protocols")}>
      <div className="grid gap-3 p-4">
        <h3 className="text-sm font-medium">SFTP</h3>
        {!q.data ? (
          <Pending query={q} loading={<Skeleton className="h-20" />} />
        ) : (
          <>
            {q.data.error && (
              <p role="alert" className="text-xs text-destructive">
                {t("SFTP is unavailable: {reason}", { reason: q.data.error })}
              </p>
            )}
            <div className="flex items-center justify-between gap-3">
              <span>{t("Allow SFTP access")}</span>
              <Toggle
                label={t("Allow SFTP access")}
                checked={q.data.enabled}
                disabled={save.isPending}
                onChange={(enabled) => save.mutate({ enabled, keys_enabled: q.data!.keys_enabled })}
              />
            </div>
            <p className="text-xs text-muted-foreground">{t("Connect with an app password. SFTP uses its own port and does not provide a shell or run commands.")}</p>
            <div className="flex items-center justify-between gap-3">
              <span>{t("Allow SSH keys")}</span>
              <Toggle
                label={t("Allow SSH keys")}
                checked={q.data.keys_enabled}
                disabled={save.isPending}
                onChange={(keys_enabled) => save.mutate({ enabled: q.data!.enabled, keys_enabled })}
              />
            </div>
            <p className="text-xs text-muted-foreground">{t("People can sign in with SSH keys added under their account menu.")}</p>
            <p className="text-xs text-muted-foreground">{t("Listen address: {address}", { address: q.data.address || "—" })}</p>
            <p className="text-xs text-muted-foreground">{t("Publish this port in Docker and use your server hostname to connect.")}</p>
            <p className="text-xs">{t("Host key fingerprint")}</p>
            <p className="break-all font-mono text-xs">{q.data.fingerprint || "—"}</p>
            <p className="text-xs text-muted-foreground">{t("Check this fingerprint before trusting the server on your first connection.")}</p>
          </>
        )}
      </div>
    </Section>
  );
}
