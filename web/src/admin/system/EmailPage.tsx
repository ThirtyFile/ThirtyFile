import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon, MailIcon, SendIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type EmailSettings, type EmailSettingsReq, type SmtpSecurity } from "@/api";
import { keys } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Pending } from "@/components/ErrorState";
import { ErrorText, errorProps } from "@/components/dialogs";
import { Section, SettingsFrame, Toggle } from "@/admin/SettingsFrame";
import { t } from "@/lib/i18n";
import { SMTP_PORT, portForSecurity } from "@/lib/notifications";
import { useMe } from "@/lib/session";
import { NativeSelect } from "@/components/ui/native-select";

type Form = EmailSettingsReq & { port: number };

function formOf(s: EmailSettings): Form {
  return { enabled: s.enabled, host: s.host, port: s.port || SMTP_PORT[s.security], security: s.security, username: s.username, password: "", from: s.from, insecure: s.insecure };
}

/** Control panel › Email: the email server that sends notification emails */
export function EmailPage() {
  const qc = useQueryClient();
  const me = useMe();
  const q = useQuery({ queryKey: keys.emailSettings(), queryFn: api.emailSettings });
  const [form, setForm] = useState<Form | null>(null);
  const [to, setTo] = useState("");
  useEffect(() => {
    if (q.data && !form) setForm(formOf(q.data));
  }, [q.data, form]);
  const set = (change: Partial<Form>) => setForm((f) => f && { ...f, ...change });

  const save = useMutation({
    mutationFn: (f: Form) => api.updateEmailSettings(f),
    onSuccess: (data) => {
      qc.setQueryData(keys.emailSettings(), data);
      qc.invalidateQueries({ queryKey: keys.notificationSettings() });
      setForm(formOf(data));
      toast.success(t("Email settings saved"));
    },
  });
  const test = useMutation({
    mutationFn: (f: Form) => api.testEmail({ ...f, to: to.trim() }),
    onSuccess: () => toast.success(t("Test email sent to {address}. Check the inbox (and the spam folder).", { address: to.trim() })),
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (form && !save.isPending) save.mutate(form);
  };

  return (
    <SettingsFrame
      item="email"
      onRefresh={() => {
        setForm(null);
        q.refetch();
      }}
    >
      {!form || !q.data ? (
        <Pending query={q} loading={<Skeleton className="h-40" />} />
      ) : (
        <form onSubmit={submit} className="grid gap-8">
          <Section title={t("Notification emails")}>
            <div className="flex items-start gap-4 p-4">
              <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-cyan-500/10 text-cyan-700 dark:text-cyan-300">
                <MailIcon className="size-4" />
              </span>
              <div className="min-w-0 flex-1">
                <div className="font-medium">{t("Send notifications by email")}</div>
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                  {t(
                    "People are always told under the bell in the app. With an email server, those who entered an email address (or signed in with single sign-on) also get an email, unless they turned it off in their notification settings. Links in emails use the site URL under General.",
                  )}
                </p>
              </div>
              <Toggle label={t("Send notifications by email")} checked={form.enabled} onChange={(enabled) => set({ enabled })} />
            </div>
          </Section>
          <Section title={t("Email server (SMTP)")}>
            <div className="grid gap-4 p-4">
              <div className="grid gap-3 sm:grid-cols-[1fr_7rem]">
                <div className="grid gap-1.5">
                  <Label htmlFor="smtp-host">{t("Server")}</Label>
                  <Input id="smtp-host" value={form.host} placeholder="smtp.example.com" autoComplete="off" onChange={(e) => set({ host: e.target.value })} />
                </div>
                <div className="grid gap-1.5">
                  <Label htmlFor="smtp-port">{t("Port")}</Label>
                  <Input id="smtp-port" inputMode="numeric" value={form.port || ""} onChange={(e) => set({ port: Math.min(65535, Number(e.target.value.replace(/\D/g, "")) || 0) })} />
                </div>
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="smtp-security">{t("Encryption")}</Label>
                <NativeSelect
                  id="smtp-security"
                  size="lg"
                  value={form.security}
                  onChange={(e) => {
                    const security = e.target.value as SmtpSecurity;
                    set({ security, port: portForSecurity(form.port, security) });
                  }}
                >
                  <option value="starttls">{t("STARTTLS (usually port 587)")}</option>
                  <option value="tls">{t("TLS (usually port 465)")}</option>
                  <option value="none">{t("None (only for a server on your own network)")}</option>
                </NativeSelect>
              </div>
              {form.security !== "none" && (
                <label className="flex items-start gap-2 text-sm">
                  <Checkbox className="mt-0.5" checked={form.insecure} onCheckedChange={(v) => set({ insecure: !!v })} />
                  <span>
                    {t("Accept a self-signed certificate")}
                    <span className="block text-xs text-muted-foreground">{t("Only for a server on your own network whose certificate isn't issued by a known authority.")}</span>
                  </span>
                </label>
              )}
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="grid gap-1.5">
                  <Label htmlFor="smtp-user">{t("Username")}</Label>
                  <Input id="smtp-user" value={form.username} autoComplete="off" placeholder={t("Blank if the server doesn't ask")} onChange={(e) => set({ username: e.target.value })} />
                </div>
                <div className="grid gap-1.5">
                  <Label htmlFor="smtp-password">{t("Password")}</Label>
                  <Input
                    id="smtp-password"
                    type="password"
                    value={form.password}
                    autoComplete="new-password"
                    disabled={!form.username.trim()}
                    placeholder={q.data.has_password ? t("Saved (leave blank to keep it)") : ""}
                    onChange={(e) => set({ password: e.target.value })}
                  />
                </div>
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="smtp-from">{t("Sender address")}</Label>
                <Input id="smtp-from" type="email" value={form.from} placeholder="drive@example.com" autoComplete="off" onChange={(e) => set({ from: e.target.value })} />
                <p className="text-xs text-muted-foreground">{t("Emails come from this address, with the site name as the sender's name. The server must allow sending from it.")}</p>
              </div>
              <ErrorText>{save.error?.message}</ErrorText>
              <div className="flex justify-end">
                <Button type="submit" disabled={save.isPending}>
                  {save.isPending && <Loader2Icon className="animate-spin" />}
                  {t("Save")}
                </Button>
              </div>
            </div>
          </Section>
          <Section title={t("Test")}>
            <div className="grid gap-2 p-4">
              <p className="text-xs text-muted-foreground">{t("Sends an email with the settings above, saved or not, and shows what went wrong if it can't.")}</p>
              <div className="flex flex-wrap gap-2">
                <Input
                  type="email"
                  aria-label={t("Send the test to")}
                  className="h-8 min-w-60 flex-1"
                  value={to}
                  placeholder={t("name@example.com")}
                  onChange={(e) => setTo(e.target.value)}
                  {...errorProps(test.error, "smtp-test-error")}
                />
                <Button type="button" size="sm" variant="outline" disabled={!to.trim() || !form.host.trim() || test.isPending} onClick={() => test.mutate(form)}>
                  {test.isPending ? <Loader2Icon className="animate-spin" /> : <SendIcon />}
                  {t("Send test email")}
                </Button>
              </div>
              <ErrorText id="smtp-test-error">{test.error?.message}</ErrorText>
              {!me.public_url && (
                <p className="text-xs text-muted-foreground">
                  {t("Set the site URL under General, so emails can link to what they are about. Until then, the sign-in page doesn't offer to reset a forgotten password by email.")}
                </p>
              )}
            </div>
          </Section>
        </form>
      )}
    </SettingsFrame>
  );
}
