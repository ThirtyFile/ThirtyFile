import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyIcon, KeySquareIcon, Loader2Icon, PlusIcon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { api, type AppPassword } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ErrorText } from "@/components/dialogs";
import { useMe } from "@/lib/session";
import { copyText, formatDate, formatDateTime } from "@/lib/utils";
import { t } from "@/lib/i18n";

const selectCls = "h-9 rounded-md border bg-background px-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring/50";

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

/** Account menu › App passwords: tokens for scripts, backups and file clients (shown once when created) */
export function AppPasswordsDialog({ onClose }: { onClose(): void }) {
  const me = useMe();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["app-passwords"], queryFn: api.appPasswords });
  const [name, setName] = useState("");
  const [scope, setScope] = useState<"read" | "write">("read");
  const [days, setDays] = useState(90);
  /** The token just created: shown until the dialog closes */
  const [created, setCreated] = useState<{ name: string; token: string } | null>(null);

  const create = useMutation({
    mutationFn: () => api.createAppPassword({ name: name.trim(), scope, expires_days: days || undefined }),
    onSuccess: (r) => {
      setCreated({ name: r.app_password.name, token: r.token });
      setName("");
      qc.invalidateQueries({ queryKey: ["app-passwords"] });
    },
  });
  const remove = useMutation({
    mutationFn: (p: AppPassword) => api.deleteAppPassword(p.id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["app-passwords"] });
      toast.success(t("App password removed"));
    },
    onError: (e) => toast.error(e.message),
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (name.trim() && !create.isPending) create.mutate();
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("App passwords")}</DialogTitle>
          <DialogDescription>
            {t("For scripts, backup tools and file clients, including accounts that sign in with Microsoft, Google or GitHub. They work for files only, not for your account settings, sharing or administration, and don't ask for a two-factor code.")}
          </DialogDescription>
        </DialogHeader>

        {created ? (
          <div className="grid gap-2 rounded-lg border border-brand/40 bg-brand/5 p-3">
            <div className="text-sm font-medium">{t("App password \"{name}\" created", { name: created.name })}</div>
            <p className="text-xs text-muted-foreground">{t("Copy it now. It won't be shown again.")}</p>
            <div className="flex gap-2">
              <Input readOnly value={created.token} className="font-mono text-xs" aria-label={t("App password")} onFocus={(e) => e.target.select()} />
              <Button
                type="button"
                variant="outline"
                onClick={() => copyText(created.token).then(() => toast.success(t("Copied")))}
                title={t("Copy")}
                aria-label={t("Copy")}
              >
                <CopyIcon />
              </Button>
            </div>
            <p className="text-xs text-muted-foreground">
              {t("Send it as the header \"Authorization: Bearer <app password>\", or sign in with the username {username} and the app password as the password.", { username: me.username })}
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
            <div className="flex flex-wrap items-end gap-2">
              <div className="grid gap-1.5">
                <Label htmlFor="ap-scope">{t("Access")}</Label>
                <select id="ap-scope" className={selectCls} value={scope} onChange={(e) => setScope(e.target.value as "read" | "write")}>
                  <option value="read">{t("Read files only")}</option>
                  <option value="write">{t("Read and change files")}</option>
                </select>
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="ap-expiry">{t("Expires")}</Label>
                <select id="ap-expiry" className={selectCls} value={days} onChange={(e) => setDays(Number(e.target.value))}>
                  {EXPIRY.map((d) => (
                    <option key={d} value={d}>
                      {d === 0 ? t("Never") : t("In {n} day|In {n} days", { n: d })}
                    </option>
                  ))}
                </select>
              </div>
              <Button type="submit" className="ml-auto" disabled={!name.trim() || create.isPending}>
                {create.isPending ? <Loader2Icon className="animate-spin" /> : <PlusIcon />}
                {t("Create")}
              </Button>
            </div>
            <ErrorText>{create.error?.message}</ErrorText>
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
                      {p.scope === "read" ? t("Read only") : t("Read and write")}
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
