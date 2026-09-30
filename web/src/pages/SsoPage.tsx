import { useState, useEffect, useRef } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyIcon, Loader2Icon, PlusIcon, Trash2Icon, TriangleAlertIcon } from "lucide-react";
import { Link } from "react-router";
import { toast } from "sonner";
import { api, type SsoDomainRule, type SsoProviderReq, type SsoProvisioning, type SsoSettings, type SsoSettingsReq } from "@/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Pending } from "@/components/ErrorState";
import { LocationSelect } from "@/components/LocationSelect";
import { ProviderIcon, SSO_LABEL, type SsoProviderId } from "@/components/ProviderIcon";
import { copyText } from "@/lib/utils";
import { t } from "@/lib/i18n";
import { Section, SettingsFrame, Toggle } from "@/pages/SettingsFrame";

const PROVIDERS: SsoProviderId[] = ["microsoft", "google", "github", "oidc"];

/** Steps for creating an app with each provider */
const GUIDE: Record<SsoProviderId, string> = {
  microsoft: t("Microsoft Entra admin center › App registrations › New registration. Select the \"Web\" platform and enter the redirect URI below, then add a client secret under \"Certificates & secrets\". The Application (client) ID and Directory (tenant) ID are on the \"Overview\" page."),
  google: t("Google Cloud Console › APIs & Services › Credentials › Create credentials › OAuth client ID. Choose \"Web application\" as the application type and add the URL below under \"Authorized redirect URIs\". If this is your first time, configure the OAuth consent screen first."),
  github: t("GitHub › Settings › Developer settings › OAuth Apps › New OAuth App. Enter the URL below as the Authorization callback URL, then click Generate a new client secret after the app is created. For company use, create the app under your organization's settings."),
  oidc: t("Any OpenID Connect provider, such as Keycloak, Authentik, Authelia or Zitadel: create a confidential client (web application) with the redirect URI below, and enter its issuer URL, client ID and secret. The provider must send a verified email address."),
};

const GB = 1024 ** 3;

function toReq(s: SsoSettings): SsoSettingsReq {
  const p = (id: SsoProviderId): SsoProviderReq => ({
    enabled: s[id].enabled,
    client_id: s[id].client_id,
    client_secret: "",
    tenant: s[id].tenant,
    name: s[id].name,
    issuer: s[id].issuer,
    provisioning: s[id].provisioning,
    allowed_domains: s[id].allowed_domains,
    defaults: s[id].defaults,
    groups: s[id].groups,
  });
  return { microsoft: p("microsoft"), google: p("google"), github: p("github"), oidc: p("oidc"), allowed_domains: s.allowed_domains, domain_rules: s.domain_rules };
}

/** A rule as edited: the domain and the size are typed as text */
interface RuleDraft {
  /** Stable key for the card: removing a rule mustn't move focus or IME state into the next card */
  key: string;
  domain: string;
  can_write: boolean;
  can_delete: boolean;
  can_share: boolean;
  quotaGb: string;
  groups: number[];
  /** "My files": "" = the system setting, "yes" or "no" */
  personal: "" | "yes" | "no";
  /** Its storage location; "" = the system setting's */
  location: string;
}

const toGb = (bytes: number | null) => (bytes === null ? "" : String(+(bytes / GB).toFixed(2)));
const ruleKey = () => Math.random().toString(36).slice(2);
const newRule = (): RuleDraft => ({ key: ruleKey(), domain: "", can_write: true, can_delete: true, can_share: true, quotaGb: "", groups: [], personal: "", location: "" });
const ruleDraft = (r: SsoDomainRule): RuleDraft => ({
  key: ruleKey(),
  domain: r.domain,
  can_write: r.can_write,
  can_delete: r.can_delete,
  can_share: r.can_share,
  quotaGb: toGb(r.quota_bytes),
  groups: r.groups,
  personal: r.personal_space === null || r.personal_space === undefined ? "" : r.personal_space ? "yes" : "no",
  location: r.personal_location ?? "",
});
const ruleReq = (d: RuleDraft): SsoDomainRule => {
  const q = d.quotaGb.trim();
  const bytes = q === "" ? null : Math.max(0, Math.round(Number(q) * GB));
  return {
    domain: d.domain.trim().toLowerCase(),
    can_write: d.can_write,
    can_delete: d.can_delete,
    can_share: d.can_share,
    quota_bytes: Number.isFinite(bytes ?? 0) ? bytes : null,
    groups: d.groups,
    personal_space: d.personal === "" ? null : d.personal === "yes",
    // A location only matters when the accounts get a personal space
    personal_location: d.personal === "no" ? null : d.location || null,
  };
};

const splitDomains = (text: string) => text.split(/[,\s;]+/).filter(Boolean);

const PROVISIONING: { value: SsoProvisioning; label: string; help: string }[] = [
  { value: "off", label: t("Linked accounts only"), help: t("People sign in with their password first and link the account under \"Sign-in methods\".") },
  { value: "link", label: t("Match existing users by email"), help: t("Users whose username is their verified email sign in directly; nobody else.") },
  { value: "create", label: t("Create accounts automatically"), help: t("Like the above, and people without an account get one on first sign-in, with the settings below.") },
];

export function SsoPage() {
  const q = useQuery({ queryKey: ["sso-settings"], queryFn: api.ssoSettings });
  return (
    <SettingsFrame item="sso" onRefresh={() => q.refetch()}>
      {q.data ? <SsoForm saved={q.data} /> : <Pending query={q} loading={<Skeleton className="h-60" />} />}
    </SettingsFrame>
  );
}

/** The text fields of the form for the given settings (lists and sizes are edited as text, converted when saving) */
const fieldsOf = (saved: SsoSettings) => ({
  domains: saved.allowed_domains.join(", "),
  providerDomains: Object.fromEntries(PROVIDERS.map((id) => [id, saved[id].allowed_domains.join(", ")])) as Record<SsoProviderId, string>,
  quotaGb: Object.fromEntries(PROVIDERS.map((id) => [id, toGb(saved[id].defaults.quota_bytes)])) as Record<SsoProviderId, string>,
});

function SsoForm({ saved }: { saved: SsoSettings }) {
  const qc = useQueryClient();
  const [draft, setDraft] = useState<SsoSettingsReq>(() => toReq(saved));
  const [domains, setDomains] = useState(() => fieldsOf(saved).domains);
  const [providerDomains, setProviderDomains] = useState(() => fieldsOf(saved).providerDomains);
  const [quotaGb, setQuotaGb] = useState(() => fieldsOf(saved).quotaGb);
  const groups = useQuery({ queryKey: ["groups"], queryFn: api.groups });
  const [rules, setRules] = useState<RuleDraft[]>(() => saved.domain_rules.map(ruleDraft));
  /** Show these settings in the form, dropping what was typed */
  const reset = (to: SsoSettings) => {
    const f = fieldsOf(to);
    setDraft(toReq(to));
    setDomains(f.domains);
    setProviderDomains(f.providerDomains);
    setQuotaGb(f.quotaGb);
    setRules(to.domain_rules.map(ruleDraft));
  };
  const setRule = (i: number, patch: Partial<RuleDraft>) => setRules((list) => list.map((r, k) => (k === i ? { ...r, ...patch } : r)));
  const setProvider = (id: SsoProviderId, patch: Partial<SsoProviderReq>) => setDraft((d) => ({ ...d, [id]: { ...d[id], ...patch } }));
  const withPolicy = (id: SsoProviderId): SsoProviderReq => {
    const q = quotaGb[id].trim();
    const quota_bytes = q === "" ? null : Math.max(0, Math.round(Number(q) * GB));
    return { ...draft[id], allowed_domains: splitDomains(providerDomains[id]), defaults: { ...draft[id].defaults, quota_bytes: Number.isFinite(quota_bytes ?? 0) ? quota_bytes : null } };
  };
  const req: SsoSettingsReq = {
    microsoft: withPolicy("microsoft"),
    google: withPolicy("google"),
    github: withPolicy("github"),
    oidc: withPolicy("oidc"),
    allowed_domains: splitDomains(domains),
    domain_rules: rules.filter((r) => r.domain.trim()).map(ruleReq),
  };
  // A blank rule card counts as a change too, so it can be discarded
  const dirty = JSON.stringify(req) !== JSON.stringify(toReq(saved)) || rules.some((r) => !r.domain.trim());
  // Space sizes must be blank, or a number of GB that isn't negative
  const badQuota = (q: string) => {
    const v = q.trim();
    return v !== "" && (!/^\d+(\.\d+)?$/.test(v) || Number(v) < 0);
  };
  // Rules without a domain are dropped when saving, so their quota field can't make the form invalid
  const invalid = PROVIDERS.some((id) => badQuota(quotaGb[id])) || rules.some((r) => r.domain.trim() && badQuota(r.quotaGb));

  // Settings refreshed from the server (the Refresh button, or someone else saved): shown when nothing was typed;
  // with unsaved edits the form is left alone
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const shown = useRef(saved);
  useEffect(() => {
    if (shown.current === saved) return;
    shown.current = saved;
    if (!dirtyRef.current) reset(saved);
    // reset only sets state
  }, [saved]);

  const save = useMutation({
    mutationFn: () => api.updateSsoSettings(req),
    onSuccess: (data) => {
      // What the server stored: secrets are blank again, domains are trimmed and lower-cased
      shown.current = data;
      reset(data);
      qc.setQueryData(["sso-settings"], data);
      qc.invalidateQueries({ queryKey: ["sso-providers"] });
      toast.success(t("Single sign-on settings updated"));
    },
    onError: (e) => toast.error(e.message),
  });

  return (
    <>
      {!saved.public_url_set && (
        <p className="flex items-start gap-2 rounded-lg border border-amber-500/40 bg-amber-500/10 p-3 text-xs leading-relaxed text-amber-800 dark:text-amber-200">
          <TriangleAlertIcon className="mt-px size-4 shrink-0" />
          <span>
            {(() => {
              const [before, after] = t("The site URL isn't set, so the redirect URIs below are based on your browser's current address. Before going live, set the site URL in {link}, then enter the URI in the provider's app settings.").split("{link}");
              return (
                <>
                  {before}
                  <Link to="/admin/general" className="mx-0.5 underline">
                    {t("General settings")}
                  </Link>
                  {after}
                </>
              );
            })()}
          </span>
        </p>
      )}

      <Section title={t("Sign-in policy")}>
        <div className="grid gap-5 p-4">
          <div className="grid gap-1.5">
            <Label htmlFor="sso-domains">{t("Allow only these email domains")}</Label>
            <Input id="sso-domains" value={domains} onChange={(e) => setDomains(e.target.value)} placeholder={t("e.g. example.com (separate multiple domains with commas; leave blank for no restriction)")} />
            <p className="text-xs text-muted-foreground">
              {t("Enter your company domain to prevent personal Google or GitHub accounts from signing in or being created automatically. Already linked accounts aren't affected. A provider with its own domain list below uses that list instead.")}
            </p>
          </div>
          <p className="text-xs leading-relaxed text-muted-foreground">
            {t("What happens on first sign-in, the permissions of new accounts and the groups they join are set per provider below. To limit damage from a misconfiguration, a provider creates at most {n} accounts per hour.", { n: saved.max_created_per_hour })}
          </p>
        </div>
      </Section>

      <Section title={t("Rules by email domain")}>
        <div className="grid gap-4 p-4">
          <p className="text-xs leading-relaxed text-muted-foreground">
            {t("Accounts created automatically for an email domain listed here get these settings instead of the provider's defaults (for example, fewer permissions and a smaller space for partners). A rule doesn't allow a domain to sign in by itself: the domain must also be in an allowed list, or the lists must be empty.")}
          </p>
          {rules.map((r, i) => (
            <div key={r.key} className="grid gap-3 rounded-lg border p-3">
              <div className="flex items-center gap-2">
                <Input value={r.domain} onChange={(e) => setRule(i, { domain: e.target.value })} placeholder={t("e.g. partner.example")} className="max-w-xs" autoComplete="off" />
                <span className="flex-1" />
                <Button type="button" variant="ghost" size="icon" title={t("Remove rule")} onClick={() => setRules((list) => list.filter((_, k) => k !== i))}>
                  <Trash2Icon />
                </Button>
              </div>
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="grid gap-2">
                  <div className="text-[13px] font-medium">{t("New accounts may")}</div>
                  {(["can_write", "can_delete", "can_share"] as const).map((k) => (
                    <label key={k} className="flex items-center justify-between gap-3 text-sm">
                      <span>{k === "can_write" ? t("Edit files") : k === "can_delete" ? t("Delete files") : t("Share files")}</span>
                      <Toggle label={k} checked={r[k]} onChange={(v) => setRule(i, { [k]: v })} />
                    </label>
                  ))}
                </div>
                <div className="grid content-start gap-3">
                  <div className="grid gap-1.5">
                    <Label htmlFor={`rule-${i}-quota`}>{t("Space size of new accounts (GB)")}</Label>
                    <Input id={`rule-${i}-quota`} inputMode="decimal" value={r.quotaGb} onChange={(e) => setRule(i, { quotaGb: e.target.value })} placeholder={t("Blank = the default for new users; 0 = unlimited")} />
                  </div>
                  <div className="grid gap-1.5">
                    <Label htmlFor={`rule-${i}-personal`}>{t("\"My files\" of new accounts")}</Label>
                    <select
                      id={`rule-${i}-personal`}
                      className="h-9 rounded-md border bg-background px-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
                      value={r.personal}
                      onChange={(e) => setRule(i, { personal: e.target.value as RuleDraft["personal"] })}
                    >
                      <option value="">{t("As in the system settings")}</option>
                      <option value="yes">{t("Create")}</option>
                      <option value="no">{t("Don't create")}</option>
                    </select>
                    {r.personal !== "no" && (
                      <LocationSelect
                        aria-label={t("Storage location of \"My files\"")}
                        value={r.location}
                        onChange={(location) => setRule(i, { location })}
                        blank={t("Location as in the system settings")}
                      />
                    )}
                  </div>
                  <div className="grid gap-1.5">
                    <div className="text-[13px] font-medium">{t("Add new accounts to these groups")}</div>
                    {groups.data?.length ? (
                      <div className="flex flex-wrap gap-2">
                        {groups.data.map((g) => {
                          const on = r.groups.includes(g.id);
                          return (
                            <button
                              key={g.id}
                              type="button"
                              aria-pressed={on}
                              onClick={() => setRule(i, { groups: on ? r.groups.filter((x) => x !== g.id) : [...r.groups, g.id] })}
                              className={on ? "rounded-full border border-brand bg-brand/10 px-2.5 py-0.5 text-xs text-brand" : "rounded-full border px-2.5 py-0.5 text-xs text-muted-foreground hover:bg-muted"}
                            >
                              {g.name}
                            </button>
                          );
                        })}
                      </div>
                    ) : (
                      <p className="text-xs text-muted-foreground">{t("No groups yet. Create them under Groups first.")}</p>
                    )}
                  </div>
                </div>
              </div>
            </div>
          ))}
          <div>
            <Button type="button" variant="outline" size="sm" onClick={() => setRules((list) => [...list, newRule()])}>
              <PlusIcon />
              {t("Add rule")}
            </Button>
          </div>
        </div>
      </Section>

      {PROVIDERS.map((id) => (
        <Section key={id} title={SSO_LABEL[id]}>
          <div className="grid gap-4 p-4">
            <div className="flex items-center gap-3">
              <span className="flex size-8 items-center justify-center rounded-lg border bg-background">
                <ProviderIcon provider={id} className="size-[18px]" />
              </span>
              <div className="min-w-0 flex-1">
                <div className="text-[13px] font-medium">
                  {t("Sign in with {name}", { name: id === "microsoft" ? "Microsoft Entra ID" : id === "oidc" ? draft.oidc.name.trim() || SSO_LABEL.oidc : SSO_LABEL[id] })}
                </div>
                <p className="text-xs text-muted-foreground">{draft[id].enabled ? t("On: this button appears on the sign-in page") : t("Not enabled")}</p>
              </div>
              <Toggle label={t("Turn on sign-in with {name}", { name: SSO_LABEL[id] })} checked={draft[id].enabled} onChange={(v) => setProvider(id, { enabled: v })} />
            </div>
            <p className="text-xs leading-relaxed text-muted-foreground">{GUIDE[id]}</p>
            {id === "oidc" && (
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="grid gap-1.5">
                  <Label htmlFor="sso-oidc-name">{t("Name on the sign-in button")}</Label>
                  <Input id="sso-oidc-name" value={draft.oidc.name} maxLength={40} placeholder={t("e.g. Company login")} onChange={(e) => setProvider("oidc", { name: e.target.value })} autoComplete="off" />
                </div>
                <div className="grid gap-1.5">
                  <Label htmlFor="sso-oidc-issuer">{t("Issuer URL")}</Label>
                  <Input
                    id="sso-oidc-issuer"
                    value={draft.oidc.issuer}
                    placeholder="https://auth.example.com/realms/staff"
                    onChange={(e) => setProvider("oidc", { issuer: e.target.value })}
                    autoComplete="off"
                  />
                </div>
              </div>
            )}
            <div className="grid gap-1.5">
              <Label>{t("Redirect URI")}</Label>
              <div className="flex gap-2">
                <Input readOnly value={saved[id].redirect_uri} className="font-mono text-xs" onFocus={(e) => e.target.select()} />
                <Button
                  type="button"
                  variant="outline"
                  size="icon"
                  title={t("Copy")}
                  onClick={async () => {
                    await copyText(saved[id].redirect_uri);
                    toast.success(t("Redirect URI copied"));
                  }}
                >
                  <CopyIcon />
                </Button>
              </div>
            </div>
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="grid gap-1.5">
                <Label htmlFor={`sso-${id}-id`}>{id === "microsoft" ? t("Application (client) ID") : "Client ID"}</Label>
                <Input id={`sso-${id}-id`} value={draft[id].client_id} onChange={(e) => setProvider(id, { client_id: e.target.value })} autoComplete="off" />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor={`sso-${id}-secret`}>{id === "microsoft" ? t("Client secret") : "Client Secret"}</Label>
                <Input
                  id={`sso-${id}-secret`}
                  type="password"
                  value={draft[id].client_secret}
                  onChange={(e) => setProvider(id, { client_secret: e.target.value })}
                  placeholder={saved[id].has_secret ? t("Already set; leave blank to keep it") : ""}
                  autoComplete="new-password"
                />
              </div>
            </div>
            <div className="grid gap-3 border-t pt-4">
              <div className="grid gap-1.5">
                <Label htmlFor={`sso-${id}-provisioning`}>{t("Accounts that aren't linked yet")}</Label>
                <select
                  id={`sso-${id}-provisioning`}
                  value={draft[id].provisioning}
                  onChange={(e) => setProvider(id, { provisioning: e.target.value as SsoProvisioning })}
                  className="h-9 rounded-md border border-input bg-background px-3 text-sm shadow-xs outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring"
                >
                  {PROVISIONING.map((o) => (
                    <option key={o.value} value={o.value}>
                      {o.label}
                    </option>
                  ))}
                </select>
                <p className="text-xs text-muted-foreground">{PROVISIONING.find((o) => o.value === draft[id].provisioning)?.help}</p>
                {draft[id].provisioning === "create" && splitDomains(providerDomains[id]).length === 0 && splitDomains(domains).length === 0 && (
                  <p className="flex items-start gap-1.5 text-xs text-amber-700 dark:text-amber-300">
                    <TriangleAlertIcon className="mt-px size-3.5 shrink-0" />
                    {t("No domain restriction: anyone with a {name} account can create an account on this site.", { name: SSO_LABEL[id] })}
                  </p>
                )}
              </div>
              {draft[id].provisioning !== "off" && (
                <div className="grid gap-1.5">
                  <Label htmlFor={`sso-${id}-domains`}>{t("Email domains for this provider")}</Label>
                  <Input
                    id={`sso-${id}-domains`}
                    value={providerDomains[id]}
                    onChange={(e) => setProviderDomains((d) => ({ ...d, [id]: e.target.value }))}
                    placeholder={t("Leave blank to use the global list above")}
                  />
                </div>
              )}
              {draft[id].provisioning === "create" && (
                <>
                  <div className="grid gap-2">
                    <div className="text-[13px] font-medium">{t("New accounts may")}</div>
                    {(["can_write", "can_delete", "can_share"] as const).map((k) => (
                      <label key={k} className="flex items-center justify-between gap-3 text-sm">
                        <span>{k === "can_write" ? t("Edit files") : k === "can_delete" ? t("Delete files") : t("Share files")}</span>
                        <Toggle label={k} checked={draft[id].defaults[k]} onChange={(v) => setProvider(id, { defaults: { ...draft[id].defaults, [k]: v } })} />
                      </label>
                    ))}
                  </div>
                  <div className="grid gap-1.5">
                    <Label htmlFor={`sso-${id}-quota`}>{t("Space size of new accounts (GB)")}</Label>
                    <Input
                      id={`sso-${id}-quota`}
                      inputMode="decimal"
                      value={quotaGb[id]}
                      onChange={(e) => setQuotaGb((q) => ({ ...q, [id]: e.target.value }))}
                      placeholder={t("Blank = the default for new users; 0 = unlimited")}
                    />
                  </div>
                  <div className="grid gap-1.5">
                    <div className="text-[13px] font-medium">{t("Add new accounts to these groups")}</div>
                    {groups.data?.length ? (
                      <div className="flex flex-wrap gap-2">
                        {groups.data.map((g) => {
                          const on = draft[id].groups.includes(g.id);
                          return (
                            <button
                              key={g.id}
                              type="button"
                              aria-pressed={on}
                              onClick={() => setProvider(id, { groups: on ? draft[id].groups.filter((x) => x !== g.id) : [...draft[id].groups, g.id] })}
                              className={
                                on
                                  ? "rounded-full border border-brand bg-brand/10 px-2.5 py-0.5 text-xs text-brand"
                                  : "rounded-full border px-2.5 py-0.5 text-xs text-muted-foreground hover:bg-muted"
                              }
                            >
                              {g.name}
                            </button>
                          );
                        })}
                      </div>
                    ) : (
                      <p className="text-xs text-muted-foreground">{t("No groups yet. Create them under Groups first.")}</p>
                    )}
                  </div>
                </>
              )}
            </div>
            {id === "microsoft" && (
              <div className="grid gap-1.5">
                <Label htmlFor="sso-microsoft-tenant">{t("Directory (tenant) ID")}</Label>
                <Input
                  id="sso-microsoft-tenant"
                  value={draft.microsoft.tenant}
                  onChange={(e) => setProvider("microsoft", { tenant: e.target.value })}
                  placeholder={t("e.g. 00000000-0000-0000-0000-000000000000 or contoso.onmicrosoft.com")}
                  autoComplete="off"
                />
                <p className="text-xs text-muted-foreground">
                  {t("Recommended: only your company's Microsoft accounts can sign in, and accounts can be matched automatically by email. If left blank, accounts from any organization can try to sign in; because other organizations can set any email, users must then sign in first and link their account manually.")}
                </p>
              </div>
            )}
          </div>
        </Section>
      ))}

      <div className="sticky bottom-0 -mx-6 -mb-6 flex items-center gap-2 border-t bg-background/95 px-6 py-3 backdrop-blur">
        <span className="text-xs text-muted-foreground">{dirty ? t("You have unsaved changes") : t("Secrets are stored only on the server and won't be shown again")}</span>
        <span className="flex-1" />
        {dirty && (
          <Button
            variant="outline"
            size="sm"
            onClick={() => reset(saved)}
          >
            {t("Discard changes")}
          </Button>
        )}
        {invalid && <span className="text-xs text-destructive">{t("Enter the space size as a number of GB (blank = default, 0 = unlimited)")}</span>}
        <Button size="sm" disabled={!dirty || invalid || save.isPending} onClick={() => save.mutate()}>
          {save.isPending && <Loader2Icon className="animate-spin" />}
          {t("Save")}
        </Button>
      </div>
    </>
  );
}
