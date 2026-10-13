import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { api } from "@/api";
import type { S3Config } from "@/api/s3";
import { Section } from "@/admin/SettingsFrame";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ErrorState } from "@/components/ErrorState";
import { t } from "@/lib/i18n";

function S3Form({ value }: { value: S3Config }) {
  const qc = useQueryClient();
  const [config, setConfig] = useState(value);
  const save = useMutation({ mutationFn: () => api.saveS3Config(config), onSuccess: (data) => { qc.setQueryData(["s3-config"], data); void qc.invalidateQueries({ queryKey: ["s3-buckets"] }); void qc.invalidateQueries({ queryKey: ["s3-options"] }); toast.success(t("S3 settings updated")); }, onError: (e) => toast.error(e.message) });
  return <form className="grid gap-4 p-4" onSubmit={(e) => { e.preventDefault(); save.mutate(); }}><h3 className="font-medium">S3</h3><p className="text-xs text-muted-foreground">{t("Publish team folders as buckets for backup software and other programs. Use a separate HTTPS hostname routed to this server's HTTP port.")}</p><Label className="flex gap-2"><input type="checkbox" checked={config.enabled} onChange={(e) => setConfig({ ...config, enabled: e.target.checked })} />{t("Enable S3 access")}</Label><Label>{t("S3 hostname")}<Input className="mt-2" value={config.hostname} onChange={(e) => setConfig({ ...config, hostname: e.target.value })} placeholder="s3.files.example.com" required={config.enabled} /></Label><Label>{t("Region")}<Input className="mt-2" value={config.region} onChange={(e) => setConfig({ ...config, region: e.target.value })} required /></Label><Label className="flex gap-2"><input type="checkbox" checked={config.managers_publish} onChange={(e) => setConfig({ ...config, managers_publish: e.target.checked })} />{t("Space managers can publish buckets")}</Label><p className="text-xs text-muted-foreground">{t("Personal spaces and spaces with share links turned off can't be published.")}</p><Button className="justify-self-start" type="submit" disabled={save.isPending}>{save.isPending ? t("Saving…") : t("Save")}</Button></form>;
}

export function S3Settings() {
  const q = useQuery({ queryKey: ["s3-config"], queryFn: ({ signal }) => api.s3Config(signal) });
  return <Section title={t("Apps and protocols")}>{q.error ? <ErrorState message={q.error.message} onRetry={() => q.refetch()} /> : q.data ? <S3Form key={JSON.stringify(q.data)} value={q.data} /> : <p className="p-4" role="status">{t("Loading…")}</p>}</Section>;
}
