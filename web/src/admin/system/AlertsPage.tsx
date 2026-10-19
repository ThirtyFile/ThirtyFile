import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon } from "lucide-react";
import { Link } from "react-router";
import { toast } from "sonner";
import { api, type AlertSettings, type AlertsInfo } from "@/api";
import { keys } from "@/api/queryKeys";
import { Section, SettingsFrame } from "@/admin/SettingsFrame";
import { ErrorState } from "@/components/ErrorState";
import { ErrorText } from "@/components/dialogs";
import { SecretInput } from "@/components/SecretField";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { alertText } from "@/lib/alerts";
import { t, tServer } from "@/lib/i18n";
import { errorMessage, formatDateTime } from "@/lib/utils";
import { useSubmit } from "@/lib/useSubmit";

/** Control panel › Administrator alerts: what is sent to administrators by email and webhook, and what was */
export function AlertsPage() {
  const q = useQuery({ queryKey: keys.alerts(), queryFn: ({ signal }) => api.alerts(signal), refetchInterval: 15_000 });
  return (
    <SettingsFrame item="alerts" onRefresh={() => q.refetch()}>
      {q.error ? <ErrorState message={q.error.message} onRetry={() => q.refetch()} /> : !q.data ? <p role="status">{t("Loading…")}</p> : <Alerts info={q.data} />}
    </SettingsFrame>
  );
}

const WHICH: { key: keyof AlertSettings; label: () => string; hint: () => string }[] = [
  { key: "usage", label: () => t("A storage location over its usage threshold or failing"), hint: () => t("The thresholds are the ones of Control panel › Storage usage") },
  { key: "offline", label: () => t("A storage location that can't be reached"), hint: () => t("And when it can be reached again") },
  { key: "data_disk", label: () => t("The disk of the data folder nearly full"), hint: () => t("The database, thumbnails and uploads in progress are on it") },
  { key: "sign_ins", label: () => t("Failed sign-ins"), hint: () => t("From one address or for one account, passing the limit below") },
  { key: "backups", label: () => t("A backup or replica that fails or is overdue"), hint: () => t("Also told in the app; here by email and webhook too") },
  { key: "certificate", label: () => t("An FTPS certificate that ends within 14 days"), hint: () => t("The certificate the FTP listener serves") },
];

function Alerts({ info }: { info: AlertsInfo }) {
  const qc = useQueryClient();
  const [s, setS] = useState<AlertSettings>(info);
  const [secret, setSecret] = useState("");
  const [testing, setTesting] = useState<"email" | "webhook" | null>(null);
  const set = <K extends keyof AlertSettings>(key: K, value: AlertSettings[K]) => setS((cur) => ({ ...cur, [key]: value }));
  const secretProblem =
    secret && secret.length < 16
      ? t("The shared secret needs at least {n} characters", { n: 16 })
      : s.webhook_url.trim() && !secret && !info.has_secret
        ? t("Set a shared secret to sign the webhook's messages with")
        : null;
  const ready = !secretProblem && s.sign_in_failures >= 3 && s.sign_in_minutes >= 1;
  const { busy, error, run } = useSubmit(async () => {
    if (!ready) return;
    const saved = await api.saveAlerts({ ...s, webhook_url: s.webhook_url.trim(), secret: secret || undefined });
    qc.setQueryData(keys.alerts(), saved);
    setSecret("");
    toast.success(t("Saved"));
  }, t("Couldn't make the change"));
  const test = async (channel: "email" | "webhook") => {
    setTesting(channel);
    try {
      await api.testAlert(channel);
      toast.success(channel === "email" ? t("A test email was sent to you") : t("A test message was sent to the webhook"));
    } catch (e) {
      toast.error(errorMessage(e, t("Operation failed")));
    } finally {
      setTesting(null);
    }
  };
  // The test sends what is saved, not what is typed
  const saved = s.webhook_url.trim() === info.webhook_url && !secret && s.allow_private === info.allow_private;
  const number = (key: "sign_in_failures" | "sign_in_minutes" | "cooldown_minutes", min: number, max: number, label: string) => (
    <Input type="number" min={min} max={max} aria-label={label} className="w-20" value={s[key]} onChange={(e) => set(key, Number(e.target.value))} />
  );
  return (
    <>
      {info.active.length > 0 && (
        <Section title={t("Wrong now")}>
          <ul className="divide-y text-sm">
            {info.active.map((a, i) => (
              <li key={i} className="flex flex-wrap items-baseline gap-x-3 px-4 py-2">
                <span className="font-medium">{alertText(a.kind, a.params)}</span>
                <span className="text-xs text-muted-foreground">{t("Since {time}", { time: formatDateTime(a.since) })}</span>
                {!a.announced && <span className="text-xs text-muted-foreground">{t("Not told yet: waiting out the cooldown")}</span>}
              </li>
            ))}
          </ul>
        </Section>
      )}
      <form className="grid gap-8" onSubmit={run}>
        <Section title={t("Where alerts go")}>
          <div className="grid gap-3 p-4 text-sm">
            <label className="flex items-center gap-2">
              <input type="checkbox" className="size-4 accent-brand" checked={s.email} onChange={(e) => set("email", e.target.checked)} />
              {t("Email every administrator who has an address")}
            </label>
            {!info.email_ready && (
              <p className="text-xs text-muted-foreground">
                {t("No email server is set up, so no email is sent.")}{" "}
                <Link className="underline" to="/admin/email">
                  {t("Set it up")}
                </Link>
              </p>
            )}
            <div className="grid gap-2">
              <Label htmlFor="alerts-url">{t("Webhook address (optional)")}</Label>
              <Input id="alerts-url" type="url" placeholder="https://example.com/thirtyfile-alerts" value={s.webhook_url} onChange={(e) => set("webhook_url", e.target.value)} />
            </div>
            <div className="grid gap-2">
              <Label htmlFor="alerts-secret">{t("Shared secret")}</Label>
              <SecretInput
                id="alerts-secret"
                revealLabel={t("Show shared secret")}
                autoComplete="new-password"
                placeholder={info.has_secret ? t("Leave blank to keep the current secret") : undefined}
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                {t(
                  "Each message is a JSON body with the headers X-ThirtyFile-Timestamp and X-ThirtyFile-Signature: sha256= and the HMAC-SHA256 of the timestamp, a full stop and the body, with this secret. A message that fails is tried again later.",
                )}
              </p>
            </div>
            <label className="flex items-center gap-2">
              <input type="checkbox" className="size-4 accent-brand" checked={s.allow_private} onChange={(e) => set("allow_private", e.target.checked)} />
              {t("Allow a webhook on the private network or on this computer")}
            </label>
            <div className="flex flex-wrap gap-2">
              <Button type="button" variant="outline" size="sm" disabled={testing !== null || !info.email_ready} onClick={() => void test("email")}>
                {testing === "email" && <Loader2Icon className="animate-spin" />}
                {t("Send a test email to me")}
              </Button>
              <Button type="button" variant="outline" size="sm" disabled={testing !== null || !info.webhook_url || !saved} onClick={() => void test("webhook")}>
                {testing === "webhook" && <Loader2Icon className="animate-spin" />}
                {t("Send a test to the webhook")}
              </Button>
            </div>
            {!saved && <p className="text-xs text-muted-foreground">{t("Save first: the test sends what is saved.")}</p>}
          </div>
        </Section>
        <Section title={t("What is sent")}>
          <div className="grid gap-3 p-4 text-sm">
            {WHICH.map((w) => (
              <label key={w.key} className="grid gap-0.5">
                <span className="flex items-center gap-2">
                  <input type="checkbox" className="size-4 accent-brand" checked={s[w.key] as boolean} onChange={(e) => set(w.key, e.target.checked as never)} />
                  {w.label()}
                </span>
                <span className="pl-6 text-xs text-muted-foreground">{w.hint()}</span>
              </label>
            ))}
            <div className="grid gap-2 pl-6">
              <p className="text-xs text-muted-foreground">{t("Failed sign-ins are told when one address or one account fails at least this many times within this many minutes:")}</p>
              <div className="flex flex-wrap gap-4">
                <label className="grid gap-1">
                  {t("Failed sign-ins")}
                  {number("sign_in_failures", 3, 1000, t("Failed sign-ins"))}
                </label>
                <label className="grid gap-1">
                  {t("Minutes")}
                  {number("sign_in_minutes", 1, 1440, t("Minutes"))}
                </label>
              </div>
            </div>
            <label className="grid gap-1">
              {t("Minutes before a problem that comes back is told again")}
              {number("cooldown_minutes", 0, 1440, t("Minutes before a problem that comes back is told again"))}
            </label>
            <p className="text-xs text-muted-foreground">
              {t("Each problem is told once when it starts and once when it is over. Messages hold names of locations, addresses and accounts, never the name of a file or folder.")}
            </p>
          </div>
        </Section>
        <ErrorText>{secretProblem && secret ? secretProblem : error}</ErrorText>
        <div>
          <Button type="submit" disabled={busy || !ready}>
            {busy && <Loader2Icon className="animate-spin" />}
            {t("Save")}
          </Button>
        </div>
      </form>
      <Section title={t("Latest messages")}>
        {info.recent.length === 0 ? (
          <p className="p-4 text-sm text-muted-foreground">{t("None yet")}</p>
        ) : (
          <ul className="divide-y text-sm">
            {info.recent.map((m) => (
              <li key={m.id} className="grid gap-0.5 px-4 py-2">
                <span className="flex flex-wrap items-baseline gap-x-3">
                  <span className="font-medium">{m.state === "resolved" ? t("Over: {what}", { what: alertText(m.kind, m.params) }) : alertText(m.kind, m.params)}</span>
                  <span className="text-xs text-muted-foreground">{m.channel === "email" ? t("Email to {name}", { name: m.recipient ?? "—" }) : t("Webhook")}</span>
                  <span className="text-xs text-muted-foreground">{formatDateTime(m.created_at)}</span>
                </span>
                <span className={m.failed ? "text-xs text-destructive" : "text-xs text-muted-foreground"}>
                  {m.done_at
                    ? t("Sent")
                    : m.failed
                      ? t("Given up after {n} tries: {error}", { n: m.attempts, error: m.error ? tServer(m.error) : "—" })
                      : m.attempts > 0
                        ? t("Tried {n} times, to be tried again: {error}", { n: m.attempts, error: m.error ? tServer(m.error) : "—" })
                        : t("Waiting to be sent")}
                </span>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </>
  );
}
