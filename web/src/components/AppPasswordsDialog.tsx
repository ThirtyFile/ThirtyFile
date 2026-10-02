import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyIcon, KeySquareIcon, Loader2Icon, PlusIcon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type AppPassword } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ErrorText, errorProps } from "@/components/dialogs";
import { useMe } from "@/lib/session";
import { copyAndSay, formatDate, formatDateTime } from "@/lib/utils";
import { t } from "@/lib/i18n";
import { NativeSelect } from "@/components/ui/native-select";

/** Expiry choices, in days (0 = never) */
const EXPIRY = [30, 90, 365, 0];

function describe(p: AppPassword) {
  const parts = [
    t("Created {date}", { date: formatDate(p.created_at) }),
    p.expires_at
      ? p.expires_at * 1000 < Date.now()
        ? t("Expired {date}", { date: formatDate(p.expires_at) })
        : t("Expires {date}", { date: formatDate(p.expires_at) })
      : t("Never expires"),
    p.last_used_at ? t("Last used {time} from {ip}", { time: formatDateTime(p.last_used_at), ip: p.last_ip || "—" }) : t("Never used"),
  ];
  return parts.join(" · ");
}

/** Where WebDAV clients connect: the site's public URL when one is set, else the address in use now */
function davAddress(publicUrl: string) {
  return `${(publicUrl || window.location.origin).replace(/\/+$/, "")}/dav/`;
}

/** Account menu › App passwords: tokens for scripts, backups and file clients (shown once when created) */
export function AppPasswordsDialog({ onClose }: { onClose(): void }) {
  const me = useMe();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: keys.appPasswords(), queryFn: api.appPasswords });
  // Making one asks for the password again (and a code with two-factor sign-in); accounts without a password must
  // have signed in recently instead
  const tf = useQuery(queries.twoFactor);
  const hasPassword = tf.data?.has_password ?? true;
  const needsCode = !!tf.data?.enabled;
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [name, setName] = useState("");
  const [scope, setScope] = useState<"read" | "write">("read");
  const [days, setDays] = useState(90);
  /** The token just created: shown until the dialog closes */
  const [created, setCreated] = useState<{ name: string; token: string } | null>(null);

  const create = useMutation({
    mutationFn: () =>
      api.createAppPassword({
        name: name.trim(),
        scope,
        expires_days: days || undefined,
        password: hasPassword ? password : undefined,
        code: needsCode ? code.trim() : undefined,
      }),
    onSuccess: (r) => {
      setCreated({ name: r.app_password.name, token: r.token });
      setName("");
      setPassword("");
      setCode("");
      qc.invalidateQueries({ queryKey: keys.notifications() });
      qc.invalidateQueries({ queryKey: keys.appPasswords() });
    },
  });
  const remove = useMutation({
    mutationFn: (p: AppPassword) => api.deleteAppPassword(p.id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: keys.appPasswords() });
      toast.success(t("App password removed"));
    },
    onError: (e) => toast.error(e.message),
  });
  const ready = !!name.trim() && (!hasPassword || !!password) && (!needsCode || !!code.trim());
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (ready && !create.isPending) create.mutate();
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("App passwords")}</DialogTitle>
          <DialogDescription>
            {t(
              "For scripts, backup tools and file clients, including accounts that use single sign-on (Microsoft, Google, GitHub or OpenID Connect). They work for files only, not for your account settings, sharing or administration, and don't ask for a two-factor code.",
            )}
          </DialogDescription>
        </DialogHeader>

        {created ? (
          <div className="grid gap-2 rounded-lg border border-brand/40 bg-brand/5 p-3">
            <div className="text-sm font-medium">{t('App password "{name}" created', { name: created.name })}</div>
            <p className="text-xs text-muted-foreground">{t("Copy it now. It won't be shown again.")}</p>
            <div className="flex gap-2">
              <Input readOnly value={created.token} className="font-mono text-xs" aria-label={t("App password")} onFocus={(e) => e.target.select()} />
              <Button type="button" variant="outline" onClick={() => copyAndSay(created.token)} title={t("Copy")} aria-label={t("Copy")}>
                <CopyIcon />
              </Button>
            </div>
            <p className="text-xs text-muted-foreground">
              {t('Send it as the header "Authorization: Bearer <app password>", or sign in with the username {username} and the app password as the password.', { username: me.username })}
            </p>
            <p className="text-xs text-muted-foreground">
              {t("To open your files as a drive in File Explorer, Finder or a phone's Files app, connect to {address} over WebDAV with this username and app password.", {
                address: davAddress(me.public_url),
              })}
            </p>
            <div className="flex justify-end">
              <Button type="button" size="sm" variant="outline" onClick={() => setCreated(null)}>
                {t("Done")}
              </Button>
            </div>
          </div>
        ) : (
          <form onSubmit={submit} className="grid gap-2 rounded-lg border p-3">
            <div className="grid gap-1.5">
              <Label htmlFor="ap-name">{t("Name")}</Label>
              <Input id="ap-name" value={name} maxLength={60} placeholder={t("For example: Nightly backup")} onChange={(e) => setName(e.target.value)} autoComplete="off" />
            </div>
            {hasPassword ? (
              <div className="flex flex-wrap gap-2">
                <div className="grid min-w-40 flex-1 gap-1.5">
                  <Label htmlFor="ap-password">{t("Your current password")}</Label>
                  <Input
                    id="ap-password"
                    type="password"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    autoComplete="current-password"
                    {...errorProps(create.error, "ap-error")}
                  />
                </div>
                {needsCode && (
                  <div className="grid w-40 gap-1.5">
                    <Label htmlFor="ap-code">{t("Code from your app")}</Label>
                    <Input id="ap-code" value={code} onChange={(e) => setCode(e.target.value)} inputMode="numeric" autoComplete="one-time-code" />
                  </div>
                )}
              </div>
            ) : (
              <p className="text-xs text-muted-foreground">{t("For your security, app passwords can only be created within 10 minutes of signing in.")}</p>
            )}
            <div className="flex flex-wrap items-end gap-2">
              <div className="grid gap-1.5">
                <Label htmlFor="ap-scope">{t("Access")}</Label>
                <NativeSelect id="ap-scope" size="lg" value={scope} onChange={(e) => setScope(e.target.value as "read" | "write")}>
                  <option value="read">{t("Read files only")}</option>
                  <option value="write">{t("Read and change files")}</option>
                </NativeSelect>
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="ap-expiry">{t("Expires")}</Label>
                <NativeSelect id="ap-expiry" size="lg" value={days} onChange={(e) => setDays(Number(e.target.value))}>
                  {EXPIRY.map((d) => (
                    <option key={d} value={d}>
                      {d === 0 ? t("Never") : t("In {n} day|In {n} days", { n: d })}
                    </option>
                  ))}
                </NativeSelect>
              </div>
              <Button type="submit" className="ml-auto" disabled={!ready || create.isPending}>
                {create.isPending ? <Loader2Icon className="animate-spin" /> : <PlusIcon />}
                {t("Create")}
              </Button>
            </div>
            <ErrorText id="ap-error">{create.error?.message}</ErrorText>
          </form>
        )}

        {q.isLoading ? (
          <Loader2Icon className="mx-auto size-5 animate-spin text-muted-foreground" />
        ) : !q.data?.length ? (
          <p className="text-sm text-muted-foreground">{t("You have no app passwords.")}</p>
        ) : (
          <div className="max-h-[40vh] divide-y overflow-y-auto rounded-lg border">
            {q.data.map((p) => (
              <div key={p.id} className="flex items-center gap-3 px-3 py-2.5">
                <KeySquareIcon className="size-5 shrink-0 text-muted-foreground" />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2 text-sm">
                    <span className="truncate">{p.name}</span>
                    <Badge variant="outline" className="h-4 px-1.5 text-[10px]">
                      {p.scope === "read" ? t("Read files only") : t("Read and change files")}
                    </Badge>
                  </div>
                  <div className="truncate text-xs text-muted-foreground" title={describe(p)}>
                    {describe(p)}
                  </div>
                </div>
                <Button variant="ghost" size="sm" disabled={remove.isPending} onClick={() => remove.mutate(p)} title={t("Scripts using it stop working right away")}>
                  <Trash2Icon /> {t("Remove")}
                </Button>
              </div>
            ))}
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
