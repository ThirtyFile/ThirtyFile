import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyIcon, Loader2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type MonitoringSettings } from "@/api";
import { keys } from "@/api/queryKeys";
import { Section, SettingsFrame, Toggle } from "@/admin/SettingsFrame";
import { useConfirmIdentity } from "@/components/ConfirmIdentity";
import { ErrorState } from "@/components/ErrorState";
import { ErrorText } from "@/components/dialogs";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { t } from "@/lib/i18n";
import { copyAndSay, formatDateTime } from "@/lib/utils";
import { useSubmit } from "@/lib/useSubmit";

/** Control panel › Monitoring: whether /metrics answers, its token, and the addresses it answers to */
export function MonitoringPage() {
  const q = useQuery({ queryKey: keys.monitoring(), queryFn: api.monitoring });
  return (
    <SettingsFrame item="monitoring" onRefresh={() => q.refetch()}>
      {q.error ? <ErrorState message={q.error.message} onRetry={() => q.refetch()} /> : !q.data ? <p role="status">{t("Loading…")}</p> : <Monitoring info={q.data} />}
    </SettingsFrame>
  );
}

const allowText = (list: string[]) => list.join("\n");
const allowList = (text: string) => text.split(/[\s,;]+/).filter(Boolean);

function Monitoring({ info }: { info: MonitoringSettings }) {
  const qc = useQueryClient();
  const [enabled, setEnabled] = useState(info.enabled);
  const [allow, setAllow] = useState(allowText(info.allow));
  /** The token just made: it is shown here, once, and nowhere else */
  const [token, setToken] = useState<string | null>(null);
  const identity = useConfirmIdentity("monitoring-me");
  const url = `${location.origin}${info.path}`;
  const dirty = enabled !== info.enabled || JSON.stringify(allowList(allow)) !== JSON.stringify(info.allow);
  const noToken = !info.has_token && !token;

  const send = async (newToken: boolean) => {
    const saved = await api.saveMonitoring({
      enabled,
      allow: allowList(allow),
      new_token: newToken,
      my_password: identity.values.password,
      my_code: identity.values.code,
    });
    const { token: made, ...rest } = saved;
    qc.setQueryData(keys.monitoring(), rest);
    identity.clear();
    if (made) setToken(made);
    setEnabled(rest.enabled);
    setAllow(allowText(rest.allow));
    toast.success(t("Saved"));
  };
  const saving = useSubmit(() => send(false), t("Couldn't make the change"));
  const making = useSubmit(() => send(true), t("Couldn't make the change"));
  const busy = saving.busy || making.busy;
  const error = saving.error ?? making.error;

  return (
    <>
      <p className="text-xs leading-relaxed text-muted-foreground">
        {t("Lets a monitoring system such as Prometheus read figures about the server: requests, sign-ins, storage, disks, tasks and backups. They hold no names of people or files.")}
      </p>

      <Section title={t("Metrics")}>
        <div className="grid gap-4 p-4">
          <div className="flex items-center justify-between gap-3 text-sm">
            <span className="grid gap-0.5">
              <span className="font-medium">{t("Answer requests for the metrics")}</span>
              <span className="text-xs text-muted-foreground">{t("Off, the address below doesn't exist. On, it answers only to the token.")}</span>
            </span>
            <Toggle label={t("Answer requests for the metrics")} checked={enabled} onChange={setEnabled} disabled={noToken && !enabled} />
          </div>
          <div className="grid gap-1.5">
            <Label>{t("Address")}</Label>
            <code className="w-fit rounded bg-muted px-2 py-1 text-xs select-text">{url}</code>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="monitoring-allow">{t("Only these addresses may ask")}</Label>
            <Textarea
              id="monitoring-allow"
              value={allow}
              rows={3}
              onChange={(e) => setAllow(e.target.value)}
              placeholder={t("One address or network per line, such as 203.0.113.7 or 10.1.0.0/16. Blank: any address, with the token.")}
              spellCheck={false}
            />
          </div>
        </div>
      </Section>

      <Section title={t("Token")}>
        <div className="grid gap-3 p-4 text-sm">
          {token ? (
            <div className="grid gap-2 rounded-lg border border-amber-500/40 bg-amber-500/10 p-3" role="status">
              <p className="text-xs">{t("This is the token. Copy it now: it is not shown again, and only a fingerprint of it is kept.")}</p>
              <div className="flex items-center gap-2">
                <code className="min-w-0 flex-1 rounded bg-background px-2 py-1 text-xs break-all select-text">{token}</code>
                <Button type="button" variant="outline" size="icon-sm" onClick={() => copyAndSay(token)} title={t("Copy")} aria-label={t("Copy")}>
                  <CopyIcon />
                </Button>
              </div>
            </div>
          ) : (
            <p className="text-xs text-muted-foreground">
              {info.has_token && info.token_created_at
                ? t("A token was made on {date}. It is not shown again.", { date: formatDateTime(info.token_created_at) })
                : t("There is no token yet.")}
            </p>
          )}
          <div className="grid gap-2">{identity.fields(t("For your security, this can only be done within 10 minutes of signing in."))}</div>
          <div className="flex flex-wrap gap-2">
            <Button type="button" variant="outline" disabled={busy || !identity.ready} onClick={() => void making.run()}>
              {busy && <Loader2Icon className="animate-spin" />}
              {info.has_token ? t("Make a new token") : t("Make a token")}
            </Button>
            {info.has_token && <span className="self-center text-xs text-muted-foreground">{t("The old token stops working at once.")}</span>}
          </div>
        </div>
      </Section>

      <ErrorText>{error}</ErrorText>
      <div>
        <Button type="button" disabled={busy || !dirty || !identity.ready || (enabled && noToken)} onClick={() => void saving.run()}>
          {busy && <Loader2Icon className="animate-spin" />}
          {t("Save")}
        </Button>
      </div>

      <Section title={t("In Prometheus")}>
        <div className="grid gap-2 p-4 text-xs leading-relaxed">
          <p className="text-muted-foreground">{t("Add a scrape job with the token as its credentials (use https where the site has it):")}</p>
          <pre className="overflow-x-auto rounded bg-muted p-3 select-text">{`scrape_configs:
  - job_name: thirtyfile
    scheme: ${location.protocol.replace(":", "")}
    authorization:
      credentials: <token>
    static_configs:
      - targets: ["${location.host}"]`}</pre>
          <p className="text-muted-foreground">{t("To look at it by hand:")}</p>
          <pre className="overflow-x-auto rounded bg-muted p-3 select-text">{`curl -H "Authorization: Bearer <token>" ${url}`}</pre>
        </div>
      </Section>
    </>
  );
}
