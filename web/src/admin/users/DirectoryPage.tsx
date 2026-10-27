import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2Icon, Loader2Icon, PlusIcon, XCircleIcon, XIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type DirectoryGroupMode, type DirectoryInfo, type DirectorySettings, type DirectoryStep } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { Section, SettingsFrame, Toggle } from "@/admin/SettingsFrame";
import { useConfirmIdentity } from "@/components/ConfirmIdentity";
import { ErrorState } from "@/components/ErrorState";
import { ErrorText } from "@/components/dialogs";
import { SecretInput } from "@/components/SecretField";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { Textarea } from "@/components/ui/textarea";
import { t } from "@/lib/i18n";
import { errorMessage, formatDateTime } from "@/lib/utils";
import { useSubmit } from "@/lib/useSubmit";

/** Settings that suit each kind of directory, to start from */
const PRESETS: Record<string, Partial<DirectorySettings>> = {
  ad: {
    user_filter: "(&(objectClass=user)(objectCategory=person)(|(sAMAccountName={username})(userPrincipalName={username})))",
    attr_id: "objectGUID",
    attr_username: "sAMAccountName",
    attr_name: "displayName",
    attr_email: "mail",
    group_mode: "member_of",
  },
  openldap: {
    user_filter: "(&(objectClass=inetOrgPerson)(uid={username}))",
    attr_id: "entryUUID",
    attr_username: "uid",
    attr_name: "cn",
    attr_email: "mail",
    group_mode: "search",
    group_filter: "(&(objectClass=groupOfNames)(member={dn}))",
  },
  freeipa: {
    user_filter: "(&(objectClass=posixAccount)(uid={username}))",
    attr_id: "ipaUniqueID",
    attr_username: "uid",
    attr_name: "cn",
    attr_email: "mail",
    group_mode: "member_of",
  },
};

/** Control panel › Directory sign-in */
export function DirectoryPage() {
  const q = useQuery({ queryKey: keys.directorySignIn(), queryFn: ({ signal }) => api.directorySettings(signal) });
  return (
    <SettingsFrame item="directory" onRefresh={() => q.refetch()}>
      {q.error ? <ErrorState message={q.error.message} onRetry={() => q.refetch()} /> : !q.data ? <p role="status">{t("Loading…")}</p> : <Directory info={q.data} />}
    </SettingsFrame>
  );
}

function Field({ id, label, hint, children }: { id: string; label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="grid gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      {children}
      {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
    </div>
  );
}

function Directory({ info }: { info: DirectoryInfo }) {
  const qc = useQueryClient();
  const groups = useQuery(queries.groups);
  const identity = useConfirmIdentity("directory-me");
  const [s, setS] = useState<DirectorySettings>(info.settings);
  const [username, setUsername] = useState("");
  const [steps, setSteps] = useState<DirectoryStep[] | null>(null);
  const [testing, setTesting] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const set = <K extends keyof DirectorySettings>(key: K, value: DirectorySettings[K]) => setS((cur) => ({ ...cur, [key]: value }));
  const plain = s.url.trim().toLowerCase().startsWith("ldap://");
  const dirty = JSON.stringify({ ...s, bind_password: "" }) !== JSON.stringify({ ...info.settings, bind_password: "" }) || s.bind_password !== "";

  const { busy, error, run } = useSubmit(async () => {
    const saved = await api.saveDirectory({ ...s, my_password: identity.values.password, my_code: identity.values.code });
    qc.setQueryData(keys.directorySignIn(), saved);
    setS(saved.settings);
    identity.clear();
    toast.success(t("Saved"));
  }, t("Couldn't make the change"));

  const test = async () => {
    setTesting(true);
    setSteps(null);
    try {
      setSteps((await api.testDirectory(s, username.trim())).steps);
    } catch (e) {
      setSteps([{ step: t("Test"), ok: false, detail: errorMessage(e, t("Couldn't make the change")) }]);
    } finally {
      setTesting(false);
    }
  };

  const sync = async () => {
    setSyncing(true);
    try {
      const report = await api.syncDirectory();
      toast.success(t("{n} person checked|{n} people checked", { n: report.checked }));
      void qc.invalidateQueries({ queryKey: keys.directorySignIn() });
      void qc.invalidateQueries({ queryKey: keys.groups() });
      void qc.invalidateQueries({ queryKey: keys.adminUsers() });
    } catch (e) {
      toast.error(errorMessage(e, t("Couldn't make the change")));
    } finally {
      setSyncing(false);
    }
  };

  const setMapping = (i: number, change: Partial<DirectorySettings["mappings"][number]>) =>
    set(
      "mappings",
      s.mappings.map((m, j) => (j === i ? { ...m, ...change } : m)),
    );
  const report = info.last_sync;

  return (
    <form className="grid max-w-3xl gap-6" onSubmit={run} aria-label={t("Directory sign-in")}>
      <Section title={t("Directory sign-in")}>
        <div className="grid gap-3 p-4">
          <div className="flex items-center justify-between gap-3">
            <div>
              <div className="text-sm font-medium">{t("Sign in with the directory's password")}</div>
              <p className="text-xs text-muted-foreground">
                {t(
                  "A user name that has no account made here is looked up in the directory, and the password is checked by the directory itself: ThirtyFile never keeps it. Accounts made by hand keep their own passwords. Two-factor sign-in, if required, applies on top.",
                )}
              </p>
            </div>
            <Toggle checked={s.enabled} onChange={(v) => set("enabled", v)} label={t("Sign in with the directory's password")} />
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Label htmlFor="dir-preset" className="text-xs text-muted-foreground">
              {t("Start from")}
            </Label>
            <NativeSelect
              id="dir-preset"
              size="sm"
              value=""
              onChange={(e) => {
                const preset = PRESETS[e.target.value];
                if (preset) setS((cur) => ({ ...cur, ...preset }));
              }}
            >
              <option value="">{t("Choose a kind of directory…")}</option>
              <option value="ad">{t("Active Directory")}</option>
              <option value="openldap">OpenLDAP</option>
              <option value="freeipa">FreeIPA</option>
            </NativeSelect>
          </div>
        </div>
      </Section>

      <Section title={t("Server")}>
        <div className="grid gap-3 p-4">
          <Field id="dir-url" label={t("Address")} hint={t("ldaps://dc.example.com, or ldap://dc.example.com with StartTLS")}>
            <Input id="dir-url" value={s.url} onChange={(e) => set("url", e.target.value)} placeholder="ldaps://dc.example.com" autoComplete="off" spellCheck={false} />
          </Field>
          {plain && (
            <div className="flex items-center gap-3">
              <Toggle checked={s.starttls} onChange={(v) => set("starttls", v)} label={t("Use StartTLS")} />
              <div className="text-sm">
                {t("Use StartTLS")}
                <p className="text-xs text-muted-foreground">{t("Without it, the connection is unencrypted and only allowed to a directory on the private network.")}</p>
              </div>
            </div>
          )}
          <Field
            id="dir-ca"
            label={t("Certificate of the authority (PEM)")}
            hint={t("Only when the directory's certificate was made by an authority of your own; leave it empty for a public one")}
          >
            <Textarea
              id="dir-ca"
              rows={4}
              value={s.ca_pem}
              onChange={(e) => set("ca_pem", e.target.value)}
              spellCheck={false}
              className="font-mono text-xs"
              placeholder="-----BEGIN CERTIFICATE-----"
            />
          </Field>
        </div>
      </Section>

      <Section title={t("Service account")}>
        <div className="grid gap-3 p-4">
          <p className="text-sm text-muted-foreground">{t("An account that may search the directory. People's passwords are checked by signing in as them, not by this account.")}</p>
          <Field id="dir-bind-dn" label={t("Name (DN)")}>
            <Input id="dir-bind-dn" value={s.bind_dn} onChange={(e) => set("bind_dn", e.target.value)} placeholder="cn=reader,dc=example,dc=com" autoComplete="off" spellCheck={false} />
          </Field>
          <Field id="dir-bind-password" label={t("Password")} hint={info.has_bind_password ? t("One is saved. Type a new one to replace it.") : undefined}>
            <SecretInput
              id="dir-bind-password"
              value={s.bind_password}
              onChange={(e) => set("bind_password", e.target.value)}
              revealLabel={t("Show password")}
              autoComplete="new-password"
              placeholder={info.has_bind_password ? "••••••••" : undefined}
            />
          </Field>
        </div>
      </Section>

      <Section title={t("People")}>
        <div className="grid gap-3 p-4">
          <Field id="dir-user-base" label={t("Search base")}>
            <Input
              id="dir-user-base"
              value={s.user_base}
              onChange={(e) => set("user_base", e.target.value)}
              placeholder="ou=people,dc=example,dc=com"
              autoComplete="off"
              spellCheck={false}
            />
          </Field>
          <Field id="dir-user-filter" label={t("Filter that finds a person")} hint={t("{username} stands for what the person typed")}>
            <Input id="dir-user-filter" value={s.user_filter} onChange={(e) => set("user_filter", e.target.value)} className="font-mono text-xs" autoComplete="off" spellCheck={false} />
          </Field>
          <div className="grid gap-3 sm:grid-cols-2">
            <Field id="dir-attr-id" label={t("Attribute with the stable id")} hint={t("objectGUID in Active Directory, entryUUID in OpenLDAP")}>
              <Input id="dir-attr-id" value={s.attr_id} onChange={(e) => set("attr_id", e.target.value)} autoComplete="off" spellCheck={false} />
            </Field>
            <Field id="dir-attr-username" label={t("Attribute with the user name")}>
              <Input id="dir-attr-username" value={s.attr_username} onChange={(e) => set("attr_username", e.target.value)} autoComplete="off" spellCheck={false} />
            </Field>
            <Field id="dir-attr-name" label={t("Attribute with the name")}>
              <Input id="dir-attr-name" value={s.attr_name} onChange={(e) => set("attr_name", e.target.value)} autoComplete="off" spellCheck={false} />
            </Field>
            <Field id="dir-attr-email" label={t("Attribute with the email address")}>
              <Input id="dir-attr-email" value={s.attr_email} onChange={(e) => set("attr_email", e.target.value)} autoComplete="off" spellCheck={false} />
            </Field>
          </div>
          <div className="flex items-center gap-3">
            <Toggle checked={s.create_accounts} onChange={(v) => set("create_accounts", v)} label={t("Create an account at the first sign-in")} />
            <div className="text-sm">
              {t("Create an account at the first sign-in")}
              <p className="text-xs text-muted-foreground">{t("Otherwise only people who already have an account here can sign in.")}</p>
            </div>
          </div>
        </div>
      </Section>

      <Section title={t("Groups")}>
        <div className="grid gap-3 p-4">
          <Field id="dir-group-mode" label={t("How a person's groups are found")}>
            <NativeSelect id="dir-group-mode" size="lg" value={s.group_mode} onChange={(e) => set("group_mode", e.target.value as DirectoryGroupMode)}>
              <option value="none">{t("Not at all")}</option>
              <option value="member_of">{t("From the person's own memberOf values")}</option>
              <option value="search">{t("By searching the groups that have the person as a member")}</option>
            </NativeSelect>
          </Field>
          {s.group_mode === "search" && (
            <>
              <Field id="dir-group-base" label={t("Search base for groups")}>
                <Input
                  id="dir-group-base"
                  value={s.group_base}
                  onChange={(e) => set("group_base", e.target.value)}
                  placeholder="ou=groups,dc=example,dc=com"
                  autoComplete="off"
                  spellCheck={false}
                />
              </Field>
              <Field
                id="dir-group-filter"
                label={t("Filter that finds a person's groups")}
                hint={t("{dn} stands for the person's DN. For nested groups in Active Directory: (member:1.2.840.113556.1.4.1941:={dn})")}
              >
                <Input
                  id="dir-group-filter"
                  value={s.group_filter}
                  onChange={(e) => set("group_filter", e.target.value)}
                  className="font-mono text-xs"
                  autoComplete="off"
                  spellCheck={false}
                />
              </Field>
            </>
          )}
          {s.group_mode !== "none" && (
            <>
              <Field id="dir-require" label={t("Only members of this group may sign in")} hint={t("The group's DN; empty: everyone found")}>
                <Input id="dir-require" value={s.require_group} onChange={(e) => set("require_group", e.target.value)} autoComplete="off" spellCheck={false} />
              </Field>
              <div className="grid gap-2">
                <div className="text-sm font-medium">{t("Groups that follow a group of the directory")}</div>
                <p className="text-xs text-muted-foreground">
                  {t("Their members who sign in through the directory are put in and taken out as the directory says, and can't be changed here. Others added by hand stay.")}
                </p>
                {s.mappings.map((m, i) => (
                  <div key={i} className="flex flex-wrap items-center gap-2">
                    <Input
                      aria-label={t("DN of the directory group")}
                      className="min-w-60 flex-1"
                      value={m.dn}
                      onChange={(e) => setMapping(i, { dn: e.target.value })}
                      placeholder="cn=staff,ou=groups,dc=example,dc=com"
                      spellCheck={false}
                    />
                    <NativeSelect aria-label={t("Group here")} size="lg" value={m.group_id || ""} onChange={(e) => setMapping(i, { group_id: Number(e.target.value) })}>
                      <option value="">{t("Choose…")}</option>
                      {(groups.data ?? []).map((g) => (
                        <option key={g.id} value={g.id}>
                          {g.name}
                        </option>
                      ))}
                    </NativeSelect>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("Remove")}
                      title={t("Remove")}
                      onClick={() =>
                        set(
                          "mappings",
                          s.mappings.filter((_, j) => j !== i),
                        )
                      }
                    >
                      <XIcon />
                    </Button>
                  </div>
                ))}
                <Button type="button" variant="outline" size="sm" className="w-fit" onClick={() => set("mappings", [...s.mappings, { dn: "", group_id: 0 }])}>
                  <PlusIcon /> {t("Add a group")}
                </Button>
              </div>
            </>
          )}
        </div>
      </Section>

      <Section title={t("Test")}>
        <div className="grid gap-3 p-4">
          <p className="text-sm text-muted-foreground">
            {t("Tries the settings as they are typed here: the connection, the service account and, for the name below, the person and their groups.")}
          </p>
          <div className="flex flex-wrap items-end gap-2">
            <Field id="dir-test-user" label={t("A user name to look for")}>
              <Input id="dir-test-user" value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="off" spellCheck={false} />
            </Field>
            <Button type="button" variant="outline" disabled={testing} onClick={() => void test()}>
              {testing && <Loader2Icon className="animate-spin" />}
              {t("Test")}
            </Button>
          </div>
          {steps && (
            <ol className="grid gap-2" aria-label={t("Test")}>
              {steps.map((step) => (
                <li key={step.step} className="flex gap-2 text-sm">
                  {step.ok ? (
                    <CheckCircle2Icon className="mt-0.5 size-4 shrink-0 text-emerald-600" aria-label={t("Worked")} />
                  ) : (
                    <XCircleIcon className="mt-0.5 size-4 shrink-0 text-destructive" aria-label={t("Failed")} />
                  )}
                  <div className="min-w-0">
                    <div className="font-medium">{stepName(step.step)}</div>
                    <pre className="font-sans text-xs whitespace-pre-wrap text-muted-foreground [overflow-wrap:anywhere]">{step.detail}</pre>
                  </div>
                </li>
              ))}
            </ol>
          )}
        </div>
      </Section>

      <Section title={t("Sync")}>
        <div className="grid gap-3 p-4">
          <p className="text-sm text-muted-foreground">
            {t(
              "Every 15 minutes, groups follow the directory, and a person who is gone or turned off there has their account turned off and their sessions ended. Accounts are never deleted, and their files stay.",
            )}
          </p>
          {report && (
            <p className="text-xs text-muted-foreground">
              {t("Last run {time}: {checked}, {off} turned off, {on} turned on, {groups} group memberships changed", {
                time: formatDateTime(report.at),
                checked: t("{n} person checked|{n} people checked", { n: report.checked }),
                off: report.turned_off,
                on: report.turned_on,
                groups: report.groups_changed,
              })}
            </p>
          )}
          {report?.error && <ErrorText>{report.error}</ErrorText>}
          <Button type="button" variant="outline" className="w-fit" disabled={syncing || !info.settings.enabled} onClick={() => void sync()}>
            {syncing && <Loader2Icon className="animate-spin" />}
            {t("Sync now")}
          </Button>
        </div>
      </Section>

      <div className="grid gap-3">
        {identity.fields(t("For your security, this can only be changed within 10 minutes of signing in."))}
        <ErrorText>{error}</ErrorText>
        <div>
          <Button type="submit" disabled={busy || !dirty || !identity.ready}>
            {busy && <Loader2Icon className="animate-spin" />}
            {t("Save")}
          </Button>
        </div>
      </div>
    </form>
  );
}

/** The name of a step of the test, as the server says it (English) */
function stepName(step: string) {
  switch (step) {
    case "Settings":
      return t("Settings");
    case "Connect":
      return t("Connect");
    case "Service account":
      return t("Service account");
    case "Find the person":
      return t("Find the person");
    case "Groups":
      return t("Groups");
    default:
      return step;
  }
}
