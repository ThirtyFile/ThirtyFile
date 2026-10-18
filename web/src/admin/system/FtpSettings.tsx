import { useState } from "react";
import { useQuery, useQueryClient, useMutation } from "@tanstack/react-query";
import { toast } from "sonner";
import { api } from "@/api";
import type { FtpStatus } from "@/api/ftp";
import { keys } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { ErrorState } from "@/components/ErrorState";
import { t, tServer } from "@/lib/i18n";

function Form({ value }: { value: FtpStatus }) {
  const qc = useQueryClient();
  const [config, setConfig] = useState(value);
  const save = useMutation({
    mutationFn: () => api.saveFtpConfig({ enabled: config.enabled, managers_publish: config.managers_publish, allow_private_plaintext: config.allow_private_plaintext }),
    onSuccess: (data) => {
      qc.setQueryData(keys.ftpConfig(), data);
      void qc.invalidateQueries({ queryKey: keys.receiveFolderLists() });
      void qc.invalidateQueries({ queryKey: keys.me() });
      toast.success(t("FTP settings updated"));
    },
    onError: (e) => toast.error(e.message),
  });
  return (
    <form
      className="grid gap-4 p-4"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <h3 className="font-medium">FTP / FTPS</h3>
      <p className="text-xs text-muted-foreground">{t("Let scanners and other devices upload to a receive folder with their own credentials. Public connections require explicit FTPS.")}</p>
      <Label className="flex gap-2">
        <input type="checkbox" className="size-4 accent-brand" checked={config.enabled} onChange={(e) => setConfig({ ...config, enabled: e.target.checked })} />
        {t("Enable FTP receive folders")}
      </Label>
      <Label className="flex gap-2">
        <input type="checkbox" className="size-4 accent-brand" checked={config.managers_publish} onChange={(e) => setConfig({ ...config, managers_publish: e.target.checked })} />
        {t("Space managers can publish receive folders")}
      </Label>
      <Label className="flex gap-2">
        <input
          type="checkbox"
          className="size-4 accent-brand"
          checked={config.allow_private_plaintext}
          onChange={(e) => setConfig({ ...config, allow_private_plaintext: e.target.checked })}
        />
        {t("Allow plaintext FTP from directly connected private networks")}
      </Label>
      <p className="text-xs text-muted-foreground">
        {t("Plaintext sends credentials and files without encryption. Use it only for trusted devices on a private network. Reverse proxy headers do not allow plaintext access.")}
      </p>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
        <dt>{t("Listen address")}</dt>
        <dd className="break-all font-mono">{value.address}</dd>
        <dt>{t("Passive ports")}</dt>
        <dd className="break-all font-mono">{value.passive_ports}</dd>
        <dt>{t("Passive host")}</dt>
        <dd className="break-all font-mono">{value.passive_host || t("Connection address")}</dd>
        <dt>{t("TLS certificate")}</dt>
        <dd>{value.tls_ready ? t("Ready") : t("Not configured")}</dd>
        <dt>{t("Listener")}</dt>
        <dd>{value.listening ? t("Running") : t("Not running")}</dd>
      </dl>
      <p className="text-xs text-muted-foreground">{t("Mount the TLS certificate and key and publish the control and passive ports in Docker. See the protocol guide for NAT settings.")}</p>
      {value.error && (
        <p role="alert" className="text-sm text-destructive">
          {tServer(value.error)}
        </p>
      )}
      <Button className="justify-self-start" type="submit" disabled={save.isPending}>
        {save.isPending ? t("Saving…") : t("Save")}
      </Button>
    </form>
  );
}
export function FtpSettings() {
  const q = useQuery({ queryKey: keys.ftpConfig(), queryFn: ({ signal }) => api.ftpConfig(signal), refetchInterval: 5000 });
  return (
    <div className="border-t">
      {q.error ? (
        <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
      ) : q.data ? (
        <Form key={JSON.stringify([q.data.enabled, q.data.managers_publish, q.data.allow_private_plaintext])} value={q.data} />
      ) : (
        <p className="p-4" role="status">
          {t("Loading…")}
        </p>
      )}
    </div>
  );
}
